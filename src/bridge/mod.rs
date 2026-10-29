//! The desktop bridge: a long-running process that keeps one clipboard across the desktop and
//! every running Redroid instance, and shows Android notifications as desktop notifications.
//! Waydroid needs neither, since its session does both itself.

mod device;
mod host;
mod hub;
mod notifications;
mod protocol;
pub mod service;
mod tray;

use std::collections::HashMap;
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::thread::{self, sleep};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::adb::Adb;
use crate::instance::{Instance, Mode, State};
use crate::{instance, launcher};
use device::{ServerJar, Session};
use hub::Event;
use protocol::{DeviceMessage, read_message};

const RECONCILE_INTERVAL: Duration = Duration::from_secs(2);
const RETRY_DELAY: Duration = Duration::from_secs(15);
const ATTACH_TIMEOUT: Duration = Duration::from_secs(5);

static VERBOSE: AtomicBool = AtomicBool::new(false);

/// Logs clipboard traffic when the bridge runs with --verbose. Only sizes are logged: the
/// clipboard often holds passwords.
fn trace(message: impl FnOnce() -> String) {
    if VERBOSE.load(Ordering::Relaxed) {
        eprintln!("{}", message());
    }
}

/// Runs the bridge in the foreground until the desktop clipboard connection breaks.
pub fn run(adb: &Adb, is_verbose: bool) -> Result<()> {
    VERBOSE.store(is_verbose, Ordering::Relaxed);
    let jar = match ServerJar::locate() {
        Ok(jar) => Some(jar),
        Err(error) => {
            eprintln!("clipboard sync is off: {error:#}");
            None
        }
    };
    let (events, hub) = hub::spawn()?;
    let mut bridge = Bridge {
        adb,
        jar,
        events,
        attached: HashMap::new(),
        retry_after: HashMap::new(),
    };
    if let Err(error) = launcher::install_icon() {
        eprintln!("could not install annex's icon: {error:#}");
    }
    let menu = match ksni::blocking::TrayMethods::spawn(tray::Tray::default()) {
        Ok(menu) => Some(menu),
        Err(error) => {
            eprintln!("panel menu unavailable (no tray host on the session bus): {error}");
            None
        }
    };
    let mut shown: Vec<tray::Entry> = Vec::new();
    eprintln!("bridge running");
    loop {
        if hub.is_finished() {
            return hub.join().expect("clipboard hub panicked");
        }
        match instance::list() {
            Ok(instances) => {
                bridge.reconcile(&instances);
                let entries: Vec<tray::Entry> =
                    instances.iter().map(tray::Entry::from_instance).collect();
                if let Some(menu) = &menu
                    && entries != shown
                {
                    menu.update(|tray| tray.entries = entries.clone());
                    shown = entries;
                }
            }
            Err(error) => eprintln!("could not list instances: {error:#}"),
        }
        sleep(RECONCILE_INTERVAL);
    }
}

struct Bridge<'a> {
    adb: &'a Adb,
    jar: Option<ServerJar>,
    events: Sender<Event>,
    attached: HashMap<String, Attachment>,
    retry_after: HashMap<String, Instant>,
}

/// Everything the bridge runs for one instance; dropping it stops all of it.
struct Attachment {
    clipboard: Option<Clipboard>,
    _notifications: notifications::Watcher,
}

struct Clipboard {
    name: String,
    events: Sender<Event>,
    reader: thread::JoinHandle<()>,
    _session: Session,
}

impl Drop for Clipboard {
    fn drop(&mut self) {
        let _ = self.events.send(Event::Detach {
            name: self.name.clone(),
        });
    }
}

impl Bridge<'_> {
    /// Attaches to instances that are running and detaches from the rest. A frozen instance
    /// is detached too: adb calls against it would block until it thaws.
    fn reconcile(&mut self, instances: &[Instance]) {
        let running: Vec<&Instance> = instances
            .iter()
            .filter(|instance| instance.state == State::Running)
            .filter(|instance| instance.backend.mode() == Mode::Container)
            .collect();
        let retry_after = &mut self.retry_after;
        self.attached.retain(|name, attachment| {
            let is_running = running.iter().any(|instance| &instance.name == name);
            let is_healthy = attachment
                .clipboard
                .as_ref()
                .is_none_or(|clipboard| !clipboard.reader.is_finished());
            if !is_running {
                eprintln!("{name}: detached (no longer running)");
            } else if !is_healthy {
                eprintln!("{name}: clipboard connection closed, reattaching shortly");
                retry_after.insert(name.clone(), Instant::now() + RETRY_DELAY);
            }
            is_running && is_healthy
        });
        for instance in running {
            let is_waiting = self
                .retry_after
                .get(&instance.name)
                .is_some_and(|retry| Instant::now() < *retry);
            if self.attached.contains_key(&instance.name) || is_waiting {
                continue;
            }
            match self.attach(instance) {
                Ok(Some(attachment)) => {
                    self.retry_after.remove(&instance.name);
                    self.attached.insert(instance.name.clone(), attachment);
                }
                Ok(None) => {}
                Err(error) => {
                    eprintln!("{}: could not attach: {error:#}", instance.name);
                    self.retry_after
                        .insert(instance.name.clone(), Instant::now() + RETRY_DELAY);
                }
            }
        }
    }

    /// Returns None while the instance is still booting.
    fn attach(&self, instance: &Instance) -> Result<Option<Attachment>> {
        let Instance { name, serial, .. } = instance;
        self.adb.connect_within(serial, ATTACH_TIMEOUT)?;
        let is_booted = self
            .adb
            .shell(serial, "getprop sys.boot_completed", ATTACH_TIMEOUT)
            .is_ok_and(|value| value == "1");
        if !is_booted {
            return Ok(None);
        }
        let clipboard = match &self.jar {
            Some(jar) => Some(self.attach_clipboard(name, serial, jar)?),
            None => None,
        };
        eprintln!(
            "{name}: attached (clipboard {}, notifications on)",
            if clipboard.is_some() { "on" } else { "off" }
        );
        Ok(Some(Attachment {
            clipboard,
            _notifications: notifications::Watcher::start(self.adb, name, serial),
        }))
    }

    fn attach_clipboard(&self, name: &str, serial: &str, jar: &ServerJar) -> Result<Clipboard> {
        let (session, control) = Session::start(self.adb, name, serial, jar)?;
        let reader_socket = control.try_clone()?;
        self.events
            .send(Event::Attach {
                name: name.to_string(),
                control,
            })
            .context("the clipboard hub stopped")?;
        let events = self.events.clone();
        let reader_name = name.to_string();
        let reader = thread::Builder::new()
            .name(format!("clipboard-{name}"))
            .spawn(move || read_device(&reader_name, reader_socket, &events))?;
        Ok(Clipboard {
            name: name.to_string(),
            events: self.events.clone(),
            reader,
            _session: session,
        })
    }
}

fn read_device(name: &str, mut socket: TcpStream, events: &Sender<Event>) {
    while let Ok(message) = read_message(&mut socket) {
        if let DeviceMessage::Clipboard(text) = message {
            let event = Event::Device {
                name: name.to_string(),
                text,
            };
            if events.send(event).is_err() {
                return;
            }
        }
    }
}
