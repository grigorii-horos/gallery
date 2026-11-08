//! Manual routing decisions ("this file → that target", "leave it in its
//! source"), kept apart from plans so that they survive a rescan.
//!
//! An entry is keyed by the source path and holds the size and mtime the file
//! had when the decision was made: once the file changes, the entry no longer
//! applies (it is stale) and routing falls back to masks and AI.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::{Config, expand_tilde};
use crate::mask::Mask;
use crate::scan::{FileEntry, mtime_ns};
use crate::{Error, Result};

const VERSION: u32 = 1;

/// What the user decided for a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    /// Route the file to this target.
    Target { name: String },
    /// Leave the file where it is, untouched.
    Keep,
}

/// One decision with the file state it was made for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub path: PathBuf,
    pub size: u64,
    pub mtime_ns: i128,
    pub action: Action,
}

#[derive(Serialize, Deserialize)]
struct FileFormat {
    version: u32,
    entries: Vec<Entry>,
}

/// The set of manual decisions, one per source path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Overrides {
    entries: BTreeMap<PathBuf, Entry>,
}

impl Overrides {
    /// `<user data dir>/gallery/overrides.json`.
    pub fn default_path() -> Option<PathBuf> {
        dirs::data_dir().map(|d| d.join("gallery").join("overrides.json"))
    }

    /// Read the decisions; a missing file is an empty set. A file that cannot
    /// be read or parsed is an error: it is never silently replaced.
    pub fn load(path: &Path) -> Result<Overrides> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Overrides::default());
            }
            Err(e) => return Err(e.into()),
        };
        Overrides::parse(&text, path)
    }

    /// [`Overrides::load`] over text already read (`label` is only for
    /// messages), so a caller can hash and parse the very same bytes.
    pub fn parse(text: &str, label: &Path) -> Result<Overrides> {
        let bad = |msg: String| Error::Config(format!("{}: {msg}", label.display()));
        let file: FileFormat = serde_json::from_str(text).map_err(|e| bad(e.to_string()))?;
        if file.version != VERSION {
            return Err(bad(crate::t!(
                "overrides.unsupported_version",
                version = file.version
            )));
        }
        Ok(Overrides {
            entries: file
                .entries
                .into_iter()
                .map(|e| (e.path.clone(), e))
                .collect(),
        })
    }

    /// Write the decisions atomically (temp file in the same directory,
    /// fsync, rename), creating the directory if needed.
    pub fn save(&self, path: &Path) -> Result<()> {
        let dir = match path.parent() {
            Some(d) if !d.as_os_str().is_empty() => d,
            _ => Path::new("."),
        };
        std::fs::create_dir_all(dir)?;
        let file = FileFormat {
            version: VERSION,
            entries: self.entries.values().cloned().collect(),
        };
        let json = serde_json::to_string_pretty(&file)
            .map_err(|e| Error::Config(format!("{}: {e}", path.display())))?;
        // Dropped (and removed) on any error before the rename.
        let mut tmp = tempfile::Builder::new()
            .prefix(".overrides.")
            .suffix(".tmp")
            .tempfile_in(dir)?;
        tmp.write_all(json.as_bytes())?;
        tmp.as_file().sync_all()?;
        tmp.persist(path).map_err(|e| Error::Io(e.error))?;
        Ok(())
    }

    /// Decide `action` for the file at `path` in the given state, replacing
    /// any earlier decision for that path.
    pub fn set(&mut self, path: PathBuf, size: u64, mtime_ns: i128, action: Action) {
        self.entries.insert(
            path.clone(),
            Entry {
                path,
                size,
                mtime_ns,
                action,
            },
        );
    }

    /// [`Overrides::set`] for a decision coming from outside (the GUI),
    /// checked against `cfg`: a target action must name a configured target,
    /// and `path` must be a regular file (not a symlink, not reached through a
    /// symlinked folder below the root) that the scan of a
    /// configured source would list: under its root (a direct child when the
    /// source is not recursive), neither hidden nor skipped. The size and
    /// mtime are read from disk now. The decision is keyed by the path in the
    /// form the scan produces (the configured root joined with the relative
    /// path), which is returned; nothing is recorded on error.
    pub fn set_checked(&mut self, cfg: &Config, path: &Path, action: Action) -> Result<PathBuf> {
        if let Action::Target { name } = &action {
            let t = &cfg.targets;
            let known = t
                .before_ai
                .iter()
                .map(|m| &m.name)
                .chain(t.ai.iter().map(|a| &a.name))
                .chain(t.after_ai.iter().map(|m| &m.name))
                .any(|n| n == name);
            if !known {
                return Err(Error::Config(crate::t!(
                    "overrides.unknown_target",
                    name = name
                )));
            }
        }
        let shown = || path.display().to_string();
        let meta = std::fs::symlink_metadata(path)
            .map_err(|e| Error::Config(format!("{}: {e}", shown())))?;
        if !meta.file_type().is_file() {
            return Err(Error::Config(crate::t!(
                "overrides.not_regular",
                path = shown()
            )));
        }
        let real = path.canonicalize()?;
        for src in &cfg.sources {
            let root = expand_tilde(&src.path);
            let Ok(real_root) = root.canonicalize() else {
                continue;
            };
            let Ok(rel) = real.strip_prefix(&real_root) else {
                continue;
            };
            let names: Vec<_> = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect();
            if names.is_empty() || (!src.recursive && names.len() > 1) {
                continue;
            }
            let skip = src
                .skip
                .iter()
                .map(|g| Mask::parse(g))
                .collect::<Result<Vec<_>>>()?;
            if names
                .iter()
                .any(|n| n.starts_with('.') || skip.iter().any(|m| m.matches(n)))
            {
                continue;
            }
            // The scan does not follow symlinked folders: a file reached
            // through one below the root would never get this decision.
            if !direct_below(path, &real_root) {
                return Err(Error::Config(crate::t!(
                    "overrides.symlinked",
                    path = shown()
                )));
            }
            let key = root.join(rel);
            let mtime = mtime_ns(&meta)?;
            self.set(key.clone(), meta.len(), mtime, action);
            return Ok(key);
        }
        Err(Error::Config(crate::t!(
            "overrides.not_in_source",
            path = shown()
        )))
    }

    /// Drop the decision for `path`; whether there was one.
    pub fn remove(&mut self, path: &Path) -> bool {
        self.entries.remove(path).is_some()
    }

    /// Drop the decisions whose files no longer exist; how many went. A path
    /// whose existence cannot be determined is kept.
    pub fn prune(&mut self) -> usize {
        let before = self.entries.len();
        self.entries.retain(|p, _| {
            !matches!(std::fs::symlink_metadata(p),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound)
        });
        before - self.entries.len()
    }

    /// The decision for `entry`, if one exists and is not stale.
    pub fn get(&self, entry: &FileEntry) -> Option<&Action> {
        self.entries
            .get(&entry.path)
            .filter(|e| e.size == entry.size && e.mtime_ns == entry.mtime_ns)
            .map(|e| &e.action)
    }

    /// Every stored decision (stale ones too), by path.
    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.entries.values()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

/// Whether every folder of `path` (as given) between the file and the folder
/// that resolves to `real_root` is a real folder, not a symlink. The root
/// itself may be a symlink, as the scan starts from it.
fn direct_below(path: &Path, real_root: &Path) -> bool {
    for anc in path.ancestors().skip(1) {
        let anc = if anc.as_os_str().is_empty() {
            Path::new(".")
        } else {
            anc
        };
        match anc.canonicalize() {
            Ok(real) if real == real_root => return true,
            Ok(_) => {}
            Err(_) => return false,
        }
        match std::fs::symlink_metadata(anc) {
            Ok(m) if !m.file_type().is_symlink() => {}
            _ => return false,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn entry(path: &str, size: u64, mtime_ns: i128) -> FileEntry {
        FileEntry {
            source_id: "a".into(),
            path: PathBuf::from(path),
            rel: PathBuf::from(path.trim_start_matches('/')),
            size,
            mtime_ns,
        }
    }

    fn target(name: &str) -> Action {
        Action::Target { name: name.into() }
    }

    #[test]
    fn missing_file_loads_empty() {
        let dir = tempfile::tempdir().unwrap();
        let o = Overrides::load(&dir.path().join("nope.json")).unwrap();
        assert!(o.is_empty());
    }

    #[test]
    fn corrupt_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("o.json");
        std::fs::write(&p, "{not json").unwrap();
        assert!(Overrides::load(&p).is_err());
    }

    #[test]
    fn save_then_load_round_trips_and_creates_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir
            .path()
            .join("deep")
            .join("gallery")
            .join("overrides.json");
        let mut o = Overrides::default();
        o.set("/s/a.jpg".into(), 10, -5, target("cats"));
        o.set(
            "/s/b.jpg".into(),
            20,
            1_700_000_000_123_456_789,
            Action::Keep,
        );
        o.save(&p).unwrap();
        let back = Overrides::load(&p).unwrap();
        assert_eq!(back, o);
        // Nothing but the file itself is left in the directory.
        let names: Vec<_> = std::fs::read_dir(p.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("overrides.json")]);
        // Overwrite keeps working.
        o.remove(Path::new("/s/a.jpg"));
        o.save(&p).unwrap();
        assert_eq!(Overrides::load(&p).unwrap().len(), 1);
    }

    #[test]
    fn json_shape() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("o.json");
        let mut o = Overrides::default();
        o.set("/s/a.jpg".into(), 10, 7, target("cats"));
        o.save(&p).unwrap();
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["version"], 1);
        assert_eq!(v["entries"][0]["path"], "/s/a.jpg");
        assert_eq!(v["entries"][0]["size"], 10);
        assert_eq!(v["entries"][0]["mtime_ns"], 7);
        assert_eq!(
            v["entries"][0]["action"],
            serde_json::json!({"kind": "target", "name": "cats"})
        );
    }

    #[test]
    fn unknown_version_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("o.json");
        std::fs::write(&p, r#"{"version": 2, "entries": []}"#).unwrap();
        assert!(Overrides::load(&p).is_err());
    }

    #[test]
    fn get_ignores_stale_entries() {
        let mut o = Overrides::default();
        o.set("/s/a.jpg".into(), 10, 100, target("cats"));
        assert_eq!(o.get(&entry("/s/a.jpg", 10, 100)), Some(&target("cats")));
        assert_eq!(o.get(&entry("/s/a.jpg", 11, 100)), None);
        assert_eq!(o.get(&entry("/s/a.jpg", 10, 101)), None);
        assert_eq!(o.get(&entry("/s/b.jpg", 10, 100)), None);
    }

    #[test]
    fn set_replaces_and_remove_drops() {
        let mut o = Overrides::default();
        o.set("/s/a.jpg".into(), 10, 100, target("cats"));
        o.set("/s/a.jpg".into(), 12, 200, Action::Keep);
        assert_eq!(o.len(), 1);
        assert_eq!(o.get(&entry("/s/a.jpg", 12, 200)), Some(&Action::Keep));
        assert!(o.remove(Path::new("/s/a.jpg")));
        assert!(!o.remove(Path::new("/s/a.jpg")));
        assert!(o.is_empty());
    }

    #[test]
    fn prune_drops_entries_of_vanished_files() {
        let dir = tempfile::tempdir().unwrap();
        let here = dir.path().join("here.jpg");
        std::fs::write(&here, b"x").unwrap();
        let gone = dir.path().join("gone.jpg");
        let mut o = Overrides::default();
        o.set(here.clone(), 1, 0, Action::Keep);
        o.set(gone.clone(), 1, 0, Action::Keep);
        assert_eq!(o.prune(), 1);
        let paths: Vec<_> = o.entries().map(|e| e.path.clone()).collect();
        assert_eq!(paths, vec![here]);
    }

    #[test]
    fn default_path_is_under_data_dir() {
        if let Some(p) = Overrides::default_path() {
            assert!(p.ends_with("gallery/overrides.json"));
        }
    }

    fn checked_config(root: &Path, recursive: bool) -> Config {
        let text = format!(
            r#"
            [[sources]]
            id = "s"
            path = {path:?}
            recursive = {recursive}
            skip = ["*.tmp"]

            [[targets.before_ai]]
            name = "cats"

            [[targets.ai]]
            name = "dogs"
            description = "dogs"
            "#,
            path = root.display().to_string(),
        );
        toml::from_str(&text).unwrap()
    }

    #[test]
    fn set_checked_records_the_scan_key_with_the_file_state() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("src");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("a.jpg"), b"abc").unwrap();
        std::fs::write(root.join("sub/b.jpg"), b"x").unwrap();
        let cfg = checked_config(&root, true);
        let scanned = crate::scan::scan_sources(&cfg.sources).unwrap();

        let mut o = Overrides::default();
        // A path with `..` is normalised to the key the scan produces.
        let key = o
            .set_checked(&cfg, &root.join("sub/../a.jpg"), target("cats"))
            .unwrap();
        assert_eq!(key, root.join("a.jpg"));
        let a = scanned.iter().find(|e| e.path == key).unwrap();
        assert_eq!(o.get(a), Some(&target("cats")));
        // AI targets count, nested files in a recursive source too.
        o.set_checked(&cfg, &root.join("sub/b.jpg"), target("dogs"))
            .unwrap();
        o.set_checked(&cfg, &root.join("a.jpg"), Action::Keep)
            .unwrap();
        assert_eq!(o.len(), 2);
        assert_eq!(o.get(a), Some(&Action::Keep));
    }

    #[test]
    fn set_checked_keys_like_the_scan_through_a_symlinked_root() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("a.jpg"), b"abc").unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let cfg = checked_config(&link, false);
        let scanned = crate::scan::scan_sources(&cfg.sources).unwrap();
        let mut o = Overrides::default();
        // Given by its real path, keyed by the configured (scanned) one.
        let key = o
            .set_checked(&cfg, &real.join("a.jpg"), Action::Keep)
            .unwrap();
        assert_eq!(key, link.join("a.jpg"));
        assert_eq!(o.get(&scanned[0]), Some(&Action::Keep));
    }

    #[test]
    fn set_checked_rejects_what_the_scan_would_not_route() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("src");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::create_dir_all(root.join(".hidden")).unwrap();
        for f in ["a.jpg", "sub/b.jpg", "x.tmp", ".h.jpg", ".hidden/c.jpg"] {
            std::fs::write(root.join(f), b"abc").unwrap();
        }
        std::fs::write(dir.path().join("outside.jpg"), b"abc").unwrap();
        std::os::unix::fs::symlink(root.join("a.jpg"), root.join("ln.jpg")).unwrap();
        let flat = checked_config(&root, false);
        let deep = checked_config(&root, true);
        let mut o = Overrides::default();

        // Unknown target.
        assert!(
            o.set_checked(&flat, &root.join("a.jpg"), target("nope"))
                .is_err()
        );
        // Outside every source, also via `..`.
        assert!(
            o.set_checked(&flat, &dir.path().join("outside.jpg"), Action::Keep)
                .is_err()
        );
        assert!(
            o.set_checked(&flat, &root.join("../outside.jpg"), Action::Keep)
                .is_err()
        );
        // Nested file of a non-recursive source.
        assert!(
            o.set_checked(&flat, &root.join("sub/b.jpg"), Action::Keep)
                .is_err()
        );
        // Missing file, a directory, a symlink, hidden and skipped entries.
        assert!(
            o.set_checked(&deep, &root.join("nope.jpg"), Action::Keep)
                .is_err()
        );
        assert!(
            o.set_checked(&deep, &root.join("sub"), Action::Keep)
                .is_err()
        );
        assert!(
            o.set_checked(&deep, &root.join("ln.jpg"), Action::Keep)
                .is_err()
        );
        assert!(
            o.set_checked(&deep, &root.join(".h.jpg"), Action::Keep)
                .is_err()
        );
        assert!(
            o.set_checked(&deep, &root.join(".hidden/c.jpg"), Action::Keep)
                .is_err()
        );
        assert!(
            o.set_checked(&deep, &root.join("x.tmp"), Action::Keep)
                .is_err()
        );
        // No sources at all.
        assert!(
            o.set_checked(&Config::default(), &root.join("a.jpg"), Action::Keep)
                .is_err()
        );
        assert!(o.is_empty());
        // The nested file is fine once the source is recursive.
        o.set_checked(&deep, &root.join("sub/b.jpg"), Action::Keep)
            .unwrap();
        assert_eq!(o.len(), 1);
    }

    #[test]
    fn set_checked_rejects_a_path_through_a_symlinked_dir_below_the_root() {
        // The scan does not follow symlinked directories, so a decision for a
        // file reached through one would never apply.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("src");
        std::fs::create_dir_all(root.join("real/deep")).unwrap();
        std::fs::write(root.join("real/a.jpg"), b"abc").unwrap();
        std::fs::write(root.join("real/deep/b.jpg"), b"abc").unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("ln")).unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("c.jpg"), b"abc").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("out")).unwrap();
        let cfg = checked_config(&root, true);
        let mut o = Overrides::default();

        assert!(
            o.set_checked(&cfg, &root.join("ln/a.jpg"), Action::Keep)
                .is_err()
        );
        assert!(
            o.set_checked(&cfg, &root.join("ln/deep/b.jpg"), Action::Keep)
                .is_err()
        );
        assert!(
            o.set_checked(&cfg, &root.join("real/../ln/a.jpg"), Action::Keep)
                .is_err()
        );
        assert!(
            o.set_checked(&cfg, &root.join("out/c.jpg"), Action::Keep)
                .is_err()
        );
        assert!(o.is_empty());
        // The same files by their real paths are fine.
        o.set_checked(&cfg, &root.join("real/a.jpg"), Action::Keep)
            .unwrap();
        o.set_checked(&cfg, &root.join("real/deep/../a.jpg"), Action::Keep)
            .unwrap();
        assert_eq!(o.len(), 1);
    }

    #[test]
    fn save_failure_leaves_old_file() {
        // A directory where the file should be: rename fails, nothing lost.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("o.json");
        std::fs::create_dir(&p).unwrap();
        std::fs::write(p.join("keep"), b"x").unwrap();
        let mut o = Overrides::default();
        o.set("/s/a.jpg".into(), 1, 0, Action::Keep);
        assert!(o.save(&p).is_err());
        assert!(p.join("keep").exists());
        let leftovers = std::fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(leftovers, 1, "temp file cleaned up");
    }
}
