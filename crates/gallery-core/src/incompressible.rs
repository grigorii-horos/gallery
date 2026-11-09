//! Files a content pipe could not make any smaller.
//!
//! When an in-place output's pipe fell back to the source bytes (the encoder
//! result was not smaller), apply leaves the source as it is and records it
//! here; planning then keeps such a file where it is instead of encoding it
//! again on every scan. An entry holds only for the exact file (size and
//! mtime) and the exact pipe (preset names and their settings): a changed
//! file or a changed preset chain is encoded again.
//!
//! The store is a small JSON file in [`crate::paths::cache_dir`], written
//! atomically. Losing it only costs one more encode per file.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::config::ContentPreset;
use crate::scan::mtime_ns;

const FILE: &str = "incompressible.json";
const VERSION: u32 = 1;

/// Read-modify-write of the store within this process.
static LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    size: u64,
    mtime_ns: i128,
    /// [`fingerprint`] of the pipe that could not make the file smaller.
    pipe: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Store {
    version: u32,
    /// Canonical path -> entry.
    #[serde(default)]
    files: BTreeMap<String, Entry>,
}

/// Fingerprint of a content pipe: the preset names in order and their full
/// settings. `None` when a preset is unknown (nothing is recorded then).
pub fn fingerprint<'a>(
    steps: &[String],
    preset: impl Fn(&str) -> Option<&'a ContentPreset>,
) -> Option<String> {
    let mut chain = Vec::with_capacity(steps.len());
    for s in steps {
        chain.push((s.as_str(), preset(s)?));
    }
    let json = serde_json::to_vec(&chain).ok()?;
    Some(blake3::hash(&json).to_hex().to_string())
}

fn store_path() -> Option<PathBuf> {
    crate::paths::cache_dir().map(|d| d.join(FILE))
}

/// Size and mtime of the store file, `None` when there is none: a quick
/// answer that holds while the store is the same (see `pending`).
pub fn stamp() -> Option<(u64, i128)> {
    let meta = std::fs::metadata(store_path()?).ok()?;
    Some((meta.len(), mtime_ns(&meta).ok()?))
}

fn key(path: &Path) -> Option<String> {
    std::fs::canonicalize(path)
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

fn load(path: &Path) -> Store {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Store>(&b).ok())
        .filter(|s| s.version == VERSION)
        .unwrap_or_default()
}

/// The known incompressible files, loaded once for a planning pass.
#[derive(Debug, Default)]
pub struct Known {
    files: BTreeMap<String, Entry>,
}

impl Known {
    /// The store as it is now (empty when absent or unreadable).
    pub fn load() -> Known {
        let files = store_path().map(|p| load(&p).files).unwrap_or_default();
        Known { files }
    }

    /// Whether `path`, of `size` and `mtime_ns`, is known to come out no
    /// smaller from the pipe of fingerprint `pipe`.
    pub fn contains(&self, path: &Path, size: u64, mtime_ns: i128, pipe: &str) -> bool {
        if self.files.is_empty() {
            return false;
        }
        let Some(k) = key(path) else {
            return false;
        };
        self.files
            .get(&k)
            .is_some_and(|e| e.size == size && e.mtime_ns == mtime_ns && e.pipe == pipe)
    }
}

/// Record that the pipe of fingerprint `pipe` could not make the file at
/// `path` (as it is on disk now) any smaller. Entries of files that are gone
/// or changed are dropped on the way. Best effort: a failure only means the
/// file is encoded again next time.
pub fn remember(path: &Path, pipe: &str) {
    let (Some(store), Some(k)) = (store_path(), key(path)) else {
        return;
    };
    let Ok(meta) = std::fs::metadata(path) else {
        return;
    };
    let Ok(mtime) = mtime_ns(&meta) else {
        return;
    };
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut s = load(&store);
    s.version = VERSION;
    s.files.retain(|p, e| {
        std::fs::metadata(p)
            .ok()
            .is_some_and(|m| m.len() == e.size && mtime_ns(&m).is_ok_and(|t| t == e.mtime_ns))
    });
    s.files.insert(
        k,
        Entry {
            size: meta.len(),
            mtime_ns: mtime,
            pipe: pipe.to_string(),
        },
    );
    if let Some(dir) = store.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(bytes) = serde_json::to_vec_pretty(&s) {
        let _ = crate::paths::write_atomic(&store, &bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{MetadataPreset, PhotoPreset};

    #[test]
    fn fingerprint_follows_names_and_settings() {
        let a = ContentPreset::Photo(PhotoPreset::avif_high());
        let b = ContentPreset::Photo(PhotoPreset::avif_low());
        let m = ContentPreset::Metadata(MetadataPreset::full());
        let fp = |steps: &[&str], p: &ContentPreset| {
            let steps: Vec<String> = steps.iter().map(|s| s.to_string()).collect();
            fingerprint(&steps, |n| if n == "M" { Some(&m) } else { Some(p) })
        };
        assert_eq!(fp(&["X"], &a), fp(&["X"], &a));
        assert_ne!(fp(&["X"], &a), fp(&["X"], &b));
        assert_ne!(fp(&["X"], &a), fp(&["Y"], &a));
        assert_ne!(fp(&["X"], &a), fp(&["X", "M"], &a));
        assert_eq!(fingerprint(&["X".to_string()], |_| None), None);
    }

    #[test]
    fn remembered_file_is_known_until_it_changes() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("shot.png");
        std::fs::write(&f, b"pixels").unwrap();
        let meta = std::fs::metadata(&f).unwrap();
        let t = mtime_ns(&meta).unwrap();
        assert!(!Known::load().contains(&f, 6, t, "p"));
        remember(&f, "p");
        let k = Known::load();
        assert!(k.contains(&f, 6, t, "p"));
        assert!(!k.contains(&f, 6, t, "q"));
        assert!(!k.contains(&f, 7, t, "p"));
        assert!(!k.contains(&f, 6, t + 1, "p"));
        // Another name of the same folder still finds it (canonical key).
        assert!(k.contains(&d.path().join(".").join("shot.png"), 6, t, "p"));
    }
}
