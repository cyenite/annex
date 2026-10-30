//! Location for container-mode instances, at a fixed position or at this computer's own (shared
//! by the desktop bridge). Redroid has no GPS, so annex mounts its own GNSS HAL
//! (`android/gnss/`) into every instance; it reads the files below and reports them as real GPS
//! fixes. Android's test providers would be simpler, but it marks their fixes as mock locations,
//! which apps with fraud checks refuse.

use std::fmt;
use std::fs::{self, Permissions};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::str::FromStr;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::docker;
use crate::instance::{Backend, Instance};
use crate::paths::data_dir;
use crate::process::stdout_within;
use crate::redroid::container_name;

const SETTING_PATH: &str = "/data/local/tmp/annex-location";
const HOST_FIX_PATH: &str = "/dev/annex-host-location";
/// The Android emulator's default position, which developers already expect from an emulator.
/// The GNSS HAL reports the same one when an instance has no setting.
pub const DEFAULT_POSITION: &str = "37.4220,-122.0841";
const GNSS_HAL: &[u8] = include_bytes!("../android/gnss/android.hardware.gnss-service.annex");
const GNSS_INIT: &str = include_str!("../android/gnss/annex-gnss.rc");
const GNSS_MANIFEST: &str = include_str!("../android/gnss/annex-gnss.xml");
const LINK_TIMEOUT: Duration = Duration::from_secs(20);
const SHORT_LINK_HOSTS: [&str; 2] = ["maps.app.goo.gl/", "goo.gl/maps/"];

/// Where an instance says it is, saved on its data volume so it survives restarts and snapshots.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Location {
    #[default]
    Default,
    Host,
    Fixed {
        latitude: f64,
        longitude: f64,
    },
}

/// One reading of this computer's position; accuracy is in meters. A fix guessed from the
/// computer's IP address is off by kilometers, which is worth telling the user.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fix {
    pub accuracy: f64,
    pub latitude: f64,
    pub longitude: f64,
    pub is_from_ip: bool,
}

impl fmt::Display for Location {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Default => write!(formatter, "default"),
            Self::Host => write!(formatter, "host"),
            Self::Fixed {
                latitude,
                longitude,
            } => write!(formatter, "{latitude},{longitude}"),
        }
    }
}

impl FromStr for Location {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, String> {
        match value {
            "default" => return Ok(Self::Default),
            "host" => return Ok(Self::Host),
            _ => {}
        }
        let position = if is_link(value) {
            position_in_link(value).ok_or_else(|| {
                "found no position in that link: in Google Maps, right-click the spot and click its coordinates to copy them".to_string()
            })?
        } else {
            parse_position(value).ok_or_else(|| {
                format!(
                    "{value:?} is not `default`, `host`, a position like -1.2921,36.8219 or a Google Maps link"
                )
            })?
        };
        let (latitude, longitude) = position;
        if !(-90.0..=90.0).contains(&latitude) || !(-180.0..=180.0).contains(&longitude) {
            return Err(format!(
                "{value} is outside latitude -90 to 90 and longitude -180 to 180"
            ));
        }
        Ok(Self::Fixed {
            latitude,
            longitude,
        })
    }
}

/// Reads what the user typed: `default`, `host`, a position, or a Google Maps link. Short share
/// links (maps.app.goo.gl) carry no position until followed, so they are expanded first.
pub fn parse_input(value: &str) -> Result<Location> {
    if !SHORT_LINK_HOSTS.iter().any(|host| value.contains(host)) {
        return value.parse().map_err(anyhow::Error::msg);
    }
    let expanded = stdout_within(
        Command::new("curl").args([
            "--silent",
            "--show-error",
            "--location",
            "--output",
            "/dev/null",
            "--write-out",
            "%{url_effective}",
            value,
        ]),
        LINK_TIMEOUT,
    )
    .context("could not open that link")?;
    expanded.parse().map_err(anyhow::Error::msg)
}

/// Writes the GNSS HAL's files under annex's data folder and returns the `--mount` values that
/// put them in an instance's /vendor: the binary, its init script and its VINTF declaration.
pub fn gnss_mounts() -> Result<Vec<String>> {
    let folder = data_dir().join("gnss").join(env!("CARGO_PKG_VERSION"));
    let files: [(&str, &[u8], u32, &str); 3] = [
        (
            "android.hardware.gnss-service.annex",
            GNSS_HAL,
            0o755,
            "/vendor/bin/hw/android.hardware.gnss-service.annex",
        ),
        (
            "annex-gnss.rc",
            GNSS_INIT.as_bytes(),
            0o644,
            "/vendor/etc/init/annex-gnss.rc",
        ),
        (
            "annex-gnss.xml",
            GNSS_MANIFEST.as_bytes(),
            0o644,
            "/vendor/etc/vintf/manifest/annex-gnss.xml",
        ),
    ];
    files
        .into_iter()
        .map(|(name, content, mode, target)| {
            let path = folder.join(name);
            write_if_changed(&path, content, mode)?;
            Ok(format!(
                "type=bind,src={},dst={target},readonly",
                path.display()
            ))
        })
        .collect()
}

pub fn get(instance: &Instance) -> Result<Location> {
    let container = container(instance)?;
    let script = format!("cat {SETTING_PATH} 2>/dev/null; true");
    let setting = docker::run(&["exec", &container, "sh", "-c", &script])?;
    if setting.is_empty() {
        return Ok(Location::Default);
    }
    setting.parse().map_err(anyhow::Error::msg)
}

pub fn set(instance: &Instance, location: Location) -> Result<()> {
    let container = container(instance)?;
    if location != Location::Host {
        forget(&instance.name)?;
    }
    // A position shared from this computer is only kept while the instance asks for it.
    let script = match location {
        Location::Default => format!("rm -f {SETTING_PATH} {HOST_FIX_PATH}"),
        Location::Host => format!("echo \"$1\" > {SETTING_PATH}"),
        Location::Fixed { .. } => format!("echo \"$1\" > {SETTING_PATH} && rm -f {HOST_FIX_PATH}"),
    };
    let value = location.to_string();
    docker::run(&["exec", &container, "sh", "-c", &script, "sh", &value])?;
    if location == Location::Host {
        let path = consent_path(&instance.name);
        fs::create_dir_all(path.parent().expect("consent path has a parent"))?;
        fs::write(path, "")?;
    }
    Ok(())
}

/// Whether this computer agreed to share its location with the instance. The agreement is kept
/// on the host because the instance's own setting can be changed from inside the instance.
pub fn is_shared(name: &str) -> bool {
    consent_path(name).exists()
}

/// Withdraws the agreement, so a later instance with the same name doesn't inherit it.
pub fn forget(name: &str) -> Result<()> {
    match fs::remove_file(consent_path(name)) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
        _ => Ok(()),
    }
}

/// Hands the instance this computer's latest position. The file is readable by root only, since
/// SELinux is off in Redroid and any app could otherwise read it.
pub fn share_host_fix(container: &str, fix: Fix) -> Result<()> {
    let script = format!("umask 077; echo \"$1\" > {HOST_FIX_PATH}");
    docker::run(&[
        "exec",
        container,
        "sh",
        "-c",
        &script,
        "sh",
        &host_fix_line(fix),
    ])
    .map(drop)
}

/// The position last shared with an instance set to `host`, if there is one yet.
pub fn host_fix(instance: &Instance) -> Result<Option<Fix>> {
    let container = container(instance)?;
    let script = format!("cat {HOST_FIX_PATH} 2>/dev/null; true");
    let line = docker::run(&["exec", &container, "sh", "-c", &script])?;
    Ok(parse_host_fix(&line))
}

/// `LAT,LON ACCURACY`, read by the GNSS HAL, then `ip` for a guess from the IP address, which the
/// HAL ignores.
fn host_fix_line(fix: Fix) -> String {
    let source = if fix.is_from_ip { " ip" } else { "" };
    format!(
        "{},{} {:.0}{source}",
        fix.latitude, fix.longitude, fix.accuracy
    )
}

fn parse_host_fix(line: &str) -> Option<Fix> {
    let mut words = line.split_whitespace();
    let (latitude, longitude) = parse_position(words.next()?)?;
    let accuracy = words.next()?.parse().ok()?;
    Some(Fix {
        accuracy,
        latitude,
        longitude,
        is_from_ip: words.next() == Some("ip"),
    })
}

fn parse_position(text: &str) -> Option<(f64, f64)> {
    let (latitude, longitude) = text.split_once(',')?;
    Some((
        latitude.trim().parse().ok()?,
        longitude.trim().parse().ok()?,
    ))
}

fn is_link(value: &str) -> bool {
    value.starts_with("http://") || value.starts_with("https://") || value.contains("google.")
}

/// Finds the position in a Google Maps link: a place's pin (`!3dLAT!4dLON`), a search or route
/// (`q=`, `query=`, `ll=`, `destination=`), or failing those the map's centre (`@LAT,LON,ZOOM`).
fn position_in_link(link: &str) -> Option<(f64, f64)> {
    let link = percent_decode(link);
    if let Some((_, pin)) = link.rsplit_once("!3d")
        && let Some((latitude, rest)) = pin.split_once("!4d")
    {
        let longitude = rest.split(['!', '?', '&', '/']).next()?;
        return parse_position(&format!("{latitude},{longitude}"));
    }
    let query = link.split_once('?').map_or("", |(_, query)| query);
    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        if matches!(key, "q" | "query" | "ll" | "destination" | "center") {
            let value = value.trim_start_matches("loc:").replace('+', " ");
            if let Some(position) = parse_position(&value) {
                return Some(position);
            }
        }
    }
    let (_, view) = link.split_once('@')?;
    let mut parts = view.split(',');
    parse_position(&format!("{},{}", parts.next()?, parts.next()?))
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let hex = bytes
            .get(index + 1..index + 3)
            .and_then(|hex| std::str::from_utf8(hex).ok())
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match (bytes[index], hex) {
            (b'%', Some(byte)) => {
                decoded.push(byte);
                index += 3;
            }
            (byte, _) => {
                decoded.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

/// A running instance keeps the binary busy, so a changed file is replaced by renaming a new one
/// over it, never rewritten in place.
fn write_if_changed(path: &Path, content: &[u8], mode: u32) -> Result<()> {
    if fs::read(path).is_ok_and(|current| current == content) {
        return Ok(());
    }
    let folder = path.parent().expect("GNSS file has a parent");
    fs::create_dir_all(folder)?;
    let name = path.file_name().expect("GNSS file has a name");
    let staged = folder.join(format!(".{}.new", name.to_string_lossy()));
    fs::write(&staged, content)?;
    fs::set_permissions(&staged, Permissions::from_mode(mode))?;
    fs::rename(&staged, path)?;
    Ok(())
}

fn consent_path(name: &str) -> PathBuf {
    data_dir().join("host-location").join(name)
}

fn container(instance: &Instance) -> Result<String> {
    match &instance.backend {
        Backend::Redroid(_) => Ok(container_name(&instance.name)),
        Backend::Waydroid { .. } => bail!(
            "{} runs in window mode, where annex doesn't simulate location",
            instance.name
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip() {
        for value in ["default", "host", "-1.2921,36.8219"] {
            let location: Location = value.parse().unwrap();
            assert_eq!(location.to_string(), value);
        }
        assert_eq!(
            "-1.2921, 36.8219".parse::<Location>().unwrap(),
            Location::Fixed {
                latitude: -1.2921,
                longitude: 36.8219
            }
        );
    }

    #[test]
    fn rejects_malformed_and_out_of_range_positions() {
        for value in ["", "here", "1.2", "a,b", "91,0", "0,-181", "1,2,3"] {
            assert!(value.parse::<Location>().is_err(), "{value}");
        }
    }

    #[test]
    fn reads_positions_from_google_maps_links() {
        let fixed = |latitude, longitude| Location::Fixed {
            latitude,
            longitude,
        };
        for (link, expected) in [
            (
                "https://www.google.com/maps/place/Kenyatta+Avenue/@-1.2840,36.8170,17z/data=!3m1!4b1!4m6!3m5!1s0x0:0x0!8m2!3d-1.2863893!4d36.8219462!16s",
                fixed(-1.2863893, 36.8219462),
            ),
            (
                "https://www.google.com/maps/@-1.2920659,36.8219462,15z",
                fixed(-1.2920659, 36.8219462),
            ),
            (
                "https://www.google.com/maps/search/?api=1&query=-1.2921%2C36.8219",
                fixed(-1.2921, 36.8219),
            ),
            (
                "https://maps.google.com/?q=loc:-1.2921,36.8219&z=17",
                fixed(-1.2921, 36.8219),
            ),
            (
                "https://consent.google.com/ml?continue=https://www.google.com/maps/place/x/data%3D!3d-1.5!4d36.9%3Fentry",
                fixed(-1.5, 36.9),
            ),
        ] {
            assert_eq!(link.parse::<Location>().unwrap(), expected, "{link}");
        }
        assert!(
            "https://www.google.com/maps/search/coffee"
                .parse::<Location>()
                .is_err()
        );
    }

    #[test]
    fn host_fixes_round_trip_with_their_source() {
        for is_from_ip in [false, true] {
            let fix = Fix {
                accuracy: 25000.0,
                latitude: -1.14665,
                longitude: 36.9609,
                is_from_ip,
            };
            assert_eq!(parse_host_fix(&host_fix_line(fix)), Some(fix));
        }
        assert_eq!(parse_host_fix(""), None);
    }

    #[test]
    fn default_position_is_a_valid_setting() {
        assert!(matches!(
            DEFAULT_POSITION.parse::<Location>(),
            Ok(Location::Fixed { .. })
        ));
    }
}
