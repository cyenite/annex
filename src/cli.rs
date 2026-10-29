use clap::{Parser, Subcommand};
use clap_complete::Shell;

use crate::instance::{AndroidVersion, Mode};
use crate::redroid::Display;

#[derive(Parser)]
#[command(
    name = "annex",
    version,
    about = "Fast Android instances for Linux developers",
    long_about = "Fast Android instances for Linux developers.\n\n\
        Instances run in container mode (as many as you like, Android 13-15, snapshots) or in \
        window mode (one instance whose apps open in native desktop windows). Android Studio, \
        Flutter and adb see each one as an ordinary device."
)]
pub struct Cli {
    /// Report the outcome as a desktop notification, for launchers and menus with no terminal
    #[arg(long, global = true, hide = true)]
    pub notify: bool,
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Check that this machine can run Android instances
    Doctor,
    /// Install and configure everything annex needs, after showing the plan
    Setup {
        /// Don't ask before making changes
        #[arg(long)]
        yes: bool,
        /// Show the plan and change nothing
        #[arg(long)]
        dry_run: bool,
        /// Also run the desktop bridge with your session (clipboard, notifications)
        #[arg(long)]
        bridge: bool,
        /// Also set up window mode (apps in native desktop windows)
        #[arg(long)]
        window_mode: bool,
        /// Also restart window mode's system service when its state is out of sync
        #[arg(long)]
        repair: bool,
    },
    /// Create or start an instance and wait until adb can use it
    Up {
        /// The instance [default: `default`]
        name: Option<String>,
        /// How a new instance runs [default: container]
        #[arg(long, value_enum)]
        mode: Option<Mode>,
        /// Android version for a new instance [default: 15]
        #[arg(long, value_enum)]
        android: Option<AndroidVersion>,
        /// Screen size and density, like 1080x2400/420 [default: 1080x2400/420]; changing it
        /// on an existing instance rebuilds its container and keeps its data
        #[arg(long, value_name = "WIDTHxHEIGHT/DPI")]
        display: Option<Display>,
    },
    /// Stop an instance (its data is kept)
    Down {
        /// The instance [default: `default`, or the only running instance]
        name: Option<String>,
    },
    /// Pause an instance in memory; `thaw` resumes it in well under a second
    Freeze {
        /// The instance [default: `default`, or the only running instance]
        name: Option<String>,
    },
    /// Resume a frozen instance
    Thaw {
        /// The instance [default: `default`, or the only running instance]
        name: Option<String>,
    },
    /// List instances
    Ls,
    /// Open a window showing an instance's whole screen
    Show {
        /// The instance [default: `default`, or the only running instance]
        name: Option<String>,
    },
    /// Start an instance if needed, then open its screen; what its app launcher runs
    OpenWindow {
        /// The instance [default: `default`, or the only running instance]
        name: Option<String>,
    },
    /// Add or remove an instance's entry in your desktop's app launcher
    #[command(subcommand)]
    Launcher(LauncherCommand),
    /// Open one app in a desktop window of its own
    #[command(
        override_usage = "annex open [NAME] <PACKAGE>",
        after_help = "For example: annex open com.android.settings"
    )]
    Open {
        /// An optional instance, then the app's package name
        #[arg(value_name = "ARGS", num_args = 1..=2, required = true)]
        args: Vec<String>,
    },
    /// Install or update APKs, granting their runtime permissions
    #[command(override_usage = "annex install [NAME] <APK>...")]
    Install {
        /// An optional instance, then one or more APK files
        #[arg(value_name = "ARGS", required = true)]
        args: Vec<String>,
    },
    /// Copy files or folders from this machine to an instance
    #[command(override_usage = "annex push [NAME] <PATH>... [--to <FOLDER>]")]
    Push {
        /// An optional instance, then the files or folders to copy
        #[arg(value_name = "ARGS", required = true)]
        args: Vec<String>,
        /// Folder on the instance to copy into
        #[arg(long, value_name = "FOLDER", default_value = "/sdcard/Download")]
        to: String,
    },
    /// Copy a file or folder from an instance to this machine
    #[command(
        override_usage = "annex pull [NAME] <SOURCE> [DESTINATION]",
        after_help = "The destination defaults to the current folder."
    )]
    Pull {
        /// An optional instance, the path on it, and where to put it
        #[arg(value_name = "ARGS", num_args = 1..=3, required = true)]
        args: Vec<String>,
    },
    /// Wipe an instance back to first boot, or to a snapshot with --to
    Reset {
        name: String,
        #[arg(long, value_name = "LABEL")]
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
    /// Share the clipboard and notifications between instances and the desktop
    #[command(subcommand)]
    Bridge(BridgeCommand),
    /// Show the version, project links and third-party licenses
    About {
        /// Print the notices for the software annex uses
        #[arg(long)]
        licenses: bool,
    },
    /// Print a shell completion script
    #[command(after_help = "For example: annex completions zsh > ~/.zfunc/_annex")]
    Completions { shell: Shell },
}

#[derive(Subcommand)]
pub enum SnapCommand {
    /// Save the instance's current data under a label
    #[command(override_usage = "annex snap save [NAME] <LABEL>")]
    Save {
        /// An optional instance, then the snapshot's label
        #[arg(value_name = "ARGS", num_args = 1..=2, required = true)]
        args: Vec<String>,
    },
    /// List an instance's snapshots
    Ls {
        /// The instance [default: `default`, or the only running instance]
        name: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum LauncherCommand {
    /// Add the launcher entry (`annex up` does this for you)
    Install {
        /// The instance [default: `default`, or the only running instance]
        name: Option<String>,
    },
    /// Remove the launcher entry (`annex rm` does this for you)
    Remove {
        /// The instance [default: `default`, or the only running instance]
        name: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum BridgeCommand {
    /// Run the bridge in the foreground
    Run {
        /// Log every clipboard transfer (sizes only, never the text)
        #[arg(long)]
        verbose: bool,
    },
    /// Run the bridge as a systemd user service that starts with your desktop session
    Install,
    /// Stop the service and remove it
    Uninstall,
    /// Show whether the service is running
    Status,
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn command_line_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_display_and_android_options() {
        let cli = Cli::try_parse_from([
            "annex",
            "up",
            "tab",
            "--android",
            "14",
            "--display",
            "1600x2560/320",
        ])
        .unwrap();
        let Commands::Up {
            android, display, ..
        } = cli.command
        else {
            panic!("expected up");
        };
        assert_eq!(android, Some(AndroidVersion::Android14));
        assert_eq!(display.unwrap().to_string(), "1600x2560/320");
    }

    #[test]
    fn rejects_a_malformed_display() {
        assert!(Cli::try_parse_from(["annex", "up", "tab", "--display", "big"]).is_err());
    }
}
