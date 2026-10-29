use std::env;
use std::path::PathBuf;
use std::process::Command;
use std::thread::sleep;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::adb::Adb;
use crate::paths::{log_dir, which};
use crate::process::{log_tail, spawn_detached, stdout_within};

const STARTUP: Duration = Duration::from_secs(3);
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Scrcpy {
    path: PathBuf,
}

impl Scrcpy {
    pub fn locate() -> Result<Self> {
        which("scrcpy")
            .map(|path| Self { path })
            .context("the window helper isn't installed: run `annex setup`")
    }

    /// The server half must be exactly this version: scrcpy-server refuses any other.
    pub fn version(&self) -> Result<String> {
        let output = stdout_within(Command::new(&self.path).arg("--version"), VERSION_TIMEOUT)?;
        parse_version(&output).context("could not read the window helper's version")
    }

    /// The scrcpy-server jar scrcpy pushes to devices, which the desktop bridge reuses.
    pub fn server(&self) -> Result<PathBuf> {
        let installed = self
            .path
            .canonicalize()
            .ok()
            .and_then(|path| Some(path.parent()?.parent()?.join("share/scrcpy/scrcpy-server")));
        env::var_os("SCRCPY_SERVER_PATH")
            .map(PathBuf::from)
            .into_iter()
            .chain(installed)
            .chain([PathBuf::from("/usr/share/scrcpy/scrcpy-server")])
            .find(|candidate| candidate.is_file())
            .context("the window helper is incomplete: run `annex setup --repair`")
    }

    /// Opens a scrcpy window and waits briefly so a startup failure is reported, not lost.
    pub fn open_window(&self, adb: &Adb, serial: &str, title: &str, extra: &[&str]) -> Result<()> {
        let slug: String = title
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let log = log_dir().join(format!("window-{slug}.log"));
        // scrcpy starts its own adb unless told which one, and a mismatched adb kills the
        // running server. Audio is off because Redroid images have no Opus encoder, which
        // crashes scrcpy's server.
        let mut child = spawn_detached(
            Command::new(&self.path)
                .args(["--serial", serial, "--window-title", title, "--no-audio"])
                .args(extra)
                .env("ADB", adb.path()),
            &log,
        )?;
        sleep(STARTUP);
        if let Some(status) = child.try_wait()? {
            bail!(
                "the window closed as it opened ({status}); details:\n{}",
                log_tail(&log, 8)
            );
        }
        Ok(())
    }
}

fn parse_version(output: &str) -> Option<String> {
    let version = output.split_whitespace().nth(1)?;
    version
        .chars()
        .all(|c| c.is_ascii_digit() || c == '.')
        .then(|| version.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_version_from_the_banner() {
        let banner = "scrcpy 3.3.4 <https://github.com/Genymobile/scrcpy>\n\nDependencies...";
        assert_eq!(parse_version(banner).as_deref(), Some("3.3.4"));
    }

    #[test]
    fn rejects_unexpected_banners() {
        assert_eq!(parse_version("scrcpy"), None);
        assert_eq!(parse_version("scrcpy v3; rm -rf"), None);
    }
}
