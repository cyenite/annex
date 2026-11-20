//! Google Play services for container-mode instances. Redroid has none, so apps that link Play
//! services libraries lose maps, push messages and fused location. annex stands in for it with
//! microG mounted as privileged system apps (`gmscore`, which also adds offline push tokens), and
//! patches the image's framework so client libraries accept it as Google's (`framework`). `push`
//! serves `annex fcm`. Patching runs in a throwaway JRE container (`run_java`), so the host needs
//! no Java; everything it makes is cached under `~/.local/share/annex/play-services/`.

mod framework;
mod gmscore;
pub mod push;
mod smali;

use std::fs::{self, Permissions};
use std::iter;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use clap::ValueEnum;

use crate::instance::AndroidVersion;
use crate::paths::data_dir;
use crate::update::{download, sha256};
use crate::{docker, redroid};

const BUILDER: &str = "eclipse-temurin:21-jre-alpine@sha256:ac05c3c42b7c2f1a52c9bd52f01041fa1958231b15ebeae7420429cbc5baf474";
const APKTOOL: Tool = Tool {
    file: "apktool-3.0.3.jar",
    url: "https://github.com/iBotPeaches/Apktool/releases/download/v3.0.3/apktool_3.0.3.jar",
    sha256: "dbf930b076c6b9be08d57c449cacefc3bdd6b71ebd59b3066fc0e1f5b14f9423",
};

/// Whether an instance has Google Play services, as a container label. Containers from before
/// the label have none.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum PlayServices {
    /// No Google Play services, like a phone sold without them
    None,
    /// An open-source stand-in for maps, push messages and fused location
    StandIn,
}

impl PlayServices {
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::StandIn => "stand-in",
        }
    }

    pub fn from_label(label: &str) -> Option<Self> {
        Self::value_variants()
            .iter()
            .copied()
            .find(|value| value.label() == label)
    }

    /// What a new instance gets unless asked otherwise: the stand-in wherever it works.
    pub fn default_for(android: AndroidVersion) -> Self {
        if is_supported(android) {
            Self::StandIn
        } else {
            Self::None
        }
    }
}

/// The framework patch is only verified against Android 15's image.
pub fn is_supported(android: AndroidVersion) -> bool {
    android == AndroidVersion::Android15
}

/// The `docker create` mounts that add the stand-in: its APKs, and the patched framework in place
/// of the image's. The framework's precompiled code is masked, or ART would find it stale and
/// discard it at every boot anyway.
pub fn mounts(android: AndroidVersion) -> Result<Vec<String>> {
    let apks = gmscore::patched()?;
    let framework = framework::patched(android)?;
    let mut mounts = vec![bind(&framework, framework::TARGET)];
    mounts.extend(framework::PRECOMPILED.map(redroid::mask));
    for (path, apk) in apks.iter().zip(&gmscore::APKS) {
        mounts.push(bind(path, apk.target));
    }
    Ok(mounts)
}

/// A jar a build step downloads into the tools folder, pinned by SHA-256.
struct Tool {
    url: &'static str,
    file: &'static str,
    sha256: &'static str,
}

impl Tool {
    /// The jar's path inside the build container.
    fn inside(&self) -> String {
        format!("/tools/{}", self.file)
    }
}

fn root() -> PathBuf {
    data_dir().join("play-services")
}

fn bind(source: &Path, target: &str) -> String {
    format!("type=bind,src={},dst={target},readonly", source.display())
}

/// Downloads `url` to `path` unless it's already there, through a staged file that only takes
/// the final name once its SHA-256 matches.
fn fetch(url: &str, expected: &str, path: &Path) -> Result<()> {
    if path.is_file() {
        return Ok(());
    }
    let folder = path.parent().expect("a download has a folder");
    fs::create_dir_all(folder)?;
    let name = path.file_name().expect("a download has a name");
    let staged = folder.join(format!(".{}.part", name.to_string_lossy()));
    download(url, &staged)?;
    let actual = sha256(&staged)?;
    if actual != expected {
        let _ = fs::remove_file(&staged);
        bail!("{url} has SHA-256 {actual}, expected {expected}");
    }
    keep(&staged, path)
}

/// Runs `command` in a throwaway JRE container as the user, so everything it writes can be
/// removed afterwards. The tools folder is at /tools, with `tools` downloaded into it first, and
/// each of `volumes` is mounted as `HOST:INSIDE[:ro]`.
fn run_java(tools: &[&Tool], volumes: &[(&Path, &str)], command: &[&str]) -> Result<()> {
    let folder = root().join("tools");
    for tool in tools {
        fetch(tool.url, tool.sha256, &folder.join(tool.file))?;
    }
    fs::create_dir_all(&folder)?;
    let owner = fs::metadata(&folder)?;
    let user = format!("{}:{}", owner.uid(), owner.gid());
    let mounts: Vec<String> = iter::once((folder.as_path(), "/tools"))
        .chain(volumes.iter().copied())
        .map(|(host, inside)| format!("{}:{inside}", host.display()))
        .collect();
    let mut args = vec!["run", "--rm", "--user", &user, "--env", "HOME=/tmp"];
    for mount in &mounts {
        args.extend(["--volume", mount]);
    }
    args.push(BUILDER);
    args.extend(command);
    docker::run(&args).map(drop)
}

/// Moves a built file into the cache in one rename, so an instance never mounts half a file.
fn keep(built: &Path, cached: &Path) -> Result<()> {
    fs::create_dir_all(cached.parent().expect("a cached file has a folder"))?;
    fs::set_permissions(built, Permissions::from_mode(0o644))?;
    fs::rename(built, cached)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_round_trip() {
        for value in PlayServices::value_variants() {
            assert_eq!(PlayServices::from_label(value.label()), Some(*value));
        }
        assert_eq!(PlayServices::from_label("gapps"), None);
    }

    #[test]
    fn new_instances_get_the_stand_in_where_it_works() {
        assert_eq!(
            PlayServices::default_for(AndroidVersion::Android15),
            PlayServices::StandIn
        );
        for android in [AndroidVersion::Android13, AndroidVersion::Android14] {
            assert_eq!(PlayServices::default_for(android), PlayServices::None);
        }
    }

    #[test]
    fn the_framework_trusts_exactly_the_folders_annex_mounts() {
        for apk in &gmscore::APKS {
            let folder = apk.target.rsplit_once('/').unwrap().0;
            assert!(
                framework::HELPER.contains(&format!("\"{folder}\"")),
                "{folder}"
            );
        }
    }
}
