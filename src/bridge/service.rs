use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::paths::config_home;
use crate::process::stdout_within;

const UNIT: &str = "annex-bridge.service";
const SYSTEMCTL_TIMEOUT: Duration = Duration::from_secs(30);

pub fn install() -> Result<PathBuf> {
    let executable = env::current_exe()?
        .canonicalize()
        .context("could not resolve annex's own path")?;
    let path = unit_path();
    fs::create_dir_all(path.parent().expect("unit path has a parent"))?;
    fs::write(&path, unit(&executable))?;
    systemctl(&["daemon-reload"])?;
    systemctl(&["enable", "--now", UNIT])?;
    // enable --now leaves an already running bridge on the old binary.
    systemctl(&["restart", UNIT])?;
    Ok(path)
}

pub fn restart() -> Result<()> {
    systemctl(&["restart", UNIT])
}

pub fn uninstall() -> Result<()> {
    let path = unit_path();
    if path.exists() {
        systemctl(&["disable", "--now", UNIT])?;
        fs::remove_file(&path)?;
        systemctl(&["daemon-reload"])?;
    }
    Ok(())
}

/// systemd's one-word state for the bridge: active, inactive, failed, or not-installed.
pub fn state() -> String {
    if !unit_path().exists() {
        return "not-installed".to_string();
    }
    let output = Command::new("systemctl")
        .args(["--user", "is-active", UNIT])
        .output();
    output.map_or("unknown".to_string(), |output| {
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    })
}

fn unit_path() -> PathBuf {
    config_home().join("systemd/user").join(UNIT)
}

fn unit(executable: &Path) -> String {
    format!(
        "\
[Unit]
Description=annex desktop bridge: clipboard and notifications for Android instances
Documentation=https://github.com/cyenite/annex
PartOf=graphical-session.target
After=graphical-session.target

[Service]
ExecStart=\"{}\" bridge run
Restart=on-failure
RestartSec=5

[Install]
WantedBy=graphical-session.target
",
        executable.display()
    )
}

fn systemctl(args: &[&str]) -> Result<()> {
    stdout_within(
        Command::new("systemctl").arg("--user").args(args),
        SYSTEMCTL_TIMEOUT,
    )
    .map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_runs_the_bridge_inside_the_graphical_session() {
        let unit = unit(Path::new("/opt/annex bin/annex"));
        assert!(unit.contains("ExecStart=\"/opt/annex bin/annex\" bridge run"));
        assert!(unit.contains("PartOf=graphical-session.target"));
        assert!(unit.contains("WantedBy=graphical-session.target"));
    }
}
