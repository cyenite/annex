use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::adb::{Adb, validate_package};
use crate::instance::{Backend, Instance, State};
use crate::paths::{data_dir, data_home, log_dir, which};
use crate::process::{log_tail, output_within, spawn_detached, stdout_within};

const ADB_PORT: u16 = 5555;
const DEFAULT_IP: &str = "192.168.240.112";
const CONFIG: &str = "/var/lib/waydroid/waydroid.cfg";
// The container's own cgroup is the truth about whether it runs or is frozen. `waydroid status`
// asks Waydroid's root service, whose lxc-info calls can fail and then report a running
// container as stopped.
const CGROUP_EVENTS: &str = "/sys/fs/cgroup/lxc.payload.waydroid/cgroup.events";
const FREEZE_TIMEOUT: Duration = Duration::from_secs(5);
const BOOT_TIMEOUT: Duration = Duration::from_secs(120);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const STOP_TIMEOUT: Duration = Duration::from_secs(30);
/// The container manager is still tearing down for a moment after the container is gone.
const STOP_SETTLE: Duration = Duration::from_secs(3);
// Waydroid opens each app in its own desktop window only with multi_windows on, and with
// suspend on it freezes the container whenever no window is open, which drops adb.
const PROPERTIES: [(&str, &str); 2] = [
    ("persist.waydroid.multi_windows", "true"),
    ("persist.waydroid.suspend", "false"),
];

pub fn is_installed() -> bool {
    which("waydroid").is_some()
}

pub fn is_initialized() -> bool {
    Path::new(CONFIG).is_file()
}

/// Waydroid runs one system-wide session, so window mode has at most one instance. Its name is
/// the one state file annex keeps, since nothing in Waydroid can carry a label.
pub fn registered_name() -> Option<String> {
    fs::read_to_string(state_dir().join("name"))
        .ok()
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
}

/// The window-mode instance, or None when none is registered or Waydroid isn't set up.
pub fn instance() -> Result<Option<Instance>> {
    let Some(name) = registered_name() else {
        return Ok(None);
    };
    if !is_installed() || !is_initialized() {
        return Ok(None);
    }
    Ok(Some(Instance {
        name,
        serial: format!("{DEFAULT_IP}:{ADB_PORT}"),
        state: container_state(),
        backend: Backend::Waydroid {
            release: fs::read_to_string(release_cache())
                .ok()
                .map(|release| release.trim().to_string()),
        },
    }))
}

pub fn up(adb: &Adb, name: &str) -> Result<Instance> {
    if !is_installed() || !is_initialized() {
        bail!("window mode isn't set up: run `annex setup --window-mode`");
    }
    match registered_name() {
        Some(existing) if existing != name => bail!(
            "window mode runs one instance, and it is {existing}: use `annex up {existing}`, or `annex rm {existing}` first"
        ),
        Some(_) => {}
        None => {
            fs::create_dir_all(state_dir())?;
            fs::write(state_dir().join("name"), name)?;
        }
    }
    match container_state() {
        State::Stopped => start_session()?,
        State::Frozen => container_call("Unfreeze")?,
        State::Running => {}
    }
    if apply_properties()? {
        println!("restarting {name} to turn on app windows (first time only)");
        down()?;
        wait_until_stopped()?;
        start_session()?;
    }
    let serial = require_instance()?.serial;
    if !authorize_key(adb)? {
        println!(
            "approve the USB debugging prompt on {name}'s screen (`annex show {name}`) to continue"
        );
    }
    wait_until_booted(adb, name, &serial)?;
    if let Ok(release) = adb.shell(&serial, "getprop ro.build.version.release", COMMAND_TIMEOUT) {
        let cache = release_cache();
        fs::create_dir_all(cache.parent().expect("cache path has a parent"))?;
        fs::write(cache, release)?;
    }
    require_instance()
}

pub fn down() -> Result<()> {
    waydroid(&["session", "stop"]).map(drop)
}

/// Stops the session and forgets the instance's name. Android's data stays in Waydroid's
/// system install, which annex can't delete without root.
pub fn remove() -> Result<()> {
    if container_state() != State::Stopped {
        down()?;
    }
    fs::remove_file(state_dir().join("name"))?;
    Ok(())
}

/// Waydroid's container service lets any local user freeze and unfreeze over the system bus,
/// so this needs no root, unlike `waydroid container freeze`.
pub fn freeze() -> Result<()> {
    container_call("Freeze")?;
    wait_for_state(State::Frozen)
}

pub fn thaw() -> Result<()> {
    container_call("Unfreeze")?;
    wait_for_state(State::Running)
}

pub fn show() -> Result<()> {
    let log = log_dir().join("window-mode-screen.log");
    spawn_detached(Command::new("waydroid").arg("show-full-ui"), &log).map(drop)
}

pub fn open(package: &str) -> Result<()> {
    validate_package(package)?;
    waydroid(&["app", "launch", package]).map(drop)
}

fn require_instance() -> Result<Instance> {
    instance()?.context("window mode isn't set up: run `annex setup --window-mode`")
}

fn container_state() -> State {
    fs::read_to_string(CGROUP_EVENTS).map_or(State::Stopped, |events| parse_cgroup_events(&events))
}

fn parse_cgroup_events(events: &str) -> State {
    let value = |key: &str| {
        events
            .lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix(' '))
            .map(str::trim)
    };
    match (value("populated"), value("frozen")) {
        (Some("1"), Some("1")) => State::Frozen,
        (Some("1"), _) => State::Running,
        _ => State::Stopped,
    }
}

/// Waydroid's service answers a freeze or unfreeze call even when it skipped the work, which
/// it does whenever its own view of the container is wrong, so the result is checked.
fn wait_for_state(wanted: State) -> Result<()> {
    let started = Instant::now();
    while container_state() != wanted {
        if started.elapsed() > FREEZE_TIMEOUT {
            bail!(
                "window mode's system service accepted the request but the instance is still {}; its state is out of sync: run `annex setup --repair`",
                container_state().label()
            );
        }
        sleep(Duration::from_millis(100));
    }
    Ok(())
}

fn start_session() -> Result<()> {
    let log = log_dir().join("window-mode-session.log");
    let mut session = spawn_detached(Command::new("waydroid").args(["session", "start"]), &log)?;
    wait_for_container(&mut session, &log)
}

fn wait_for_container(session: &mut Child, log: &Path) -> Result<()> {
    let started = Instant::now();
    loop {
        if container_state() == State::Running {
            return Ok(());
        }
        if let Some(exit) = session.try_wait()? {
            bail!(
                "window mode's session exited ({exit}):\n{}",
                log_tail(log, 8)
            );
        }
        if started.elapsed() > BOOT_TIMEOUT {
            bail!(
                "window mode's session did not start within {} s: see {}",
                BOOT_TIMEOUT.as_secs(),
                log.display()
            );
        }
        sleep(Duration::from_millis(500));
    }
}

/// `waydroid session stop` returns before the container is down, and a session started in
/// that window boots an Android whose SurfaceFlinger never registers, so system_server hangs.
fn wait_until_stopped() -> Result<()> {
    let started = Instant::now();
    while container_state() != State::Stopped {
        if started.elapsed() > STOP_TIMEOUT {
            bail!(
                "window mode's session did not stop within {} s",
                STOP_TIMEOUT.as_secs()
            );
        }
        sleep(Duration::from_millis(500));
    }
    sleep(STOP_SETTLE);
    Ok(())
}

/// Sets annex's Waydroid properties and reports whether multi_windows changed, which only
/// takes effect after the session restarts.
fn apply_properties() -> Result<bool> {
    let mut needs_restart = false;
    for (key, wanted) in PROPERTIES {
        if waydroid(&["prop", "get", key])? != wanted {
            waydroid(&["prop", "set", key, wanted])?;
            needs_restart |= key.ends_with("multi_windows");
        }
    }
    Ok(needs_restart)
}

/// Adds the user's adb key to the keys Waydroid's adbd trusts, so a new session needs no
/// on-screen approval. Returns false when the file isn't writable: its owner is Android's
/// system uid 1000, which is only the desktop user on a machine whose first user has uid 1000.
fn authorize_key(adb: &Adb) -> Result<bool> {
    let public_key = fs::read_to_string(adb.public_key()?)?;
    let keys_path = waydroid_data().join("misc/adb/adb_keys");
    let trusted = fs::read_to_string(&keys_path).unwrap_or_default();
    if trusted.lines().any(|line| line.trim() == public_key.trim()) {
        return Ok(true);
    }
    let Ok(mut keys) = OpenOptions::new().append(true).open(&keys_path) else {
        return Ok(false);
    };
    let separator = if trusted.is_empty() || trusted.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    writeln!(keys, "{separator}{}", public_key.trim())?;
    Ok(true)
}

fn wait_until_booted(adb: &Adb, name: &str, serial: &str) -> Result<()> {
    let started = Instant::now();
    adb.connect_within(serial, BOOT_TIMEOUT)?;
    loop {
        let is_booted = adb
            .shell(serial, "getprop sys.boot_completed", COMMAND_TIMEOUT)
            .is_ok_and(|value| value == "1");
        if is_booted {
            return Ok(());
        }
        if started.elapsed() > BOOT_TIMEOUT {
            bail!(
                "{name} did not finish booting within {} s",
                BOOT_TIMEOUT.as_secs()
            );
        }
        sleep(Duration::from_millis(500));
    }
}

fn container_call(method: &str) -> Result<()> {
    stdout_within(
        Command::new("busctl").args([
            "--system",
            "call",
            "id.waydro.Container",
            "/ContainerManager",
            "id.waydro.ContainerManager",
            method,
        ]),
        COMMAND_TIMEOUT,
    )
    .with_context(|| format!("window mode's system service refused {method}"))
    .map(drop)
}

/// Runs the waydroid tool, reporting failures in annex's own terms.
fn waydroid(args: &[&str]) -> Result<String> {
    let output = output_within(Command::new("waydroid").args(args), COMMAND_TIMEOUT)?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("window mode: {}", stderr.trim());
    }
    Ok(stdout)
}

fn waydroid_data() -> PathBuf {
    data_home().join("waydroid/data")
}

fn state_dir() -> PathBuf {
    data_dir().join("window-mode")
}

fn release_cache() -> PathBuf {
    state_dir().join("release")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_running_container() {
        assert_eq!(
            parse_cgroup_events("populated 1\nfrozen 0\n"),
            State::Running
        );
    }

    #[test]
    fn reads_a_frozen_container() {
        assert_eq!(
            parse_cgroup_events("populated 1\nfrozen 1\n"),
            State::Frozen
        );
    }

    #[test]
    fn an_empty_or_missing_cgroup_is_stopped() {
        assert_eq!(
            parse_cgroup_events("populated 0\nfrozen 0\n"),
            State::Stopped
        );
        assert_eq!(parse_cgroup_events(""), State::Stopped);
    }
}
