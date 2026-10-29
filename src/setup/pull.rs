//! Image downloads through the Docker Engine API, which reports bytes per layer; `docker pull`
//! only prints them to a terminal, so setup couldn't show speed or time left.

use std::collections::HashMap;
use std::env;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

const DEFAULT_SOCKET: &str = "/var/run/docker.sock";

pub enum PullError {
    /// The socket refused this user: it isn't in the docker group in this login session yet.
    PermissionDenied,
    Failed(anyhow::Error),
}

impl From<anyhow::Error> for PullError {
    fn from(error: anyhow::Error) -> Self {
        Self::Failed(error)
    }
}

impl From<io::Error> for PullError {
    fn from(error: io::Error) -> Self {
        Self::Failed(error.into())
    }
}

#[derive(Deserialize)]
struct Event {
    id: Option<String>,
    error: Option<String>,
    status: Option<String>,
    #[serde(rename = "progressDetail")]
    detail: Option<Detail>,
}

#[derive(Deserialize)]
struct Detail {
    total: Option<u64>,
    current: Option<u64>,
}

#[derive(Default)]
struct Layer {
    total: u64,
    current: u64,
    is_done: bool,
}

/// Downloads `image`, calling `progress(done, total)` in bytes as layers arrive.
pub fn pull(image: &str, mut progress: impl FnMut(u64, u64)) -> Result<(), PullError> {
    let mut socket = match UnixStream::connect(socket_path()) {
        Ok(socket) => socket,
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            return Err(PullError::PermissionDenied);
        }
        Err(error) => {
            return Err(anyhow::Error::from(error)
                .context("the container engine isn't running")
                .into());
        }
    };
    let (repository, tag) = image.rsplit_once(':').unwrap_or((image, "latest"));
    // HTTP/1.0 makes the engine stream the body and close, with no chunked encoding to undo.
    write!(
        socket,
        "POST /images/create?fromImage={}&tag={} HTTP/1.0\r\nHost: docker\r\n\r\n",
        repository.replace('/', "%2F"),
        tag
    )?;
    let mut reader = BufReader::new(socket);
    let mut status = String::new();
    reader.read_line(&mut status)?;
    let mut header = String::new();
    while reader.read_line(&mut header)? > 2 {
        header.clear();
    }
    if !status
        .split_whitespace()
        .nth(1)
        .is_some_and(|code| code == "200")
    {
        let mut body = String::new();
        reader.read_to_string(&mut body)?;
        return Err(anyhow::anyhow!("{}: {}", status.trim(), body.trim()).into());
    }
    track(reader, &mut progress).map_err(PullError::Failed)
}

fn track(reader: impl BufRead, progress: &mut impl FnMut(u64, u64)) -> Result<()> {
    let mut layers: HashMap<String, Layer> = HashMap::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let event: Event = serde_json::from_str(&line)
            .with_context(|| format!("unexpected download progress: {line}"))?;
        if let Some(error) = event.error {
            bail!("{error}");
        }
        let (Some(id), Some(status)) = (event.id, event.status) else {
            continue;
        };
        let layer = layers.entry(id).or_default();
        match status.as_str() {
            "Downloading" => {
                if let Some(detail) = event.detail {
                    layer.total = detail.total.unwrap_or(layer.total);
                    layer.current = detail.current.unwrap_or(layer.current);
                }
            }
            "Download complete" | "Pull complete" | "Already exists" => layer.is_done = true,
            _ => continue,
        }
        let total: u64 = layers.values().map(|layer| layer.total).sum();
        let done: u64 = layers
            .values()
            .map(|layer| {
                if layer.is_done {
                    layer.total
                } else {
                    layer.current
                }
            })
            .sum();
        progress(done, total);
    }
    Ok(())
}

fn socket_path() -> String {
    env::var("DOCKER_HOST")
        .ok()
        .and_then(|host| host.strip_prefix("unix://").map(str::to_string))
        .unwrap_or_else(|| DEFAULT_SOCKET.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_bytes_across_layers() {
        let stream = r#"{"status":"Pulling from redroid/redroid","id":"15.0.0-latest"}
{"status":"Pulling fs layer","progressDetail":{},"id":"a"}
{"status":"Downloading","progressDetail":{"current":100,"total":1000},"id":"a"}
{"status":"Downloading","progressDetail":{"current":50,"total":500},"id":"b"}
{"status":"Download complete","progressDetail":{},"id":"b"}
{"status":"Extracting","progressDetail":{"current":10,"total":500},"id":"b"}
{"status":"Downloading","progressDetail":{"current":900,"total":1000},"id":"a"}
"#;
        let mut seen = Vec::new();
        track(stream.as_bytes(), &mut |done, total| {
            seen.push((done, total))
        })
        .unwrap();
        assert_eq!(seen.last(), Some(&(1400, 1500)));
        assert!(seen.contains(&(100, 1000)));
    }

    #[test]
    fn reports_engine_errors() {
        let stream = r#"{"error":"manifest unknown","errorDetail":{"message":"manifest unknown"}}"#;
        let error = track(stream.as_bytes(), &mut |_, _| {}).unwrap_err();
        assert_eq!(error.to_string(), "manifest unknown");
    }

    #[test]
    #[ignore = "needs a running container engine and network"]
    fn pulls_a_small_image() {
        // A cached download reports no byte counts at all, so only consistency is checked.
        let mut is_consistent = true;
        let result = pull("hello-world:latest", |done, total| {
            is_consistent &= done <= total
        });
        assert!(result.is_ok());
        assert!(is_consistent);
    }
}
