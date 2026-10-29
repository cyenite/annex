use std::fs::{self, File};
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::thread::{self, sleep};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Runs a command to completion, killing it after `timeout`. adb calls against a frozen or
/// half-booted instance can block forever, and a background bridge must never hang on one.
pub fn output_within(command: &mut Command, timeout: Duration) -> Result<Output> {
    let program = command.get_program().to_string_lossy().to_string();
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to run {program}"))?;
    // Both pipes are drained on their own threads so a chatty child can't fill one and stall.
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            bail!("{program} did not finish within {} s", timeout.as_secs());
        }
        sleep(POLL_INTERVAL);
    };
    Ok(Output {
        status,
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
    })
}

/// Like `output_within`, but fails with the command's stderr when it exits unsuccessfully.
pub fn stdout_within(command: &mut Command, timeout: Duration) -> Result<String> {
    let program = command.get_program().to_string_lossy().to_string();
    let output = output_within(command, timeout)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let message = if stderr.trim().is_empty() {
            stdout.trim().to_string()
        } else {
            stderr.trim().to_string()
        };
        bail!("{program}: {message}");
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Starts a long-running program in its own process group, so it outlives annex and a Ctrl-C
/// in the terminal that launched it.
pub fn spawn_detached(command: &mut Command, log: &Path) -> Result<Child> {
    let program = command.get_program().to_string_lossy().to_string();
    if let Some(directory) = log.parent() {
        fs::create_dir_all(directory)?;
    }
    let stdout = File::create(log)?;
    let stderr = stdout.try_clone()?;
    command
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .process_group(0)
        .spawn()
        .with_context(|| format!("failed to start {program}"))
}

/// The last lines of a log file, for error messages about a program that exited early.
pub fn log_tail(log: &Path, lines: usize) -> String {
    let contents = fs::read_to_string(log).unwrap_or_default();
    let mut tail: Vec<&str> = contents.lines().rev().take(lines).collect();
    tail.reverse();
    tail.join("\n")
}

fn drain(pipe: Option<impl Read + Send + 'static>) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut buffer = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut buffer);
        }
        buffer
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_output_larger_than_a_pipe_buffer() {
        let output = output_within(
            Command::new("sh").args(["-c", "head -c 200000 /dev/zero"]),
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(output.stdout.len(), 200_000);
    }

    #[test]
    fn kills_a_command_that_runs_too_long() {
        let started = Instant::now();
        let result = output_within(Command::new("sleep").arg("5"), Duration::from_millis(200));
        assert!(result.is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn reports_stderr_on_failure() {
        let error = stdout_within(
            Command::new("sh").args(["-c", "echo broken >&2; exit 3"]),
            Duration::from_secs(5),
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "sh: broken");
    }
}
