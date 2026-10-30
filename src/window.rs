use std::fs;

use anyhow::Result;

use crate::adb::{Adb, validate_package};
use crate::instance::{Backend, Instance};
use crate::scrcpy::Scrcpy;
use crate::{launcher, waydroid};

/// Opens a window with the instance's whole screen.
pub fn show(adb: &Adb, instance: &Instance) -> Result<()> {
    match &instance.backend {
        Backend::Redroid(_) => {
            let title = format!("annex: {}", instance.name);
            let app_id = launcher::app_id(&instance.name);
            Scrcpy::locate()?.open_window(adb, &instance.serial, &title, &app_id, &[])
        }
        Backend::Waydroid { .. } => waydroid::show(),
    }
}

/// Opens the instance's screen window unless it already has one, so running `annex up` or the
/// launcher again doesn't stack duplicates. Returns whether a window was opened.
pub fn show_once(adb: &Adb, instance: &Instance) -> Result<bool> {
    if is_screen_open(instance) {
        return Ok(false);
    }
    show(adb, instance)?;
    Ok(true)
}

/// Window mode's full-screen UI is a single window its session reuses, so only container mode
/// can stack duplicates; its screen windows are found by their title in /proc.
fn is_screen_open(instance: &Instance) -> bool {
    if !matches!(instance.backend, Backend::Redroid(_)) {
        return false;
    }
    let title = format!("annex: {}", instance.name);
    fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| fs::read(entry.path().join("cmdline")).ok())
        .any(|cmdline| is_screen_window(&cmdline, &title))
}

fn is_screen_window(cmdline: &[u8], title: &str) -> bool {
    let args: Vec<&[u8]> = cmdline.split(|byte| *byte == 0).collect();
    let is_helper = args
        .first()
        .is_some_and(|program| program.ends_with(b"scrcpy"));
    is_helper
        && args
            .windows(2)
            .any(|pair| pair[0] == b"--window-title" && pair[1] == title.as_bytes())
}

/// Opens one app in a desktop window of its own.
pub fn open(adb: &Adb, instance: &Instance, package: &str) -> Result<()> {
    validate_package(package)?;
    match &instance.backend {
        Backend::Redroid(container) => {
            let scrcpy = Scrcpy::locate()?;
            // Redroid's shell user lacks ADD_TRUSTED_DISPLAY, which scrcpy needs to create the
            // app's virtual display on Android 13 and later; root has it.
            if !adb.is_root(&instance.serial) {
                println!(
                    "enabling app windows on {} (adb reconnects once)",
                    instance.name
                );
                adb.root(&instance.serial)?;
            }
            let title = format!("annex: {}: {package}", instance.name);
            let display = format!("--new-display={}", container.display);
            let app = format!("--start-app={package}");
            let app_id = launcher::app_id(&instance.name);
            scrcpy.open_window(adb, &instance.serial, &title, &app_id, &[&display, &app])
        }
        Backend::Waydroid { .. } => waydroid::open(package),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_screen_window_by_its_exact_title() {
        let screen =
            b"/usr/bin/scrcpy\x00--serial\x00127.0.0.1:5600\x00--window-title\x00annex: demo\x00--no-audio\x00";
        assert!(is_screen_window(screen, "annex: demo"));
        assert!(!is_screen_window(screen, "annex: dem"));
    }

    #[test]
    fn ignores_app_windows_and_other_programs() {
        let app = b"/usr/bin/scrcpy\x00--window-title\x00annex: demo: com.android.settings\x00";
        assert!(!is_screen_window(app, "annex: demo"));
        let editor = b"/usr/bin/vim\x00--window-title\x00annex: demo\x00";
        assert!(!is_screen_window(editor, "annex: demo"));
    }
}
