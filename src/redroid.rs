use std::fmt;
use std::fs::File;
use std::net::TcpListener;
use std::str::FromStr;
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::adb::Adb;
use crate::instance::{AndroidVersion, Backend, Instance, State, UpOptions, require};
use crate::{docker, gpu};

const PORTS: std::ops::Range<u16> = 5600..5700;
const BOOT_TIMEOUT: Duration = Duration::from_secs(180);
const PULL_ATTEMPTS: u32 = 3;
const CONTAINER_CONFIG: &str = "4";
const ADB_KEYS_PATH: &str = "/product/etc/security/adb_keys";
// adbd also trusts keys saved on the data volume ("always allow" and wireless pairings). Masking
// both with a read-only /dev/null means only the mounted user key is ever trusted, including on
// volumes that ran without authentication before.
const SAVED_ADB_KEYS: &str = "/data/misc/adb/adb_keys";
const SAVED_WIRELESS_KEYS: &str = "/data/misc/adb/adb_temp_keys.xml";
// The simulated Bluetooth HAL listens on TCP 6111/6211/6311 on every interface, reachable by any
// local user through the Docker bridge; masking its init script keeps it from starting.
const BLUETOOTH_SIM_INIT: &str = "/vendor/etc/init/android.hardware.bluetooth@1.1-service.sim.rc";

/// Screen size and density, written like scrcpy's `--new-display`: `1080x2400/420`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Display {
    pub dpi: u16,
    pub width: u16,
    pub height: u16,
}

impl Default for Display {
    fn default() -> Self {
        Self {
            dpi: 420,
            width: 1080,
            height: 2400,
        }
    }
}

impl fmt::Display for Display {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        write!(formatter, "{}x{}/{}", self.width, self.height, self.dpi)
    }
}

impl FromStr for Display {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, String> {
        let invalid = || format!("{value:?} is not WIDTHxHEIGHT/DPI, like 1080x2400/420");
        let (size, dpi) = value.split_once('/').ok_or_else(invalid)?;
        let (width, height) = size.split_once('x').ok_or_else(invalid)?;
        let [dpi, width, height] = [dpi, width, height].map(|part| part.parse::<u16>().ok());
        let (Some(dpi), Some(width), Some(height)) = (dpi, width, height) else {
            return Err(invalid());
        };
        if !(120..=640).contains(&dpi) {
            return Err(format!(
                "density {dpi} is outside Android's 120-640 dpi range"
            ));
        }
        if !(320..=4096).contains(&width) || !(320..=4096).contains(&height) {
            return Err(format!(
                "{width}x{height} is outside 320-4096 pixels per side"
            ));
        }
        Ok(Self { dpi, width, height })
    }
}

/// What annex knows about a Redroid instance, all of it read back from the container's labels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Container {
    pub port: u16,
    pub config: String,
    pub display: Display,
    pub android: AndroidVersion,
}

impl Container {
    pub fn is_outdated(&self) -> bool {
        self.config != CONTAINER_CONFIG
    }
}

pub fn list() -> Result<Vec<Instance>> {
    let format = [
        "{{.State}}",
        "{{.Label \"annex.name\"}}",
        "{{.Label \"annex.android\"}}",
        "{{.Label \"annex.adb-port\"}}",
        "{{.Label \"annex.config\"}}",
        "{{.Label \"annex.display\"}}",
    ]
    .join("\t");
    let output = docker::run(&[
        "ps",
        "-a",
        "--filter",
        "label=annex.name",
        "--format",
        &format,
    ])?;
    output.lines().map(parse).collect()
}

pub fn up(adb: &Adb, name: &str, options: UpOptions) -> Result<Instance> {
    let existing = list()?.into_iter().find(|instance| instance.name == name);
    let instance = match existing {
        Some(existing) => {
            let Backend::Redroid(container) = &existing.backend else {
                unreachable!("redroid::list only returns Redroid instances");
            };
            if let Some(wanted) = options
                .android
                .filter(|wanted| *wanted != container.android)
            {
                bail!(
                    "{name} runs Android {}, not {}: remove it with `annex rm {name}` first",
                    container.android.label(),
                    wanted.label()
                );
            }
            let display = options.display.unwrap_or(container.display);
            if container.is_outdated() || display != container.display {
                if container.config.is_empty() {
                    println!(
                        "warning: {name} ran without adb authentication, so another local user could have changed it; `annex reset {name}` gives a clean instance"
                    );
                }
                println!("updating {name}'s container (data kept)");
                adb.disconnect(&existing.serial);
                docker::run(&["rm", "--force", &container_name(name)])?;
                create(
                    adb,
                    name,
                    &Container {
                        display,
                        ..container.clone()
                    },
                )?;
                docker::run(&["start", &container_name(name)])?;
            } else {
                match existing.state {
                    State::Running => {}
                    State::Frozen => docker::run(&["unpause", &container_name(name)]).map(drop)?,
                    State::Stopped => docker::run(&["start", &container_name(name)]).map(drop)?,
                }
            }
            require(name)?
        }
        None => {
            let android = options.android.unwrap_or(AndroidVersion::Android15);
            ensure_image(android)?;
            let container = Container {
                port: free_port(&list()?)?,
                config: CONTAINER_CONFIG.to_string(),
                display: options.display.unwrap_or_default(),
                android,
            };
            create(adb, name, &container)?;
            docker::run(&["start", &container_name(name)])?;
            require(name)?
        }
    };
    wait_until_booted(&instance)?;
    adb.connect(&instance.serial)?;
    Ok(instance)
}

pub fn down(instance: &Instance) -> Result<()> {
    if instance.state == State::Frozen {
        docker::run(&["unpause", &container_name(&instance.name)])?;
    }
    docker::run(&["stop", "--time", "5", &container_name(&instance.name)]).map(drop)
}

pub fn freeze(instance: &Instance) -> Result<()> {
    docker::run(&["pause", &container_name(&instance.name)]).map(drop)
}

pub fn thaw(instance: &Instance) -> Result<()> {
    docker::run(&["unpause", &container_name(&instance.name)]).map(drop)
}

pub fn remove(instance: &Instance, keep_data: bool) -> Result<()> {
    docker::run(&["rm", "--force", &container_name(&instance.name)])?;
    if !keep_data {
        docker::run(&["volume", "rm", &volume_name(&instance.name)])?;
    }
    Ok(())
}

/// Recreates the container on a fresh data volume, filled from `archive` (a `/data` snapshot) or
/// left empty for a first boot, keeping its port, display and Android version.
pub fn recreate(
    adb: &Adb,
    instance: &Instance,
    container: &Container,
    archive: Option<File>,
) -> Result<()> {
    let volume = volume_name(&instance.name);
    adb.disconnect(&instance.serial);
    docker::run(&["rm", "--force", &container_name(&instance.name)])?;
    docker::run(&["volume", "rm", "--force", &volume])?;
    if let Some(archive) = archive {
        // The real container mounts read-only /dev/null masks inside /data, and docker cp
        // can't replace a mount point, so the volume is filled through a bare container first.
        let filler = format!("{}-restore", container_name(&instance.name));
        let data_mount = format!("{volume}:/data");
        let _ = docker::run(&["rm", "--force", &filler]);
        docker::run(&[
            "create",
            "--name",
            &filler,
            "--volume",
            &data_mount,
            container.android.image(),
        ])?;
        let target = format!("{filler}:/");
        let copied = docker::run_from(&["cp", "--archive", "-", &target], archive);
        docker::run(&["rm", "--force", &filler])?;
        copied?;
    }
    create(adb, &instance.name, container)
}

pub fn start_and_connect(adb: &Adb, instance: &Instance) -> Result<()> {
    docker::run(&["start", &container_name(&instance.name)])?;
    wait_until_booted(instance)?;
    adb.connect(&instance.serial)
}

pub fn container_name(name: &str) -> String {
    format!("annex-{name}")
}

fn parse(line: &str) -> Result<Instance> {
    let fields: Vec<&str> = line.split('\t').collect();
    let [state, name, android, port, ..] = fields[..] else {
        bail!("unexpected docker output: {line}");
    };
    // Labels added in later versions are missing on older containers, and docker's trailing
    // empty columns are dropped by trimming, so every column after the port is optional.
    let config = fields.get(4).copied().unwrap_or_default();
    let display = match fields.get(5).copied().unwrap_or_default() {
        "" => Display::default(),
        raw => raw
            .parse()
            .map_err(anyhow::Error::msg)
            .with_context(|| format!("bad display label on {name}"))?,
    };
    let port: u16 = port
        .parse()
        .with_context(|| format!("bad adb port label on {name}"))?;
    let android = AndroidVersion::from_label(android)
        .with_context(|| format!("unsupported Android version {android} on {name}"))?;
    Ok(Instance {
        name: name.to_string(),
        serial: format!("127.0.0.1:{port}"),
        state: match state {
            "running" | "restarting" => State::Running,
            "paused" => State::Frozen,
            _ => State::Stopped,
        },
        backend: Backend::Redroid(Container {
            port,
            config: config.to_string(),
            display,
            android,
        }),
    })
}

fn create(adb: &Adb, name: &str, container: &Container) -> Result<()> {
    let gpu_mode = if gpu::has_hardware_rendering() {
        "host"
    } else {
        "guest"
    };
    let Container {
        port,
        display,
        android,
        ..
    } = container;
    let public_key = adb.public_key()?;
    let container_name = container_name(name);
    let dpi = format!("androidboot.redroid_dpi={}", display.dpi);
    let width = format!("androidboot.redroid_width={}", display.width);
    let height = format!("androidboot.redroid_height={}", display.height);
    let key_mount = format!("{}:{ADB_KEYS_PATH}:ro", public_key.display());
    let name_label = format!("annex.name={name}");
    let port_label = format!("annex.adb-port={port}");
    let data_mount = format!("{}:/data", volume_name(name));
    let config_label = format!("annex.config={CONTAINER_CONFIG}");
    let port_binding = format!("127.0.0.1:{port}:5555");
    let gpu_argument = format!("androidboot.redroid_gpu_mode={gpu_mode}");
    let display_label = format!("annex.display={display}");
    let android_label = format!("annex.android={}", android.label());
    // IDEs list devices by model, which would otherwise be the image's "redroid15_x86_64". The
    // boot arguments split on spaces, so words are joined with underscores, which Flutter and
    // VS Code show as spaces.
    let model = format!("ro.product.model={}", model_name(name, *android));
    let bluetooth_mask = mask(BLUETOOTH_SIM_INIT);
    let saved_keys_mask = mask(SAVED_ADB_KEYS);
    let wireless_keys_mask = mask(SAVED_WIRELESS_KEYS);
    // Redroid cannot boot without --privileged (its init needs writable sysfs and cgroups), so the
    // container is root-equivalent: adbd must only accept the user's key, and nothing else may listen.
    docker::run(&[
        "create",
        "--privileged",
        "--name",
        &container_name,
        "--hostname",
        name,
        "--label",
        &name_label,
        "--label",
        &android_label,
        "--label",
        &port_label,
        "--label",
        &config_label,
        "--label",
        &display_label,
        "--volume",
        &data_mount,
        "--volume",
        &key_mount,
        "--mount",
        &bluetooth_mask,
        "--mount",
        &saved_keys_mask,
        "--mount",
        &wireless_keys_mask,
        "--publish",
        &port_binding,
        android.image(),
        "androidboot.use_memfd=true",
        "ro.adb.secure=1",
        &model,
        "ro.product.brand=Annex",
        "ro.product.manufacturer=Annex",
        &gpu_argument,
        &width,
        &height,
        &dpi,
    ])
    .map(drop)
}

fn ensure_image(android: AndroidVersion) -> Result<()> {
    if docker::succeeds(&["image", "inspect", android.image()]) {
        return Ok(());
    }
    println!(
        "downloading the Android {} image (about 3 GB, first time only)",
        android.label()
    );
    let mut last_error = None;
    for _ in 0..PULL_ATTEMPTS {
        match docker::run(&["pull", "--quiet", android.image()]) {
            Ok(_) => return Ok(()),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.expect("at least one pull attempt"))
        .with_context(|| format!("could not download the Android {} image", android.label()))
}

fn free_port(instances: &[Instance]) -> Result<u16> {
    let taken: Vec<u16> = instances
        .iter()
        .filter_map(|instance| match &instance.backend {
            Backend::Redroid(container) => Some(container.port),
            Backend::Waydroid { .. } => None,
        })
        .collect();
    PORTS
        .filter(|port| !taken.contains(port))
        .find(|port| TcpListener::bind(("127.0.0.1", *port)).is_ok())
        .with_context(|| {
            format!(
                "no free adb port between {} and {}",
                PORTS.start,
                PORTS.end - 1
            )
        })
}

fn wait_until_booted(instance: &Instance) -> Result<()> {
    let started = Instant::now();
    let container = container_name(&instance.name);
    loop {
        let is_booted = docker::run(&["exec", &container, "getprop", "sys.boot_completed"])
            .is_ok_and(|value| value == "1");
        if is_booted {
            return Ok(());
        }
        let state = docker::run(&["inspect", "--format", "{{.State.Status}}", &container])?;
        if !matches!(state.as_str(), "running" | "restarting" | "created") {
            bail!(
                "{} stopped while booting: see `docker logs {container}`",
                instance.name
            );
        }
        if started.elapsed() > BOOT_TIMEOUT {
            bail!(
                "{} did not finish booting within {} s",
                instance.name,
                BOOT_TIMEOUT.as_secs()
            );
        }
        sleep(Duration::from_millis(250));
    }
}

/// "Annex_Default_15" for the instance `default` on Android 15.
fn model_name(name: &str, android: AndroidVersion) -> String {
    let words: Vec<String> = name
        .split('-')
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().chain(chars).collect())
                .unwrap_or_default()
        })
        .collect();
    format!("Annex_{}_{}", words.join("_"), android.label())
}

fn mask(path: &str) -> String {
    format!("type=bind,src=/dev/null,dst={path},readonly")
}

fn volume_name(name: &str) -> String {
    format!("annex-{name}-data")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn container(instance: &Instance) -> &Container {
        match &instance.backend {
            Backend::Redroid(container) => container,
            Backend::Waydroid { .. } => panic!("expected a Redroid instance"),
        }
    }

    #[test]
    fn parses_a_current_container() {
        let instance = parse("running\tpixel\t15\t5600\t4\t720x1280/320").unwrap();
        assert_eq!(instance.name, "pixel");
        assert_eq!(instance.serial, "127.0.0.1:5600");
        assert_eq!(instance.state, State::Running);
        let container = container(&instance);
        assert_eq!(container.android, AndroidVersion::Android15);
        assert!(!container.is_outdated());
        assert_eq!(
            container.display,
            Display {
                dpi: 320,
                width: 720,
                height: 1280
            }
        );
    }

    #[test]
    fn parses_containers_from_before_later_labels() {
        let instance = parse("paused\told\t13\t5601").unwrap();
        assert_eq!(instance.state, State::Frozen);
        let container = container(&instance);
        assert!(container.is_outdated());
        assert_eq!(container.config, "");
        assert_eq!(container.display, Display::default());
    }

    #[test]
    fn maps_docker_states() {
        for (raw, state) in [
            ("running", State::Running),
            ("restarting", State::Running),
            ("paused", State::Frozen),
            ("exited", State::Stopped),
            ("created", State::Stopped),
        ] {
            let instance = parse(&format!("{raw}\tx\t15\t5600\t3")).unwrap();
            assert_eq!(instance.state, state, "{raw}");
        }
    }

    #[test]
    fn rejects_unknown_android_versions_and_ports() {
        assert!(parse("running\tx\t16\t5600\t3").is_err());
        assert!(parse("running\tx\t15\tnot-a-port\t3").is_err());
        assert!(parse("running\tx").is_err());
    }

    #[test]
    fn names_the_device_after_the_instance() {
        assert_eq!(
            model_name("default", AndroidVersion::Android15),
            "Annex_Default_15"
        );
        assert_eq!(
            model_name("old-phone", AndroidVersion::Android13),
            "Annex_Old_Phone_13"
        );
    }

    #[test]
    fn display_round_trips() {
        let display: Display = "1080x2400/420".parse().unwrap();
        assert_eq!(display, Display::default());
        assert_eq!(display.to_string(), "1080x2400/420");
    }

    #[test]
    fn display_rejects_malformed_and_out_of_range_values() {
        for value in [
            "1080x2400",
            "1080/420",
            "axb/c",
            "1080x2400/1000",
            "100x2400/420",
            "",
        ] {
            assert!(value.parse::<Display>().is_err(), "{value}");
        }
    }
}
