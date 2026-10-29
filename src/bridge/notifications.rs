use std::collections::HashSet;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, sleep};
use std::time::Duration;

use crate::adb::Adb;

const POLL_INTERVAL: Duration = Duration::from_secs(2);
const ADB_TIMEOUT: Duration = Duration::from_secs(10);
/// Android's IMPORTANCE_DEFAULT: the lowest level that makes a sound or shows a heads-up.
const ALERTING_IMPORTANCE: u8 = 3;
/// FLAG_ONGOING_EVENT, FLAG_FOREGROUND_SERVICE and FLAG_GROUP_SUMMARY.
const QUIET_FLAGS: u32 = 0x2 | 0x40 | 0x200;
const QUIET_FLAG_NAMES: [&str; 3] = ["ONGOING_EVENT", "FOREGROUND_SERVICE", "GROUP_SUMMARY"];

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Notification {
    pub key: String,
    pub text: String,
    pub title: String,
    pub package: String,
    pub is_alerting: bool,
}

/// Polls an instance's notifications and shows new alerting ones on the desktop.
pub struct Watcher {
    stop: Arc<AtomicBool>,
}

impl Watcher {
    pub fn start(adb: &Adb, name: &str, serial: &str) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let adb = adb.clone();
        let name = name.to_string();
        let serial = serial.to_string();
        let should_stop = stop.clone();
        thread::spawn(move || watch(&adb, &name, &serial, &should_stop));
        Self { stop }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn watch(adb: &Adb, name: &str, serial: &str, stop: &AtomicBool) {
    // What is already posted when the bridge attaches has been seen, or is stale.
    let mut known = keys(adb, serial).unwrap_or_default();
    while !stop.load(Ordering::Relaxed) {
        sleep(POLL_INTERVAL);
        let Some(current) = keys(adb, serial) else {
            continue;
        };
        if current.iter().any(|key| !known.contains(key)) {
            let dump = adb.shell(serial, "dumpsys notification --noredact", ADB_TIMEOUT);
            for notification in parse(&dump.unwrap_or_default()) {
                if notification.is_alerting && !known.contains(&notification.key) {
                    show(name, notification);
                }
            }
        }
        known = current;
    }
}

fn keys(adb: &Adb, serial: &str) -> Option<HashSet<String>> {
    let output = adb
        .shell(serial, "cmd notification list", ADB_TIMEOUT)
        .ok()?;
    Some(output.lines().map(|line| line.trim().to_string()).collect())
}

/// Parses the posted notifications out of `dumpsys notification --noredact`.
pub fn parse(dump: &str) -> Vec<Notification> {
    let mut notifications = Vec::new();
    let mut current: Option<Notification> = None;
    let section = dump
        .lines()
        .skip_while(|line| line.trim_end() != "  Notification List:")
        .skip(1)
        .take_while(|line| !is_section_header(line));
    for line in section {
        let trimmed = line.trim_start();
        if let Some(header) = trimmed.strip_prefix("NotificationRecord(") {
            notifications.extend(current.take());
            current = Some(Notification {
                package: word_after(header, "pkg=").unwrap_or_default().to_string(),
                is_alerting: word_after(header, "importance=")
                    .and_then(|importance| importance.parse::<u8>().ok())
                    .is_some_and(|importance| importance >= ALERTING_IMPORTANCE),
                ..Notification::default()
            });
            continue;
        }
        let Some(notification) = current.as_mut() else {
            continue;
        };
        if let Some(key) = trimmed.strip_prefix("key=") {
            notification.key = key.to_string();
        } else if let Some(flags) = trimmed.strip_prefix("flags=") {
            notification.is_alerting &= !is_quiet(flags);
        } else if let Some(title) = trimmed.strip_prefix("android.title=") {
            notification.title = extra_value(title);
        } else if let Some(text) = trimmed.strip_prefix("android.text=") {
            notification.text = extra_value(text);
        }
    }
    notifications.extend(current);
    notifications
}

/// Section headers are the only lines indented by exactly two spaces.
fn is_section_header(line: &str) -> bool {
    line.starts_with("  ") && !line.starts_with("   ")
}

fn word_after<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let start = text.find(prefix)? + prefix.len();
    text[start..].split_whitespace().next()
}

/// Extras print as `Type (value)`, such as `String (Build finished)`. A value spanning lines
/// keeps only its first line.
fn extra_value(raw: &str) -> String {
    let Some((_, value)) = raw.split_once(" (") else {
        return String::new();
    };
    let value = value.strip_suffix(')').unwrap_or(value);
    if value == "null" {
        String::new()
    } else {
        value.to_string()
    }
}

/// Recent Android versions print flag names; older ones print the number.
fn is_quiet(flags: &str) -> bool {
    let number = match flags.strip_prefix("0x") {
        Some(hex) => u32::from_str_radix(hex, 16).ok(),
        None => flags.parse().ok(),
    };
    match number {
        Some(flags) => flags & QUIET_FLAGS != 0,
        None => QUIET_FLAG_NAMES.iter().any(|name| flags.contains(name)),
    }
}

fn show(name: &str, notification: Notification) {
    let app = format!("annex: {name}");
    let title = if notification.title.is_empty() {
        notification.package
    } else {
        notification.title
    };
    let shown = Command::new("notify-send")
        .args([
            "--app-name",
            &app,
            "--icon",
            "phone",
            &title,
            &notification.text,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    if let Err(error) = shown {
        eprintln!("{name}: could not show a notification: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DUMP: &str = "\
Current Notification Manager state:
  Notification List:
    NotificationRecord(0x0918e712: pkg=com.example.chat user=UserHandle{0} id=7 tag=null importance=4 key=0|com.example.chat|7|null|10123: Notification(channel=messages))
      uid=10123 userId=0
      flags=AUTO_CANCEL
      key=0|com.example.chat|7|null|10123
      notification=
            extras={
                android.title=String (Ada Lovelace)
                android.text=SpannableString (Are we still on for (lunch)?)
            }
    NotificationRecord(0x0aaaaaaa: pkg=com.example.music user=UserHandle{0} id=1 tag=null importance=3 key=0|com.example.music|1|null|10124: Notification(channel=playback))
      flags=ONGOING_EVENT|FOREGROUND_SERVICE
      key=0|com.example.music|1|null|10124
            extras={
                android.title=String (Now playing)
            }
    NotificationRecord(0x0bbbbbbb: pkg=com.android.shell user=UserHandle{0} id=2020 tag=x importance=2 key=0|com.android.shell|2020|x|2000: Notification(channel=shell_cmd))
      flags=0
      key=0|com.android.shell|2020|x|2000
            extras={
                android.title=String (Quiet)
                android.text=null
            }
  Enqueued Notification List:
    NotificationRecord(0x0ccccccc: pkg=com.example.late user=UserHandle{0} id=9 tag=null importance=4 key=late: Notification(channel=x))
      key=late
";

    #[test]
    fn parses_posted_notifications_only() {
        let notifications = parse(DUMP);
        let keys: Vec<&str> = notifications.iter().map(|n| n.key.as_str()).collect();
        assert_eq!(
            keys,
            [
                "0|com.example.chat|7|null|10123",
                "0|com.example.music|1|null|10124",
                "0|com.android.shell|2020|x|2000",
            ]
        );
    }

    #[test]
    fn reads_title_text_and_package() {
        let chat = &parse(DUMP)[0];
        assert_eq!(chat.package, "com.example.chat");
        assert_eq!(chat.title, "Ada Lovelace");
        assert_eq!(chat.text, "Are we still on for (lunch)?");
        assert!(chat.is_alerting);
    }

    #[test]
    fn ongoing_and_low_importance_notifications_stay_quiet() {
        let notifications = parse(DUMP);
        assert!(!notifications[1].is_alerting);
        assert!(!notifications[2].is_alerting);
        assert_eq!(notifications[2].text, "");
    }

    #[test]
    fn reads_numeric_and_named_flags() {
        assert!(is_quiet("0x62"));
        assert!(is_quiet("2"));
        assert!(!is_quiet("0"));
        assert!(!is_quiet("0x10"));
        assert!(is_quiet("NO_CLEAR|GROUP_SUMMARY"));
        assert!(!is_quiet("AUTO_CANCEL"));
    }

    #[test]
    fn an_empty_dump_has_no_notifications() {
        assert!(parse("").is_empty());
        assert!(parse("  Notification List:\n  Snoozed:\n").is_empty());
    }
}
