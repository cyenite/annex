mod adb;
mod bridge;
mod cli;
mod desktop;
mod docker;
mod doctor;
mod gpu;
mod instance;
mod launcher;
mod paths;
mod process;
mod redroid;
mod scrcpy;
mod setup;
mod snapshot;
mod update;
mod waydroid;
mod window;

use std::io;
use std::process::ExitCode;
use std::sync::Mutex;
use std::time::Instant;

use anyhow::{Result, bail};
use clap::{CommandFactory, Parser};

use adb::Adb;
use cli::{BridgeCommand, Cli, Commands, LauncherCommand, SnapCommand};
use instance::{Backend, State, UpOptions};

/// The last outcome line a command printed, which `--notify` repeats as a notification.
static OUTCOME: Mutex<Option<String>> = Mutex::new(None);

fn main() -> ExitCode {
    let cli = Cli::parse();
    // A launcher has no terminal, so its command always reports through notifications.
    let should_notify = cli.notify || matches!(cli.command, Commands::OpenWindow { .. });
    match run(cli.command) {
        Ok(code) => {
            let outcome = OUTCOME.lock().expect("outcome lock").take();
            if let Some(outcome) = outcome.filter(|_| should_notify) {
                desktop::notify("annex", &outcome);
            }
            code
        }
        Err(error) => {
            eprintln!("error: {error:#}");
            if should_notify {
                desktop::notify("annex: something went wrong", &format!("{error:#}"));
            }
            ExitCode::FAILURE
        }
    }
}

/// Prints a command's result and remembers it for `--notify`.
fn report(outcome: String) {
    println!("{outcome}");
    *OUTCOME.lock().expect("outcome lock") = Some(outcome);
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
        Commands::Setup {
            yes,
            dry_run,
            bridge,
            no_bridge,
            window_mode,
            repair,
        } => setup::run(setup::Run {
            is_yes: yes,
            is_dry_run: dry_run,
            options: setup::Options {
                // The bridge belongs to a desktop session; over SSH there is none to start it in.
                bridge: !no_bridge && (bridge || desktop::has_session()),
                repair,
                window_mode,
            },
        })?,
        Commands::Update { check } => match update::run(check)? {
            update::Outcome::UpToDate(version) => {
                report(format!("annex {version} is the latest version"));
            }
            update::Outcome::NoRelease(version) => report(format!(
                "annex {version}: no release is published yet; `annex update` builds the latest source"
            )),
            update::Outcome::Available { current, latest } => report(format!(
                "annex {latest} is available (you have {current}): run `annex update`"
            )),
            update::Outcome::Updated { from, to } => {
                report(format!("updated annex from {from} to {to}"));
            }
            update::Outcome::BuiltFromSource { path } => {
                report(format!("installed the latest annex at {}", path.display()));
                let running = std::env::current_exe()?.canonicalize()?;
                if path.canonicalize().ok() != Some(running.clone()) {
                    println!(
                        "note: this copy of annex is {}; use the new one, or put {} first on your PATH",
                        running.display(),
                        path.parent()
                            .map(|p| p.display().to_string())
                            .unwrap_or_default()
                    );
                }
            }
        },
        Commands::About { licenses } => {
            if licenses {
                print!("{}", include_str!("../THIRD_PARTY_NOTICES.md"));
            } else {
                println!("annex {}", env!("CARGO_PKG_VERSION"));
                println!("{}", env!("CARGO_PKG_DESCRIPTION"));
                println!("{}", env!("CARGO_PKG_REPOSITORY"));
                println!("License: {}", env!("CARGO_PKG_LICENSE"));
                println!("Third-party notices: annex about --licenses");
            }
        }
        Commands::Up {
            name,
            mode,
            android,
            display,
            no_window,
        } => {
            let adb = Adb::locate()?;
            let name = name.unwrap_or_else(|| instance::DEFAULT_NAME.to_string());
            let options = UpOptions {
                mode,
                display,
                android,
            };
            let instance = instance::up(&adb, &name, options)?;
            launcher::refresh_or_install(&instance);
            report(format!(
                "{name} ready at {} (Android {}) in {}",
                instance.serial,
                instance.android(),
                elapsed()
            ));
            // The instance is up either way, so a window that fails to open is only a warning:
            // scripts waiting on `annex up` must not see a failure.
            if !no_window
                && desktop::has_session()
                && let Err(error) = window::show_once(&adb, &instance)
            {
                eprintln!("warning: {name} is up, but its window didn't open: {error:#}");
            }
        }
        Commands::Down { name } => {
            let name = instance::resolve(name)?;
            instance::down(&Adb::locate()?, &name)?;
            report(format!("{name} stopped in {}", elapsed()));
        }
        Commands::Freeze { name } => {
            let name = instance::resolve(name)?;
            instance::freeze(&name)?;
            report(format!("{name} frozen in {}", elapsed()));
        }
        Commands::Thaw { name } => {
            let name = instance::resolve(name)?;
            instance::thaw(&Adb::locate()?, &name)?;
            report(format!("{name} thawed in {}", elapsed()));
        }
        Commands::Ls => list()?,
        Commands::OpenWindow { name } => {
            let name = instance::resolve(name)?;
            let adb = Adb::locate()?;
            match instance::find(&name)?.map(|instance| instance.state) {
                Some(State::Running) => {}
                Some(State::Frozen) => instance::thaw(&adb, &name)?,
                Some(State::Stopped) | None => {
                    desktop::notify("annex", &format!("starting {name}..."));
                    let instance = instance::up(&adb, &name, UpOptions::default())?;
                    launcher::refresh_or_install(&instance);
                }
            }
            if !window::show_once(&adb, &instance::require_running(&name)?)? {
                report(format!("{name}'s window is already open"));
            }
        }
        Commands::Launcher(LauncherCommand::Install { name }) => {
            let instance = instance::require(&instance::resolve(name)?)?;
            let path = launcher::install(&instance)?;
            report(format!(
                "added {} to your app launcher ({})",
                instance.name,
                path.display()
            ));
        }
        Commands::Launcher(LauncherCommand::Remove { name }) => {
            let name = instance::resolve(name)?;
            launcher::remove(&name)?;
            report(format!("removed {name} from your app launcher"));
        }
        Commands::Show { name } => {
            let name = instance::resolve(name)?;
            window::show(&Adb::locate()?, &instance::require_running(&name)?)?;
            report(format!("opened a window for {name}"));
        }
        Commands::Open { args } => {
            let (name, rest) = instance::split_name(args, 1)?;
            let [package] = &rest[..] else {
                bail!("give one app package name, like com.android.settings");
            };
            window::open(&Adb::locate()?, &instance::require_running(&name)?, package)?;
            report(format!("opened {package} on {name}"));
        }
        Commands::Install { args } => {
            let (name, apks) = instance::split_name(args, 1)?;
            if apks.is_empty() {
                bail!("give at least one APK file");
            }
            let adb = Adb::locate()?;
            let instance = instance::require_running(&name)?;
            for apk in &apks {
                adb.run_attached(&instance.serial, &["install", "-r", "-g", apk])?;
            }
            println!(
                "installed {} on {name} in {}",
                plural(apks.len(), "APK"),
                elapsed()
            );
        }
        Commands::Push { args, to } => {
            let (name, paths) = instance::split_name(args, 1)?;
            if paths.is_empty() {
                bail!("give at least one file or folder to copy");
            }
            let adb = Adb::locate()?;
            let instance = instance::require_running(&name)?;
            let mut command: Vec<&str> = vec!["push"];
            command.extend(paths.iter().map(String::as_str));
            command.push(&to);
            adb.run_attached(&instance.serial, &command)?;
            println!("copied {} to {name}:{to}", plural(paths.len(), "item"));
        }
        Commands::Pull { args } => {
            let (name, rest) = instance::split_name(args, 1)?;
            let (source, destination) = match &rest[..] {
                [source] => (source.as_str(), "."),
                [source, destination] => (source.as_str(), destination.as_str()),
                _ => bail!("give the path on the instance and, optionally, where to put it"),
            };
            let adb = Adb::locate()?;
            let instance = instance::require_running(&name)?;
            adb.run_attached(&instance.serial, &["pull", source, destination])?;
        }
        Commands::Reset { name, to } => {
            snapshot::reset(&Adb::locate()?, &name, to.as_deref())?;
            let target = to.map_or("first boot".to_string(), |label| {
                format!("snapshot {label}")
            });
            report(format!("{name} reset to {target} in {}", elapsed()));
        }
        Commands::Rm { name, keep_data } => {
            instance::remove(&Adb::locate()?, &name, keep_data)?;
            launcher::remove(&name)?;
            report(format!("{name} removed"));
        }
        Commands::Snap(SnapCommand::Save { args }) => {
            let (name, rest) = instance::split_name(args, 1)?;
            let [label] = &rest[..] else {
                bail!("give one label for the snapshot");
            };
            let path = snapshot::save(&name, label)?;
            launcher::refresh(&instance::require(&name)?);
            report(format!("saved {} in {}", path.display(), elapsed()));
        }
        Commands::Snap(SnapCommand::Ls { name }) => {
            let name = instance::resolve(name)?;
            instance::require(&name)?.container("snapshots")?;
            for snapshot in snapshot::list(&name)? {
                println!(
                    "{:<24} {:>8.1} MB",
                    snapshot.label,
                    snapshot.bytes as f64 / 1_048_576.0
                );
            }
        }
        Commands::Bridge(command) => bridge_command(command)?,
        Commands::Completions { shell } => {
            clap_complete::generate(shell, &mut Cli::command(), "annex", &mut io::stdout());
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn bridge_command(command: BridgeCommand) -> Result<()> {
    match command {
        BridgeCommand::Run { verbose } => bridge::run(&Adb::locate()?, verbose)?,
        BridgeCommand::Install => {
            let path = bridge::service::install()?;
            println!("bridge installed as {} and running", path.display());
            println!("logs: journalctl --user -u annex-bridge -f");
        }
        BridgeCommand::Uninstall => {
            bridge::service::uninstall()?;
            println!("bridge removed");
        }
        BridgeCommand::Status => {
            println!("bridge service: {}", bridge::service::state());
        }
    }
    Ok(())
}

fn list() -> Result<()> {
    let instances = instance::list()?;
    if instances.is_empty() {
        println!("no instances: create one with `annex up <name>`");
        return Ok(());
    }
    println!(
        "{:<20} {:<8} {:<10} {:<8} {:<22} SNAPSHOTS",
        "NAME", "STATE", "MODE", "ANDROID", "ADB"
    );
    for instance in instances {
        let snapshots = match instance.backend {
            Backend::Redroid(_) => snapshot::list(&instance.name)?.len().to_string(),
            Backend::Waydroid { .. } => "-".to_string(),
        };
        println!(
            "{:<20} {:<8} {:<10} {:<8} {:<22} {snapshots}",
            instance.name,
            instance.state.label(),
            instance.backend.mode().label(),
            instance.android(),
            instance.serial,
        );
    }
    Ok(())
}

fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}
