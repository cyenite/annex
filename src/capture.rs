//! Evidence from a test run: screen recordings (`annex record`) and Android's log (`annex logs`).
//!
//! A recording is a window-less scrcpy process; annex keeps no record of it and finds it again
//! from `/proc` by its command line, so `stop` works from another shell or CI step.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::str::FromStr;
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::adb::Adb;
use crate::instance::Instance;
use crate::paths::log_dir;
use crate::process::log_tail;
use crate::scrcpy::Scrcpy;

const STOP_TIMEOUT: Duration = Duration::from_secs(15);
const DATE_TIMEOUT: Duration = Duration::from_secs(10);
const FORMATS: [&str; 2] = ["mp4", "mkv"];

/// How far back `annex logs --since` reaches: `30s`, `10m` or `2h`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Since(Duration);

impl FromStr for Since {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, String> {
        let invalid = || format!("{value:?} is not a duration like 30s, 10m or 2h");
        let unit = value.chars().last().ok_or_else(invalid)?;
        let seconds = match unit {
            's' => 1,
            'm' => 60,
            'h' => 60 * 60,
            _ => return Err(invalid()),
        };
        let count: u64 = value[..value.len() - 1].parse().map_err(|_| invalid())?;
        if count == 0 {
            return Err(invalid());
        }
        Ok(Self(Duration::from_secs(count * seconds)))
    }
}

pub fn start_recording(adb: &Adb, instance: &Instance, output: &Path) -> Result<PathBuf> {
    if let Some(path) = recording(&instance.serial)?.map(|(_, path)| path) {
        bail!(
            "{} is already recording to {}: `annex record {} stop` first",
            instance.name,
            path.display(),
            instance.name
        );
    }
    let is_known_format = output
        .extension()
        .is_some_and(|extension| FORMATS.iter().any(|format| extension == *format));
    if !is_known_format {
        bail!("recordings are .mp4 or .mkv files, like run.mp4");
    }
    let path = std::path::absolute(output)?;
    if let Some(folder) = path.parent() {
        fs::create_dir_all(folder)
            .with_context(|| format!("could not create {}", folder.display()))?;
    }
    let log = log_dir().join(format!("record-{}.log", instance.name));
    Scrcpy::locate()?.record(adb, &instance.serial, &path, &log)?;
    Ok(path)
}

/// Stops the instance's recording and waits until the file is complete.
pub fn stop_recording(instance: &Instance) -> Result<PathBuf> {
    let Some((pid, path)) = recording(&instance.serial)? else {
        bail!(
            "{} isn't recording: start with `annex record {} start -o run.mp4`",
            instance.name,
            instance.name
        );
    };
    // SIGINT is scrcpy's signal to finish the file; a harder kill leaves an unplayable MP4.
    let status = Command::new("kill")
        .args(["-INT", &pid.to_string()])
        .status()
        .context("failed to run kill")?;
    if !status.success() {
        bail!("could not stop the recording (process {pid})");
    }
    let started = Instant::now();
    while Path::new(&format!("/proc/{pid}")).exists() {
        if started.elapsed() > STOP_TIMEOUT {
            bail!(
                "the recording (process {pid}) didn't finish within {} s",
                STOP_TIMEOUT.as_secs()
            );
        }
        sleep(Duration::from_millis(100));
    }
    // scrcpy writes nothing until the first frame arrives, so an empty file means the instance
    // never sent video (its display or encoder failed), which a CI run must not take as evidence.
    let is_empty = fs::metadata(&path).map_or(true, |metadata| metadata.len() == 0);
    if is_empty {
        let log = log_dir().join(format!("record-{}.log", instance.name));
        bail!(
            "the recording of {} is empty: the instance sent no video; details:\n{}",
            instance.name,
            log_tail(&log, 8)
        );
    }
    Ok(path)
}

/// Writes Android's log, in logcat's threadtime format, to `output` or the terminal. Android
/// only keeps its most recent log, so a long run's start may be gone.
pub fn write_logs(
    adb: &Adb,
    instance: &Instance,
    since: Option<Since>,
    output: Option<&Path>,
) -> Result<()> {
    let start = match since {
        Some(Since(window)) => {
            let now = adb.shell(&instance.serial, "date +%s", DATE_TIMEOUT)?;
            let now: u64 = now
                .parse()
                .with_context(|| format!("{} gave its time as {now:?}", instance.name))?;
            Some(format!("{}.000", now.saturating_sub(window.as_secs())))
        }
        None => None,
    };
    let args = logcat_args(start.as_deref());
    let stdout = match output {
        Some(path) => Stdio::from(
            File::create(path).with_context(|| format!("could not create {}", path.display()))?,
        ),
        None => Stdio::inherit(),
    };
    adb.run_into(&instance.serial, &args, stdout)
}

/// `-t` with a start time implies a dump; without one, `-d` dumps everything kept.
fn logcat_args(start: Option<&str>) -> Vec<&str> {
    let mut args = vec!["logcat", "-v", "threadtime"];
    match start {
        Some(start) => args.extend(["-t", start]),
        None => args.push("-d"),
    }
    args
}

/// The process recording `serial`, and the file it records to.
fn recording(serial: &str) -> Result<Option<(u32, PathBuf)>> {
    for entry in fs::read_dir("/proc")? {
        let entry = entry?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse().ok())
        else {
            continue;
        };
        let Ok(cmdline) = fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        if let Some(path) = recording_in(&cmdline, serial) {
            return Ok(Some((pid, path)));
        }
    }
    Ok(None)
}

fn recording_in(cmdline: &[u8], serial: &str) -> Option<PathBuf> {
    let args: Vec<&[u8]> = cmdline.split(|byte| *byte == 0).collect();
    let program = Path::new(std::str::from_utf8(args.first()?).ok()?);
    if program.file_name()? != "scrcpy" {
        return None;
    }
    let serial_arg = format!("--serial={serial}");
    if !args.contains(&serial_arg.as_bytes()) {
        return None;
    }
    args.iter().find_map(|arg| {
        let path = std::str::from_utf8(arg).ok()?.strip_prefix("--record=")?;
        Some(PathBuf::from(path))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_durations() {
        assert_eq!("30s".parse(), Ok(Since(Duration::from_secs(30))));
        assert_eq!("10m".parse(), Ok(Since(Duration::from_secs(600))));
        assert_eq!("2h".parse(), Ok(Since(Duration::from_secs(7200))));
        for value in ["", "s", "10", "0m", "-5m", "1d", "1.5h", "5 m"] {
            assert!(value.parse::<Since>().is_err(), "{value}");
        }
    }

    #[test]
    fn finds_the_recording_for_one_instance() {
        let cmdline =
            b"/usr/bin/scrcpy\0--serial=127.0.0.1:5603\0--record=/tmp/run.mp4\0--no-window\0";
        assert_eq!(
            recording_in(cmdline, "127.0.0.1:5603"),
            Some(PathBuf::from("/tmp/run.mp4"))
        );
        assert_eq!(recording_in(cmdline, "127.0.0.1:5600"), None);
        let window = b"scrcpy\0--serial\x00127.0.0.1:5603\0--window-title\0x\0";
        assert_eq!(recording_in(window, "127.0.0.1:5603"), None);
        let other = b"/bin/vim\0--serial=127.0.0.1:5603\0--record=/tmp/run.mp4\0";
        assert_eq!(recording_in(other, "127.0.0.1:5603"), None);
    }

    #[test]
    fn logcat_dumps_in_threadtime_format() {
        assert_eq!(logcat_args(None), ["logcat", "-v", "threadtime", "-d"]);
        assert_eq!(
            logcat_args(Some("1793690000.000")),
            ["logcat", "-v", "threadtime", "-t", "1793690000.000"]
        );
    }
}
