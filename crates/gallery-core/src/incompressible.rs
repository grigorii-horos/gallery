//! Files a content pipe could not make any smaller.
//!
//! When an in-place output's pipe fell back to the source bytes (a same-format
//! recompression, such as avif to avif, that came out no smaller; a pipe that
//! changes the format always produces its target and never lands here), apply
//! leaves the source as it is and records it here; planning then keeps such a file where it is instead of encoding it
//! again on every scan. An entry holds only for the exact file (size and
//! mtime) and the exact pipe (preset names and their settings): a changed
//! file or a changed preset chain is encoded again.
//!
//! The store is a small JSON file in [`crate::paths::cache_dir`], written
//! atomically. Losing it only costs one more encode per file.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

use crate::config::ContentPreset;
use crate::scan::mtime_ns;
use crate::tools::Tools;

const FILE: &str = "incompressible.json";
const VERSION: u32 = 1;

/// Version of the rule that decides which pipes can be remembered. Part of
/// every fingerprint: 2 is "a conversion always converts" (only same-format
/// recompression can be kept for not shrinking), so entries recorded under
/// the older rule for pipes that change the format no longer match.
const POLICY: u32 = 2;

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

/// Fingerprint of a content pipe: the preset names in order, their full
/// settings and the encoder versions (an upgrade tries again). `None` when
/// a preset is unknown (nothing is recorded then).
pub fn fingerprint<'a>(
    steps: &[String],
    preset: impl Fn(&str) -> Option<&'a ContentPreset>,
    versions: &BTreeMap<String, String>,
) -> Option<String> {
    let mut chain = Vec::with_capacity(steps.len());
    for s in steps {
        chain.push((s.as_str(), preset(s)?));
    }
    let json = serde_json::to_vec(&(POLICY, chain, versions)).ok()?;
    Some(blake3::hash(&json).to_hex().to_string())
}

/// The encoder versions of this machine's tools ([`Tools::encoder_versions`]);
/// empty when the tools are not all found.
pub fn current_versions() -> BTreeMap<String, String> {
    Tools::discover()
        .map(|t| t.encoder_versions())
        .unwrap_or_default()
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

/// The entry `e` of file `p` is of no use any more: the file changed
/// (another size or mtime), or it is gone from a folder that is still
/// there. A file whose folder is missing too (an SD card or a share not
/// mounted now) keeps its entry for when it is back; so does a file that
/// cannot be read now.
fn stale(p: &Path, e: &Entry) -> bool {
    match std::fs::metadata(p) {
        Ok(m) => m.len() != e.size || mtime_ns(&m).map_or(true, |t| t != e.mtime_ns),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            p.parent().is_some_and(Path::is_dir)
        }
        Err(_) => false,
    }
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
    /// [`current_versions`], probed only once an entry matches.
    versions: OnceLock<BTreeMap<String, String>>,
}

impl Known {
    /// The store as it is now (empty when absent or unreadable).
    pub fn load() -> Known {
        let files = store_path().map(|p| load(&p).files).unwrap_or_default();
        Known {
            files,
            versions: OnceLock::new(),
        }
    }

    /// Whether `path`, of `size` and `mtime_ns`, is known to come out no
    /// smaller from the pipe `steps` (presets from `preset`) with this
    /// machine's encoders.
    pub fn keeps<'a>(
        &self,
        path: &Path,
        size: u64,
        mtime_ns: i128,
        steps: &[String],
        preset: impl Fn(&str) -> Option<&'a ContentPreset>,
    ) -> bool {
        let Some(e) = self
            .entry(path)
            .filter(|e| e.size == size && e.mtime_ns == mtime_ns)
        else {
            return false;
        };
        let versions = self.versions.get_or_init(current_versions);
        fingerprint(steps, preset, versions).is_some_and(|fp| fp == e.pipe)
    }

    fn entry(&self, path: &Path) -> Option<&Entry> {
        if self.files.is_empty() {
            return None;
        }
        self.files.get(&key(path)?)
    }

    /// Whether `path`, of `size` and `mtime_ns`, is known to come out no
    /// smaller from the pipe of fingerprint `pipe`.
    pub fn contains(&self, path: &Path, size: u64, mtime_ns: i128, pipe: &str) -> bool {
        self.entry(path)
            .is_some_and(|e| e.size == size && e.mtime_ns == mtime_ns && e.pipe == pipe)
    }
}

/// A file the pipe of fingerprint `pipe` could not make any smaller, as
/// apply saw it: `size` and `mtime_ns` are the plan's (already checked
/// against the file), or those of the file apply just placed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub path: PathBuf,
    pub size: u64,
    pub mtime_ns: i128,
    pub pipe: String,
}

/// Record `records` in one write of the store; entries of files that are
/// gone or changed are dropped on the way (see [`stale`]). Best effort: a
/// failure only means the files are encoded again next time.
pub fn remember_many(records: &[Record]) {
    if records.is_empty() {
        return;
    }
    let Some(store) = store_path() else {
        return;
    };
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut s = load(&store);
    s.version = VERSION;
    s.files.retain(|p, e| !stale(Path::new(p), e));
    for r in records {
        let Some(k) = key(&r.path) else {
            continue;
        };
        s.files.insert(
            k,
            Entry {
                size: r.size,
                mtime_ns: r.mtime_ns,
                pipe: r.pipe.clone(),
            },
        );
    }
    if let Some(dir) = store.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(bytes) = serde_json::to_vec_pretty(&s) {
        let _ = crate::paths::write_atomic(&store, &bytes);
    }
}

/// [`remember_many`] of one file as it is on disk now (tests).
#[cfg(test)]
pub(crate) fn remember(path: &Path, pipe: &str) {
    let meta = std::fs::metadata(path).unwrap();
    remember_many(&[Record {
        path: path.to_path_buf(),
        size: meta.len(),
        mtime_ns: mtime_ns(&meta).unwrap(),
        pipe: pipe.to_string(),
    }]);
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
        let v = BTreeMap::from([("avifenc".to_string(), "avifenc 1.1.1".to_string())]);
        let fp = |steps: &[&str], p: &ContentPreset| {
            let steps: Vec<String> = steps.iter().map(|s| s.to_string()).collect();
            fingerprint(&steps, |n| if n == "M" { Some(&m) } else { Some(p) }, &v)
        };
        assert_eq!(fp(&["X"], &a), fp(&["X"], &a));
        assert_ne!(fp(&["X"], &a), fp(&["X"], &b));
        assert_ne!(fp(&["X"], &a), fp(&["Y"], &a));
        assert_ne!(fp(&["X"], &a), fp(&["X", "M"], &a));
        assert_eq!(fingerprint(&["X".to_string()], |_| None, &v), None);
        // Another encoder version is another pipe.
        let x = vec!["X".to_string()];
        let newer = BTreeMap::from([("avifenc".to_string(), "avifenc 1.3.0".to_string())]);
        assert_ne!(
            fingerprint(&x, |_| Some(&a), &v),
            fingerprint(&x, |_| Some(&a), &newer)
        );
    }

    /// An entry recorded under the earlier rule (before conversions always
    /// converted) was fingerprinted without the policy: it never matches now.
    #[test]
    fn fingerprint_of_the_old_policy_does_not_match() {
        let a = ContentPreset::Photo(PhotoPreset::avif_high());
        let v = BTreeMap::new();
        let steps = vec!["X".to_string()];
        let old = serde_json::to_vec(&(vec![("X", &a)], &v)).unwrap();
        let old = blake3::hash(&old).to_hex().to_string();
        assert_ne!(Some(old), fingerprint(&steps, |_| Some(&a), &v));
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

    fn stored() -> BTreeMap<String, Entry> {
        load(&store_path().unwrap()).files
    }

    /// Entries of a folder that is not there now (an unmounted card or
    /// share) survive other writes; a file gone from a folder that is
    /// there, or changed, is dropped.
    #[test]
    fn pruning_keeps_entries_of_missing_folders() {
        let d = tempfile::tempdir().unwrap();
        let card = d.path().join("card");
        let here = d.path().join("here");
        std::fs::create_dir_all(&card).unwrap();
        std::fs::create_dir_all(&here).unwrap();
        let (on_card, gone, changed, other) = (
            card.join("a.png"),
            here.join("gone.png"),
            here.join("changed.png"),
            here.join("other.png"),
        );
        for f in [&on_card, &gone, &changed, &other] {
            std::fs::write(f, b"pixels").unwrap();
            remember(f, "p");
        }
        let keys: Vec<String> = [&on_card, &gone, &changed]
            .iter()
            .map(|f| key(f).unwrap())
            .collect();
        // The card is unplugged, one file deleted, one edited.
        std::fs::remove_dir_all(&card).unwrap();
        std::fs::remove_file(&gone).unwrap();
        std::fs::write(&changed, b"other pixels").unwrap();
        remember(&other, "p");
        let s = stored();
        assert!(s.contains_key(&keys[0]), "unmounted folder's entry kept");
        assert!(!s.contains_key(&keys[1]), "deleted file's entry dropped");
        assert!(!s.contains_key(&keys[2]), "changed file's entry dropped");
        assert!(s.contains_key(&key(&other).unwrap()));
    }
}
