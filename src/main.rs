mod adb;
mod docker;
mod doctor;
mod gpu;
mod instance;
mod snapshot;

use std::env;
use std::fs::{self, File};
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

use adb::Adb;
use instance::AndroidVersion;

const SHOW_STARTUP: Duration = Duration::from_secs(3);

#[derive(Parser)]
#[command(
    name = "annex",
    version,
    about = "Fast Android instances for Linux developers"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Check that this machine can run Android instances
    Doctor,
    /// Create or start an instance and wait until adb can use it
    Up {
        name: String,
        /// Android version for a new instance [default: 15]
        #[arg(long, value_enum)]
        android: Option<AndroidVersion>,
    },
    /// Stop an instance (its data is kept)
    Down { name: String },
    /// Pause an instance in memory; `thaw` resumes it in well under a second
    Freeze { name: String },
    /// Resume a frozen instance
    Thaw { name: String },
    /// List instances
    Ls,
    /// Open a window showing an instance's screen (needs scrcpy)
    Show { name: String },
    /// Wipe an instance back to first boot, or to a snapshot with --to
    Reset {
        name: String,
        #[arg(long)]
        to: Option<String>,
    },
    /// Delete an instance and its data
    Rm {
        name: String,
        /// Keep the data volume
        #[arg(long)]
        keep_data: bool,
    },
    /// Save and list snapshots of an instance's data
    #[command(subcommand)]
    Snap(SnapCommand),
}

#[derive(Subcommand)]
enum SnapCommand {
    /// Save the instance's current data under a label
    Save { name: String, label: String },
    /// List an instance's snapshots
    Ls { name: String },
}

fn main() -> ExitCode {
    match run(Cli::parse().command) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(command: Commands) -> Result<ExitCode> {
    let started = Instant::now();
    let elapsed = || format!("{:.1} s", started.elapsed().as_secs_f64());
    match command {
        Commands::Doctor => {
            let checks = doctor::run();
            doctor::print(&checks);
            let has_failures = checks
                .iter()
                .any(|check| check.level == doctor::Level::Fail);
            return Ok(if has_failures {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            });
        }
        Commands::Up { name, android } => {
            instance::validate_name(&name)?;
            let instance = instance::up(&Adb::locate()?, &name, android)?;
            println!(
                "{name} ready at {} (Android {}, API {}) in {}",
                instance.serial(),
                instance.android.label(),
                instance.android.api(),
                elapsed()
            );
        }
        Commands::Down { name } => {
            instance::down(&Adb::locate()?, &name)?;
            println!("{name} stopped in {}", elapsed());
        }
        Commands::Freeze { name } => {
            instance::freeze(&name)?;
            println!("{name} frozen in {}", elapsed());
        }
        Commands::Thaw { name } => {
            instance::thaw(&Adb::locate()?, &name)?;
            println!("{name} thawed in {}", elapsed());
        }
        Commands::Ls => list()?,
        Commands::Show { name } => show(&name)?,
        Commands::Reset { name, to } => {
            snapshot::reset(&Adb::locate()?, &name, to.as_deref())?;
            let target = to.map_or("first boot".to_string(), |label| {
                format!("snapshot {label}")
            });
            println!("{name} reset to {target} in {}", elapsed());
        }
        Commands::Rm { name, keep_data } => {
            instance::remove(&Adb::locate()?, &name, keep_data)?;
            println!("{name} removed");
        }
        Commands::Snap(SnapCommand::Save { name, label }) => {
            let path = snapshot::save(&name, &label)?;
            println!("saved {} in {}", path.display(), elapsed());
        }
        Commands::Snap(SnapCommand::Ls { name }) => {
            for snapshot in snapshot::list(&name)? {
                println!(
                    "{:<24} {:>8.1} MB",
                    snapshot.label,
                    snapshot.bytes as f64 / 1_048_576.0
                );
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn list() -> Result<()> {
    let instances = instance::list()?;
    if instances.is_empty() {
        println!("no instances: create one with `annex up <name>`");
        return Ok(());
    }
    println!(
        "{:<20} {:<8} {:<8} {:<16} SNAPSHOTS",
        "NAME", "STATE", "ANDROID", "ADB"
    );
    for instance in instances {
        println!(
            "{:<20} {:<8} {:<8} {:<16} {}",
            instance.name,
            instance.state.label(),
            instance.android.label(),
            instance.serial(),
            snapshot::list(&instance.name)?.len()
        );
    }
    Ok(())
}

fn show(name: &str) -> Result<()> {
    let adb = Adb::locate()?;
    let instance = instance::require(name)?;
    let scrcpy =
        which("scrcpy").context("scrcpy not found: install it to open instance windows")?;
    let log_path = data_dir().join("logs").join(format!("show-{name}.log"));
    fs::create_dir_all(log_path.parent().expect("log path has a parent"))?;
    // scrcpy starts its own adb unless told which one, and a mismatched adb kills the running server.
    // Audio is off because Redroid images have no Opus encoder, which crashes scrcpy's server.
    let mut child = Command::new(scrcpy)
        .args([
            "--serial",
            &instance.serial(),
            "--window-title",
            &format!("annex: {name}"),
            "--no-audio",
        ])
        .env("ADB", adb.path())
        .stdin(Stdio::null())
        .stdout(File::create(&log_path)?)
        .stderr(File::create(&log_path)?)
        .spawn()
        .context("failed to start scrcpy")?;
    sleep(SHOW_STARTUP);
    if let Some(status) = child.try_wait()? {
        let log = fs::read_to_string(&log_path).unwrap_or_default();
        let tail: Vec<&str> = log.lines().rev().take(8).collect();
        let tail: Vec<&str> = tail.into_iter().rev().collect();
        bail!("scrcpy exited ({status}):\n{}", tail.join("\n"));
    }
    println!("opened a window for {name} (log: {})", log_path.display());
    Ok(())
}

pub fn which(program: &str) -> Option<PathBuf> {
    env::split_paths(&env::var_os("PATH")?)
        .map(|directory| directory.join(program))
        .find(|candidate| candidate.is_file())
}

pub fn data_dir() -> PathBuf {
    env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("annex")
}
