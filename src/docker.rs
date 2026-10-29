use std::fs::File;
use std::process::{Command, Output, Stdio};

use anyhow::{Context, Result, bail};

pub fn run(args: &[&str]) -> Result<String> {
    let output = Command::new("docker")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .context("failed to run docker")?;
    stdout_or_error(output)
}

pub fn succeeds(args: &[&str]) -> bool {
    Command::new("docker")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

pub fn run_into(args: &[&str], file: File) -> Result<()> {
    let output = Command::new("docker")
        .args(args)
        .stdin(Stdio::null())
        .stdout(file)
        .output()
        .context("failed to run docker")?;
    stdout_or_error(output).map(drop)
}

pub fn run_from(args: &[&str], file: File) -> Result<()> {
    let output = Command::new("docker")
        .args(args)
        .stdin(file)
        .output()
        .context("failed to run docker")?;
    stdout_or_error(output).map(drop)
}

fn stdout_or_error(output: Output) -> Result<String> {
    if !output.status.success() {
        bail!("docker: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
