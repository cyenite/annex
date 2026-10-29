use std::fs;
use std::path::Path;

use crate::adb::Adb;
use crate::instance::AndroidVersion;
use crate::{docker, gpu, which};

const LOW_MEMORY_KIB: u64 = 2 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Ok,
    Warn,
    Fail,
}

impl Level {
    fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok  ",
            Self::Warn => "warn",
            Self::Fail => "FAIL",
        }
    }
}

pub struct Check {
    pub level: Level,
    pub name: &'static str,
    pub detail: String,
}

impl Check {
    fn new(level: Level, name: &'static str, detail: impl Into<String>) -> Self {
        Self {
            level,
            name,
            detail: detail.into(),
        }
    }
}

pub fn run() -> Vec<Check> {
    vec![
        kernel(),
        binder(),
        binder_on_boot(),
        shared_memory(),
        docker_access(),
        graphics(),
        adb(),
        adb_key(),
        scrcpy(),
        memory(),
        image(),
    ]
}

pub fn print(checks: &[Check]) {
    for check in checks {
        println!(
            "[{}] {:<14} {}",
            check.level.label(),
            check.name,
            check.detail
        );
    }
}

fn kernel() -> Check {
    let release = fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
    Check::new(Level::Ok, "kernel", release.trim())
}

fn binder() -> Check {
    let filesystems = fs::read_to_string("/proc/filesystems").unwrap_or_default();
    if filesystems
        .lines()
        .any(|line| line.trim_end().ends_with("binder"))
    {
        Check::new(Level::Ok, "binder", "binderfs available")
    } else {
        Check::new(
            Level::Fail,
            "binder",
            "not loaded: run `sudo modprobe binder_linux`",
        )
    }
}

fn binder_on_boot() -> Check {
    let is_configured = fs::read_dir("/etc/modules-load.d")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| fs::read_to_string(entry.path()).ok())
        .any(|contents| contents.lines().any(|line| line.trim() == "binder_linux"));
    if is_configured {
        Check::new(Level::Ok, "binder on boot", "loaded at boot")
    } else {
        Check::new(
            Level::Warn,
            "binder on boot",
            "not loaded at boot: run `echo binder_linux | sudo tee /etc/modules-load.d/binder.conf`",
        )
    }
}

fn shared_memory() -> Check {
    if Path::new("/dev/ashmem").exists() {
        Check::new(Level::Ok, "shared memory", "ashmem present")
    } else {
        Check::new(
            Level::Ok,
            "shared memory",
            "no ashmem, using memfd (Android 16 images can't boot without ashmem)",
        )
    }
}

fn docker_access() -> Check {
    match docker::run(&["info", "--format", "{{.ServerVersion}}"]) {
        Ok(version) => Check::new(Level::Ok, "docker", format!("server {version}")),
        Err(_) => Check::new(
            Level::Fail,
            "docker",
            "unreachable: install Docker and add yourself to the docker group",
        ),
    }
}

fn graphics() -> Check {
    let nodes = gpu::render_nodes();
    if let Some(node) = nodes.iter().find(|node| node.vendor.has_mesa_driver()) {
        return Check::new(
            Level::Ok,
            "gpu",
            format!("{} ({})", node.vendor.label(), node.name),
        );
    }
    match nodes.first() {
        Some(node) => Check::new(
            Level::Warn,
            "gpu",
            format!(
                "{} has no Mesa driver: software rendering",
                node.vendor.label()
            ),
        ),
        None => Check::new(Level::Warn, "gpu", "no render node: software rendering"),
    }
}

fn adb() -> Check {
    match Adb::locate() {
        Ok(adb) => Check::new(Level::Ok, "adb", adb.path().display().to_string()),
        Err(error) => Check::new(Level::Fail, "adb", error.to_string()),
    }
}

fn adb_key() -> Check {
    match Adb::locate().and_then(|adb| adb.public_key()) {
        Ok(key) => Check::new(
            Level::Ok,
            "adb key",
            format!("{} (only this key can open instances)", key.display()),
        ),
        Err(error) => Check::new(Level::Fail, "adb key", error.to_string()),
    }
}

fn scrcpy() -> Check {
    match which("scrcpy") {
        Some(path) => Check::new(Level::Ok, "scrcpy", path.display().to_string()),
        None => Check::new(
            Level::Warn,
            "scrcpy",
            "not installed: `annex show` needs it",
        ),
    }
}

fn memory() -> Check {
    let meminfo = fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let available_kib = meminfo
        .lines()
        .find_map(|line| line.strip_prefix("MemAvailable:"))
        .and_then(|value| value.trim().trim_end_matches(" kB").parse::<u64>().ok())
        .unwrap_or(0);
    let detail = format!("{:.1} GB available", available_kib as f64 / 1024.0 / 1024.0);
    let level = if available_kib < LOW_MEMORY_KIB {
        Level::Warn
    } else {
        Level::Ok
    };
    Check::new(level, "memory", detail)
}

fn image() -> Check {
    let image = AndroidVersion::Android15.image();
    if docker::succeeds(&["image", "inspect", image]) {
        Check::new(Level::Ok, "image", format!("{image} downloaded"))
    } else {
        Check::new(
            Level::Warn,
            "image",
            format!("{image} missing: first `annex up` downloads ~3 GB"),
        )
    }
}
