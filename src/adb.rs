use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::which;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

pub struct Adb {
    path: PathBuf,
}

impl Adb {
    /// Prefers the SDK's adb over one on PATH: two adb versions on one machine
    /// keep killing each other's server, which also drops Android Studio's devices.
    pub fn locate() -> Result<Adb> {
        let home = env::var_os("HOME").map(PathBuf::from);
        let sdk_roots = [
            env::var_os("ANDROID_HOME").map(PathBuf::from),
            env::var_os("ANDROID_SDK_ROOT").map(PathBuf::from),
            home.map(|home| home.join("Android/Sdk")),
        ];
        sdk_roots
            .into_iter()
            .flatten()
            .map(|root| root.join("platform-tools/adb"))
            .find(|candidate| candidate.is_file())
            .or_else(|| which("adb"))
            .map(|path| Adb { path })
            .context("adb not found: install the Android SDK platform-tools")
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The public half of the key this adb authenticates with; the adb server creates it on first start.
    pub fn public_key(&self) -> Result<PathBuf> {
        let directory = env::var_os("ANDROID_USER_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".android")))
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

    pub fn connect(&self, serial: &str) -> Result<()> {
        let started = Instant::now();
        loop {
            self.quiet(&["connect", serial]);
            if self.state(serial).as_deref() == Some("device") {
                return Ok(());
            }
            if started.elapsed() > CONNECT_TIMEOUT {
                bail!("adb could not connect to {serial}");
            }
            sleep(Duration::from_millis(250));
        }
    }

    pub fn disconnect(&self, serial: &str) {
        self.quiet(&["disconnect", serial]);
    }

    pub fn state(&self, serial: &str) -> Option<String> {
        let output = Command::new(&self.path)
            .args(["-s", serial, "get-state"])
            .stderr(Stdio::null())
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    fn quiet(&self, args: &[&str]) {
        let _ = Command::new(&self.path)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}
