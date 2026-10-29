use std::env;
use std::fs;
use std::path::Path;

use crate::adb::Adb;
use crate::instance::AndroidVersion;
use crate::paths::which;
use crate::scrcpy::Scrcpy;
use crate::setup::facts;
use crate::{docker, gpu, waydroid};

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
        windows(),
        memory(),
        image(),
        window_mode(),
        desktop_clipboard(),
        desktop_notifications(),
        panel_menu(),
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
    if facts::has_binder() {
        Check::new(Level::Ok, "binder", "binderfs available")
    } else {
        Check::new(Level::Fail, "binder", "not loaded: run `annex setup`")
    }
}

fn binder_on_boot() -> Check {
    if facts::has_binder_at_boot() {
        Check::new(Level::Ok, "binder on boot", "loaded at boot")
    } else {
        Check::new(
            Level::Warn,
            "binder on boot",
            "not loaded at boot: run `annex setup`",
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
        Err(_) => Check::new(Level::Fail, "docker", "unreachable: run `annex setup`"),
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

fn windows() -> Check {
    let Ok(helper) = Scrcpy::locate() else {
        return Check::new(
            Level::Warn,
            "windows",
            "helper missing: `annex show`, `annex open` and clipboard sync need it; run `annex setup`",
        );
    };
    match (helper.version(), helper.server()) {
        (Ok(_), Ok(_)) => Check::new(
            Level::Ok,
            "windows",
            "screen, app windows and clipboard ready",
        ),
        (Err(error), _) | (_, Err(error)) => Check::new(Level::Warn, "windows", error.to_string()),
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
    if docker::succeeds(&["image", "inspect", AndroidVersion::Android15.image()]) {
        Check::new(Level::Ok, "image", "Android 15 downloaded")
    } else {
        Check::new(
            Level::Warn,
            "image",
            "Android 15 missing: `annex setup` or the first `annex up` downloads about 3 GB",
        )
    }
}

fn window_mode() -> Check {
    if !waydroid::is_installed() || !waydroid::is_initialized() {
        return Check::new(
            Level::Ok,
            "window mode",
            "not set up (optional: native app windows): `annex setup --window-mode`",
        );
    }
    Check::new(
        Level::Ok,
        "window mode",
        "ready: `annex up <name> --mode window`",
    )
}

fn desktop_clipboard() -> Check {
    if env::var_os("DISPLAY").is_some() {
        Check::new(Level::Ok, "clipboard", "X11/XWayland clipboard reachable")
    } else {
        Check::new(
            Level::Warn,
            "clipboard",
            "DISPLAY not set: the bridge syncs clipboards between instances only",
        )
    }
}

fn desktop_notifications() -> Check {
    match which("notify-send") {
        Some(_) => Check::new(Level::Ok, "notifications", "notify-send available"),
        None => Check::new(
            Level::Warn,
            "notifications",
            "missing: `annex setup --bridge` installs them",
        ),
    }
}

fn panel_menu() -> Check {
    if facts::has_tray_host() {
        Check::new(
            Level::Ok,
            "panel menu",
            "the bridge's menu can show in your panel",
        )
    } else {
        Check::new(
            Level::Warn,
            "panel menu",
            "your panel shows no app indicators: on GNOME, enable the AppIndicator extension",
        )
    }
}
