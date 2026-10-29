use std::process::{Command, Stdio};

/// Shows a desktop notification, for commands run from a launcher or menu with no terminal to
/// print to. Silently does nothing on a system without notify-send.
pub fn notify(title: &str, body: &str) {
    let _ = Command::new("notify-send")
        .args(["--app-name", "annex", "--icon", "annex", title, body])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}
