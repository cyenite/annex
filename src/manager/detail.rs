use std::rc::Rc;

use adw::prelude::*;
use annex_core::adb::Adb;
use annex_core::instance::{self, Backend, Instance, UpOptions};
use annex_core::redroid::Display;
use annex_core::{launcher, snapshot};

use crate::app::App;

/// One instance in depth: its details, screen size, snapshots, and the destructive actions,
/// each behind a confirmation.
pub struct DetailPage {
    name: String,
    page: adw::NavigationPage,
    toolbar: adw::ToolbarView,
}

impl DetailPage {
    pub fn new(app: &Rc<App>, name: &str) -> Self {
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());
        let page = adw::NavigationPage::builder()
            .title(name)
            .child(&toolbar)
            .build();
        let detail = Self {
            name: name.to_string(),
            page,
            toolbar,
        };
        detail.reload(app);
        detail
    }

    pub fn page(&self) -> &adw::NavigationPage {
        &self.page
    }

    /// Rebuilds the page from the app's latest view of the instance.
    pub fn reload(&self, app: &Rc<App>) {
        let Some(instance) = app.instance(&self.name) else {
            app.close_detail();
            return;
        };
        let content = adw::PreferencesPage::new();
        content.add(&Overview::new(app, &instance).group);
        if let Backend::Redroid(container) = &instance.backend {
            content.add(&Screen::new(app, &instance.name, container.display).group);
            content.add(&Snapshots::new(app, &instance.name).group);
        }
        content.add(&DangerZone::new(app, &instance).group);
        self.toolbar.set_content(Some(&content));
    }
}

struct Overview {
    group: adw::PreferencesGroup,
}

impl Overview {
    fn new(app: &Rc<App>, instance: &Instance) -> Self {
        let group = adw::PreferencesGroup::builder().title("Overview").build();
        let status = app
            .busy_with(&instance.name)
            .unwrap_or_else(|| instance.state.label().to_string());
        for (title, value) in [
            ("State", status),
            ("Mode", instance.backend.mode().label().to_string()),
            ("Android", instance.android().to_string()),
        ] {
            group.add(
                &adw::ActionRow::builder()
                    .title(title)
                    .subtitle(value)
                    .build(),
            );
        }
        let serial = adw::ActionRow::builder()
            .title("adb serial")
            .subtitle(&instance.serial)
            .subtitle_selectable(true)
            .build();
        let copy = gtk::Button::builder()
            .icon_name("edit-copy-symbolic")
            .tooltip_text("Copy")
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        let text = instance.serial.clone();
        let app_for_copy = app.clone();
        copy.connect_clicked(move |button| {
            button.clipboard().set_text(&text);
            app_for_copy.toast("Copied the adb serial");
        });
        serial.add_suffix(&copy);
        group.add(&serial);
        Self { group }
    }
}

struct Screen {
    group: adw::PreferencesGroup,
}

impl Screen {
    fn new(app: &Rc<App>, name: &str, display: Display) -> Self {
        let group = adw::PreferencesGroup::builder()
            .title("Screen")
            .description("Changing it restarts the instance; its apps and data are kept.")
            .build();
        let entry = adw::EntryRow::builder()
            .title("Size and density (WIDTHxHEIGHT/DPI)")
            .text(display.to_string())
            .show_apply_button(true)
            .build();
        let app = app.clone();
        let name = name.to_string();
        entry.connect_apply(move |entry| {
            let display = match entry.text().parse::<Display>() {
                Ok(display) => display,
                Err(error) => {
                    app.show_error("That screen size isn't valid", &anyhow::anyhow!(error));
                    return;
                }
            };
            let target = name.clone();
            app.act(&name, "Applying the screen size...", move || {
                let options = UpOptions {
                    display: Some(display),
                    ..UpOptions::default()
                };
                instance::up(&Adb::locate()?, &target, options)?;
                Ok(format!("{target} now runs at {display}"))
            });
        });
        group.add(&entry);
        Self { group }
    }
}

struct Snapshots {
    group: adw::PreferencesGroup,
}

impl Snapshots {
    fn new(app: &Rc<App>, name: &str) -> Self {
        let group = adw::PreferencesGroup::builder()
            .title("Snapshots")
            .description("A snapshot saves every app's data; restoring one takes about 20 seconds.")
            .build();
        let save = adw::EntryRow::builder()
            .title("Save a snapshot as...")
            .show_apply_button(true)
            .build();
        {
            let app = app.clone();
            let name = name.to_string();
            save.connect_apply(move |entry| {
                let label = entry.text().trim().to_string();
                if let Err(error) = instance::validate_name(&label) {
                    app.show_error("That label isn't valid", &error);
                    return;
                }
                let target = name.clone();
                app.act(&name, "Saving a snapshot...", move || {
                    snapshot::save(&target, &label)?;
                    launcher::refresh(&instance::require(&target)?);
                    Ok(format!("saved snapshot {label} of {target}"))
                });
            });
        }
        group.add(&save);

        for saved in snapshot::list(name).unwrap_or_default() {
            let row = adw::ActionRow::builder()
                .title(&saved.label)
                .subtitle(format!("{:.0} MB", saved.bytes as f64 / 1_048_576.0))
                .build();
            let restore = gtk::Button::builder()
                .label("Restore")
                .valign(gtk::Align::Center)
                .build();
            let delete = gtk::Button::builder()
                .icon_name("user-trash-symbolic")
                .tooltip_text("Delete snapshot")
                .valign(gtk::Align::Center)
                .css_classes(["flat"])
                .build();
            {
                let app = app.clone();
                let name = name.to_string();
                let label = saved.label.clone();
                restore.connect_clicked(move |_| {
                    let body = format!(
                        "{name}'s current data is replaced with snapshot {label}. Anything not in the snapshot is lost."
                    );
                    let name = name.clone();
                    let label = label.clone();
                    let app_for_action = app.clone();
                    Confirm::new(&format!("Restore {label}?"), &body, "Restore").present(&app, move || {
                        let target = name.clone();
                        let label = label.clone();
                        app_for_action.act(&name, "Restoring the snapshot...", move || {
                            snapshot::reset(&Adb::locate()?, &target, Some(&label))?;
                            Ok(format!("{target} restored to {label}"))
                        });
                    });
                });
            }
            {
                let app = app.clone();
                let name = name.to_string();
                let label = saved.label.clone();
                delete.connect_clicked(move |_| {
                    let name = name.clone();
                    let label = label.clone();
                    let app_for_action = app.clone();
                    Confirm::new(
                        &format!("Delete snapshot {label}?"),
                        "The snapshot's file is removed. The instance itself isn't changed.",
                        "Delete",
                    )
                    .present(&app, move || {
                        let target = name.clone();
                        let label = label.clone();
                        app_for_action.act(&name, "Deleting the snapshot...", move || {
                            snapshot::delete(&target, &label)?;
                            launcher::refresh(&instance::require(&target)?);
                            Ok(format!("deleted snapshot {label}"))
                        });
                    });
                });
            }
            row.add_suffix(&restore);
            row.add_suffix(&delete);
            group.add(&row);
        }
        Self { group }
    }
}

struct DangerZone {
    group: adw::PreferencesGroup,
}

impl DangerZone {
    fn new(app: &Rc<App>, instance: &Instance) -> Self {
        let group = adw::PreferencesGroup::builder()
            .title("Danger zone")
            .build();
        let name = instance.name.clone();
        if matches!(instance.backend, Backend::Redroid(_)) {
            let reset = DangerRow::new(
                "Reset to first boot",
                "Wipes every app and setting; snapshots are kept",
                "Reset",
            );
            let app = app.clone();
            let name = name.clone();
            reset.button.connect_clicked(move |_| {
                let name = name.clone();
                let app_for_action = app.clone();
                Confirm::new(
                    &format!("Reset {name}?"),
                    "The instance goes back to a freshly booted Android. Its snapshots are kept.",
                    "Reset",
                )
                .present(&app, move || {
                    let target = name.clone();
                    app_for_action.act(&name, "Resetting...", move || {
                        snapshot::reset(&Adb::locate()?, &target, None)?;
                        Ok(format!("{target} reset to first boot"))
                    });
                });
            });
            group.add(&reset.row);
        }
        let remove = DangerRow::new(
            "Delete instance",
            "Removes the instance and its data; snapshots stay on disk",
            "Delete",
        );
        let app = app.clone();
        remove.button.connect_clicked(move |_| {
            let name = name.clone();
            let app_for_action = app.clone();
            Confirm::new(
                &format!("Delete {name}?"),
                "The instance and all of its data are removed. Its snapshots stay on disk.",
                "Delete",
            )
            .present(&app, move || {
                let target = name.clone();
                app_for_action.close_detail();
                app_for_action.act(&name, "Deleting...", move || {
                    instance::remove(&Adb::locate()?, &target, false)?;
                    launcher::remove(&target)?;
                    Ok(format!("deleted {target}"))
                });
            });
        });
        group.add(&remove.row);
        Self { group }
    }
}

/// A row explaining a destructive action, with its red button.
struct DangerRow {
    row: adw::ActionRow,
    button: gtk::Button,
}

impl DangerRow {
    fn new(title: &str, subtitle: &str, action: &str) -> Self {
        let button = gtk::Button::builder()
            .label(action)
            .valign(gtk::Align::Center)
            .css_classes(["destructive-action"])
            .build();
        let row = adw::ActionRow::builder()
            .title(title)
            .subtitle(subtitle)
            .build();
        row.add_suffix(&button);
        Self { row, button }
    }
}

/// Asks before a destructive action; the action runs only on the destructive answer.
pub struct Confirm {
    dialog: adw::AlertDialog,
}

impl Confirm {
    pub fn new(heading: &str, body: &str, action: &str) -> Self {
        let dialog = adw::AlertDialog::new(Some(heading), Some(body));
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("confirm", action);
        dialog.set_response_appearance("confirm", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        Self { dialog }
    }

    pub fn present(self, app: &Rc<App>, on_confirm: impl Fn() + 'static) {
        self.dialog.connect_response(None, move |_, response| {
            if response == "confirm" {
                on_confirm();
            }
        });
        self.dialog.present(Some(app.window()));
    }
}
