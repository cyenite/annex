use std::fs::{self, DirBuilder, File, OpenOptions};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use crate::adb::Adb;
use crate::instance::{self, State};
use crate::{data_dir, docker};

pub struct Snapshot {
    pub bytes: u64,
    pub label: String,
}

pub fn save(name: &str, label: &str) -> Result<PathBuf> {
    instance::validate_name(label)
        .context("snapshot labels follow the same rules as instance names")?;
    let instance = instance::require(name)?;
    let directory = snapshot_dir(name);
    // Snapshots hold every app's private data, including login tokens.
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&directory)?;
    let path = directory.join(format!("{label}.tar"));
    let partial = directory.join(format!("{label}.tar.part"));
    let source = format!("{}:/data", instance.container());

    // Freezing gives a crash-consistent copy without rebooting Android.
    let is_running = instance.state == State::Running;
    if is_running {
        docker::run(&["pause", &instance.container()])?;
    }
    let archive = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&partial)?;
    let copied = docker::run_into(&["cp", "--archive", &source, "-"], archive);
    if is_running {
        docker::run(&["unpause", &instance.container()])?;
    }
    copied?;
    fs::rename(&partial, &path)?;
    Ok(path)
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
    let archive = label.map(|label| snapshot_dir(name).join(format!("{label}.tar")));
    if let Some(archive) = archive.as_ref().filter(|archive| !archive.is_file()) {
        bail!(
            "no snapshot at {}: see `annex snap ls {name}`",
            archive.display()
        );
    }
    instance::recreate(adb, &instance)?;
    if let Some(archive) = archive {
        let target = format!("{}:/", instance.container());
        docker::run_from(&["cp", "--archive", "-", &target], File::open(archive)?)?;
    }
    instance::start_and_connect(adb, &instance)
}

fn snapshot_dir(name: &str) -> PathBuf {
    data_dir().join("snapshots").join(name)
}
