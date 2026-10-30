//! The requirement checks `annex setup` plans from and `annex doctor` reports.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::distro::{Package, PackageManager};
use super::plan::Facts;
use crate::adb::Adb;
use crate::instance::AndroidVersion;
use crate::paths::{home, which};
use crate::scrcpy::Scrcpy;
use crate::{bridge, docker, waydroid};

pub const BINDER_MODULE: &str = "binder_linux";
pub const BINDER_BOOT_FILE: &str = "/etc/modules-load.d/annex-binder.conf";
pub const IMAGE_RULE_FILE: &str = "/etc/udev/rules.d/90-annex-hide-android-images.rules";
/// Android mounts each of its system modules (APEX files) through a loop device, and a
/// privileged container's loop devices are the host's, so every running instance adds about
/// 36 "volumes" to the dock and file manager. This marks them for udisks to ignore.
pub const IMAGE_RULE: &str = "# Added by annex setup: hide the disk images Android mounts inside annex instances.\nSUBSYSTEM==\"block\", KERNEL==\"loop[0-9]*\", ATTR{loop/backing_file}==\"*.apex\", ENV{UDISKS_IGNORE}=\"1\"\n";

pub fn gather() -> Facts {
    let manager = PackageManager::detect();
    let unavailable = manager
        .map(|manager| {
            [
                Package::ContainerEngine,
                Package::DebugTools,
                Package::WindowHelper,
                Package::Notifications,
                Package::WindowMode,
            ]
            .into_iter()
            .filter(|package| {
                manager
                    .package_name(*package)
                    .is_some_and(|name| !manager.can_install(name))
            })
            .collect()
        })
        .unwrap_or_default();
    let can_use_docker = can_use_docker();
    Facts {
        manager,
        unavailable,
        has_adb: Adb::locate().is_ok(),
        has_image: can_use_docker
            && docker::succeeds(&["image", "inspect", AndroidVersion::Android15.image()]),
        has_binder: has_binder(),
        has_docker: which("docker").is_some(),
        has_notify: which("notify-send").is_some(),
        has_adb_key: adb_key_path().is_some_and(|key| key.is_file()),
        has_image_rule: has_image_rule(),
        can_use_docker,
        has_window_mode: waydroid::is_installed(),
        has_window_helper: has_window_helper(),
        is_docker_running: is_docker_running(),
        has_binder_at_boot: has_binder_at_boot(),
        is_in_docker_group: is_in_docker_group(),
        is_bridge_installed: bridge::service::state() != "not-installed",
        is_window_mode_ready: waydroid::is_installed() && waydroid::is_initialized(),
    }
}

pub fn has_binder() -> bool {
    fs::read_to_string("/proc/filesystems")
        .unwrap_or_default()
        .lines()
        .any(|line| line.trim_end().ends_with("binder"))
}

pub fn has_binder_at_boot() -> bool {
    fs::read_dir("/etc/modules-load.d")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| fs::read_to_string(entry.path()).ok())
        .any(|contents| contents.lines().any(|line| line.trim() == BINDER_MODULE))
}

pub fn has_image_rule() -> bool {
    Path::new(IMAGE_RULE_FILE).is_file()
}

pub fn can_use_docker() -> bool {
    docker::succeeds(&["info", "--format", "{{.ServerVersion}}"])
}

pub fn has_window_helper() -> bool {
    Scrcpy::locate().is_ok_and(|helper| helper.version().is_ok() && helper.server().is_ok())
}

fn is_docker_running() -> bool {
    Path::new("/var/run/docker.sock").exists()
        || Command::new("systemctl")
            .args(["is-active", "--quiet", "docker"])
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
}

/// Membership as written in /etc/group, which a login session only picks up after the next login.
fn is_in_docker_group() -> bool {
    let Some(user) = current_user() else {
        return false;
    };
    fs::read_to_string("/etc/group")
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.strip_prefix("docker:"))
        .any(|rest| {
            rest.rsplit(':')
                .next()
                .is_some_and(|members| members.split(',').any(|member| member == user))
        })
}

/// Whether a panel can show the bridge's menu: KDE has a StatusNotifierWatcher built in, GNOME
/// only with its AppIndicator extension enabled.
pub fn has_tray_host() -> bool {
    Command::new("busctl")
        .args(["--user", "status", "org.kde.StatusNotifierWatcher"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

pub fn current_user() -> Option<String> {
    env::var("USER").ok().or_else(|| env::var("LOGNAME").ok())
}

pub fn adb_key_path() -> Option<PathBuf> {
    env::var_os("ANDROID_USER_HOME")
        .map(PathBuf::from)
        .or_else(|| home().map(|home| home.join(".android")))
        .map(|directory| directory.join("adbkey.pub"))
}
