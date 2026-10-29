//! The panel menu: a StatusNotifierItem (KDE natively, GNOME through its AppIndicator
//! extension) listing every instance with its actions. Actions run annex itself as a child
//! process with --notify, so each one reports its result as a desktop notification.

use std::env;
use std::process::{Command, Stdio};

use ksni::menu::{StandardItem, SubMenu};
use ksni::{Category, MenuItem, ToolTip};

use crate::instance::{Instance, Mode, State, validate_name};
use crate::paths::which;
use crate::snapshot;

/// One instance as the menu shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    name: String,
    state: State,
    mode: Mode,
    snapshots: Vec<String>,
}

impl Entry {
    pub fn from_instance(instance: &Instance) -> Self {
        let mode = instance.backend.mode();
        let snapshots = match mode {
            Mode::Container => snapshot::list(&instance.name)
                .unwrap_or_default()
                .into_iter()
                .map(|snapshot| snapshot.label)
                .collect(),
            Mode::Window => Vec::new(),
        };
        Self {
            name: instance.name.clone(),
            state: instance.state,
            mode,
            snapshots,
        }
    }
}

#[derive(Default)]
pub struct Tray {
    pub entries: Vec<Entry>,
}

impl ksni::Tray for Tray {
    fn id(&self) -> String {
        "annex".to_string()
    }

    fn title(&self) -> String {
        "annex".to_string()
    }

    fn icon_name(&self) -> String {
        "annex".to_string()
    }

    fn category(&self) -> Category {
        Category::ApplicationStatus
    }

    fn tool_tip(&self) -> ToolTip {
        let running = self
            .entries
            .iter()
            .filter(|entry| entry.state == State::Running)
            .count();
        ToolTip {
            title: "annex".to_string(),
            description: format!("{running} of {} instances running", self.entries.len()),
            ..ToolTip::default()
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mut menu: Vec<MenuItem<Self>> = self.entries.iter().map(instance_menu).collect();
        if !menu.is_empty() {
            menu.push(MenuItem::Separator);
        }
        menu.push(item("New instance...", true, |_| new_instance()));
        menu.push(item(
            "Open Annex Manager",
            which("annex-manager").is_some(),
            |_| {
                spawn_program("annex-manager", &[]);
            },
        ));
        menu.push(MenuItem::Separator);
        menu.push(item("Quit the bridge", true, |_| std::process::exit(0)));
        menu
    }
}

fn instance_menu(entry: &Entry) -> MenuItem<Tray> {
    let name = entry.name.clone();
    let mut submenu = vec![action("Show", &name, &["open-window"])];
    match entry.state {
        State::Running => submenu.push(action("Freeze", &name, &["freeze"])),
        State::Frozen => submenu.push(action("Thaw", &name, &["thaw"])),
        State::Stopped => {}
    }
    if entry.mode == Mode::Container {
        let name = name.clone();
        submenu.push(item(
            "Snapshot now",
            entry.state != State::Stopped,
            move |_| {
                let label = timestamp_label();
                spawn_annex(&["snap", "save", &name, &label]);
            },
        ));
        let resets: Vec<MenuItem<Tray>> = entry
            .snapshots
            .iter()
            .map(|label| {
                let name = entry.name.clone();
                let label = label.clone();
                item(&label.clone(), true, move |_| {
                    spawn_annex(&["reset", &name, "--to", &label]);
                })
            })
            .collect();
        submenu.push(
            SubMenu {
                label: "Reset to".to_string(),
                enabled: !resets.is_empty(),
                submenu: resets,
                ..SubMenu::default()
            }
            .into(),
        );
    }
    if entry.state != State::Stopped {
        submenu.push(action("Stop", &name, &["down"]));
    }
    SubMenu {
        label: format!(
            "{} {}  ({})",
            dot(entry.state),
            entry.name,
            entry.state.label()
        ),
        submenu,
        ..SubMenu::default()
    }
    .into()
}

fn dot(state: State) -> &'static str {
    match state {
        State::Running => "●",
        State::Frozen => "◐",
        State::Stopped => "○",
    }
}

fn action(label: &str, name: &str, command: &[&str]) -> MenuItem<Tray> {
    let mut args: Vec<String> = command.iter().map(|part| part.to_string()).collect();
    args.push(name.to_string());
    item(label, true, move |_| {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        spawn_annex(&args);
    })
}

fn item(
    label: &str,
    enabled: bool,
    activate: impl Fn(&mut Tray) + Send + 'static,
) -> MenuItem<Tray> {
    StandardItem {
        label: label.to_string(),
        enabled,
        activate: Box::new(activate),
        ..StandardItem::default()
    }
    .into()
}

/// Asks for a name with zenity (GNOME) or kdialog (KDE), then creates and opens the instance.
fn new_instance() {
    std::thread::spawn(|| {
        let asked = if which("zenity").is_some() {
            Command::new("zenity")
                .args([
                    "--entry",
                    "--title=New Android instance",
                    "--text=Name (lowercase letters, digits, -):",
                ])
                .output()
        } else if which("kdialog").is_some() {
            Command::new("kdialog")
                .args([
                    "--title",
                    "New Android instance",
                    "--inputbox",
                    "Name (lowercase letters, digits, -):",
                ])
                .output()
        } else {
            crate::desktop::notify(
                "annex",
                "Run `annex up <name>` in a terminal to create an instance.",
            );
            return;
        };
        let Ok(output) = asked else {
            return;
        };
        let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !output.status.success() || name.is_empty() {
            return;
        }
        if let Err(error) = validate_name(&name) {
            crate::desktop::notify("annex: invalid name", &error.to_string());
            return;
        }
        spawn_annex(&["open-window", &name]);
    });
}

fn spawn_annex(args: &[&str]) {
    match env::current_exe() {
        Ok(annex) => {
            let mut full = vec!["--notify"];
            full.extend(args);
            spawn_program(&annex.to_string_lossy(), &full);
        }
        Err(error) => eprintln!("could not find annex's own path: {error}"),
    }
}

fn spawn_program(program: &str, args: &[&str]) {
    let spawned = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    if let Err(error) = spawned {
        eprintln!("could not run {program}: {error}");
    }
}

/// A snapshot label from the local time, like snap-20261029-1705.
fn timestamp_label() -> String {
    let stamp = Command::new("date")
        .arg("+%Y%m%d-%H%M%S")
        .output()
        .ok()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|stamp| !stamp.is_empty())
        .unwrap_or_else(|| "now".to_string());
    format!("snap-{stamp}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(menu: &[MenuItem<Tray>]) -> Vec<String> {
        menu.iter()
            .map(|item| match item {
                MenuItem::Standard(item) => item.label.clone(),
                MenuItem::SubMenu(menu) => format!("{} >", menu.label),
                MenuItem::Separator => "---".to_string(),
                _ => "?".to_string(),
            })
            .collect()
    }

    fn submenu(item: &MenuItem<Tray>) -> &[MenuItem<Tray>] {
        match item {
            MenuItem::SubMenu(menu) => &menu.submenu,
            _ => panic!("expected a submenu"),
        }
    }

    fn entry(name: &str, state: State, mode: Mode, snapshots: &[&str]) -> Entry {
        Entry {
            name: name.to_string(),
            state,
            mode,
            snapshots: snapshots.iter().map(|label| label.to_string()).collect(),
        }
    }

    #[test]
    fn lists_instances_then_global_actions() {
        let tray = Tray {
            entries: vec![
                entry("pixel", State::Running, Mode::Container, &["clean"]),
                entry("desk", State::Frozen, Mode::Window, &[]),
            ],
        };
        let menu = ksni::Tray::menu(&tray);
        assert_eq!(
            labels(&menu),
            [
                "● pixel  (running) >",
                "◐ desk  (frozen) >",
                "---",
                "New instance...",
                "Open Annex Manager",
                "---",
                "Quit the bridge"
            ]
        );
    }

    #[test]
    fn a_running_container_instance_offers_every_action() {
        let tray = Tray {
            entries: vec![entry("pixel", State::Running, Mode::Container, &["clean"])],
        };
        let menu = ksni::Tray::menu(&tray);
        assert_eq!(
            labels(submenu(&menu[0])),
            ["Show", "Freeze", "Snapshot now", "Reset to >", "Stop"]
        );
        assert_eq!(labels(submenu(&submenu(&menu[0])[3])), ["clean"]);
    }

    #[test]
    fn window_mode_and_stopped_instances_show_only_what_applies() {
        let tray = Tray {
            entries: vec![
                entry("desk", State::Frozen, Mode::Window, &[]),
                entry("old", State::Stopped, Mode::Container, &[]),
            ],
        };
        let menu = ksni::Tray::menu(&tray);
        assert_eq!(labels(submenu(&menu[0])), ["Show", "Thaw", "Stop"]);
        assert_eq!(
            labels(submenu(&menu[1])),
            ["Show", "Snapshot now", "Reset to >"]
        );
    }
}
