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
