//! Annex Manager: a GTK 4 / libadwaita window over annex_core, for everything the CLI does
//! that is nicer with a mouse. Every long operation runs off the UI thread (see `task`).

mod app;
mod card;
mod detail;
mod new_instance;
mod settings;
mod task;

use gtk::glib;

fn main() -> glib::ExitCode {
    app::run()
}
