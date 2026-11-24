//! annex running inside WSL 2 on Windows. Instances still run on the WSL kernel; what differs is
//! where binder comes from, how the container engine starts, and that Windows may have an adb
//! of its own, with its own server and key, for Windows tools to reach instances through.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use crate::process::output_within;

/// An adb on Windows listens on 5037, and WSL forwards localhost ports between the two systems,
/// so while Windows has one, annex's adb server in WSL takes another port instead of fighting
/// over that one.
pub const ADB_SERVER_PORT: &str = "5038";
pub const ADB_SERVER_PORT_VARIABLE: &str = "ANDROID_ADB_SERVER_PORT";

const WINDOWS_TIMEOUT: Duration = Duration::from_secs(10);
const CMD_FALLBACK: &str = "/mnt/c/Windows/System32/cmd.exe";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wsl {
    Version1,
    Version2,
}

impl Wsl {
    pub fn detect() -> Option<Wsl> {
        let release = fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
        parse_release(&release, env::var_os("WSL_DISTRO_NAME").is_some())
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Version1 => "WSL 1",
            Self::Version2 => "WSL 2",
        }
    }
}

/// WSL 1 has no Linux kernel at all and reports `...-Microsoft`; WSL 2's kernel reports
/// `...-microsoft-standard-WSL2`. A custom kernel may report neither, but WSL still sets
/// WSL_DISTRO_NAME in every login shell.
fn parse_release(release: &str, has_distro_variable: bool) -> Option<Wsl> {
    if release.trim_end().ends_with("-Microsoft") {
        Some(Wsl::Version1)
    } else if release.to_lowercase().contains("microsoft") || has_distro_variable {
        Some(Wsl::Version2)
    } else {
        None
    }
}

pub fn is_wsl2() -> bool {
    Wsl::detect() == Some(Wsl::Version2)
}

/// Windows' own adb and its key, as WSL paths. Windows needn't have an adb at all: it only lets
/// Windows tools such as Android Studio or flutter.exe reach instances.
pub struct Windows {
    pub adb: Option<PathBuf>,
    pub adb_key: PathBuf,
}

/// Read from Windows once per process: each lookup starts cmd.exe, which takes a moment.
pub fn windows() -> Option<&'static Windows> {
    static WINDOWS: OnceLock<Option<Windows>> = OnceLock::new();
    WINDOWS.get_or_init(lookup_windows).as_ref()
}

fn lookup_windows() -> Option<Windows> {
    if !is_wsl2() {
        return None;
    }
    let profile = windows_variable("USERPROFILE")?;
    let android_home =
        windows_variable("ANDROID_USER_HOME").unwrap_or_else(|| format!("{profile}\\.android"));
    let adb_key = linux_path(&format!("{android_home}\\adbkey.pub"))?;
    let sdk_roots = [
        windows_variable("ANDROID_HOME"),
        windows_variable("LOCALAPPDATA").map(|local| format!("{local}\\Android\\Sdk")),
    ];
    let adb = sdk_roots
        .into_iter()
        .flatten()
        .map(|root| format!("{root}\\platform-tools\\adb.exe"))
        .chain(windows_program("adb"))
        .filter_map(|path| linux_path(&path))
        .find(|candidate| candidate.is_file());
    Some(Windows { adb, adb_key })
}

fn windows_program(name: &str) -> Option<String> {
    let output = output_within(
        Command::new(cmd()).args(["/d", "/c", "where", name]),
        WINDOWS_TIMEOUT,
    )
    .ok()?;
    parse_program(&String::from_utf8_lossy(&output.stdout))
}

/// `where` lists every match on PATH, one per line, in the order Windows would pick them.
fn parse_program(output: &str) -> Option<String> {
    output
        .lines()
        .map(str::trim)
        .find(|line| line.to_lowercase().ends_with(".exe"))
        .map(str::to_string)
}

fn cmd() -> PathBuf {
    if Path::new(CMD_FALLBACK).is_file() {
        PathBuf::from(CMD_FALLBACK)
    } else {
        PathBuf::from("cmd.exe")
    }
}

/// One variable per cmd.exe call: cmd.exe parses its command line itself, and an argument with
/// spaces or `&` would reach it re-quoted by WSL's interop.
fn windows_variable(name: &str) -> Option<String> {
    let output = output_within(
        Command::new(cmd()).args(["/d", "/c", "echo", &format!("%{name}%")]),
        WINDOWS_TIMEOUT,
    )
    .ok()?;
    parse_variable(name, &String::from_utf8_lossy(&output.stdout))
}

/// cmd.exe echoes an unset variable back as `%NAME%`.
fn parse_variable(name: &str, output: &str) -> Option<String> {
    let value = output.trim();
    (!value.is_empty() && value != format!("%{name}%")).then(|| value.to_string())
}

fn linux_path(windows_path: &str) -> Option<PathBuf> {
    let output = output_within(
        Command::new("wslpath").args(["-u", windows_path]),
        WINDOWS_TIMEOUT,
    )
    .ok()?;
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (output.status.success() && !path.is_empty()).then(|| PathBuf::from(path))
}

/// What to do when the WSL kernel has no binder.
pub fn binder_steps() -> String {
    "the WSL kernel has no Android binder: use a WSL kernel built with CONFIG_ANDROID_BINDER_IPC and CONFIG_ANDROID_BINDERFS, point `kernel=` under [wsl2] in %USERPROFILE%\\.wslconfig at it, then run `wsl --shutdown` in Windows and reopen this terminal".to_string()
}

/// What to do when WSL started without systemd, which starts the container engine.
pub fn systemd_steps() -> String {
    "WSL started without systemd, which starts the container engine: add `[boot]` and `systemd=true` to /etc/wsl.conf, then run `wsl --shutdown` in Windows, reopen this terminal and run `annex setup` again".to_string()
}

/// What to do when the WSL kernel can't shape traffic for `annex net slow` and `lossy`.
pub fn network_steps(modules: &[&str]) -> String {
    format!(
        "the WSL kernel can't shape traffic, so `annex net slow` and `lossy` fail: use a WSL kernel with {} built in",
        modules.join(", ")
    )
}

pub fn has_systemd() -> bool {
    Path::new("/run/systemd/system").is_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tells_wsl_versions_apart() {
        assert_eq!(
            parse_release("5.15.167.4-microsoft-standard-WSL2\n", false),
            Some(Wsl::Version2)
        );
        assert_eq!(
            parse_release("4.4.0-22621-Microsoft\n", true),
            Some(Wsl::Version1)
        );
        assert_eq!(
            parse_release("6.6.36-annex-binder", true),
            Some(Wsl::Version2)
        );
        assert_eq!(parse_release("7.0.0-44-generic\n", false), None);
    }

    #[test]
    fn treats_an_echoed_name_as_unset() {
        assert_eq!(
            parse_variable("USERPROFILE", "C:\\Users\\Aaron\r\n").as_deref(),
            Some("C:\\Users\\Aaron")
        );
        assert_eq!(parse_variable("ANDROID_HOME", "%ANDROID_HOME%\r\n"), None);
        assert_eq!(parse_variable("ANDROID_HOME", "\r\n"), None);
    }

    #[test]
    fn takes_the_first_program_windows_would_run() {
        let output = "C:\\tools\\platform-tools\\adb.exe\r\nC:\\scoop\\shims\\adb.exe\r\n";
        assert_eq!(
            parse_program(output).as_deref(),
            Some("C:\\tools\\platform-tools\\adb.exe")
        );
        assert_eq!(
            parse_program("INFO: Could not find files for the given pattern(s).\r\n"),
            None
        );
    }
}
