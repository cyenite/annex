//! Device preferences `annex up` can set after boot (time zone, locale, animations), so a test
//! run starts from known settings without ad hoc `adb shell` calls. They are saved on the data
//! volume, like a user's own changes, and survive restarts and snapshots.

use std::str::FromStr;
use std::time::Duration;

use anyhow::{Result, bail};
use clap::ValueEnum;

use crate::adb::Adb;
use crate::docker;
use crate::instance::{self, Backend, Instance};
use crate::redroid::{self, container_name};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(15);
const ANIMATION_SCALES: [&str; 3] = [
    "window_animation_scale",
    "transition_animation_scale",
    "animator_duration_scale",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Animations {
    On,
    Off,
}

impl Animations {
    fn scale(self) -> &'static str {
        match self {
            Self::On => "1",
            Self::Off => "0",
        }
    }
}

/// An IANA time zone name, like Africa/Nairobi or UTC.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimeZone(String);

/// A BCP 47 language tag of the kind Android's language settings use, like en-KE or zh-Hant-TW.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Locale(String);

#[derive(Clone, Debug, Default)]
pub struct Preferences {
    pub locale: Option<Locale>,
    pub timezone: Option<TimeZone>,
    pub animations: Option<Animations>,
}

impl Preferences {
    pub fn is_empty(&self) -> bool {
        self.locale.is_none() && self.timezone.is_none() && self.animations.is_none()
    }
}

impl FromStr for TimeZone {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, String> {
        let is_valid = !value.is_empty()
            && value.len() <= 64
            && value.split('/').all(|part| {
                part.starts_with(|c: char| c.is_ascii_alphabetic())
                    && part
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+'))
            });
        if !is_valid {
            return Err(format!(
                "{value:?} is not a time zone name like Africa/Nairobi or UTC"
            ));
        }
        Ok(Self(value.to_string()))
    }
}

impl FromStr for Locale {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, String> {
        let invalid = || format!("{value:?} is not a language tag like en-KE, sw-KE or fr-FR");
        let mut parts = value.split('-');
        let language = parts.next().unwrap_or_default();
        if !(2..=3).contains(&language.len()) || !language.chars().all(|c| c.is_ascii_lowercase()) {
            return Err(invalid());
        }
        let mut tag = language.to_string();
        let mut rest: Vec<&str> = parts.collect();
        if let Some(script) = rest.first().filter(|part| part.len() == 4) {
            let mut chars = script.chars();
            let is_script = chars.next().is_some_and(|c| c.is_ascii_uppercase())
                && chars.all(|c| c.is_ascii_lowercase());
            if !is_script {
                return Err(invalid());
            }
            tag.push('-');
            tag.push_str(script);
            rest.remove(0);
        }
        match rest[..] {
            [] => {}
            [region]
                if (region.len() == 2 && region.chars().all(|c| c.is_ascii_uppercase()))
                    || (region.len() == 3 && region.chars().all(|c| c.is_ascii_digit())) =>
            {
                tag.push('-');
                tag.push_str(region);
            }
            _ => return Err(invalid()),
        }
        Ok(Self(tag))
    }
}

/// Applies each preference that is set and returns what changed, in words. The instance must
/// be running and connected.
pub fn apply(adb: &Adb, instance: &Instance, preferences: &Preferences) -> Result<Vec<String>> {
    let mut applied = Vec::new();
    if let Some(locale) = &preferences.locale {
        set_locale(adb, instance, locale)?;
        applied.push(format!("language {}", locale.0));
    }
    if let Some(timezone) = &preferences.timezone {
        set_timezone(adb, instance, timezone)?;
        applied.push(format!("time zone {}", timezone.0));
    }
    if let Some(animations) = preferences.animations {
        let commands: Vec<String> = ANIMATION_SCALES
            .iter()
            .map(|setting| format!("settings put global {setting} {}", animations.scale()))
            .collect();
        adb.shell(&instance.serial, &commands.join(" && "), COMMAND_TIMEOUT)?;
        applied.push(match animations {
            Animations::On => "animations on".to_string(),
            Animations::Off => "animations off".to_string(),
        });
    }
    Ok(applied)
}

fn set_timezone(adb: &Adb, instance: &Instance, timezone: &TimeZone) -> Result<()> {
    let serial = &instance.serial;
    let command = format!(
        "settings put global auto_time_zone 0 && cmd alarm set-timezone {}",
        timezone.0
    );
    adb.shell(serial, &command, COMMAND_TIMEOUT)?;
    // Android falls back to GMT for a zone its database doesn't know, without an error.
    let current = adb.shell(serial, "getprop persist.sys.timezone", COMMAND_TIMEOUT)?;
    if current != timezone.0 {
        bail!(
            "{}'s Android doesn't know the time zone {}: use a name from the IANA database, like Africa/Nairobi",
            instance.name,
            timezone.0
        );
    }
    Ok(())
}

/// Android reads the system language once, when its framework starts, so a change restarts the
/// framework (about 15 s); an instance already in that language is left alone.
fn set_locale(adb: &Adb, instance: &Instance, locale: &Locale) -> Result<()> {
    let Backend::Redroid(_) = &instance.backend else {
        bail!(
            "--locale works in container mode only: set window mode's language in its Settings app"
        );
    };
    let container = container_name(&instance.name);
    let current = docker::run(&["exec", &container, "getprop", "persist.sys.locale"])?;
    if current == locale.0 {
        return Ok(());
    }
    // sys.boot_completed stays 1 across a framework restart unless cleared, and the wait below
    // would then return before the framework is back.
    let script = "setprop persist.sys.locale \"$1\" && setprop sys.boot_completed 0 && setprop ctl.restart zygote";
    docker::run(&["exec", &container, "sh", "-c", script, "sh", &locale.0])?;
    redroid::wait_until_booted(instance)?;
    instance::wait_until_ready(adb, instance)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_time_zone_names() {
        for value in [
            "Africa/Nairobi",
            "UTC",
            "America/Argentina/Buenos_Aires",
            "Etc/GMT+3",
            "America/Port-au-Prince",
        ] {
            assert_eq!(value.parse::<TimeZone>().unwrap().0, value);
        }
    }

    #[test]
    fn rejects_time_zones_a_shell_could_interpret() {
        for value in [
            "",
            "/etc/passwd",
            "Africa/../x",
            "UTC; reboot",
            "a b",
            "Africa/",
        ] {
            assert!(value.parse::<TimeZone>().is_err(), "{value}");
        }
    }

    #[test]
    fn accepts_language_tags() {
        for value in ["en", "en-KE", "sw-KE", "zh-Hant-TW", "es-419", "fil-PH"] {
            assert_eq!(value.parse::<Locale>().unwrap().0, value);
        }
    }

    #[test]
    fn rejects_malformed_language_tags() {
        for value in [
            "",
            "EN",
            "en_KE",
            "en-ke",
            "english",
            "en-KE-x",
            "zh-hant-TW",
            "en;id",
        ] {
            assert!(value.parse::<Locale>().is_err(), "{value}");
        }
    }

    #[test]
    fn empty_preferences_change_nothing() {
        assert!(Preferences::default().is_empty());
        let preferences = Preferences {
            animations: Some(Animations::Off),
            ..Preferences::default()
        };
        assert!(!preferences.is_empty());
    }
}
