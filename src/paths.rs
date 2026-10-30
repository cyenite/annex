use std::fs::{self, Permissions};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::{env, io};

pub fn which(program: &str) -> Option<PathBuf> {
    env::split_paths(&env::var_os("PATH")?)
        .map(|directory| directory.join(program))
        .find(|candidate| candidate.is_file())
}

pub fn home() -> Option<PathBuf> {
    env::var_os("HOME").map(PathBuf::from)
}

pub fn data_home() -> PathBuf {
    env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| home().map(|home| home.join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn config_home() -> PathBuf {
    env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| home().map(|home| home.join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn data_dir() -> PathBuf {
    data_home().join("annex")
}

pub fn log_dir() -> PathBuf {
    data_dir().join("logs")
}

/// For files mounted into instances: a running instance keeps a mounted binary busy, so a changed
/// file is replaced by renaming a new one over it, never rewritten in place.
pub fn write_if_changed(path: &Path, content: &[u8], mode: u32) -> io::Result<()> {
    if fs::read(path).is_ok_and(|current| current == content) {
        return Ok(());
    }
    let folder = path.parent().expect("mounted file has a parent");
    fs::create_dir_all(folder)?;
    let name = path.file_name().expect("mounted file has a name");
    let staged = folder.join(format!(".{}.new", name.to_string_lossy()));
    fs::write(&staged, content)?;
    fs::set_permissions(&staged, Permissions::from_mode(mode))?;
    fs::rename(&staged, path)
}
