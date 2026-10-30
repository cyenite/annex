use std::rc::Rc;

use adw::prelude::*;
use annex_core::adb::Adb;
use annex_core::instance::{self, Instance, State, UpOptions};
use annex_core::{launcher, window};
use gtk::{gdk, gio};

use crate::app::App;

/// One instance on the home page: its state and the actions that fit it. APK files dropped on
/// a running instance's card are installed on it.
pub struct InstanceCard {
    root: gtk::Box,
}

impl InstanceCard {
    pub fn new(app: &Rc<App>, instance: &Instance) -> Self {
        let name = instance.name.clone();
        let title = gtk::Label::builder()
            .label(&name)
            .halign(gtk::Align::Start)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["title-3"])
            .build();
        let badge = gtk::Label::builder()
            .label(instance.state.label())
            .css_classes(["caption-heading", badge_class(instance.state)])
            .build();
        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        heading.append(&title);
        heading.append(&badge);

        let details = gtk::Label::builder()
            .label(format!(
                "{} mode · Android {} · {}",
                capitalized(instance.backend.mode().label()),
                instance.android(),
                instance.serial
            ))
            .halign(gtk::Align::Start)
            .wrap(true)
            .css_classes(["dim-label"])
            .build();

        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .css_classes(["card"])
            .build();
        let inner = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .margin_top(16)
            .margin_bottom(16)
            .margin_start(16)
            .margin_end(16)
            .build();
        inner.append(&heading);
        inner.append(&details);
        inner.append(&Actions::new(app, instance).root);
        root.append(&inner);

        if instance.state == State::Running {
            accept_apks(app, &root, &name);
        }
        Self { root }
    }

    pub fn root(&self) -> &gtk::Box {
        &self.root
    }
}

/// The card's buttons, or a spinner with what the instance is busy doing.
struct Actions {
    root: gtk::Box,
}

impl Actions {
    fn new(app: &Rc<App>, instance: &Instance) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let name = instance.name.clone();
        if let Some(label) = app.busy_with(&name) {
            let spinner = gtk::Spinner::builder().spinning(true).build();
            let text = gtk::Label::builder()
                .label(label)
                .css_classes(["dim-label"])
                .build();
            root.append(&spinner);
            root.append(&text);
            return Self { root };
        }

        let primary = match instance.state {
            State::Running => PillButton::new("Show", "video-display-symbolic", true).root,
            State::Frozen => PillButton::new("Thaw", "media-playback-start-symbolic", true).root,
            State::Stopped => PillButton::new("Start", "media-playback-start-symbolic", true).root,
        };
        {
            let app = app.clone();
            let name = name.clone();
            let state = instance.state;
            primary.connect_clicked(move |_| start_and_show(&app, &name, state));
        }
        root.append(&primary);

        if instance.state == State::Running {
            let freeze = PillButton::new("Freeze", "media-playback-pause-symbolic", false).root;
            let app = app.clone();
            let name = name.clone();
            freeze.connect_clicked(move |_| {
                let target = name.clone();
                app.act(&name, "Freezing...", move || {
                    instance::freeze(&target)?;
                    Ok(format!("{target} frozen"))
                });
            });
            root.append(&freeze);
        }
        if instance.state != State::Stopped {
            let stop = PillButton::new("Stop", "media-playback-stop-symbolic", false).root;
            let app = app.clone();
            let name = name.clone();
            stop.connect_clicked(move |_| {
                let target = name.clone();
                app.act(&name, "Stopping...", move || {
                    instance::down(&Adb::locate()?, &target)?;
                    Ok(format!("{target} stopped"))
                });
            });
            root.append(&stop);
        }

        let spacer = gtk::Box::builder().hexpand(true).build();
        root.append(&spacer);
        let open = gtk::Button::builder()
            .icon_name("go-next-symbolic")
            .tooltip_text("Details, snapshots and screen")
            .css_classes(["flat", "circular"])
            .build();
        {
            let app = app.clone();
            open.connect_clicked(move |_| app.open_detail(&name));
        }
        root.append(&open);
        Self { root }
    }
}

/// Starts or thaws the instance as needed, then opens its window.
pub fn start_and_show(app: &Rc<App>, name: &str, state: State) {
    let label = match state {
        State::Running => "Opening its window...",
        State::Frozen => "Thawing...",
        State::Stopped => "Starting...",
    };
    let target = name.to_string();
    app.act(name, label, move || {
        let adb = Adb::locate()?;
        match state {
            State::Running => {}
            State::Frozen => instance::thaw(&adb, &target)?,
            State::Stopped => {
                let instance = instance::up(&adb, &target, UpOptions::default())?;
                launcher::refresh_or_install(&instance);
            }
        }
        let instance = instance::require_running(&target)?;
        Ok(if window::show_once(&adb, &instance)? {
            format!("{target} is ready")
        } else {
            format!("{target}'s window is already open")
        })
    });
}

fn accept_apks(app: &Rc<App>, card: &gtk::Box, name: &str) {
    let target = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
    let app = app.clone();
    let name = name.to_string();
    target.connect_drop(move |_, value, _, _| {
        let Ok(files) = value.get::<gdk::FileList>() else {
            return false;
        };
        let apks: Vec<String> = files
            .files()
            .iter()
            .filter_map(gio::File::path)
            .filter(|path| path.extension().is_some_and(|extension| extension == "apk"))
            .map(|path| path.to_string_lossy().into_owned())
            .collect();
        if apks.is_empty() {
            app.toast("Drop APK files to install them");
            return false;
        }
        let target = name.clone();
        let count = apks.len();
        app.act(&name, "Installing...", move || {
            let adb = Adb::locate()?;
            let instance = instance::require_running(&target)?;
            for apk in &apks {
                adb.run(
                    &instance.serial,
                    &["install", "-r", "-g", apk],
                    std::time::Duration::from_secs(300),
                )?;
            }
            Ok(format!(
                "installed {count} APK{} on {target}",
                if count == 1 { "" } else { "s" }
            ))
        });
        true
    });
    card.add_controller(target);
}

/// A rounded button with an icon and a label; the primary one is highlighted.
struct PillButton {
    root: gtk::Button,
}

impl PillButton {
    fn new(label: &str, icon: &str, is_primary: bool) -> Self {
        let content = adw::ButtonContent::builder()
            .label(label)
            .icon_name(icon)
            .build();
        let classes: &[&str] = if is_primary {
            &["suggested-action", "pill"]
        } else {
            &["pill"]
        };
        let root = gtk::Button::builder()
            .child(&content)
            .css_classes(classes.to_vec())
            .build();
        Self { root }
    }
}

fn badge_class(state: State) -> &'static str {
    match state {
        State::Running => "success",
        State::Frozen => "accent",
        State::Stopped => "dim-label",
    }
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}
