use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::paths::{data_dir, home, which, write_if_changed};
use crate::process::{output_within, stdout_within};
use crate::wsl;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(15);
const WINDOWS_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub struct Adb {
    path: PathBuf,
    windows_path: Option<PathBuf>,
    server_port: Option<&'static str>,
}

impl Adb {
    /// Prefers the SDK's adb over one on PATH: two adb versions on one machine
    /// keep killing each other's server, which also drops Android Studio's devices.
    pub fn locate() -> Result<Adb> {
        let sdk_roots = [
            env::var_os("ANDROID_HOME").map(PathBuf::from),
            env::var_os("ANDROID_SDK_ROOT").map(PathBuf::from),
            home().map(|home| home.join("Android/Sdk")),
        ];
        sdk_roots
            .into_iter()
            .flatten()
            .map(|root| root.join("platform-tools/adb"))
            .find(|candidate| candidate.is_file())
            .or_else(|| which("adb"))
            .map(|path| Adb::new(path, wsl::is_wsl2()))
            .context("adb not found: install the Android SDK platform-tools")
    }

    /// Under WSL, an adb on Windows, if there is one, also connects to each instance so Windows
    /// tools see it, and this adb then keeps its server off Windows' port. Without one, WSL
    /// works like any Linux machine, and `adb devices` in its terminal lists the instances.
    fn new(path: PathBuf, is_wsl: bool) -> Adb {
        let windows_path = is_wsl
            .then(wsl::windows)
            .flatten()
            .and_then(|windows| windows.adb.clone());
        let is_port_chosen = env::var_os(wsl::ADB_SERVER_PORT_VARIABLE).is_some();
        let server_port =
            (windows_path.is_some() && !is_port_chosen).then_some(wsl::ADB_SERVER_PORT);
        Adb {
            path,
            windows_path,
            server_port,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn windows_path(&self) -> Option<&Path> {
        self.windows_path.as_deref()
    }

    /// A command running this adb against annex's adb server.
    pub fn command(&self) -> Command {
        let mut command = Command::new(&self.path);
        command.envs(self.server_environment());
        command
    }

    /// The environment for a program that runs adb itself, such as the window helper.
    pub fn environment(&self) -> Vec<(&'static str, &OsStr)> {
        let mut environment = vec![("ADB", self.path.as_os_str())];
        environment.extend(self.server_environment());
        environment
    }

    fn server_environment(&self) -> Option<(&'static str, &OsStr)> {
        self.server_port
            .map(|port| (wsl::ADB_SERVER_PORT_VARIABLE, OsStr::new(port)))
    }

    /// The public half of the key this adb authenticates with; the adb server creates it on first start.
    pub fn public_key(&self) -> Result<PathBuf> {
        let directory = env::var_os("ANDROID_USER_HOME")
            .map(PathBuf::from)
            .or_else(|| home().map(|home| home.join(".android")))
            .context("neither ANDROID_USER_HOME nor HOME is set")?;
        let key = directory.join("adbkey.pub");
        if !key.is_file() {
            self.quiet(&["start-server"]);
        }
        if !key.is_file() {
            bail!(
                "no adb key at {}: run `adb start-server` once",
                key.display()
            );
        }
        Ok(key)
    }

    /// The keys an instance trusts: this adb's, and under WSL also the key of the adb on
    /// Windows, if there is one. adbd reads one key per line.
    pub fn trusted_keys(&self) -> Result<PathBuf> {
        let key = self.public_key()?;
        if self.windows_path.is_some() && wsl::windows().is_some_and(|w| !w.adb_key.is_file()) {
            self.on_windows(&["start-server"]);
        }
        let windows_key = wsl::windows()
            .map(|windows| windows.adb_key.as_path())
            .filter(|windows_key| windows_key.is_file());
        let Some(windows_key) = windows_key else {
            return Ok(key);
        };
        let keys = format!(
            "{}\n{}\n",
            fs::read_to_string(&key)?.trim(),
            fs::read_to_string(windows_key)?.trim()
        );
        let path = data_dir().join("adb_keys");
        write_if_changed(&path, keys.as_bytes(), 0o644)
            .with_context(|| format!("could not write {}", path.display()))?;
        Ok(path)
    }

    pub fn connect(&self, serial: &str) -> Result<()> {
        self.connect_within(serial, CONNECT_TIMEOUT)
    }

    pub fn connect_within(&self, serial: &str, timeout: Duration) -> Result<()> {
        let started = Instant::now();
        loop {
            self.quiet(&["connect", serial]);
            match self.state(serial).as_deref() {
                Some("device") => {
                    self.on_windows(&["connect", serial]);
                    return Ok(());
                }
                Some("unauthorized") if started.elapsed() > timeout => {
                    bail!("{serial} refused this machine's adb key")
                }
                _ if started.elapsed() > timeout => bail!("adb could not connect to {serial}"),
                _ => sleep(Duration::from_millis(250)),
            }
        }
    }

    pub fn disconnect(&self, serial: &str) {
        self.quiet(&["disconnect", serial]);
        self.on_windows(&["disconnect", serial]);
    }

    pub fn state(&self, serial: &str) -> Option<String> {
        let output = self
            .command()
            .args(["-s", serial, "get-state"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Runs a device command and returns its trimmed output, giving up after `timeout`.
    pub fn run(&self, serial: &str, args: &[&str], timeout: Duration) -> Result<String> {
        stdout_within(self.command().args(["-s", serial]).args(args), timeout)
    }

    /// Runs `command` in the device shell. adb joins the arguments into one shell line, so
    /// anything interpolated into `command` must already be safe for the device's shell.
    pub fn shell(&self, serial: &str, command: &str, timeout: Duration) -> Result<String> {
        self.run(serial, &["shell", command], timeout)
    }

    /// Runs a device command with its output going straight to the terminal, for transfers
    /// whose progress the user wants to see.
    pub fn run_attached(&self, serial: &str, args: &[&str]) -> Result<()> {
        let status = self
            .command()
            .args(["-s", serial])
            .args(args)
            .stdin(Stdio::null())
            .status()
            .context("failed to run adb")?;
        if !status.success() {
            bail!("adb {} failed ({status})", args.first().unwrap_or(&""));
        }
        Ok(())
    }

    /// Runs a device command with its output going to `stdout`, such as a file, for output too
    /// large to hold in memory.
    pub fn run_into(&self, serial: &str, args: &[&str], stdout: Stdio) -> Result<()> {
        let status = self
            .command()
            .args(["-s", serial])
            .args(args)
            .stdin(Stdio::null())
            .stdout(stdout)
            .status()
            .context("failed to run adb")?;
        if !status.success() {
            bail!("adb {} failed ({status})", args.first().unwrap_or(&""));
        }
        Ok(())
    }

    pub fn is_root(&self, serial: &str) -> bool {
        self.shell(serial, "id -u", COMMAND_TIMEOUT)
            .is_ok_and(|uid| uid == "0")
    }

    /// Restarts the instance's adbd as root and reconnects. adbd drops every connection while
    /// it restarts, including Android Studio's.
    pub fn root(&self, serial: &str) -> Result<()> {
        self.run(serial, &["root"], COMMAND_TIMEOUT)?;
        let started = Instant::now();
        while !self.is_root(serial) {
            if started.elapsed() > CONNECT_TIMEOUT {
                bail!("adbd on {serial} did not come back as root");
            }
            sleep(Duration::from_millis(250));
            self.quiet(&["connect", serial]);
        }
        Ok(())
    }

    /// Forwards a free local TCP port to an abstract socket on the device and returns the port.
    pub fn forward(&self, serial: &str, socket: &str) -> Result<u16> {
        let remote = format!("localabstract:{socket}");
        let port = self.run(serial, &["forward", "tcp:0", &remote], COMMAND_TIMEOUT)?;
        port.parse()
            .with_context(|| format!("adb forward returned {port:?} instead of a port"))
    }

    pub fn remove_forward(&self, serial: &str, port: u16) {
        self.quiet(&["-s", serial, "forward", "--remove", &format!("tcp:{port}")]);
    }

    fn quiet(&self, args: &[&str]) {
        let _ = self
            .command()
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    /// Best effort: Windows tools reaching instances is a convenience, and annex works without.
    fn on_windows(&self, args: &[&str]) {
        if let Some(windows_path) = &self.windows_path {
            let _ = output_within(Command::new(windows_path).args(args), WINDOWS_TIMEOUT);
        }
    }
}

/// Android package names: dot-separated Java identifiers. Checked before a name is
/// interpolated into a device shell command or handed to another tool.
pub fn validate_package(package: &str) -> Result<()> {
    let is_valid = package.split('.').count() >= 2
        && package.split('.').all(|part| {
            part.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
                && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        });
    if !is_valid {
        bail!("{package:?} is not an Android package name (like com.example.app)");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_package_names() {
        for package in ["com.android.settings", "dev.annex.demo_app", "a.b"] {
            assert!(validate_package(package).is_ok(), "{package}");
        }
    }

    #[test]
    fn rejects_anything_a_shell_could_interpret() {
        for package in [
            "settings",
            "com.example;reboot",
            "com..app",
            "com.1app",
            "com.app ",
            "",
        ] {
            assert!(validate_package(package).is_err(), "{package}");
        }
    }
}
