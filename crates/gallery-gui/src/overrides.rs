//! Manual decisions from the GUI: "Change target…" writes one into the
//! core's overrides file, keyed by the file's current size and mtime.

use std::path::{Path, PathBuf};

use gallery_core::config::Config;
use gallery_core::overrides::{Action, Overrides};
use serde::{Deserialize, Serialize};

/// The overrides file: `overrides.json` in `$GALLERY_DATA_DIR` when that is
/// set (tests, screenshots), else the core's default.
pub fn default_file() -> Result<PathBuf, String> {
    match crate::data_dir_override() {
        Some(dir) => Ok(dir.join("overrides.json")),
        None => Overrides::default_path().ok_or_else(|| gallery_core::t!("lock.no_data_dir")),
    }
}

/// One decision of a batch from the UI.
#[derive(Debug, Clone, Deserialize)]
pub struct OverrideRequest {
    pub source_path: String,
    pub action: Action,
}

/// What happened to one decision of a batch: the key it is stored under
/// and the file size and mtime it was stamped with (the UI compares them
/// with the plan's, since the core ignores a decision for a changed file),
/// or why it was refused.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct OverrideResult {
    pub source_path: String,
    pub key: Option<String>,
    pub size: Option<u64>,
    pub mtime_ns: Option<i128>,
    pub error: Option<String>,
}

/// Record several decisions in one load and save, each checked like
/// [`set_in`]. A refused one is reported and the rest still go in; the file
/// is written only when at least one went in. An unreadable config or
/// overrides file refuses the whole batch.
pub fn set_many_in(
    file: &Path,
    config: &Path,
    requests: Vec<OverrideRequest>,
) -> Result<Vec<OverrideResult>, String> {
    if requests.is_empty() {
        return Ok(Vec::new());
    }
    let cfg = Config::load(config).map_err(|e| e.to_string())?;
    let mut ov = Overrides::load(file).map_err(|e| e.to_string())?;
    let keyed: Vec<(String, Result<PathBuf, String>)> = requests
        .into_iter()
        .map(|r| {
            let res = ov
                .set_checked(&cfg, Path::new(&r.source_path), r.action)
                .map_err(|e| e.to_string());
            (r.source_path, res)
        })
        .collect();
    // A key stored twice in one batch carries the last stamp.
    let stamps: std::collections::HashMap<&Path, (u64, i128)> = ov
        .entries()
        .map(|e| (e.path.as_path(), (e.size, e.mtime_ns)))
        .collect();
    let results: Vec<OverrideResult> = keyed
        .iter()
        .map(|(source_path, res)| match res {
            Ok(key) => {
                let stamp = stamps.get(key.as_path());
                OverrideResult {
                    source_path: source_path.clone(),
                    key: Some(key.display().to_string()),
                    size: stamp.map(|s| s.0),
                    mtime_ns: stamp.map(|s| s.1),
                    error: None,
                }
            }
            Err(e) => OverrideResult {
                source_path: source_path.clone(),
                key: None,
                size: None,
                mtime_ns: None,
                error: Some(e.clone()),
            },
        })
        .collect();
    drop(stamps);
    if results.iter().any(|r| r.key.is_some()) {
        ov.save(file).map_err(|e| e.to_string())?;
    }
    Ok(results)
}

/// Decide `action` for `source` in `file`, checked against the config at
/// `config` (the target exists, the file belongs to a configured source; see
/// `Overrides::set_checked`), with the size and mtime the source has on disk
/// now. Returns the key the decision is stored under (the scan's form of
/// the path) with that size and mtime; a refusal is an `Err`.
pub fn set_in(
    file: &Path,
    config: &Path,
    source: &Path,
    action: Action,
) -> Result<OverrideResult, String> {
    let req = OverrideRequest {
        source_path: source.display().to_string(),
        action,
    };
    let res = set_many_in(file, config, vec![req])?.remove(0);
    match res.error {
        Some(e) => Err(e),
        None => Ok(res),
    }
}

/// A file of a review item as the plan saw it: the decision is made for
/// this version of the file only.
#[derive(Debug, Clone, Deserialize)]
pub struct Member {
    pub source_path: String,
    pub size: u64,
    /// As the UI holds it (a JSON number, so a double): compared as one.
    pub mtime_ns: f64,
}

impl Member {
    /// Why the file on disk is not the version the plan saw, if it is not.
    fn stale(&self) -> Option<String> {
        let path = Path::new(&self.source_path);
        let meta = match std::fs::symlink_metadata(path) {
            Ok(m) => m,
            Err(e) => return Some(format!("{}: {e}", self.source_path)),
        };
        let same = meta.len() == self.size
            && gallery_core::scan::mtime_ns(&meta).is_ok_and(|m| m as f64 == self.mtime_ns);
        (!same).then(|| gallery_core::t!("overrides.changed", path = self.source_path))
    }
}

/// Record `action` for each member through `set` (the batch recorder), but
/// refuse a member whose file changed since the plan: its result carries the
/// error and nothing is written for it. One result per member, in order.
pub fn decide_members(
    members: Vec<(Member, Action)>,
    set: impl FnOnce(Vec<OverrideRequest>) -> Result<Vec<OverrideResult>, String>,
) -> Result<Vec<OverrideResult>, String> {
    let refusal: Vec<Option<String>> = members.iter().map(|(m, _)| m.stale()).collect();
    let requests: Vec<OverrideRequest> = members
        .iter()
        .zip(&refusal)
        .filter(|(_, r)| r.is_none())
        .map(|((m, a), _)| OverrideRequest {
            source_path: m.source_path.clone(),
            action: a.clone(),
        })
        .collect();
    let mut done = set(requests)?.into_iter();
    Ok(members
        .into_iter()
        .zip(refusal)
        .map(|((m, _), r)| match r {
            Some(error) => OverrideResult {
                source_path: m.source_path,
                key: None,
                size: None,
                mtime_ns: None,
                error: Some(error),
            },
            None => done.next().expect("one result per request"),
        })
        .collect())
}

/// The user's pick in a series review: `best` is marked `SeriesBest` and
/// every other member goes to `target`. Only the listed members are
/// written. If the best shot's file changed, nothing is written.
pub fn confirm_series_in(
    members: Vec<Member>,
    best: &str,
    target: &str,
    set: impl FnOnce(Vec<OverrideRequest>) -> Result<Vec<OverrideResult>, String>,
) -> Result<Vec<OverrideResult>, String> {
    let Some(b) = members.iter().find(|m| m.source_path == best) else {
        return Err(gallery_core::t!("overrides.best_not_member", path = best));
    };
    if let Some(e) = b.stale() {
        return Err(e);
    }
    let items = members
        .into_iter()
        .map(|m| {
            let action = if m.source_path == best {
                Action::SeriesBest
            } else {
                Action::Target {
                    name: target.to_string(),
                }
            };
            (m, action)
        })
        .collect();
    decide_members(items, set)
}

/// One decision for every file of an event.
pub fn decide_event_in(
    members: Vec<Member>,
    action: Action,
    set: impl FnOnce(Vec<OverrideRequest>) -> Result<Vec<OverrideResult>, String>,
) -> Result<Vec<OverrideResult>, String> {
    let items = members.into_iter().map(|m| (m, action.clone())).collect();
    decide_members(items, set)
}

/// Drop the decision for `source`; whether there was one. The file is
/// rewritten only when something changed.
pub fn remove_in(file: &Path, source: &Path) -> Result<bool, String> {
    let mut ov = Overrides::load(file).map_err(|e| e.to_string())?;
    if !ov.remove(source) {
        return Ok(false);
    }
    ov.save(file).map_err(|e| e.to_string())?;
    Ok(true)
}

/// Move decisions over to renamed targets: `renames` is the whole mapping
/// old name → new name, applied at once (each decision's new name comes
/// from its original one, so a swap A↔B ends correct); how many moved. Every
/// new name must exist in the config at `config`, and the config must still
/// have `version` (else nothing is written). The file is rewritten only when
/// something changed.
pub fn retarget_in(
    file: &Path,
    config: &Path,
    version: &str,
    renames: &[(String, String)],
) -> Result<usize, String> {
    let (_, current) =
        crate::config_edit::file::read(config).map_err(|e| format!("{}: {e}", config.display()))?;
    if current != version {
        return Err(gallery_core::t!("overrides.config_changed"));
    }
    let cfg = Config::load(config).map_err(|e| e.to_string())?;
    let known = |name: &str| cfg.target(name).is_some();
    if let Some((_, to)) = renames.iter().find(|(_, to)| !known(to)) {
        return Err(gallery_core::t!("overrides.no_target", target = to));
    }
    let map: std::collections::HashMap<&str, &str> = renames
        .iter()
        .map(|(f, t)| (f.as_str(), t.as_str()))
        .collect();
    let mut ov = Overrides::load(file).map_err(|e| e.to_string())?;
    let moved: Vec<_> = ov
        .entries()
        .filter_map(|e| match &e.action {
            Action::Target { name } => map
                .get(name.as_str())
                .filter(|to| **to != name)
                .map(|to| (e.clone(), (*to).to_string())),
            _ => None,
        })
        .collect();
    for (e, to) in &moved {
        ov.set(
            e.path.clone(),
            e.size,
            e.mtime_ns,
            Action::Target { name: to.clone() },
        );
    }
    if !moved.is_empty() {
        ov.save(file).map_err(|e| e.to_string())?;
    }
    Ok(moved.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gallery_core::scan::{FileEntry, mtime_ns};

    fn entry(p: &Path) -> FileEntry {
        let m = std::fs::metadata(p).unwrap();
        FileEntry {
            source_id: "s".into(),
            path: p.to_path_buf(),
            rel: p.file_name().unwrap().into(),
            size: m.len(),
            mtime_ns: mtime_ns(&m).unwrap(),
        }
    }

    fn config(dir: &Path, root: &Path) -> PathBuf {
        let cfg = dir.join("gallery.toml");
        std::fs::write(
            &cfg,
            format!(
                "[[sources]]\nid = \"s\"\npath = {:?}\n\n[[targets.before_ai]]\nname = \"Photos\"\n",
                root.display().to_string()
            ),
        )
        .unwrap();
        cfg
    }

    #[test]
    fn set_reads_the_file_state_and_remove_undoes_it() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("data/overrides.json");
        let root = dir.path().join("src");
        std::fs::create_dir_all(&root).unwrap();
        let cfg = config(dir.path(), &root);
        let src = root.join("a.jpg");
        std::fs::write(&src, b"abc").unwrap();

        let action: Action =
            serde_json::from_value(serde_json::json!({"kind": "target", "name": "Photos"}))
                .unwrap();
        let set = set_in(&file, &cfg, &src, action.clone()).unwrap();
        assert_eq!(set.key, Some(src.display().to_string()));
        // The stored file state comes back, to compare with the plan's.
        let e = entry(&src);
        assert_eq!(
            (set.size, Some(set.mtime_ns.unwrap())),
            (Some(e.size), Some(e.mtime_ns))
        );
        let ov = Overrides::load(&file).unwrap();
        assert_eq!(ov.get(&entry(&src)), Some(&action));
        // A later decision replaces the earlier one.
        set_in(&file, &cfg, &src, Action::Keep).unwrap();
        assert_eq!(
            Overrides::load(&file).unwrap().get(&entry(&src)),
            Some(&Action::Keep)
        );

        assert!(remove_in(&file, &src).unwrap());
        assert!(Overrides::load(&file).unwrap().is_empty());
        assert!(!remove_in(&file, &src).unwrap());
    }

    #[test]
    fn unchecked_decisions_are_refused_and_nothing_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("overrides.json");
        let root = dir.path().join("src");
        std::fs::create_dir_all(&root).unwrap();
        let cfg = config(dir.path(), &root);
        let src = root.join("a.jpg");
        std::fs::write(&src, b"abc").unwrap();
        let outside = dir.path().join("b.jpg");
        std::fs::write(&outside, b"abc").unwrap();

        let unknown = Action::Target {
            name: "No such target".into(),
        };
        assert!(set_in(&file, &cfg, &src, unknown).is_err());
        assert!(set_in(&file, &cfg, &outside, Action::Keep).is_err());
        assert!(set_in(&file, &cfg, &root.join("nope.jpg"), Action::Keep).is_err());
        assert!(set_in(&file, &cfg, &root, Action::Keep).is_err());
        // An unreadable config refuses everything.
        assert!(set_in(&file, &dir.path().join("none.toml"), &src, Action::Keep).is_err());
        assert!(!file.exists());
    }

    #[test]
    fn set_many_checks_each_and_saves_once() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("data/overrides.json");
        let root = dir.path().join("src");
        std::fs::create_dir_all(&root).unwrap();
        let cfg = config(dir.path(), &root);
        let (a, b) = (root.join("a.jpg"), root.join("b.jpg"));
        std::fs::write(&a, b"abc").unwrap();
        std::fs::write(&b, b"x").unwrap();
        let photo = Action::Target {
            name: "Photos".into(),
        };
        let res = set_many_in(
            &file,
            &cfg,
            vec![
                OverrideRequest {
                    source_path: a.display().to_string(),
                    action: photo.clone(),
                },
                OverrideRequest {
                    source_path: root.join("gone.jpg").display().to_string(),
                    action: photo.clone(),
                },
                OverrideRequest {
                    source_path: b.display().to_string(),
                    action: Action::Keep,
                },
            ],
        )
        .unwrap();
        // One result per request, in order; a bad one does not stop the rest.
        assert_eq!(res.len(), 3);
        assert_eq!(
            res[0].key.as_deref(),
            Some(a.display().to_string().as_str())
        );
        assert!(res[0].error.is_none());
        assert!(res[1].key.is_none() && res[1].error.is_some());
        assert_eq!(
            res[1].source_path,
            root.join("gone.jpg").display().to_string()
        );
        assert_eq!(
            res[2].key.as_deref(),
            Some(b.display().to_string().as_str())
        );
        // The file state each decision was stamped with; none for a refusal.
        assert_eq!(res[0].size, Some(3));
        assert_eq!(res[0].mtime_ns, Some(entry(&a).mtime_ns));
        assert_eq!(res[2].size, Some(1));
        assert!(res[1].size.is_none() && res[1].mtime_ns.is_none());
        let ov = Overrides::load(&file).unwrap();
        assert_eq!(ov.len(), 2);
        assert_eq!(ov.get(&entry(&a)), Some(&photo));
        assert_eq!(ov.get(&entry(&b)), Some(&Action::Keep));
        // The JSON the UI reads.
        let v = serde_json::to_value(&res[1]).unwrap();
        assert!(v["key"].is_null() && v["error"].is_string(), "{v}");
    }

    #[test]
    fn set_many_writes_nothing_when_nothing_passes() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("overrides.json");
        let root = dir.path().join("src");
        std::fs::create_dir_all(&root).unwrap();
        let cfg = config(dir.path(), &root);
        let src = root.join("a.jpg");
        std::fs::write(&src, b"abc").unwrap();
        let req = |action| OverrideRequest {
            source_path: src.display().to_string(),
            action,
        };
        let res = set_many_in(
            &file,
            &cfg,
            vec![req(Action::Target {
                name: "No such target".into(),
            })],
        )
        .unwrap();
        assert!(res[0].error.is_some());
        assert!(set_many_in(&file, &cfg, vec![]).unwrap().is_empty());
        assert!(!file.exists());
        // A config that does not load refuses the whole batch.
        assert!(
            set_many_in(
                &file,
                &dir.path().join("none.toml"),
                vec![req(Action::Keep)]
            )
            .is_err()
        );
        // So does a broken overrides file, which is kept.
        std::fs::write(&file, "{").unwrap();
        assert!(set_many_in(&file, &cfg, vec![req(Action::Keep)]).is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "{");
    }

    #[test]
    fn bad_overrides_file_is_an_error_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("overrides.json");
        let root = dir.path().join("src");
        std::fs::create_dir_all(&root).unwrap();
        let cfg = config(dir.path(), &root);
        std::fs::write(&file, "{").unwrap();
        let src = root.join("a.jpg");
        std::fs::write(&src, b"abc").unwrap();
        assert!(set_in(&file, &cfg, &src, Action::Keep).is_err());
        // The broken file is left for the user, never replaced.
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "{");
    }

    fn member(p: &Path) -> Member {
        let e = entry(p);
        Member {
            source_path: p.display().to_string(),
            size: e.size,
            mtime_ns: e.mtime_ns as f64,
        }
    }

    #[test]
    fn confirm_series_marks_best_and_sends_the_others_only() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("overrides.json");
        let root = dir.path().join("src");
        std::fs::create_dir_all(&root).unwrap();
        let cfg = config(dir.path(), &root);
        let (a, b, c, other) = (
            root.join("a.jpg"),
            root.join("b.jpg"),
            root.join("c.jpg"),
            root.join("other.jpg"),
        );
        for (p, body) in [(&a, "1"), (&b, "22"), (&c, "333"), (&other, "4444")] {
            std::fs::write(p, body).unwrap();
        }
        let set = |r| set_many_in(&file, &cfg, r);
        let res = confirm_series_in(
            vec![member(&a), member(&b), member(&c)],
            &b.display().to_string(),
            "Photos",
            set,
        )
        .unwrap();
        assert!(res.iter().all(|r| r.error.is_none()));
        let ov = Overrides::load(&file).unwrap();
        assert_eq!(ov.len(), 3);
        assert_eq!(ov.get(&entry(&b)), Some(&Action::SeriesBest));
        let photos = Action::Target {
            name: "Photos".into(),
        };
        assert_eq!(ov.get(&entry(&a)), Some(&photos));
        assert_eq!(ov.get(&entry(&c)), Some(&photos));
        assert_eq!(ov.get(&entry(&other)), None);
    }

    #[test]
    fn confirm_series_refuses_changed_files() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("overrides.json");
        let root = dir.path().join("src");
        std::fs::create_dir_all(&root).unwrap();
        let cfg = config(dir.path(), &root);
        let (a, b, c) = (root.join("a.jpg"), root.join("b.jpg"), root.join("c.jpg"));
        for (p, body) in [(&a, "1"), (&b, "22"), (&c, "333")] {
            std::fs::write(p, body).unwrap();
        }
        let (ma, mb, mc) = (member(&a), member(&b), member(&c));
        // A non-best member changed: only it is refused.
        std::fs::write(&a, "changed").unwrap();
        let res = confirm_series_in(
            vec![ma.clone(), mb.clone(), mc.clone()],
            &b.display().to_string(),
            "Photos",
            |r| set_many_in(&file, &cfg, r),
        )
        .unwrap();
        assert!(res[0].error.is_some() && res[0].key.is_none());
        assert!(res[1].error.is_none() && res[2].error.is_none());
        let ov = Overrides::load(&file).unwrap();
        assert_eq!(ov.len(), 2);
        assert_eq!(ov.get(&entry(&a)), None);
        // The best shot changed: nothing is written at all.
        let file2 = dir.path().join("o2.json");
        std::fs::write(&b, "changed too").unwrap();
        let err = confirm_series_in(
            vec![mb.clone(), mc.clone()],
            &b.display().to_string(),
            "Photos",
            |r| set_many_in(&file2, &cfg, r),
        );
        assert!(err.is_err());
        assert!(!file2.exists());
        // A best that is no member is an error too.
        let err = confirm_series_in(vec![mc], &a.display().to_string(), "Photos", |r| {
            set_many_in(&file2, &cfg, r)
        });
        assert!(err.is_err());
        assert!(!file2.exists());
    }

    #[test]
    fn decide_event_writes_one_override_per_member() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("overrides.json");
        let root = dir.path().join("src");
        std::fs::create_dir_all(&root).unwrap();
        let cfg = config(dir.path(), &root);
        let (a, b, other) = (root.join("a.jpg"), root.join("b.jpg"), root.join("o.jpg"));
        for (p, body) in [(&a, "1"), (&b, "22"), (&other, "333")] {
            std::fs::write(p, body).unwrap();
        }
        let res = decide_event_in(vec![member(&a), member(&b)], Action::Keep, |r| {
            set_many_in(&file, &cfg, r)
        })
        .unwrap();
        assert_eq!(res.len(), 2);
        let ov = Overrides::load(&file).unwrap();
        assert_eq!(ov.len(), 2);
        assert_eq!(ov.get(&entry(&a)), Some(&Action::Keep));
        assert_eq!(ov.get(&entry(&b)), Some(&Action::Keep));
        assert_eq!(ov.get(&entry(&other)), None);
    }
}
