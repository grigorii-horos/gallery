//! Where Gallery keeps its own files. In debug builds `$GALLERY_DATA_DIR`
//! (when set and not empty) redirects all of them: data under it, the AI
//! cache and the pending index under `<dir>/cache`. Release builds ignore the
//! variable, so the CLI and the GUI always agree on the real places.
//!
//! Test runs (the `test-paths` feature, on for the tests of every crate here)
//! never touch the real places: without `$GALLERY_DATA_DIR`, everything goes
//! to a throwaway folder of the process under the build's target dir.

use std::path::{Path, PathBuf};

/// `$GALLERY_DATA_DIR`, debug builds only; in test runs, a folder of the
/// process when it is not set.
#[cfg(debug_assertions)]
pub fn data_override() -> Option<PathBuf> {
    let var = std::env::var_os("GALLERY_DATA_DIR")
        .filter(|v| !v.is_empty())
        .map(|v| crate::config::expand_tilde(Path::new(&v)));
    #[cfg(any(test, feature = "test-paths"))]
    let var = var.or_else(|| Some(test_root().to_path_buf()));
    var
}

/// `<target dir>/test-state/<pid>`: not tmpfs, and the folders of earlier
/// runs (older than a day) are removed when a new one is made.
#[cfg(all(debug_assertions, any(test, feature = "test-paths")))]
fn test_root() -> &'static Path {
    static ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    ROOT.get_or_init(|| {
        let exe = std::env::current_exe().unwrap_or_default();
        // target/debug/deps/<test binary>: the target dir is three levels up.
        let target = exe.ancestors().nth(3).unwrap_or(Path::new("."));
        let all = target.join("test-state");
        let day = std::time::Duration::from_secs(24 * 3600);
        for old in std::fs::read_dir(&all).into_iter().flatten().flatten() {
            let aged = old
                .metadata()
                .and_then(|m| m.modified())
                .is_ok_and(|t| t.elapsed().is_ok_and(|e| e > day));
            if aged {
                let _ = std::fs::remove_dir_all(old.path());
            }
        }
        let own = all.join(std::process::id().to_string());
        let _ = std::fs::create_dir_all(&own);
        own
    })
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

    /// Without `$GALLERY_DATA_DIR`, a test run's folders are under the
    /// target dir, not the user's real ones.
    #[test]
    fn test_runs_keep_off_the_real_dirs() {
        if std::env::var_os("GALLERY_DATA_DIR").is_some_and(|v| !v.is_empty()) {
            return;
        }
        let exe = std::env::current_exe().unwrap();
        let target = exe.ancestors().nth(3).unwrap();
        for p in [data_dir().unwrap(), cache_dir().unwrap()] {
            assert!(p.starts_with(target.join("test-state")), "{p:?}");
        }
        assert_ne!(cache_dir(), dirs::cache_dir().map(|d| d.join("gallery")));
    }

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
