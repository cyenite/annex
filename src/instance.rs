use anyhow::{Context, Result, bail};
use clap::ValueEnum;

use crate::adb::Adb;
use crate::redroid::{self, Container, Display};
use crate::{location, waydroid};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum AndroidVersion {
    #[value(name = "13")]
    Android13,
    #[value(name = "14")]
    Android14,
    #[value(name = "15")]
    Android15,
}

impl AndroidVersion {
    pub fn label(self) -> &'static str {
        match self {
            Self::Android13 => "13",
            Self::Android14 => "14",
            Self::Android15 => "15",
        }
    }

    pub fn image(self) -> &'static str {
        match self {
            Self::Android13 => "redroid/redroid:13.0.0-latest",
            Self::Android14 => "redroid/redroid:14.0.0-latest",
            Self::Android15 => "redroid/redroid:15.0.0-latest",
        }
    }

    pub fn from_label(label: &str) -> Option<Self> {
        Self::value_variants()
            .iter()
            .copied()
            .find(|version| version.label() == label)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Running,
    Frozen,
    Stopped,
}

impl State {
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Frozen => "frozen",
            Self::Stopped => "stopped",
        }
    }
}

/// How an instance runs, in the terms users see. Container mode is Redroid; window mode is
/// Waydroid. User-facing text names the mode, never the project behind it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum Mode {
    /// Any number of instances, Android 13-15, snapshots and reset
    #[default]
    Container,
    /// One instance whose apps open in native desktop windows
    Window,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Container => "container",
            Self::Window => "window",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Backend {
    Redroid(Container),
    /// The Android release is only known once the session has run; see `waydroid::release`.
    Waydroid {
        release: Option<String>,
    },
}

impl Backend {
    pub fn mode(&self) -> Mode {
        match self {
            Self::Redroid(_) => Mode::Container,
            Self::Waydroid { .. } => Mode::Window,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Instance {
    pub name: String,
    pub serial: String,
    pub state: State,
    pub backend: Backend,
}

impl Instance {
    pub fn android(&self) -> &str {
        match &self.backend {
            Backend::Redroid(container) => container.android.label(),
            Backend::Waydroid { release } => release.as_deref().unwrap_or("-"),
        }
    }

    /// The container behind this instance, for operations window mode can't offer.
    pub fn container(&self, operation: &str) -> Result<&Container> {
        match &self.backend {
            Backend::Redroid(container) => Ok(container),
            Backend::Waydroid { .. } => bail!(
                "{operation} works in container mode only: window mode keeps its data in a system install"
            ),
        }
    }
}

#[derive(Clone, Copy, Default)]
pub struct UpOptions {
    pub mode: Option<Mode>,
    pub display: Option<Display>,
    pub android: Option<AndroidVersion>,
}

pub fn validate_name(name: &str) -> Result<()> {
    let is_valid = !name.is_empty()
        && name.len() <= 40
        && name.starts_with(|c: char| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !is_valid {
        bail!(
            "names use lowercase letters, digits and '-', start with a letter or digit, and are at most 40 characters"
        );
    }
    Ok(())
}

pub fn list() -> Result<Vec<Instance>> {
    let mut instances = redroid::list()?;
    instances.extend(waydroid::instance()?);
    instances.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(instances)
}

pub fn find(name: &str) -> Result<Option<Instance>> {
    if waydroid::registered_name().as_deref() == Some(name)
        && let Some(instance) = waydroid::instance()?
    {
        return Ok(Some(instance));
    }
    Ok(redroid::list()?
        .into_iter()
        .find(|instance| instance.name == name))
}

pub fn require(name: &str) -> Result<Instance> {
    find(name)?
        .with_context(|| format!("no instance named {name}: create it with `annex up {name}`"))
}

pub fn up(adb: &Adb, name: &str, options: UpOptions) -> Result<Instance> {
    validate_name(name)?;
    let current = find(name)?.map(|instance| instance.backend.mode());
    let mode = match (options.mode, current) {
        (Some(wanted), Some(current)) if wanted != current => bail!(
            "{name} runs in {} mode: remove it with `annex rm {name}` to recreate it in {} mode",
            current.label(),
            wanted.label()
        ),
        (wanted, current) => wanted.or(current).unwrap_or_default(),
    };
    match mode {
        Mode::Container => redroid::up(adb, name, options),
        Mode::Window => {
            if options.android.is_some() || options.display.is_some() {
                bail!(
                    "--android and --display apply to container mode; window mode's Android and screen come from its system image"
                );
            }
            waydroid::up(adb, name)
        }
    }
}

pub fn down(adb: &Adb, name: &str) -> Result<()> {
    let instance = require(name)?;
    adb.disconnect(&instance.serial);
    match &instance.backend {
        Backend::Redroid(_) => redroid::down(&instance),
        Backend::Waydroid { .. } => waydroid::down(),
    }
}

pub fn freeze(name: &str) -> Result<()> {
    let instance = require(name)?;
    if instance.state != State::Running {
        bail!(
            "{name} is {}, only a running instance can be frozen",
            instance.state.label()
        );
    }
    match &instance.backend {
        Backend::Redroid(_) => redroid::freeze(&instance),
        Backend::Waydroid { .. } => waydroid::freeze(),
    }
}

pub fn thaw(adb: &Adb, name: &str) -> Result<()> {
    let instance = require(name)?;
    if instance.state != State::Frozen {
        bail!("{name} is {}, not frozen", instance.state.label());
    }
    match &instance.backend {
        Backend::Redroid(_) => redroid::thaw(&instance)?,
        Backend::Waydroid { .. } => waydroid::thaw()?,
    }
    adb.connect(&instance.serial)
}

pub fn remove(adb: &Adb, name: &str, keep_data: bool) -> Result<()> {
    let instance = require(name)?;
    adb.disconnect(&instance.serial);
    match &instance.backend {
        Backend::Redroid(_) => {
            redroid::remove(&instance, keep_data)?;
            location::forget(name)
        }
        Backend::Waydroid { .. } => {
            println!(
                "window mode keeps its Android data in a system install; the next window-mode instance starts with it"
            );
            waydroid::remove()
        }
    }
}

/// What `annex up` creates and starts when no name is given.
pub const DEFAULT_NAME: &str = "default";

/// The instance a command means when its name is left out: `default` if it exists, otherwise
/// the only running instance.
pub fn resolve(name: Option<String>) -> Result<String> {
    if let Some(name) = name {
        return Ok(name);
    }
    let instances = list()?;
    if instances
        .iter()
        .any(|instance| instance.name == DEFAULT_NAME)
    {
        return Ok(DEFAULT_NAME.to_string());
    }
    let running: Vec<&str> = instances
        .iter()
        .filter(|instance| instance.state == State::Running)
        .map(|instance| instance.name.as_str())
        .collect();
    match running[..] {
        [only] => Ok(only.to_string()),
        [] => bail!("no instance is running: start one with `annex up`"),
        _ => bail!(
            "several instances are running ({}): name the one you mean",
            running.join(", ")
        ),
    }
}

/// Splits a command's positional arguments into an optional leading instance name and the
/// rest. The first argument counts as a name only when an instance has that name and at least
/// `rest` arguments remain after it, so a file that happens to share a name still works when
/// it's the only argument.
pub fn split_name(mut args: Vec<String>, rest: usize) -> Result<(String, Vec<String>)> {
    let names: Vec<String> = list()?.into_iter().map(|instance| instance.name).collect();
    if is_leading_name(&args, rest, &names) {
        let name = args.remove(0);
        return Ok((name, args));
    }
    Ok((resolve(None)?, args))
}

fn is_leading_name(args: &[String], rest: usize, names: &[String]) -> bool {
    args.len() > rest && args.first().is_some_and(|first| names.contains(first))
}

/// Fails unless the instance is running, so adb commands don't hang on a frozen one.
pub fn require_running(name: &str) -> Result<Instance> {
    let instance = require(name)?;
    match instance.state {
        State::Running => Ok(instance),
        State::Frozen => bail!("{name} is frozen: `annex thaw {name}` first"),
        State::Stopped => bail!("{name} is stopped: `annex up {name}` first"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_simple_names() {
        for name in ["pixel", "old-13", "a", "9lives"] {
            assert!(validate_name(name).is_ok(), "{name}");
        }
    }

    #[test]
    fn rejects_names_docker_or_a_shell_would_mangle() {
        let too_long = "a".repeat(41);
        for name in ["", "Pixel", "-x", "a_b", "a b", "a/b", too_long.as_str()] {
            assert!(validate_name(name).is_err(), "{name}");
        }
    }

    #[test]
    fn a_leading_instance_name_needs_arguments_after_it() {
        let names = vec!["pixel".to_string()];
        let args = |parts: &[&str]| {
            parts
                .iter()
                .map(|part| part.to_string())
                .collect::<Vec<_>>()
        };
        assert!(is_leading_name(&args(&["pixel", "app.apk"]), 1, &names));
        assert!(!is_leading_name(&args(&["pixel"]), 1, &names));
        assert!(!is_leading_name(&args(&["other", "app.apk"]), 1, &names));
        assert!(is_leading_name(&args(&["pixel", "/sdcard/a"]), 1, &names));
        assert!(!is_leading_name(&args(&["/sdcard/a", "out"]), 1, &names));
    }

    #[test]
    fn android_labels_round_trip() {
        for version in AndroidVersion::value_variants() {
            assert_eq!(AndroidVersion::from_label(version.label()), Some(*version));
        }
        assert_eq!(AndroidVersion::from_label("16"), None);
    }
}
