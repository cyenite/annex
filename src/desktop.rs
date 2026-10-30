use std::env;
use std::process::Command;
use std::time::Duration;

use crate::process::output_within;

const TITLE_MAX_CHARS: usize = 120;
const BODY_MAX_CHARS: usize = 600;
const NOTIFY_TIMEOUT: Duration = Duration::from_secs(10);

/// Whether annex runs inside a graphical session that can show windows, which it doesn't over
/// SSH, in CI or from a plain TTY.
pub fn has_session() -> bool {
    env::var_os("WAYLAND_DISPLAY").is_some() || env::var_os("DISPLAY").is_some()
}

/// Shows a desktop notification from annex itself, for commands run from a launcher or menu
/// with no terminal to print to. Silently does nothing on a system without notify-send.
pub fn notify(title: &str, body: &str) {
    notify_as("annex", "annex", title, body);
}

/// Shows a desktop notification whose text may come from an untrusted source, such as an app
/// inside an instance: it is cut to length, the body's markup is escaped (notification servers
/// render a subset of HTML there, so `<a href>` could pose as a link), and `--` stops a title
/// that starts with `-` from being read as a notify-send option.
pub fn notify_as(app: &str, icon: &str, title: &str, body: &str) {
    let title = shorten(title, TITLE_MAX_CHARS);
    let body = escape_markup(&shorten(body, BODY_MAX_CHARS));
    let shown = output_within(
        Command::new("notify-send").args(["--app-name", app, "--icon", icon, "--", &title, &body]),
        NOTIFY_TIMEOUT,
    );
    if let Err(error) = shown.map(drop) {
        eprintln!("could not show a notification: {error:#}");
    }
}

fn shorten(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let mut short: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        short.push('…');
    }
    short
}

fn escape_markup(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            _ => escaped.push(c),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_markup_so_it_shows_as_text() {
        assert_eq!(
            escape_markup(r#"<a href="https://evil.example">Bank</a> & co"#),
            r#"&lt;a href="https://evil.example"&gt;Bank&lt;/a&gt; &amp; co"#
        );
    }

    #[test]
    fn shortens_long_text_on_character_boundaries() {
        assert_eq!(shorten("héllo", 10), "héllo");
        assert_eq!(shorten("héllo", 2), "hé…");
        assert_eq!(
            shorten(&"x".repeat(5000), BODY_MAX_CHARS).chars().count(),
            BODY_MAX_CHARS + 1
        );
    }
}
