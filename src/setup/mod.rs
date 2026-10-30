//! `annex setup`: check the machine, show a plan split by whether it needs sudo, ask once, then
//! fix everything with live progress. Safe to re-run; anything already in place is skipped.

mod distro;
pub mod facts;
mod plan;
mod pull;
mod ui;

use std::io::{self, BufRead, IsTerminal, Write};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, sleep};
use std::time::Duration;

use anyhow::{Result, bail};

use crate::adb::Adb;
use crate::bridge;
use crate::instance::AndroidVersion;
use crate::process::output_within;
pub use plan::Options;
use plan::{Facts, Step, plan};
use pull::PullError;
use ui::{Failure, Task, Ui};

const SUDO_REFRESH: Duration = Duration::from_secs(45);
const ROOT_TIMEOUT: Duration = Duration::from_secs(60 * 60);

#[derive(Clone, Copy, Default)]
pub struct Run {
    pub is_yes: bool,
    pub is_dry_run: bool,
    pub options: Options,
}

pub fn run(run: Run) -> Result<()> {
    let ui = Ui::new();
    let facts = facts::gather();
    let plan = plan(&facts, run.options);

    if !plan.done.is_empty() {
        ui.heading("Already set up");
        ui.item(&plan.done.join(", "));
        println!();
    }
    if plan.steps.is_empty() {
        ui.heading("Nothing to do: this machine is ready.");
        print_next_steps(&ui, false, run.options);
        return Ok(());
    }
    print_plan(&ui, &plan.steps);
    if run.is_dry_run {
        ui.note("Dry run: nothing was changed.");
        return Ok(());
    }
    if !run.is_yes && !confirm()? {
        bail!("setup cancelled; nothing was changed");
    }

    let is_root = is_root();
    let _keepalive = if plan.needs_sudo() && !is_root {
        Some(Sudo::start()?)
    } else {
        None
    };
    let mut context = Context {
        facts,
        is_root,
        added_group: false,
    };
    let mut needs_you = Vec::new();
    println!();
    for step in &plan.steps {
        let mut task = ui.start(&step.title());
        if let Step::Manual(instruction) = step {
            task.needs_you();
            needs_you.push(instruction.clone());
            continue;
        }
        match context.perform(step, &mut task) {
            Ok(()) => task.succeed(),
            Err(failure) => {
                task.fail(&failure);
                println!();
                bail!("setup stopped; fix the problem above and run `annex setup` again");
            }
        }
    }

    println!();
    if needs_you.is_empty() {
        ui.heading("annex is ready.");
    } else {
        ui.heading("Almost ready. These need you:");
        for instruction in &needs_you {
            ui.item(instruction);
        }
    }
    print_next_steps(&ui, context.added_group, run.options);
    Ok(())
}

fn print_plan(ui: &Ui, steps: &[Step]) {
    let (with_sudo, without_sudo): (Vec<&Step>, Vec<&Step>) =
        steps.iter().partition(|step| step.needs_sudo());
    let (manual, without_sudo): (Vec<&Step>, Vec<&Step>) = without_sudo
        .into_iter()
        .partition(|step| matches!(step, Step::Manual(_)));
    for (heading, group) in [
        (
            "Needs administrator access (sudo asks for your password once):",
            with_sudo,
        ),
        ("No administrator access needed:", without_sudo),
        ("Can't be done automatically on this system:", manual),
    ] {
        if !group.is_empty() {
            ui.heading(heading);
            for step in group {
                ui.item(&step.title());
            }
            println!();
        }
    }
}

fn print_next_steps(ui: &Ui, added_group: bool, options: Options) {
    println!();
    println!("Next: {}", console::style("annex up").cyan().bold());
    if options.window_mode {
        println!(
            "Window mode: {}",
            console::style("annex up desk --mode window").cyan().bold()
        );
    }
    if options.bridge && !facts::has_tray_host() {
        ui.note(
            "Your panel shows no app indicators, so the bridge's menu stays hidden: on GNOME, enable the AppIndicator extension.",
        );
    }
    if added_group {
        ui.note(
            "Log out and back in (or run `newgrp docker`) so your user can run containers without sudo.",
        );
    }
}

fn confirm() -> Result<bool> {
    if !io::stdin().is_terminal() {
        bail!("setup needs your approval: run it in a terminal, or pass --yes");
    }
    print!("Go ahead? [Y/n] ");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().lock().read_line(&mut answer)?;
    Ok(matches!(
        answer.trim().to_lowercase().as_str(),
        "" | "y" | "yes"
    ))
}

fn is_root() -> bool {
    Command::new("id")
        .arg("-u")
        .output()
        .is_ok_and(|output| String::from_utf8_lossy(&output.stdout).trim() == "0")
}

/// Asks for the password once through sudo itself, which annex never sees, then keeps the
/// credential fresh in the background so a long download doesn't let it expire mid-run.
struct Sudo {
    stop: Arc<AtomicBool>,
}

impl Sudo {
    fn start() -> Result<Self> {
        println!();
        let status = Command::new("sudo")
            .args(["-v", "-p", "annex setup needs your password for sudo: "])
            .status()?;
        if !status.success() {
            bail!("sudo didn't grant access; nothing was changed");
        }
        let stop = Arc::new(AtomicBool::new(false));
        let should_stop = stop.clone();
        thread::spawn(move || {
            while !should_stop.load(Ordering::Relaxed) {
                sleep(SUDO_REFRESH);
                let _ = Command::new("sudo").args(["-n", "-v"]).status();
            }
        });
        Ok(Self { stop })
    }
}

impl Drop for Sudo {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

struct Context {
    facts: Facts,
    is_root: bool,
    added_group: bool,
}

impl Context {
    fn perform(&mut self, step: &Step, task: &mut Task) -> Result<(), Failure> {
        match step {
            Step::InstallPackages(packages) => {
                let manager = self
                    .facts
                    .manager
                    .expect("only planned with a package manager");
                let names: Vec<&str> = packages
                    .iter()
                    .filter_map(|package| manager.package_name(*package))
                    .collect();
                for command in manager.install_commands(&names) {
                    let is_refresh = command.last().is_some_and(|arg| arg == "update");
                    task.detail(if is_refresh {
                        "refreshing package lists"
                    } else {
                        "installing"
                    });
                    self.root(step, &command)?;
                }
                Ok(())
            }
            Step::StartDocker => {
                self.root(step, &args(&["systemctl", "enable", "--now", "docker"]))
            }
            Step::JoinDockerGroup => {
                let user = facts::current_user().ok_or_else(|| simple(step, "USER is not set"))?;
                self.root(step, &args(&["usermod", "-aG", "docker", &user]))?;
                self.added_group = true;
                Ok(())
            }
            Step::LoadBinder => self.root(step, &args(&["modprobe", facts::BINDER_MODULE])),
            Step::LoadBinderAtBoot => self.root(
                step,
                &args(&[
                    "sh",
                    "-c",
                    &format!(
                        "echo {} > {}",
                        facts::BINDER_MODULE,
                        facts::BINDER_BOOT_FILE
                    ),
                ]),
            ),
            Step::HideAndroidImages => {
                // The rule goes in as an argument, not interpolated, so its quotes stay intact.
                self.root(
                    step,
                    &args(&[
                        "sh",
                        "-c",
                        "printf '%s' \"$1\" > \"$2\"",
                        "sh",
                        facts::IMAGE_RULE,
                        facts::IMAGE_RULE_FILE,
                    ]),
                )?;
                self.root(step, &args(&["udevadm", "control", "--reload-rules"]))?;
                // Applies the rule to the disk images of instances already running.
                self.root(
                    step,
                    &args(&[
                        "udevadm",
                        "trigger",
                        "--subsystem-match=block",
                        "--action=change",
                    ]),
                )
            }
            Step::InitWindowMode => {
                task.detail("downloading, this can take several minutes");
                self.root(step, &args(&["waydroid", "init"]))?;
                task.detail("starting its service");
                self.root(
                    step,
                    &args(&["systemctl", "enable", "--now", "waydroid-container"]),
                )
            }
            Step::RestartWindowMode => {
                self.root(step, &args(&["systemctl", "restart", "waydroid-container"]))
            }
            Step::CreateAdbKey => Adb::locate()
                .and_then(|adb| adb.public_key())
                .map(drop)
                .map_err(|error| simple(step, &format!("{error:#}"))),
            Step::PullImage => self.pull_image(step, task),
            Step::InstallBridge => bridge::service::install()
                .map(drop)
                .map_err(|error| simple(step, &format!("{error:#}"))),
            Step::Manual(_) => Ok(()),
        }
    }

    fn pull_image(&self, step: &Step, task: &mut Task) -> Result<(), Failure> {
        let image = AndroidVersion::Android15.image();
        match pull::pull(image, |done, total| task.bytes(done, total)) {
            Ok(()) => Ok(()),
            // Joined the docker group in this run, or earlier without logging in again: the
            // socket still refuses this session, so the download goes through sudo instead.
            Err(PullError::PermissionDenied) => {
                task.detail("downloading through sudo (no byte progress until you log in again)");
                self.root(step, &args(&["docker", "pull", "--quiet", image]))
            }
            Err(PullError::Failed(error)) => Err(simple(step, &format!("{error:#}"))),
        }
    }

    /// Runs a command as root through the sudo session opened at the start.
    fn root(&self, step: &Step, command: &[String]) -> Result<(), Failure> {
        let mut full: Vec<String> = Vec::new();
        if !self.is_root {
            full.extend(["sudo".to_string(), "-n".to_string()]);
        }
        full.extend(command.iter().cloned());
        let shown = full.join(" ");
        let output = output_within(Command::new(&full[0]).args(&full[1..]), ROOT_TIMEOUT).map_err(
            |error| Failure {
                output: String::new(),
                message: format!("{error:#}"),
                fix: step.fix(),
                command: Some(shown.clone()),
            },
        )?;
        if output.status.success() {
            return Ok(());
        }
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        Err(Failure {
            output: text,
            message: format!("the command failed ({})", output.status),
            fix: step.fix(),
            command: Some(shown),
        })
    }
}

fn simple(step: &Step, message: &str) -> Failure {
    Failure {
        output: String::new(),
        message: message.to_string(),
        fix: step.fix(),
        command: None,
    }
}

fn args(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| part.to_string()).collect()
}
