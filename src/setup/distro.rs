use std::process::Command;

/// What annex installs, named by role so setup's output never depends on a distro's naming.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Package {
    ContainerEngine,
    DebugTools,
    WindowHelper,
    Notifications,
    WindowMode,
}

impl Package {
    pub fn label(self) -> &'static str {
        match self {
            Self::ContainerEngine => "container engine",
            Self::DebugTools => "Android debug tools",
            Self::WindowHelper => "window helper",
            Self::Notifications => "desktop notifications",
            Self::WindowMode => "window mode",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackageManager {
    Apt,
    Dnf,
    Pacman,
    Zypper,
}

impl PackageManager {
    /// Picks the package manager from /etc/os-release's ID, then its ID_LIKE ancestry.
    pub fn from_os_release(os_release: &str) -> Option<Self> {
        let field = |key: &str| {
            os_release.lines().find_map(|line| {
                let value = line.strip_prefix(key)?.strip_prefix('=')?;
                Some(value.trim_matches('"').to_lowercase())
            })
        };
        let id = field("ID").unwrap_or_default();
        let like = field("ID_LIKE").unwrap_or_default();
        std::iter::once(id.as_str())
            .chain(like.split_whitespace())
            .find_map(|family| match family {
                "debian" | "ubuntu" => Some(Self::Apt),
                "fedora" | "rhel" | "centos" => Some(Self::Dnf),
                "arch" => Some(Self::Pacman),
                "opensuse" | "suse" | "opensuse-tumbleweed" | "opensuse-leap" => Some(Self::Zypper),
                _ => None,
            })
    }

    pub fn detect() -> Option<Self> {
        Self::from_os_release(&std::fs::read_to_string("/etc/os-release").ok()?)
    }

    /// The distro's name for a package, or None where the distro's own repositories lack it.
    pub fn package_name(self, package: Package) -> Option<&'static str> {
        match (self, package) {
            (Self::Apt, Package::ContainerEngine) => Some("docker.io"),
            (Self::Apt, Package::DebugTools) => Some("adb"),
            (Self::Apt, Package::WindowHelper) => Some("scrcpy"),
            (Self::Apt, Package::Notifications) => Some("libnotify-bin"),
            (Self::Apt, Package::WindowMode) => Some("waydroid"),
            (Self::Dnf, Package::ContainerEngine) => Some("moby-engine"),
            (Self::Dnf, Package::DebugTools) => Some("android-tools"),
            (Self::Dnf, Package::WindowHelper) => None,
            (Self::Dnf, Package::Notifications) => Some("libnotify"),
            (Self::Dnf, Package::WindowMode) => Some("waydroid"),
            (Self::Pacman, Package::ContainerEngine) => Some("docker"),
            (Self::Pacman, Package::DebugTools) => Some("android-tools"),
            (Self::Pacman, Package::WindowHelper) => Some("scrcpy"),
            (Self::Pacman, Package::Notifications) => Some("libnotify"),
            (Self::Pacman, Package::WindowMode) => None,
            (Self::Zypper, Package::ContainerEngine) => Some("docker"),
            (Self::Zypper, Package::DebugTools) => Some("android-tools"),
            (Self::Zypper, Package::WindowHelper) => Some("scrcpy"),
            (Self::Zypper, Package::Notifications) => Some("libnotify-tools"),
            (Self::Zypper, Package::WindowMode) => None,
        }
    }

    /// Whether the configured repositories can install `name` right now. Only apt is asked,
    /// since its archive differs most between releases; the others follow the static table.
    pub fn can_install(self, name: &str) -> bool {
        match self {
            Self::Apt => Command::new("apt-cache")
                .args(["show", "--no-all-versions", name])
                .output()
                .is_ok_and(|output| output.status.success() && !output.stdout.is_empty()),
            Self::Dnf | Self::Pacman | Self::Zypper => true,
        }
    }

    /// The commands that install `names`, to run as root, refreshing the package index first
    /// where the manager needs it.
    pub fn install_commands(self, names: &[&str]) -> Vec<Vec<String>> {
        let owned = |args: &[&str]| -> Vec<String> {
            args.iter()
                .chain(names)
                .map(|arg| arg.to_string())
                .collect()
        };
        match self {
            Self::Apt => vec![
                vec!["apt-get".into(), "update".into()],
                owned(&[
                    "env",
                    "DEBIAN_FRONTEND=noninteractive",
                    "apt-get",
                    "install",
                    "-y",
                ]),
            ],
            Self::Dnf => vec![owned(&["dnf", "install", "-y"])],
            Self::Pacman => vec![owned(&["pacman", "-S", "--needed", "--noconfirm"])],
            Self::Zypper => vec![owned(&["zypper", "--non-interactive", "install"])],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_distros_by_id() {
        let cases = [
            ("ID=ubuntu\nID_LIKE=debian\n", Some(PackageManager::Apt)),
            ("ID=debian\n", Some(PackageManager::Apt)),
            ("ID=fedora\n", Some(PackageManager::Dnf)),
            ("ID=arch\n", Some(PackageManager::Pacman)),
            (
                "ID=\"opensuse-tumbleweed\"\nID_LIKE=\"opensuse suse\"\n",
                Some(PackageManager::Zypper),
            ),
            ("ID=gentoo\n", None),
            ("", None),
        ];
        for (os_release, expected) in cases {
            assert_eq!(
                PackageManager::from_os_release(os_release),
                expected,
                "{os_release}"
            );
        }
    }

    #[test]
    fn falls_back_to_the_id_like_ancestry() {
        let pop = "NAME=\"Pop!_OS\"\nID=pop\nID_LIKE=\"ubuntu debian\"\n";
        assert_eq!(
            PackageManager::from_os_release(pop),
            Some(PackageManager::Apt)
        );
        let endeavour = "ID=endeavouros\nID_LIKE=arch\n";
        assert_eq!(
            PackageManager::from_os_release(endeavour),
            Some(PackageManager::Pacman)
        );
    }

    #[test]
    fn apt_refreshes_its_index_before_installing() {
        let commands = PackageManager::Apt.install_commands(&["adb", "scrcpy"]);
        assert_eq!(commands[0], ["apt-get", "update"]);
        assert_eq!(commands[1].last().map(String::as_str), Some("scrcpy"));
        assert!(commands[1].contains(&"-y".to_string()));
    }

    #[test]
    fn other_managers_install_non_interactively() {
        assert_eq!(
            PackageManager::Pacman.install_commands(&["docker"]),
            [["pacman", "-S", "--needed", "--noconfirm", "docker"]]
        );
        assert_eq!(
            PackageManager::Zypper.install_commands(&["docker"]),
            [["zypper", "--non-interactive", "install", "docker"]]
        );
    }
}
