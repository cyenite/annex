//! Simulated GPS and network location for container-mode instances, at a fixed position or at
//! this computer's own (shared by the desktop bridge).

use std::fmt;
use std::fs;
use std::path::PathBuf;
use std::str::FromStr;

use anyhow::{Result, bail};

use crate::docker;
use crate::instance::{Backend, Instance};
use crate::paths::data_dir;
use crate::redroid::container_name;

const SETTING_PATH: &str = "/data/local/tmp/annex-location";
const HOST_FIX_PATH: &str = "/dev/annex-host-location";
/// The Android emulator's default position, which developers already expect from an emulator.
const DEFAULT_POSITION: &str = "37.4220,-122.0841";

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

/// One reading of this computer's position; accuracy is in meters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fix {
    pub accuracy: f64,
    pub latitude: f64,
    pub longitude: f64,
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
        let invalid =
            || format!("{value:?} is not `default`, `host` or LAT,LON, like -1.2921,36.8219");
        let (latitude, longitude) = value.split_once(',').ok_or_else(invalid)?;
        let (Ok(latitude), Ok(longitude)) = (
            latitude.trim().parse::<f64>(),
            longitude.trim().parse::<f64>(),
        ) else {
            return Err(invalid());
        };
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

/// Starts the loop that feeds the instance's location providers. Safe to call on every `up`: a
/// feed that is already running is left alone.
pub fn start_feed(container: &str) -> Result<()> {
    docker::run(&["exec", "--detach", container, "sh", "-c", &feed_script()]).map(drop)
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
    let value = format!("{},{} {:.0}", fix.latitude, fix.longitude, fix.accuracy);
    docker::run(&["exec", container, "sh", "-c", &script, "sh", &value]).map(drop)
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

// Redroid has no GNSS HAL and no network location provider, only `passive` and `fused`. Apps
// (Geolocator among them) treat location as off unless `gps` or `network` exists, and `fused`
// never gets a fix without them. Test providers stand in for both, fed a fix every second like a
// real receiver. The loop is a child of `docker exec`, so it stops and freezes with the container;
// its pid file lives on /dev, a tmpfs that a restart empties. With `host` and no fix shared yet it
// reports nothing, like a receiver that hasn't found a signal.
fn feed_script() -> String {
    format!(
        r#"
pid=/dev/annex-location.pid
[ -f $pid ] && kill -0 "$(cat $pid)" 2>/dev/null && exit 0
echo $$ > $pid
for provider in gps network; do
  cmd location providers add-test-provider $provider --supportsAltitude --supportsSpeed --supportsBearing --powerRequirement 1 --accuracy 1
  cmd location providers set-test-provider-enabled $provider true
done
while true; do
  setting=$(cat {SETTING_PATH} 2>/dev/null)
  accuracy=5
  case "$setting" in
    host) read position accuracy 2>/dev/null < {HOST_FIX_PATH} || position= ;;
    "") position={DEFAULT_POSITION} ;;
    *) position=$setting ;;
  esac
  if [ -n "$position" ]; then
    for provider in gps network; do
      cmd location providers set-test-provider-location $provider --location "$position" --accuracy "$accuracy" >/dev/null 2>&1
    done
  fi
  sleep 1
done
"#
    )
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
    fn default_position_is_a_valid_setting() {
        assert!(matches!(
            DEFAULT_POSITION.parse::<Location>(),
            Ok(Location::Fixed { .. })
        ));
    }
}
