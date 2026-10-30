use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle, sleep};
use std::time::Duration;

use anyhow::{Context, Result};
use zbus::blocking::{Connection, Proxy, proxy::Builder};
use zbus::proxy::CacheProperties;
use zbus::zvariant::{ObjectPath, OwnedObjectPath};

use crate::launcher::BRIDGE_ID;
use crate::location::Fix;

const POLL_INTERVAL: Duration = Duration::from_secs(2);
const GEOCLUE: &str = "org.freedesktop.GeoClue2";
/// GClueAccuracyLevel's EXACT; the desktop's own setting can lower it.
const EXACT_ACCURACY: u32 = 8;

/// This computer's position from GeoClue, kept up to date until dropped. Only runs while an
/// instance asks for it, since GeoClue scans Wi-Fi networks while a client is started.
pub struct HostLocation {
    fix: Arc<Mutex<Option<Fix>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<()>>>,
}

impl HostLocation {
    pub fn start() -> Self {
        let fix = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let latest = fix.clone();
        let should_stop = stop.clone();
        let thread = Some(thread::spawn(move || watch(&latest, &should_stop)));
        Self { fix, stop, thread }
    }

    pub fn fix(&self) -> Option<Fix> {
        *self.fix.lock().expect("host location lock")
    }

    pub fn has_stopped(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }

    /// Why GeoClue stopped, such as permission refused or location turned off.
    pub fn reason(mut self) -> anyhow::Error {
        let thread = self.thread.take().expect("joined once");
        match thread.join().expect("host location thread panicked") {
            Ok(()) => anyhow::anyhow!("GeoClue stopped"),
            Err(error) => error,
        }
    }
}

impl Drop for HostLocation {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn watch(latest: &Mutex<Option<Fix>>, stop: &AtomicBool) -> Result<()> {
    let connection = Connection::system().context("could not reach the system bus")?;
    let client = start_client(&connection)?;
    let mut shown = None;
    while !stop.load(Ordering::Relaxed) {
        let path: OwnedObjectPath = client.get_property("Location")?;
        if path.as_str() != "/" && shown.as_ref() != Some(&path) {
            let fix = read_fix(&connection, &path)?;
            *latest.lock().expect("host location lock") = Some(fix);
            shown = Some(path);
        }
        sleep(POLL_INTERVAL);
    }
    client.call_method("Stop", &())?;
    Ok(())
}

/// Blocks while the desktop asks the user whether annex may use their location, the first time.
fn start_client(connection: &Connection) -> Result<Proxy<'static>> {
    let manager = proxy(
        connection,
        ObjectPath::from_static_str("/org/freedesktop/GeoClue2/Manager")?,
        "org.freedesktop.GeoClue2.Manager",
    )?;
    let path: OwnedObjectPath = manager
        .call("GetClient", &())
        .context("GeoClue, the desktop's location service, isn't available")?;
    let client = proxy(
        connection,
        path.into_inner(),
        "org.freedesktop.GeoClue2.Client",
    )?;
    client.set_property("DesktopId", BRIDGE_ID)?;
    client.set_property("RequestedAccuracyLevel", EXACT_ACCURACY)?;
    client.call_method("Start", &()).context(
        "refused: turn on Location Services in your desktop's privacy settings and allow Annex",
    )?;
    Ok(client)
}

fn read_fix(connection: &Connection, path: &OwnedObjectPath) -> Result<Fix> {
    let location = proxy(
        connection,
        path.clone().into_inner(),
        "org.freedesktop.GeoClue2.Location",
    )?;
    Ok(Fix {
        accuracy: location.get_property("Accuracy")?,
        latitude: location.get_property("Latitude")?,
        longitude: location.get_property("Longitude")?,
    })
}

// Cached properties would come from PropertiesChanged signals; reading them directly is simpler
// for a value polled every few seconds.
fn proxy(
    connection: &Connection,
    path: ObjectPath<'static>,
    interface: &'static str,
) -> Result<Proxy<'static>> {
    Ok(Builder::new(connection)
        .destination(GEOCLUE)?
        .path(path)?
        .interface(interface)?
        .cache_properties(CacheProperties::No)
        .build()?)
}
