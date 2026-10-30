use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use annex_core::instance::{self, Instance};
use gtk::{gio, glib};

use crate::card::InstanceCard;
use crate::detail::DetailPage;
use crate::new_instance::NewInstanceDialog;
use crate::settings::SettingsDialog;
use crate::task;

const APP_ID: &str = "dev.annex.Manager";
const REFRESH_INTERVAL: Duration = Duration::from_secs(3);

pub fn run() -> glib::ExitCode {
    let application = adw::Application::builder().application_id(APP_ID).build();
    application.connect_activate(|application| {
        if let Some(window) = application.active_window() {
            window.present();
            return;
        }
        App::new(application).present();
    });
    application.run()
}

/// The window and what every part of it shares: the instance list, which instances have an
/// operation running, and where toasts go.
pub struct App {
    window: adw::ApplicationWindow,
    stack: gtk::Stack,
    cards: gtk::FlowBox,
    toasts: adw::ToastOverlay,
    navigation: adw::NavigationView,
    /// What each instance is busy with, shown on its card instead of its buttons.
    busy: RefCell<HashMap<String, String>>,
    detail: RefCell<Option<DetailPage>>,
    instances: RefCell<Vec<Instance>>,
}

impl App {
    fn new(application: &adw::Application) -> Rc<Self> {
        let cards = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .homogeneous(true)
            .min_children_per_line(1)
            .max_children_per_line(3)
            .row_spacing(12)
            .column_spacing(12)
            .valign(gtk::Align::Start)
            .build();
        let clamp = adw::Clamp::builder()
            .maximum_size(1100)
            .margin_top(18)
            .margin_bottom(18)
            .margin_start(18)
            .margin_end(18)
            .child(&cards)
            .build();
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&clamp)
            .vexpand(true)
            .build();

        let create = gtk::Button::builder()
            .label("New instance")
            .halign(gtk::Align::Center)
            .css_classes(["pill", "suggested-action"])
            .build();
        let empty = adw::StatusPage::builder()
            .icon_name("annex")
            .title("No Android instances yet")
            .description("Create one to get an Android 15 device in about 20 seconds.")
            .child(&create)
            .build();
        let loading = gtk::Spinner::builder()
            .spinning(true)
            .width_request(32)
            .height_request(32)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build();

        let stack = gtk::Stack::new();
        stack.add_named(&loading, Some("loading"));
        stack.add_named(&empty, Some("empty"));
        stack.add_named(&scrolled, Some("list"));

        let add = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text("New instance")
            .build();
        let menu = gio::Menu::new();
        menu.append(Some("Settings"), Some("app.settings"));
        menu.append(Some("About Annex"), Some("app.about"));
        let menu_button = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .menu_model(&menu)
            .tooltip_text("Main menu")
            .primary(true)
            .build();
        let header = adw::HeaderBar::new();
        header.pack_start(&add);
        header.pack_end(&menu_button);

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&stack));
        let home = adw::NavigationPage::builder()
            .title("Annex")
            .tag("home")
            .child(&toolbar)
            .build();
        let navigation = adw::NavigationView::new();
        navigation.add(&home);
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&navigation));

        let window = adw::ApplicationWindow::builder()
            .application(application)
            .title("Annex")
            .icon_name("annex")
            .default_width(980)
            .default_height(680)
            .content(&toasts)
            .build();

        let app = Rc::new(Self {
            window,
            stack,
            cards,
            toasts,
            navigation,
            busy: RefCell::new(HashMap::new()),
            detail: RefCell::new(None),
            instances: RefCell::new(Vec::new()),
        });

        for button in [&add, &create] {
            let app = app.clone();
            button.connect_clicked(move |_| NewInstanceDialog::new(&app).present());
        }
        let settings = gio::SimpleAction::new("settings", None);
        let about = gio::SimpleAction::new("about", None);
        {
            let app = app.clone();
            settings.connect_activate(move |_, _| SettingsDialog::new(&app).present());
        }
        {
            let app = app.clone();
            about.connect_activate(move |_, _| app.show_about());
        }
        application.add_action(&settings);
        application.add_action(&about);
        application.set_accels_for_action("app.settings", &["<Control>comma"]);
        {
            let popped = app.clone();
            app.navigation.connect_popped(move |_, _| {
                popped.detail.replace(None);
            });
        }

        app.refresh();
        let weak = Rc::downgrade(&app);
        glib::timeout_add_local(REFRESH_INTERVAL, move || match weak.upgrade() {
            Some(app) => {
                app.refresh();
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });
        app
    }

    fn present(&self) {
        self.window.present();
    }

    pub fn window(&self) -> &adw::ApplicationWindow {
        &self.window
    }

    pub fn instance(&self, name: &str) -> Option<Instance> {
        self.instances
            .borrow()
            .iter()
            .find(|instance| instance.name == name)
            .cloned()
    }

    pub fn busy_with(&self, name: &str) -> Option<String> {
        self.busy.borrow().get(name).cloned()
    }

    /// Reads the instances off the UI thread and redraws whatever changed.
    pub fn refresh(self: &Rc<Self>) {
        let app = self.clone();
        task::spawn(instance::list, move |result| match result {
            Ok(instances) => {
                let has_changed = *app.instances.borrow() != instances;
                if has_changed || app.stack.visible_child_name().as_deref() == Some("loading") {
                    app.instances.replace(instances);
                    app.render();
                }
            }
            Err(error) => app.show_error("Couldn't list instances", &error),
        });
    }

    fn render(self: &Rc<Self>) {
        while let Some(child) = self.cards.first_child() {
            self.cards.remove(&child);
        }
        let instances = self.instances.borrow().clone();
        for instance in &instances {
            self.cards.append(InstanceCard::new(self, instance).root());
        }
        self.stack.set_visible_child_name(if instances.is_empty() {
            "empty"
        } else {
            "list"
        });
        if let Some(detail) = self.detail.borrow().as_ref() {
            detail.reload(self);
        }
    }

    /// Runs one operation on an instance in the background: its card shows `label` until it
    /// finishes, then a toast reports the outcome.
    pub fn act(
        self: &Rc<Self>,
        name: &str,
        label: &str,
        work: impl FnOnce() -> anyhow::Result<String> + Send + 'static,
    ) {
        self.busy
            .borrow_mut()
            .insert(name.to_string(), label.to_string());
        self.render();
        let app = self.clone();
        let name = name.to_string();
        task::spawn(work, move |result| {
            app.busy.borrow_mut().remove(&name);
            match result {
                Ok(message) => app.toast(&message),
                Err(error) => app.show_error(&format!("{name}: something went wrong"), &error),
            }
            app.render();
            app.refresh();
        });
    }

    pub fn open_detail(self: &Rc<Self>, name: &str) {
        let page = DetailPage::new(self, name);
        self.navigation.push(page.page());
        self.detail.replace(Some(page));
    }

    pub fn close_detail(&self) {
        self.navigation.pop_to_tag("home");
    }

    pub fn toast(&self, message: &str) {
        self.toasts.add_toast(adw::Toast::new(message));
    }

    /// A short toast with a Details button that opens the full error.
    pub fn show_error(&self, title: &str, error: &anyhow::Error) {
        let toast = adw::Toast::builder()
            .title(title)
            .button_label("Details")
            .priority(adw::ToastPriority::High)
            .timeout(8)
            .build();
        let window = self.window.clone();
        let heading = title.to_string();
        let detail = format!("{error:#}");
        toast.connect_button_clicked(move |_| {
            let dialog = adw::AlertDialog::new(Some(&heading), Some(&detail));
            dialog.add_response("close", "Close");
            dialog.present(Some(&window));
        });
        self.toasts.add_toast(toast);
    }

    fn show_about(&self) {
        let about = adw::AboutDialog::builder()
            .application_name("Annex")
            .application_icon("annex")
            .developer_name("The annex authors")
            .version(env!("CARGO_PKG_VERSION"))
            .comments("Fast Android instances for Linux developers")
            .website(env!("CARGO_PKG_REPOSITORY"))
            .issue_url(format!("{}/issues", env!("CARGO_PKG_REPOSITORY")))
            .license_type(gtk::License::Apache20)
            .build();
        about.add_legal_section(
            "Third-party software",
            None,
            gtk::License::Custom,
            Some("Annex builds on open-source projects and Rust crates. Run `annex about --licenses` for their notices."),
        );
        about.present(Some(&self.window));
    }
}
