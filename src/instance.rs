use std::net::TcpListener;
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::ValueEnum;

use crate::adb::Adb;
use crate::{docker, gpu};

const PORTS: std::ops::Range<u16> = 5600..5700;
const BOOT_TIMEOUT: Duration = Duration::from_secs(180);
const PULL_ATTEMPTS: u32 = 3;
const CONTAINER_CONFIG: &str = "2";
const ADB_KEYS_PATH: &str = "/product/etc/security/adb_keys";
// The simulated Bluetooth HAL listens on TCP 6111/6211/6311 on every interface, reachable by any
// local user through the Docker bridge; masking its init script keeps it from starting.
const BLUETOOTH_SIM_INIT: &str = "/vendor/etc/init/android.hardware.bluetooth@1.1-service.sim.rc";

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
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

    pub fn api(self) -> u8 {
        match self {
            Self::Android13 => 33,
            Self::Android14 => 34,
            Self::Android15 => 35,
        }
    }

    pub fn image(self) -> &'static str {
        match self {
            Self::Android13 => "redroid/redroid:13.0.0-latest",
            Self::Android14 => "redroid/redroid:14.0.0-latest",
            Self::Android15 => "redroid/redroid:15.0.0-latest",
        }
    }

    fn from_label(label: &str) -> Option<Self> {
        Self::value_variants()
            .iter()
            .copied()
            .find(|version| version.label() == label)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum State {
    Running,
    Paused,
    Stopped,
}

impl State {
    fn from_docker(raw: &str) -> Self {
        match raw {
            "running" | "restarting" => Self::Running,
            "paused" => Self::Paused,
            _ => Self::Stopped,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Paused => "frozen",
            Self::Stopped => "stopped",
        }
    }
}

pub struct Instance {
    pub port: u16,
    pub name: String,
    pub config: String,
    pub state: State,
    pub android: AndroidVersion,
}

impl Instance {
    pub fn serial(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }

    pub fn container(&self) -> String {
        container_name(&self.name)
    }

    pub fn is_outdated(&self) -> bool {
        self.config != CONTAINER_CONFIG
    }

    fn parse(line: &str) -> Result<Self> {
        let fields: Vec<&str> = line.split('\t').collect();
        let [state, name, android, port, ..] = fields[..] else {
            bail!("unexpected docker output: {line}");
        };
        // Containers from before the config label have an empty last column, which trimming drops.
        let config = fields.get(4).copied().unwrap_or_default();
        Ok(Self {
            port: port
                .parse()
                .with_context(|| format!("bad adb port label on {name}"))?,
            name: name.to_string(),
            config: config.to_string(),
            state: State::from_docker(state),
            android: AndroidVersion::from_label(android)
                .with_context(|| format!("unsupported Android version {android} on {name}"))?,
        })
    }
}

pub fn validate_name(name: &str) -> Result<()> {
    let is_valid = !name.is_empty()
        && name.len() <= 40
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !is_valid {
        bail!("instance names use lowercase letters, digits and '-' (at most 40 characters)");
    }
    Ok(())
}

pub fn list() -> Result<Vec<Instance>> {
    let format = "{{.State}}\t{{.Label \"annex.name\"}}\t{{.Label \"annex.android\"}}\t{{.Label \"annex.adb-port\"}}\t{{.Label \"annex.config\"}}";
    let output = docker::run(&[
        "ps",
        "-a",
        "--filter",
        "label=annex.name",
        "--format",
        format,
    ])?;
    let mut instances = output
        .lines()
        .map(Instance::parse)
        .collect::<Result<Vec<_>>>()?;
    instances.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(instances)
}

pub fn find(name: &str) -> Result<Option<Instance>> {
    Ok(list()?.into_iter().find(|instance| instance.name == name))
}

pub fn require(name: &str) -> Result<Instance> {
    find(name)?
        .with_context(|| format!("no instance named {name}: create it with `annex up {name}`"))
}

pub fn up(adb: &Adb, name: &str, android: Option<AndroidVersion>) -> Result<Instance> {
    let instance = match find(name)? {
        Some(existing) => {
            if let Some(wanted) = android.filter(|wanted| *wanted != existing.android) {
                bail!(
                    "{name} runs Android {}, not {}: remove it with `annex rm {name}` first",
                    existing.android.label(),
                    wanted.label()
                );
            }
            if existing.is_outdated() {
                println!("updating {name}'s container to the current configuration (data kept)");
                rebuild(adb, &existing)?;
                docker::run(&["start", &existing.container()])?;
            } else {
                match existing.state {
                    State::Running => {}
                    State::Paused => docker::run(&["unpause", &existing.container()]).map(drop)?,
                    State::Stopped => docker::run(&["start", &existing.container()]).map(drop)?,
                }
            }
            existing
        }
        None => {
            let android = android.unwrap_or(AndroidVersion::Android15);
            ensure_image(android)?;
            let port = free_port(&list()?)?;
            create(adb, name, android, port)?;
            docker::run(&["start", &container_name(name)])?;
            require(name)?
        }
    };
    wait_until_booted(&instance)?;
    adb.connect(&instance.serial())?;
    Ok(instance)
}

pub fn down(adb: &Adb, name: &str) -> Result<()> {
    let instance = require(name)?;
    adb.disconnect(&instance.serial());
    if instance.state == State::Paused {
        docker::run(&["unpause", &instance.container()])?;
    }
    docker::run(&["stop", "--time", "5", &instance.container()]).map(drop)
}

pub fn freeze(name: &str) -> Result<()> {
    let instance = require(name)?;
    if instance.state != State::Running {
        bail!(
            "{name} is {}, only a running instance can be frozen",
            instance.state.label()
        );
    }
    docker::run(&["pause", &instance.container()]).map(drop)
}

pub fn thaw(adb: &Adb, name: &str) -> Result<()> {
    let instance = require(name)?;
    if instance.state != State::Paused {
        bail!("{name} is {}, not frozen", instance.state.label());
    }
    docker::run(&["unpause", &instance.container()])?;
    adb.connect(&instance.serial())
}

pub fn remove(adb: &Adb, name: &str, keep_data: bool) -> Result<()> {
    let instance = require(name)?;
    adb.disconnect(&instance.serial());
    docker::run(&["rm", "--force", &instance.container()])?;
    if !keep_data {
        docker::run(&["volume", "rm", &volume_name(name)])?;
    }
    Ok(())
}

/// Recreates the container on an empty data volume, keeping its port and Android version.
pub fn recreate(adb: &Adb, instance: &Instance) -> Result<()> {
    adb.disconnect(&instance.serial());
    docker::run(&["rm", "--force", &instance.container()])?;
    docker::run(&["volume", "rm", "--force", &volume_name(&instance.name)])?;
    create(adb, &instance.name, instance.android, instance.port)
}

/// Recreates the container with the current configuration on its existing data volume.
fn rebuild(adb: &Adb, instance: &Instance) -> Result<()> {
    adb.disconnect(&instance.serial());
    docker::run(&["rm", "--force", &instance.container()])?;
    create(adb, &instance.name, instance.android, instance.port)
}

pub fn start_and_connect(adb: &Adb, instance: &Instance) -> Result<()> {
    docker::run(&["start", &instance.container()])?;
    wait_until_booted(instance)?;
    adb.connect(&instance.serial())
}

fn create(adb: &Adb, name: &str, android: AndroidVersion, port: u16) -> Result<()> {
    let gpu_mode = if gpu::has_hardware_rendering() {
        "host"
    } else {
        "guest"
    };
    let public_key = adb.public_key()?;
    let container = container_name(name);
    let key_mount = format!("{}:{ADB_KEYS_PATH}:ro", public_key.display());
    let name_label = format!("annex.name={name}");
    let port_label = format!("annex.adb-port={port}");
    let data_mount = format!("{}:/data", volume_name(name));
    let config_label = format!("annex.config={CONTAINER_CONFIG}");
    let port_binding = format!("127.0.0.1:{port}:5555");
    let gpu_argument = format!("androidboot.redroid_gpu_mode={gpu_mode}");
    let android_label = format!("annex.android={}", android.label());
    let bluetooth_mask = format!("type=bind,src=/dev/null,dst={BLUETOOTH_SIM_INIT},readonly");
    // Redroid cannot boot without --privileged (its init needs writable sysfs and cgroups), so the
    // container is root-equivalent: adbd must only accept the user's key, and nothing else may listen.
    docker::run(&[
        "create",
        "--privileged",
        "--name",
        &container,
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
        "--volume",
        &data_mount,
        "--volume",
        &key_mount,
        "--mount",
        &bluetooth_mask,
        "--publish",
        &port_binding,
        android.image(),
        "androidboot.use_memfd=true",
        "ro.adb.secure=1",
        &gpu_argument,
        "androidboot.redroid_width=1080",
        "androidboot.redroid_height=2400",
        "androidboot.redroid_dpi=420",
    ])
    .map(drop)
}

fn ensure_image(android: AndroidVersion) -> Result<()> {
    if docker::succeeds(&["image", "inspect", android.image()]) {
        return Ok(());
    }
    println!(
        "downloading {} (about 3 GB, first time only)",
        android.image()
    );
    let mut last_error = None;
    for _ in 0..PULL_ATTEMPTS {
        match docker::run(&["pull", "--quiet", android.image()]) {
            Ok(_) => return Ok(()),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.expect("at least one pull attempt"))
}

fn free_port(instances: &[Instance]) -> Result<u16> {
    PORTS
        .filter(|port| instances.iter().all(|instance| instance.port != *port))
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
    let container = instance.container();
    loop {
        let is_booted = docker::run(&["exec", &container, "getprop", "sys.boot_completed"])
            .is_ok_and(|value| value == "1");
        if is_booted {
            return Ok(());
        }
        let state = docker::run(&["inspect", "--format", "{{.State.Status}}", &container])?;
        if State::from_docker(&state) == State::Stopped {
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

fn container_name(name: &str) -> String {
    format!("annex-{name}")
}

fn volume_name(name: &str) -> String {
    format!("annex-{name}-data")
}
