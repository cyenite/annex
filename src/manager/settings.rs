use std::rc::Rc;

use adw::prelude::*;
use annex_core::bridge::service;
use annex_core::launcher;

use crate::app::App;
use crate::task;

/// The desktop bridge and whether the Manager opens when you log in.
pub struct SettingsDialog {
    app: Rc<App>,
    dialog: adw::PreferencesDialog,
}

impl SettingsDialog {
    pub fn new(app: &Rc<App>) -> Self {
        let bridge = adw::SwitchRow::builder()
            .title("Desktop bridge")
            .subtitle("Shared clipboard, Android notifications and the panel menu")
            .active(service::state() == "active")
            .build();
        {
            let app = app.clone();
            bridge.connect_active_notify(move |row| {
                let is_on = row.is_active();
                let app = app.clone();
                let row = row.clone();
                row.set_sensitive(false);
                task::spawn(
                    move || {
                        if is_on {
                            service::install().map(drop)
                        } else {
                            service::uninstall()
                        }
                    },
                    move |result| {
                        row.set_sensitive(true);
                        match result {
                            Ok(()) if is_on => app.toast("The desktop bridge is running"),
                            Ok(()) => app.toast("The desktop bridge is off"),
                            Err(error) => app.show_error("Couldn't change the bridge", &error),
                        }
                    },
                );
            });
        }
        let autostart = adw::SwitchRow::builder()
            .title("Open Annex when you log in")
            .active(launcher::is_manager_autostarted())
            .build();
        {
            let app = app.clone();
            autostart.connect_active_notify(move |row| {
                if let Err(error) = launcher::set_manager_autostart(row.is_active()) {
                    app.show_error("Couldn't change autostart", &error);
                }
            });
        }
        let group = adw::PreferencesGroup::new();
        group.add(&bridge);
        group.add(&autostart);
        let page = adw::PreferencesPage::builder()
            .title("General")
            .icon_name("preferences-system-symbolic")
            .build();
        page.add(&group);
        let dialog = adw::PreferencesDialog::new();
        dialog.add(&page);
        Self {
            app: app.clone(),
            dialog,
        }
    }

    pub fn present(&self) {
        self.dialog.present(Some(self.app.window()));
    }
}
