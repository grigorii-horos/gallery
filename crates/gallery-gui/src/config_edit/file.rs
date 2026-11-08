//! Reading the config with a version stamp, and writing it atomically with
//! a `.bak` of the previous text.

use std::io::Write;
use std::path::{Path, PathBuf};

/// Stamp of a config's bytes: an edit is saved only over the text it was
/// made from.
pub fn version_of(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

pub fn read(path: &Path) -> std::io::Result<(String, String)> {
    let bytes = std::fs::read(path)?;
    let version = version_of(&bytes);
    let text = String::from_utf8(bytes)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    Ok((text, version))
}

/// The backup of `path`: `gallery.toml.bak` next to it.
pub fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".bak");
    path.with_file_name(name)
}

/// The file was changed since `expected` was read.
#[derive(Debug)]
pub enum WriteError {
    Changed,
    Io(std::io::Error),
}

impl From<std::io::Error> for WriteError {
    fn from(e: std::io::Error) -> Self {
        WriteError::Io(e)
    }
}

/// Replace the config at `path` (through a symlink: its target) with
/// `text`, if it still has version `expected`: the new text goes to a
/// temporary file in the same folder, the old one is copied to `.bak`, then
/// the temporary file is renamed over it. Permissions are kept.
pub fn write_atomic(path: &Path, expected: &str, text: &str) -> Result<(), WriteError> {
    let real = std::fs::canonicalize(path)?;
    let dir = real.parent().unwrap_or(Path::new("."));
    let check = || -> Result<(), WriteError> {
        match std::fs::read(&real) {
            Ok(b) if version_of(&b) == expected => Ok(()),
            Ok(_) => Err(WriteError::Changed),
            Err(e) => Err(e.into()),
        }
    };
    check()?;
    let perms = std::fs::metadata(&real)?.permissions();
    let mut tmp = tempfile::Builder::new()
        .prefix(".gallery-config-")
        .suffix(".tmp")
        .tempfile_in(dir)?;
    tmp.write_all(text.as_bytes())?;
    tmp.as_file().sync_all()?;
    std::fs::set_permissions(tmp.path(), perms)?;
    let bak = backup_path(&real);
    std::fs::copy(&real, &bak)?;
    // Last look right before the swap: an outside save in between wins.
    check()?;
    tmp.persist(&real).map_err(|e| WriteError::Io(e.error))?;
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}
