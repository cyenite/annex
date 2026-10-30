use std::fs::{self, DirBuilder, File, OpenOptions, Permissions};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use crate::adb::Adb;
use crate::instance::{self, State};
use crate::paths::data_dir;
use crate::{docker, redroid};

const PRIVATE_DIR: u32 = 0o700;
const PRIVATE_FILE: u32 = 0o600;

pub struct Snapshot {
    pub bytes: u64,
    pub label: String,
}

pub fn save(name: &str, label: &str) -> Result<PathBuf> {
    instance::validate_name(label)
        .context("snapshot labels follow the same rules as instance names")?;
    let instance = instance::require(name)?;
    instance.container("saving a snapshot")?;
    let container = redroid::container_name(name);
    let directory = snapshot_dir(name);
    // Snapshots hold every app's private data, including login tokens. The modes are set
    // explicitly because creation modes don't apply to folders or files that already exist.
    DirBuilder::new()
        .recursive(true)
        .mode(PRIVATE_DIR)
        .create(&directory)?;
    for private in [data_dir(), data_dir().join("snapshots"), directory.clone()] {
        fs::set_permissions(&private, Permissions::from_mode(PRIVATE_DIR))?;
    }
    let path = directory.join(format!("{label}.tar"));
    let partial = directory.join(format!("{label}.tar.part"));
    let source = format!("{container}:/data");
    let archive = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(PRIVATE_FILE)
        .open(&partial)?;
    archive.set_permissions(Permissions::from_mode(PRIVATE_FILE))?;

    // Freezing gives a crash-consistent copy without rebooting Android.
    let is_running = instance.state == State::Running;
    if is_running {
        docker::run(&["pause", &container])?;
    }
    let copied = docker::run_into(&["cp", "--archive", &source, "-"], archive);
    if is_running {
        docker::run(&["unpause", &container])?;
    }
    copied?;
    fs::rename(&partial, &path)?;
    Ok(path)
}

pub fn delete(name: &str, label: &str) -> Result<()> {
    instance::validate_name(label)?;
    let path = snapshot_dir(name).join(format!("{label}.tar"));
    fs::remove_file(&path).with_context(|| format!("no snapshot named {label} for {name}"))
}

pub fn list(name: &str) -> Result<Vec<Snapshot>> {
    let Ok(entries) = fs::read_dir(snapshot_dir(name)) else {
        return Ok(Vec::new());
    };
    let mut snapshots: Vec<Snapshot> = entries
        .flatten()
        .filter_map(|entry| {
            let file_name = entry.file_name().to_string_lossy().to_string();
            let label = file_name.strip_suffix(".tar")?.to_string();
            let bytes = entry.metadata().ok()?.len();
            Some(Snapshot { bytes, label })
        })
        .collect();
    snapshots.sort_by(|a, b| a.label.cmp(&b.label));
    Ok(snapshots)
}

/// Restores a snapshot, or wipes the instance back to first boot when `label` is None.
pub fn reset(adb: &Adb, name: &str, label: Option<&str>) -> Result<()> {
    let instance = instance::require(name)?;
    let container = instance.container("resetting an instance")?;
    let archive = label.map(|label| snapshot_dir(name).join(format!("{label}.tar")));
    if let Some(archive) = archive.as_ref().filter(|archive| !archive.is_file()) {
        bail!(
            "no snapshot at {}: see `annex snap ls {name}`",
            archive.display()
        );
    }
    let archive = archive.map(File::open).transpose()?;
    redroid::recreate(adb, &instance, container, archive)?;
    redroid::start_and_connect(adb, &instance)
}

fn snapshot_dir(name: &str) -> PathBuf {
    data_dir().join("snapshots").join(name)
}
