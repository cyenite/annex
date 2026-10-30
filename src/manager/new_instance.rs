use std::rc::Rc;

use adw::prelude::*;
use annex_core::adb::Adb;
use annex_core::instance::{self, AndroidVersion, Mode, UpOptions};
use annex_core::redroid::Display;
use annex_core::{launcher, window};

use crate::app::App;

const ANDROID_CHOICES: [(&str, AndroidVersion); 3] = [
    ("Android 15", AndroidVersion::Android15),
    ("Android 14", AndroidVersion::Android14),
    ("Android 13", AndroidVersion::Android13),
];
const MODE_CHOICES: [(&str, Mode, &str); 2] = [
    (
        "Container",
        Mode::Container,
        "Several instances, any Android version, snapshots",
    ),
    (
        "Window",
        Mode::Window,
        "Apps open in native desktop windows; one instance",
    ),
];

/// Asks for a name, mode, Android version and screen, then creates the instance and opens it.
pub struct NewInstanceDialog {
    app: Rc<App>,
    dialog: adw::Dialog,
}

impl NewInstanceDialog {
    pub fn new(app: &Rc<App>) -> Self {
        let name = adw::EntryRow::builder()
            .title("Name")
            .text(suggested_name(app))
            .build();
        let mode = adw::ComboRow::builder()
            .title("Mode")
            .subtitle(MODE_CHOICES[0].2)
            .model(&gtk::StringList::new(
                &MODE_CHOICES.map(|(label, _, _)| label),
            ))
            .build();
        let android = adw::ComboRow::builder()
            .title("Android version")
            .model(&gtk::StringList::new(
                &ANDROID_CHOICES.map(|(label, _)| label),
            ))
            .build();
        let screen = adw::EntryRow::builder()
            .title("Screen (WIDTHxHEIGHT/DPI)")
            .text(Display::default().to_string())
            .build();
        {
            let android = android.clone();
            let screen = screen.clone();
            mode.connect_selected_notify(move |mode| {
                let (_, chosen, description) = MODE_CHOICES[mode.selected() as usize];
                mode.set_subtitle(description);
                let is_container = chosen == Mode::Container;
                android.set_sensitive(is_container);
                screen.set_sensitive(is_container);
            });
        }
        let group = adw::PreferencesGroup::builder()
            .description("Names use lowercase letters, digits and dashes.")
            .build();
        for row in [
            name.upcast_ref::<gtk::Widget>(),
            mode.upcast_ref(),
            android.upcast_ref(),
            screen.upcast_ref(),
        ] {
            group.add(row);
        }
        let content = adw::PreferencesPage::new();
        content.add(&group);

        let create = gtk::Button::builder()
            .label("Create")
            .css_classes(["suggested-action"])
            .build();
        let header = adw::HeaderBar::builder()
            .show_end_title_buttons(false)
            .show_start_title_buttons(false)
            .build();
        let cancel = gtk::Button::builder().label("Cancel").build();
        header.pack_start(&cancel);
        header.pack_end(&create);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&content));
        let dialog = adw::Dialog::builder()
            .title("New instance")
            .content_width(460)
            .child(&toolbar)
            .build();
        dialog.set_default_widget(Some(&create));

        {
            let dialog = dialog.clone();
            cancel.connect_clicked(move |_| {
                dialog.close();
            });
        }
        {
            let app = app.clone();
            let dialog = dialog.clone();
            create.connect_clicked(move |_| {
                let chosen_mode = MODE_CHOICES[mode.selected() as usize].1;
                let chosen_android = ANDROID_CHOICES[android.selected() as usize].1;
                let request = Request {
                    name: name.text().trim().to_string(),
                    mode: chosen_mode,
                    screen: screen.text().trim().to_string(),
                    android: chosen_android,
                };
                match request.options() {
                    Ok(options) => {
                        dialog.close();
                        request.start(&app, options);
                    }
                    Err(error) => app.show_error("Can't create that instance", &error),
                }
            });
        }
        Self {
            app: app.clone(),
            dialog,
        }
    }

    pub fn present(&self) {
        self.dialog.present(Some(self.app.window()));
    }
}

struct Request {
    name: String,
    mode: Mode,
    screen: String,
    android: AndroidVersion,
}

impl Request {
    fn options(&self) -> anyhow::Result<UpOptions> {
        instance::validate_name(&self.name)?;
        if self.mode == Mode::Window {
            return Ok(UpOptions {
                mode: Some(Mode::Window),
                ..UpOptions::default()
            });
        }
        let display = self.screen.parse::<Display>().map_err(anyhow::Error::msg)?;
        Ok(UpOptions {
            mode: Some(Mode::Container),
            display: Some(display),
            android: Some(self.android),
        })
    }

    fn start(&self, app: &Rc<App>, options: UpOptions) {
        let name = self.name.clone();
        app.act(
            &self.name,
            "Creating... (the first one downloads about 3 GB)",
            move || {
                let adb = Adb::locate()?;
                let instance = instance::up(&adb, &name, options)?;
                launcher::refresh_or_install(&instance);
                window::show_once(&adb, &instance)?;
                Ok(format!("{name} is ready"))
            },
        );
        app.refresh();
    }
}

/// `default` for a first instance, otherwise the first free `pixel`, `pixel-2`, ...
fn suggested_name(app: &App) -> String {
    if app.instance(instance::DEFAULT_NAME).is_none() {
        return instance::DEFAULT_NAME.to_string();
    }
    std::iter::once("pixel".to_string())
        .chain((2..).map(|number| format!("pixel-{number}")))
        .find(|name| app.instance(name).is_none())
        .expect("an unused name exists")
}
