//! `annex update`: replaces this binary with the latest GitHub release after checking its
//! SHA-256, or rebuilds from source with cargo while no release is published. Downloads go
//! through curl, sha256sum and tar, which every distro ships, rather than a TLS stack in annex.

use std::env;
use std::fs::{self, Permissions};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::bridge;
use crate::paths::{data_dir, which};
use crate::process::{output_within, stdout_within};

const RELEASES: &str = "https://api.github.com/repos/cyenite/annex/releases";
const SOURCE: &str = "https://github.com/cyenite/annex";
const API_TIMEOUT: Duration = Duration::from_secs(30);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(10 * 60);

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

/// What `annex update` reports or did, for the caller to print.
pub enum Outcome {
    UpToDate(String),
    NoRelease(String),
    Available { current: String, latest: String },
    Updated { from: String, to: String },
    BuiltFromSource { path: PathBuf },
}

/// The programs a release ships, each in its own archive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Program {
    Cli,
    Manager,
}

impl Program {
    fn file_name(self) -> &'static str {
        match self {
            Self::Cli => "annex",
            Self::Manager => "annex-manager",
        }
    }

    fn archive_name(self, version: &str) -> String {
        format!("{}-{version}-x86_64-linux.tar.gz", self.file_name())
    }
}

pub fn run(is_check: bool) -> Result<Outcome> {
    let current = env!("CARGO_PKG_VERSION").to_string();
    let Some(release) = latest_release()? else {
        if is_check {
            return Ok(Outcome::NoRelease(current));
        }
        let path = build_from_source()?;
        restart_bridge();
        return Ok(Outcome::BuiltFromSource { path });
    };
    let latest = release.tag_name.trim_start_matches('v').to_string();
    if !is_newer(&latest, &current) {
        return Ok(Outcome::UpToDate(current));
    }
    if is_check {
        return Ok(Outcome::Available { current, latest });
    }
    let current_exe = env::current_exe()?
        .canonicalize()
        .context("could not resolve annex's own path")?;
    install_release(&release, &latest, Program::Cli, &current_exe)?;
    // The Manager comes along when it's installed already, or when this is a desktop that
    // could use it; a release without one leaves things as they are.
    let manager = current_exe.with_file_name(Program::Manager.file_name());
    if manager.is_file() || crate::desktop::has_session() {
        if let Err(error) = install_release(&release, &latest, Program::Manager, &manager) {
            eprintln!("warning: annex updated, but Annex Manager didn't: {error:#}");
        } else if let Err(error) = crate::launcher::install_manager_entry() {
            eprintln!("warning: could not add Annex Manager to the app launcher: {error:#}");
        }
    }
    restart_bridge();
    Ok(Outcome::Updated {
        from: current,
        to: latest,
    })
}

/// The latest published release, or None while the repository has none.
fn latest_release() -> Result<Option<Release>> {
    fetch_release(&format!("{RELEASES}/latest"))
}

/// Installs Annex Manager from the release matching this annex's version, next to this annex.
/// Returns false when that release has no Manager (or there is no such release).
pub fn install_manager() -> Result<bool> {
    let version = env!("CARGO_PKG_VERSION");
    let Some(release) = fetch_release(&format!("{RELEASES}/tags/v{version}"))? else {
        return Ok(false);
    };
    let archive = Program::Manager.archive_name(version);
    if !release.assets.iter().any(|asset| asset.name == archive) {
        return Ok(false);
    }
    let target = env::current_exe()?
        .canonicalize()
        .context("could not resolve annex's own path")?
        .with_file_name(Program::Manager.file_name());
    install_release(&release, version, Program::Manager, &target)?;
    Ok(true)
}

/// Whether the release matching this version ships the Manager. Network or API trouble counts
/// as no, so setup simply skips the step.
pub fn is_manager_published() -> bool {
    let version = env!("CARGO_PKG_VERSION");
    let archive = Program::Manager.archive_name(version);
    fetch_release(&format!("{RELEASES}/tags/v{version}"))
        .ok()
        .flatten()
        .is_some_and(|release| release.assets.iter().any(|asset| asset.name == archive))
}

fn fetch_release(url: &str) -> Result<Option<Release>> {
    let output = output_within(
        Command::new("curl").args([
            "--silent",
            "--show-error",
            "--location",
            "--header",
            "Accept: application/vnd.github+json",
            "--write-out",
            "\n%{http_code}",
            url,
        ]),
        API_TIMEOUT,
    )
    .context("could not run curl, which `annex update` needs")?;
    let text = String::from_utf8_lossy(&output.stdout);
    let (body, status) = text.rsplit_once('\n').unwrap_or(("", text.trim()));
    match status.trim() {
        "200" => Ok(Some(
            serde_json::from_str(body).context("unexpected reply from GitHub")?,
        )),
        "404" => Ok(None),
        other => bail!(
            "could not check for updates (GitHub answered {other}): {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ),
    }
}

/// Downloads, verifies and unpacks `program` from `release`, then puts it at `target`.
fn install_release(
    release: &Release,
    version: &str,
    program: Program,
    target: &Path,
) -> Result<()> {
    if env::consts::ARCH != "x86_64" {
        bail!(
            "releases only ship an x86_64 binary; build from source with `cargo install --git {SOURCE} --locked --force`"
        );
    }
    let archive_name = program.archive_name(version);
    let asset = |name: &str| {
        release
            .assets
            .iter()
            .find(|asset| asset.name == name)
            .with_context(|| format!("release {} has no {name}", release.tag_name))
    };
    let archive = asset(&archive_name)?;
    let checksum = asset(&format!("{archive_name}.sha256"))?;

    let directory = target
        .parent()
        .map(|folder| folder.join(format!(".annex-update-{version}")))
        .filter(|folder| fs::create_dir_all(folder).is_ok())
        .unwrap_or_else(|| data_dir().join("updates").join(version));
    fs::create_dir_all(&directory)?;
    let archive_path = directory.join(&archive.name);
    let checksum_path = directory.join(&checksum.name);
    println!("downloading {} {version}", program.file_name());
    download(&archive.browser_download_url, &archive_path)?;
    download(&checksum.browser_download_url, &checksum_path)?;
    let expected = parse_checksum(&fs::read_to_string(&checksum_path)?)
        .context("the release's checksum file is malformed")?;
    let actual = sha256(&archive_path)?;
    if actual != expected {
        bail!("the download's SHA-256 doesn't match the release's; nothing was changed");
    }
    stdout_within(
        Command::new("tar")
            .arg("-xzf")
            .arg(&archive_path)
            .arg("-C")
            .arg(&directory),
        DOWNLOAD_TIMEOUT,
    )?;
    let binary = directory
        .join(archive_name.trim_end_matches(".tar.gz"))
        .join(program.file_name());
    let replaced = replace(&binary, target, program);
    let _ = fs::remove_dir_all(&directory);
    replaced
}

/// Swaps `target` for `new` with a rename in the same folder, which is atomic and safe while a
/// process still runs the old file.
fn replace(new: &Path, target: &Path, program: Program) -> Result<()> {
    let staged = target.with_file_name(".annex-staged");
    let copied = fs::copy(new, &staged)
        .and_then(|_| fs::set_permissions(&staged, Permissions::from_mode(0o755)))
        .and_then(|_| fs::rename(&staged, target));
    if let Err(error) = copied {
        let _ = fs::remove_file(&staged);
        let folder = target.parent().unwrap_or(target).display();
        bail!(
            "could not replace {} ({error}): annex needs write access to {folder}; run `sudo annex update`, or reinstall it to a folder you own",
            target.display()
        );
    }
    // The Manager would open its window rather than print a version, so only the CLI is run.
    if program == Program::Cli {
        stdout_within(Command::new(target).arg("--version"), API_TIMEOUT)
            .context("the new annex binary doesn't run")?;
    }
    Ok(())
}

/// Without a release, the newest code is on the default branch: cargo builds and installs it.
fn build_from_source() -> Result<PathBuf> {
    if which("cargo").is_none() {
        bail!(
            "no release is published yet, and building from source needs Rust: install it from https://rustup.rs"
        );
    }
    println!("no release is published yet; building the latest source with cargo");
    let status = Command::new("cargo")
        .args(["install", "--git", SOURCE, "--locked", "--force"])
        .status()
        .context("could not run cargo")?;
    if !status.success() {
        bail!("cargo could not build annex ({status})");
    }
    let installed = env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")))
        .context("HOME is not set")?
        .join("bin/annex");
    Ok(installed)
}

fn download(url: &str, path: &Path) -> Result<()> {
    stdout_within(
        Command::new("curl")
            .args([
                "--fail",
                "--silent",
                "--show-error",
                "--location",
                "--output",
            ])
            .arg(path)
            .arg(url),
        DOWNLOAD_TIMEOUT,
    )
    .with_context(|| format!("could not download {url}"))
    .map(drop)
}

fn sha256(path: &Path) -> Result<String> {
    let output = stdout_within(Command::new("sha256sum").arg(path), DOWNLOAD_TIMEOUT)?;
    parse_checksum(&output).context("unexpected sha256sum output")
}

/// The hash from a `sha256sum`-style line: `<64 hex digits>  <file name>`.
fn parse_checksum(text: &str) -> Option<String> {
    let hash = text.split_whitespace().next()?;
    (hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit())).then(|| hash.to_lowercase())
}

/// The bridge keeps running the old binary until restarted.
fn restart_bridge() {
    if bridge::service::state() == "active" && bridge::service::restart().is_err() {
        eprintln!("warning: restart the bridge to use the new version: annex bridge install");
    }
}

fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(candidate), Some(current)) => candidate > current,
        _ => false,
    }
}

/// Major, minor and patch, ignoring any pre-release or build suffix.
fn parse_version(version: &str) -> Option<(u64, u64, u64)> {
    let core = version.trim_start_matches('v').split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|part| part.parse::<u64>().ok());
    Some((parts.next()??, parts.next()??, parts.next()??))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions_numerically() {
        assert!(is_newer("0.10.0", "0.9.9"));
        assert!(is_newer("v1.0.0", "0.9.0"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.2.0"));
        assert!(!is_newer("garbage", "0.1.0"));
    }

    #[test]
    fn ignores_pre_release_suffixes() {
        assert_eq!(parse_version("1.2.3-rc.1"), Some((1, 2, 3)));
        assert_eq!(parse_version("v1.2.3+build"), Some((1, 2, 3)));
        assert_eq!(parse_version("1.2"), None);
    }

    #[test]
    fn reads_sha256sum_lines() {
        let line = format!("{}  annex-0.2.0-x86_64-linux.tar.gz\n", "AB".repeat(32));
        assert_eq!(parse_checksum(&line), Some("ab".repeat(32)));
        assert_eq!(parse_checksum("not-a-hash  file"), None);
        assert_eq!(parse_checksum(""), None);
    }

    /// Builds a release the way the release workflow does, served from file:// URLs.
    fn fake_release(root: &Path, version: &str, binary: &str) -> Release {
        let folder_name = Program::Cli
            .archive_name(version)
            .trim_end_matches(".tar.gz")
            .to_string();
        let folder = root.join(&folder_name);
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("annex"), binary).unwrap();
        fs::set_permissions(folder.join("annex"), Permissions::from_mode(0o755)).unwrap();
        let archive = root.join(Program::Cli.archive_name(version));
        let packed = Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(root)
            .arg(&folder_name)
            .status()
            .unwrap();
        assert!(packed.success());
        let checksum = root.join(format!("{}.sha256", Program::Cli.archive_name(version)));
        fs::write(&checksum, format!("{}  x\n", sha256(&archive).unwrap())).unwrap();
        let asset = |path: &Path| Asset {
            name: path.file_name().unwrap().to_string_lossy().into_owned(),
            browser_download_url: format!("file://{}", path.display()),
        };
        Release {
            tag_name: format!("v{version}"),
            assets: vec![asset(&archive), asset(&checksum)],
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let folder =
            env::temp_dir().join(format!("annex-update-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&folder);
        fs::create_dir_all(&folder).unwrap();
        folder
    }

    #[test]
    fn installs_a_verified_release_over_the_target() {
        let root = scratch("ok");
        let release = fake_release(&root, "9.9.9", "#!/bin/sh\necho annex 9.9.9\n");
        let target = root.join("bin/annex");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, "old").unwrap();
        install_release(&release, "9.9.9", Program::Cli, &target).unwrap();
        assert_eq!(
            fs::read_to_string(&target).unwrap(),
            "#!/bin/sh\necho annex 9.9.9\n"
        );
        assert!(!root.join("bin/.annex-update-9.9.9").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn refuses_a_download_whose_checksum_differs() {
        let root = scratch("tampered");
        let release = fake_release(&root, "9.9.9", "#!/bin/sh\necho annex\n");
        fs::write(root.join(Program::Cli.archive_name("9.9.9")), "tampered").unwrap();
        let target = root.join("annex");
        fs::write(&target, "old").unwrap();
        let error = install_release(&release, "9.9.9", Program::Cli, &target).unwrap_err();
        assert!(error.to_string().contains("SHA-256"));
        assert_eq!(fs::read_to_string(&target).unwrap(), "old");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn matches_the_release_workflow_archive_names() {
        assert_eq!(
            Program::Cli.archive_name("0.2.0"),
            "annex-0.2.0-x86_64-linux.tar.gz"
        );
        assert_eq!(
            Program::Manager.archive_name("0.2.0"),
            "annex-manager-0.2.0-x86_64-linux.tar.gz"
        );
    }
}
