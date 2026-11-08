//! Where Gallery keeps its own files. In debug builds `$GALLERY_DATA_DIR`
//! (when set and not empty) redirects all of them: data under it, the AI
//! cache and the pending index under `<dir>/cache`. Release builds ignore the
//! variable, so the CLI and the GUI always agree on the real places.

use std::path::{Path, PathBuf};

/// `$GALLERY_DATA_DIR`, debug builds only.
#[cfg(debug_assertions)]
pub fn data_override() -> Option<PathBuf> {
    std::env::var_os("GALLERY_DATA_DIR")
        .filter(|v| !v.is_empty())
        .map(|v| crate::config::expand_tilde(Path::new(&v)))
}

#[cfg(not(debug_assertions))]
pub fn data_override() -> Option<PathBuf> {
    None
}

/// `<user data dir>/gallery` (or the override).
pub fn data_dir() -> Option<PathBuf> {
    data_override().or_else(|| dirs::data_dir().map(|d| d.join("gallery")))
}

/// `<user cache dir>/gallery` (or `<override>/cache`).
pub fn cache_dir() -> Option<PathBuf> {
    match data_override() {
        Some(d) => Some(d.join("cache")),
        None => dirs::cache_dir().map(|d| d.join("gallery")),
    }
}

/// Write `bytes` to `path` through a uniquely named temp file in the same
/// folder, so a reader never sees half and two writers never share a temp.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    std::io::Write::write_all(&mut tmp, bytes)?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_replaces_and_leaves_no_temp() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.json");
        write_atomic(&p, b"one").unwrap();
        write_atomic(&p, b"two").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"two");
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 1);
    }
}
