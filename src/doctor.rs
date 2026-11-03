use std::env;
use std::fs;
use std::path::Path;

use serde::Serialize;

use crate::adb::Adb;
use crate::instance::AndroidVersion;
use crate::paths::which;
use crate::scrcpy::Scrcpy;
use crate::setup::facts;
use crate::{desktop, docker, gpu, network, waydroid};

const LOW_MEMORY_KIB: u64 = 2 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
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

    /// The label coloured for a terminal; console drops the colour when piped or NO_COLOR is set.
    fn styled(self) -> console::StyledObject<&'static str> {
        let label = console::style(self.label());
        match self {
            Self::Ok => label.green(),
            Self::Warn => label.yellow(),
            Self::Fail => label.red().bold(),
        }
    }
}

#[derive(Debug, Serialize)]
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
    let container = facts::container_kind();
    let container = container.as_deref();
    vec![
        kernel(),
        host(container),
        binder(container),
        binder_on_boot(container),
        shared_memory(),
        docker_access(),
        graphics(),
        adb(),
        adb_key(),
        windows(),
        memory(),
        image(),
        network_conditions(),
        window_mode(),
        desktop_clipboard(),
        desktop_notifications(),
        panel_menu(),
        disk_images(container),
    ]
}

pub fn has_failures(checks: &[Check]) -> bool {
    checks.iter().any(|check| check.level == Level::Fail)
}

/// `annex doctor --json`: every check, and whether any failed, for CI to read.
pub fn to_json(checks: &[Check]) -> String {
    let report = serde_json::json!({
        "ok": !has_failures(checks),
        "checks": checks,
    });
    serde_json::to_string_pretty(&report).expect("doctor checks serialize")
}

pub fn print(checks: &[Check]) {
    for check in checks {
        println!(
            "[{}] {:<14} {}",
            check.level.styled(),
            check.name,
            check.detail
        );
    }
}

fn kernel() -> Check {
    let release = fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
    Check::new(Level::Ok, "kernel", release.trim())
}

fn host(container: Option<&str>) -> Check {
    match container {
        Some(kind) => Check::new(
            Level::Ok,
            "host",
            format!("{kind} container: kernel modules are loaded by its host"),
        ),
        None => Check::new(Level::Ok, "host", "not a container"),
    }
}

fn binder(container: Option<&str>) -> Check {
    match (facts::has_binder(), container) {
        (true, _) => Check::new(Level::Ok, "binder", "binderfs available"),
        (false, Some(kind)) => Check::new(Level::Fail, "binder", facts::binder_host_steps(kind)),
        (false, None) => Check::new(Level::Fail, "binder", "not loaded: run `annex setup`"),
    }
}

fn binder_on_boot(container: Option<&str>) -> Check {
    if container.is_some() {
        return Check::new(
            Level::Ok,
            "binder on boot",
            "up to the host: this machine is a container",
        );
    }
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

fn network_conditions() -> Check {
    if network::can_shape() {
        Check::new(
            Level::Ok,
            "network",
            "`annex net` can make instances slow or lossy",
        )
    } else {
        Check::new(
            Level::Warn,
            "network",
            "traffic shaping modules aren't loaded, so `annex net slow` and `lossy` fail: run `annex setup`",
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
    if !desktop::has_session() {
        return no_desktop("clipboard");
    }
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
    if !desktop::has_session() {
        return no_desktop("notifications");
    }
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
    if !desktop::has_session() {
        return no_desktop("panel menu");
    }
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

/// Desktop features don't apply over SSH or on a CI runner, so they aren't worth a warning there.
fn no_desktop(name: &'static str) -> Check {
    Check::new(Level::Ok, name, "not needed: no desktop session")
}

fn disk_images(container: Option<&str>) -> Check {
    if container.is_some() {
        return Check::new(
            Level::Ok,
            "disk images",
            "nothing to hide: this machine is a container",
        );
    }
    if facts::has_image_rule() {
        Check::new(
            Level::Ok,
            "disk images",
            "Android's internal disks stay out of your dock",
        )
    } else {
        Check::new(
            Level::Warn,
            "disk images",
            "each running instance shows about 36 drives in your dock: run `annex setup`",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_reports_failures_for_ci() {
        let checks = vec![
            Check::new(Level::Ok, "kernel", "7.0.0"),
            Check::new(Level::Fail, "binder", "not loaded"),
        ];
        let report: serde_json::Value = serde_json::from_str(&to_json(&checks)).unwrap();
        assert_eq!(report["ok"], false);
        assert_eq!(report["checks"][1]["level"], "fail");
        assert_eq!(report["checks"][1]["name"], "binder");
        assert!(!has_failures(&checks[..1]));
    }

    #[test]
    fn a_container_without_binder_points_at_the_host() {
        let check = host(Some("lxc"));
        assert!(check.detail.contains("lxc"));
        assert_eq!(binder_on_boot(Some("lxc")).level, Level::Ok);
        assert_eq!(disk_images(Some("docker")).level, Level::Ok);
    }
}
