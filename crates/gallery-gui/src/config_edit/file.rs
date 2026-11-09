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
    numbered_backup(path, 0)
}

/// `gallery.toml.bak` (`n = 0`), `gallery.toml.bak.1`, ...
fn numbered_backup(path: &Path, n: usize) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".bak");
    if n > 0 {
        name.push(format!(".{n}"));
    }
    path.with_file_name(name)
}

/// The file at `p` is a config in the old three-stage shape.
fn holds_old_shape(p: &Path) -> bool {
    std::fs::read_to_string(p)
        .ok()
        .and_then(|t| t.parse::<toml_edit::DocumentMut>().ok())
        .is_some_and(|d| super::migrate::is_old_shape(&d))
}

/// How the previous text is kept when the config is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backup {
    /// In `.bak`, replaced by the next save — unless it holds an old-shape
    /// config (the copy made before the migration): then in the first
    /// `.bak.N` that does not.
    Rolling,
    /// In the first of `.bak`, `.bak.1`, ... that does not exist; never
    /// replaced (the original of an old-shape file).
    Keep,
}

/// Copy `real` to its backup; the path written.
fn back_up(real: &Path, how: Backup) -> std::io::Result<PathBuf> {
    for n in 0.. {
        let bak = numbered_backup(real, n);
        match how {
            Backup::Rolling if bak.exists() && holds_old_shape(&bak) => continue,
            Backup::Rolling => {
                std::fs::copy(real, &bak)?;
                return Ok(bak);
            }
            Backup::Keep => {
                // Created exclusively: an existing backup is never replaced.
                let out = match std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&bak)
                {
                    Ok(f) => f,
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(e) => return Err(e),
                };
                fill_or_remove(&bak, out, |f| f.write_all(&std::fs::read(real)?))?;
                return Ok(bak);
            }
        }
    }
    unreachable!("a free backup name")
}

/// Write the freshly created backup `bak` through `fill` and sync it; on
/// failure the partial file is removed, so a broken copy never takes a
/// backup name that is then never replaced.
fn fill_or_remove(
    bak: &Path,
    mut out: std::fs::File,
    fill: impl FnOnce(&mut std::fs::File) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let r = fill(&mut out).and_then(|()| out.sync_all());
    // Closed first: Windows removes an open file only once it is closed.
    drop(out);
    if r.is_err() {
        let _ = std::fs::remove_file(bak);
    }
    r
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
/// temporary file in the same folder, the old one is copied to its backup
/// (see [`Backup`]), then the temporary file is renamed over it.
/// Permissions are kept. Returns the backup's path.
pub fn write_atomic(
    path: &Path,
    expected: &str,
    text: &str,
    how: Backup,
) -> Result<PathBuf, WriteError> {
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
    let bak = back_up(&real, how)?;
    // Last look right before the swap: an outside save in between wins.
    check()?;
    tmp.persist(&real).map_err(|e| WriteError::Io(e.error))?;
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(bak)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_keep_backup_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let bak = dir.path().join("gallery.toml.bak");
        let out = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&bak)
            .unwrap();
        let err = fill_or_remove(&bak, out, |f| {
            f.write_all(b"half")?;
            Err(std::io::Error::other("disk full"))
        })
        .unwrap_err();
        assert_eq!(err.to_string(), "disk full");
        assert!(!bak.exists());

        // A good fill keeps the file.
        let out = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&bak)
            .unwrap();
        fill_or_remove(&bak, out, |f| f.write_all(b"whole")).unwrap();
        assert_eq!(std::fs::read(&bak).unwrap(), b"whole");
    }
}
