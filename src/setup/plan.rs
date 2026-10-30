use super::distro::{Package, PackageManager};

/// What setup found on this machine. Gathered once, so the plan is a pure function of it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    pub manager: Option<PackageManager>,
    /// Packages the distro's repositories can't install right now.
    pub unavailable: Vec<Package>,
    pub has_adb: bool,
    pub has_image: bool,
    pub has_binder: bool,
    pub has_docker: bool,
    pub has_notify: bool,
    pub has_adb_key: bool,
    pub has_image_rule: bool,
    pub can_use_docker: bool,
    pub has_window_mode: bool,
    pub has_window_helper: bool,
    pub is_docker_running: bool,
    pub has_binder_at_boot: bool,
    pub is_in_docker_group: bool,
    pub is_bridge_installed: bool,
    pub is_window_mode_ready: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Options {
    pub bridge: bool,
    pub repair: bool,
    pub window_mode: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    InstallPackages(Vec<Package>),
    StartDocker,
    JoinDockerGroup,
    LoadBinder,
    LoadBinderAtBoot,
    HideAndroidImages,
    InitWindowMode,
    RestartWindowMode,
    CreateAdbKey,
    PullImage,
    InstallBridge,
    /// Something setup can't do on this distro; the text says how to do it by hand.
    Manual(String),
}

impl Step {
    pub fn title(&self) -> String {
        match self {
            Self::InstallPackages(packages) => {
                let labels: Vec<&str> = packages.iter().map(|package| package.label()).collect();
                format!("Install {}", labels.join(", "))
            }
            Self::StartDocker => "Start the container engine and enable it at boot".to_string(),
            Self::JoinDockerGroup => "Let your user run containers without sudo".to_string(),
            Self::LoadBinder => "Load the Android binder kernel module".to_string(),
            Self::LoadBinderAtBoot => "Load the binder module at every boot".to_string(),
            Self::HideAndroidImages => {
                "Keep Android's internal disk images out of your dock and file manager".to_string()
            }
            Self::InitWindowMode => "Download and set up window mode's system image".to_string(),
            Self::RestartWindowMode => "Restart window mode's system service".to_string(),
            Self::CreateAdbKey => "Create your adb key".to_string(),
            Self::PullImage => "Download the Android 15 image (about 3 GB)".to_string(),
            Self::InstallBridge => {
                "Start the desktop bridge with your session (clipboard, notifications)".to_string()
            }
            Self::Manual(instruction) => instruction.clone(),
        }
    }

    pub fn needs_sudo(&self) -> bool {
        matches!(
            self,
            Self::InstallPackages(_)
                | Self::StartDocker
                | Self::JoinDockerGroup
                | Self::LoadBinder
                | Self::LoadBinderAtBoot
                | Self::HideAndroidImages
                | Self::InitWindowMode
                | Self::RestartWindowMode
        )
    }

    /// What to try when the step fails, in plain words.
    pub fn fix(&self) -> &'static str {
        match self {
            Self::InstallPackages(_) => {
                "check your network and package sources, then run `annex setup` again"
            }
            Self::StartDocker => "see `systemctl status docker` for why the engine didn't start",
            Self::JoinDockerGroup => "add yourself by hand: `sudo usermod -aG docker $USER`",
            Self::LoadBinder | Self::LoadBinderAtBoot => {
                "your kernel may lack Android binder support; Ubuntu's, Arch's linux-zen and Fedora's ship it"
            }
            Self::HideAndroidImages => {
                "see `udevadm control --reload-rules` for errors, then run `annex setup` again"
            }
            Self::InitWindowMode => {
                "the image download can stall on a slow mirror; run `annex setup --window-mode` again to retry"
            }
            Self::RestartWindowMode => "reboot, then run `annex setup --repair` again",
            Self::CreateAdbKey => "run `adb start-server` once by hand",
            Self::PullImage => "check your network, then run `annex setup` again; it resumes",
            Self::InstallBridge => "run `annex bridge install` from your desktop session",
            Self::Manual(_) => "follow the instruction above",
        }
    }
}

pub struct Plan {
    pub steps: Vec<Step>,
    /// What is already in place, for the summary.
    pub done: Vec<&'static str>,
}

impl Plan {
    pub fn needs_sudo(&self) -> bool {
        self.steps.iter().any(Step::needs_sudo)
    }
}

pub fn plan(facts: &Facts, options: Options) -> Plan {
    let mut steps = Vec::new();
    let mut done = Vec::new();
    let mut check = |is_done: bool, label: &'static str| {
        if is_done {
            done.push(label);
        }
        !is_done
    };

    let mut wanted = Vec::new();
    if check(facts.has_docker, "container engine") {
        wanted.push(Package::ContainerEngine);
    }
    if check(facts.has_adb, "Android debug tools") {
        wanted.push(Package::DebugTools);
    }
    if check(facts.has_window_helper, "window helper") {
        wanted.push(Package::WindowHelper);
    }
    if options.bridge && check(facts.has_notify, "desktop notifications") {
        wanted.push(Package::Notifications);
    }
    if options.window_mode && check(facts.has_window_mode, "window mode package") {
        wanted.push(Package::WindowMode);
    }
    let (installable, manual): (Vec<Package>, Vec<Package>) =
        wanted.into_iter().partition(|package| {
            facts
                .manager
                .is_some_and(|manager| manager.package_name(*package).is_some())
                && !facts.unavailable.contains(package)
        });
    if !installable.is_empty() {
        steps.push(Step::InstallPackages(installable));
    }
    for package in manual {
        steps.push(Step::Manual(manual_instruction(facts.manager, package)));
    }

    if check(
        facts.has_docker && facts.is_docker_running,
        "container engine running",
    ) {
        steps.push(Step::StartDocker);
    }
    if check(
        facts.can_use_docker || facts.is_in_docker_group,
        "container access for your user",
    ) {
        steps.push(Step::JoinDockerGroup);
    }
    if check(facts.has_binder, "binder loaded") {
        steps.push(Step::LoadBinder);
    }
    if check(facts.has_binder_at_boot, "binder at boot") {
        steps.push(Step::LoadBinderAtBoot);
    }
    if check(facts.has_image_rule, "Android disk images hidden") {
        steps.push(Step::HideAndroidImages);
    }
    if options.window_mode && check(facts.is_window_mode_ready, "window mode set up") {
        steps.push(Step::InitWindowMode);
    }
    if options.repair && (facts.is_window_mode_ready || options.window_mode) {
        steps.push(Step::RestartWindowMode);
    }
    if check(facts.has_adb_key, "adb key") {
        steps.push(Step::CreateAdbKey);
    }
    if check(facts.has_image, "Android 15 image") {
        steps.push(Step::PullImage);
    }
    if options.bridge && check(facts.is_bridge_installed, "desktop bridge") {
        steps.push(Step::InstallBridge);
    }
    Plan { steps, done }
}

fn manual_instruction(manager: Option<PackageManager>, package: Package) -> String {
    let source = match (manager, package) {
        (_, Package::WindowMode) => "https://docs.waydro.id/usage/install-on-desktops",
        (_, Package::WindowHelper) => "https://github.com/Genymobile/scrcpy#linux",
        (_, Package::DebugTools) => "https://developer.android.com/tools/releases/platform-tools",
        (_, Package::ContainerEngine) => "https://docs.docker.com/engine/install/",
        (_, Package::Notifications) => "your distribution's libnotify package",
    };
    format!(
        "Install the {} by hand ({}): your distribution doesn't ship it",
        package.label(),
        source
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready() -> Facts {
        Facts {
            manager: Some(PackageManager::Apt),
            unavailable: Vec::new(),
            has_adb: true,
            has_image: true,
            has_binder: true,
            has_docker: true,
            has_notify: true,
            has_adb_key: true,
            has_image_rule: true,
            can_use_docker: true,
            has_window_mode: true,
            has_window_helper: true,
            is_docker_running: true,
            has_binder_at_boot: true,
            is_in_docker_group: true,
            is_bridge_installed: true,
            is_window_mode_ready: true,
        }
    }

    #[test]
    fn a_ready_machine_needs_nothing() {
        let plan = plan(&ready(), Options::default());
        assert!(plan.steps.is_empty());
        assert!(!plan.needs_sudo());
        assert!(plan.done.contains(&"binder loaded"));
    }

    #[test]
    fn a_fresh_machine_gets_everything_in_order() {
        let facts = Facts {
            manager: Some(PackageManager::Apt),
            ..Facts::default()
        };
        let plan = plan(&facts, Options::default());
        assert_eq!(
            plan.steps,
            [
                Step::InstallPackages(vec![
                    Package::ContainerEngine,
                    Package::DebugTools,
                    Package::WindowHelper
                ]),
                Step::StartDocker,
                Step::JoinDockerGroup,
                Step::LoadBinder,
                Step::LoadBinderAtBoot,
                Step::HideAndroidImages,
                Step::CreateAdbKey,
                Step::PullImage,
            ]
        );
        assert!(plan.needs_sudo());
    }

    #[test]
    fn options_add_the_bridge_and_window_mode() {
        let facts = Facts {
            has_notify: false,
            has_window_mode: false,
            is_bridge_installed: false,
            is_window_mode_ready: false,
            ..ready()
        };
        let options = Options {
            bridge: true,
            window_mode: true,
            ..Options::default()
        };
        let steps = plan(&facts, options).steps;
        assert_eq!(
            steps,
            [
                Step::InstallPackages(vec![Package::Notifications, Package::WindowMode]),
                Step::InitWindowMode,
                Step::InstallBridge,
            ]
        );
    }

    #[test]
    fn a_package_the_distro_lacks_becomes_a_manual_step() {
        let facts = Facts {
            manager: Some(PackageManager::Dnf),
            has_window_helper: false,
            ..ready()
        };
        let steps = plan(&facts, Options::default()).steps;
        assert_eq!(steps.len(), 1);
        assert!(matches!(&steps[0], Step::Manual(text) if text.contains("window helper")));
        assert!(!steps[0].needs_sudo());
    }

    #[test]
    fn an_unknown_distro_installs_nothing_itself() {
        let facts = Facts {
            manager: None,
            has_adb: false,
            ..ready()
        };
        let steps = plan(&facts, Options::default()).steps;
        assert!(matches!(&steps[..], [Step::Manual(_)]));
    }

    #[test]
    fn a_group_added_this_session_is_not_added_again() {
        let facts = Facts {
            can_use_docker: false,
            is_in_docker_group: true,
            ..ready()
        };
        assert!(plan(&facts, Options::default()).steps.is_empty());
    }

    #[test]
    fn repair_restarts_window_mode_even_when_it_looks_ready() {
        let options = Options {
            repair: true,
            ..Options::default()
        };
        assert_eq!(plan(&ready(), options).steps, [Step::RestartWindowMode]);
    }
}
