//! App launcher entries: one `.desktop` file per instance, so it can be opened, frozen, reset
//! and stopped from the desktop's app grid and dock without a terminal.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

use crate::instance::Instance;
use crate::paths::{config_home, data_home, which};
use crate::snapshot;

const ICON: &str = include_str!("../assets/annex.svg");
/// The Manager's GTK application id, which names its launcher so its window groups under it.
pub const MANAGER_ID: &str = "dev.annex.Manager";
/// The id the desktop bridge gives GeoClue. GNOME Shell refuses location to a client whose id
/// has no launcher entry, and names the app from that entry when it asks for permission.
pub const BRIDGE_ID: &str = "dev.annex.Bridge";

/// The app id annex's windows for an instance carry, which ties them to its launcher in the dock.
pub fn app_id(name: &str) -> String {
    format!("annex-{name}")
}

/// Writes or refreshes the launcher entry for an instance, with one reset action per snapshot.
pub fn install(instance: &Instance) -> Result<PathBuf> {
    let executable = env::current_exe()?
        .canonicalize()
        .context("could not resolve annex's own path")?;
    let labels: Vec<String> = snapshot::list(&instance.name)
        .unwrap_or_default()
        .into_iter()
        .map(|snapshot| snapshot.label)
        .collect();
    install_icon()?;
    let path = entry_path(&instance.name);
    fs::create_dir_all(path.parent().expect("entry path has a parent"))?;
    fs::write(&path, render(&instance.name, &executable, &labels))?;
    refresh_database();
    Ok(path)
}

pub fn remove(name: &str) -> Result<()> {
    let path = entry_path(name);
    if path.exists() {
        fs::remove_file(path)?;
        refresh_database();
    }
    Ok(())
}

/// Adds the launcher entry after `annex up`, warning rather than failing the command.
pub fn refresh_or_install(instance: &Instance) {
    if let Err(error) = install(instance) {
        eprintln!(
            "warning: could not add {} to the app launcher: {error:#}",
            instance.name
        );
    }
}

/// Keeps an existing launcher's actions current, and does nothing for an instance without one.
pub fn refresh(instance: &Instance) {
    if entry_path(&instance.name).exists()
        && let Err(error) = install(instance)
    {
        eprintln!(
            "warning: could not update {}'s launcher: {error:#}",
            instance.name
        );
    }
}

fn render(name: &str, executable: &Path, labels: &[String]) -> String {
    let exe = quote(&executable.to_string_lossy());
    let mut actions = vec!["freeze", "thaw", "stop"];
    let reset_ids: Vec<String> = labels
        .iter()
        .map(|label| format!("reset-{label}"))
        .collect();
    actions.extend(reset_ids.iter().map(String::as_str));
    let mut entry = format!(
        "\
[Desktop Entry]
Type=Application
Version=1.5
Name={name} (Android)
GenericName=Android instance
Comment=Open the {name} Android instance
Exec={exe} open-window {name}
Icon=annex
Terminal=false
Categories=Development;Emulator;
Keywords=android;emulator;device;annex;
StartupNotify=true
StartupWMClass={app_id}
Actions={actions};
",
        app_id = app_id(name),
        actions = actions.join(";"),
    );
    for (id, label, command) in [
        ("freeze", "Freeze", "freeze"),
        ("thaw", "Thaw", "thaw"),
        ("stop", "Stop", "down"),
    ] {
        entry.push_str(&format!(
            "\n[Desktop Action {id}]\nName={label}\nExec={exe} --notify {command} {name}\n"
        ));
    }
    for label in labels {
        entry.push_str(&format!(
            "\n[Desktop Action reset-{label}]\nName=Reset to snapshot {label}\nExec={exe} --notify reset {name} --to {label}\n"
        ));
    }
    entry
}

/// Quotes an Exec argument per the Desktop Entry spec, which reserves these characters inside
/// double quotes.
fn quote(argument: &str) -> String {
    let mut quoted = String::from("\"");
    for c in argument.chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            quoted.push('\\');
        }
        quoted.push(c);
    }
    quoted.push('"');
    quoted
}

pub fn install_icon() -> Result<()> {
    let path = data_home().join("icons/hicolor/scalable/apps/annex.svg");
    if fs::read_to_string(&path).is_ok_and(|current| current == ICON) {
        return Ok(());
    }
    fs::create_dir_all(path.parent().expect("icon path has a parent"))?;
    fs::write(path, ICON)?;
    Ok(())
}

/// Writes the bridge's hidden launcher entry; true when it was missing or out of date.
pub fn install_bridge_entry() -> Result<bool> {
    let executable = env::current_exe()?
        .canonicalize()
        .context("could not resolve annex's own path")?;
    let entry = format!(
        "\
[Desktop Entry]
Type=Application
Name=Annex
Comment=Shares this computer's location with Android instances that ask for it
Icon=annex
Exec=\"{}\" location
NoDisplay=true
X-Geoclue-Reason=Android instances you set to `annex location host` see this computer's location.
",
        executable.display()
    );
    let path = data_home()
        .join("applications")
        .join(format!("{BRIDGE_ID}.desktop"));
    if fs::read_to_string(&path).is_ok_and(|current| current == entry) {
        return Ok(false);
    }
    fs::create_dir_all(path.parent().expect("entry path has a parent"))?;
    fs::write(path, entry)?;
    Ok(true)
}

fn refresh_database() {
    if which("update-desktop-database").is_some() {
        let _ = Command::new("update-desktop-database")
            .arg(data_home().join("applications"))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// The Manager binary: next to this annex (how releases and `cargo install` place it), or on PATH.
pub fn manager_path() -> Option<PathBuf> {
    env::current_exe()
        .ok()
        .and_then(|exe| exe.canonicalize().ok())
        .map(|exe| exe.with_file_name("annex-manager"))
        .filter(|candidate| candidate.is_file())
        .or_else(|| which("annex-manager"))
}

/// Adds "Annex" to the app launcher when the Manager is installed.
pub fn install_manager_entry() -> Result<Option<PathBuf>> {
    let Some(manager) = manager_path() else {
        return Ok(None);
    };
    install_icon()?;
    let path = data_home()
        .join("applications")
        .join(format!("{MANAGER_ID}.desktop"));
    fs::create_dir_all(path.parent().expect("entry path has a parent"))?;
    fs::write(&path, render_manager(&manager, false))?;
    refresh_database();
    Ok(Some(path))
}

pub fn has_manager_entry() -> bool {
    data_home()
        .join("applications")
        .join(format!("{MANAGER_ID}.desktop"))
        .is_file()
}

pub fn is_manager_autostarted() -> bool {
    autostart_path().is_file()
}

pub fn set_manager_autostart(is_on: bool) -> Result<()> {
    let path = autostart_path();
    if !is_on {
        if path.exists() {
            fs::remove_file(path)?;
        }
        return Ok(());
    }
    let manager = manager_path().context("Annex Manager isn't installed")?;
    fs::create_dir_all(path.parent().expect("autostart path has a parent"))?;
    fs::write(path, render_manager(&manager, true))?;
    Ok(())
}

fn render_manager(manager: &Path, is_autostart: bool) -> String {
    let mut entry = format!(
        "\
[Desktop Entry]
Type=Application
Version=1.5
Name=Annex
GenericName=Android instances
Comment=Create, open, snapshot and reset Android instances
Exec={}
Icon=annex
Terminal=false
Categories=Development;Emulator;
Keywords=android;emulator;device;annex;
StartupNotify=true
StartupWMClass={MANAGER_ID}
",
        quote(&manager.to_string_lossy())
    );
    if is_autostart {
        entry.push_str("X-GNOME-Autostart-enabled=true\n");
    }
    entry
}

fn autostart_path() -> PathBuf {
    config_home()
        .join("autostart")
        .join(format!("{MANAGER_ID}.desktop"))
}

fn entry_path(name: &str) -> PathBuf {
    data_home()
        .join("applications")
        .join(format!("{}.desktop", app_id(name)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(labels: &[&str]) -> String {
        let labels: Vec<String> = labels.iter().map(|label| label.to_string()).collect();
        render("pixel", Path::new("/opt/annex/annex"), &labels)
    }

    #[test]
    fn opens_the_instance_and_groups_its_windows() {
        let entry = entry(&[]);
        assert!(entry.starts_with("[Desktop Entry]\n"));
        assert!(entry.contains("\nName=pixel (Android)\n"));
        assert!(entry.contains("\nExec=\"/opt/annex/annex\" open-window pixel\n"));
        assert!(entry.contains("\nStartupWMClass=annex-pixel\n"));
        assert!(entry.contains("\nIcon=annex\n"));
        assert!(entry.contains("\nTerminal=false\n"));
    }

    #[test]
    fn offers_lifecycle_actions_that_report_by_notification() {
        let entry = entry(&[]);
        assert!(entry.contains("\nActions=freeze;thaw;stop;\n"));
        assert!(entry.contains(
            "[Desktop Action stop]\nName=Stop\nExec=\"/opt/annex/annex\" --notify down pixel\n"
        ));
    }

    #[test]
    fn adds_one_reset_action_per_snapshot() {
        let entry = entry(&["clean", "logged-in"]);
        assert!(entry.contains("\nActions=freeze;thaw;stop;reset-clean;reset-logged-in;\n"));
        assert!(entry.contains(
            "[Desktop Action reset-logged-in]\nName=Reset to snapshot logged-in\nExec=\"/opt/annex/annex\" --notify reset pixel --to logged-in\n"
        ));
    }

    #[test]
    fn quotes_reserved_characters_in_the_executable_path() {
        assert_eq!(quote("/home/a b/annex"), "\"/home/a b/annex\"");
        assert_eq!(quote("/x/$HOME`\"\\"), "\"/x/\\$HOME\\`\\\"\\\\\"");
    }
}
