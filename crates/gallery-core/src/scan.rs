//! Source scanning: list candidate files of configured sources.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use globset::{GlobBuilder, GlobMatcher};
use walkdir::WalkDir;

use crate::config::{Source, expand_tilde};
use crate::mask::Mask;
use crate::progress::{Cancel, NoProgress, Progress, ProgressEvent, Stage};
use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    pub source_id: String,
    pub path: PathBuf,
    /// Path relative to the source root.
    pub rel: PathBuf,
    pub size: u64,
    pub mtime_ns: i128,
}

/// Modification time in nanoseconds since the epoch (negative before it).
pub fn mtime_ns(meta: &std::fs::Metadata) -> std::io::Result<i128> {
    Ok(match meta.modified()?.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_nanos() as i128,
        Err(e) => -(e.duration().as_nanos() as i128),
    })
}

/// Not found below the root; a missing root itself stays an error.
fn vanished(e: &walkdir::Error) -> bool {
    e.depth() > 0
        && e.io_error()
            .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound)
}

/// Regular files yielded by `walk` (a walk of `root`). An entry that vanished
/// between the directory listing and its use (Syncthing at work) is skipped;
/// any other error ends the scan.
#[cfg(test)]
fn walk_files(
    source_id: &str,
    root: &Path,
    walk: impl Iterator<Item = walkdir::Result<walkdir::DirEntry>>,
) -> Result<Vec<FileEntry>> {
    let mut out = Vec::new();
    walk_files_into(source_id, root, walk, &mut out, &NoProgress, &Cancel::new())?;
    Ok(out)
}

/// [`walk_files`] appending to `out`, with a [`Stage::Scan`] event per file
/// (`done` counts `out`, total unknown) and a cancel check per entry.
fn walk_files_into(
    source_id: &str,
    root: &Path,
    walk: impl Iterator<Item = walkdir::Result<walkdir::DirEntry>>,
    out: &mut Vec<FileEntry>,
    progress: &dyn Progress,
    cancel: &Cancel,
) -> Result<()> {
    for entry in walk {
        cancel.check()?;
        let entry = match entry {
            Ok(e) => e,
            Err(e) if vanished(&e) => continue,
            Err(e) => return Err(Error::Io(e.into())),
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(e) if vanished(&e) => continue,
            Err(e) => return Err(Error::Io(e.into())),
        };
        let mtime_ns = mtime_ns(&meta)?;
        let path = entry.into_path();
        let rel = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
        progress.step(ProgressEvent {
            stage: Stage::Scan,
            done: out.len() + 1,
            total: 0,
            current: Some(path.clone()),
        });
        out.push(FileEntry {
            source_id: source_id.to_string(),
            path,
            rel,
            size: meta.len(),
            mtime_ns,
        });
    }
    Ok(())
}

/// A source's `skip` globs. A glob without `/` is matched against each
/// path part (a file or folder name); one with `/` is matched against the
/// whole path relative to the source root (`Screenshots/*.webm`), where `*`
/// stays inside one folder level.
struct SkipSet(Vec<(String, SkipGlob)>);

enum SkipGlob {
    Name(Mask),
    Path(GlobMatcher),
}

impl SkipSet {
    fn new(globs: &[String]) -> Result<SkipSet> {
        globs
            .iter()
            .map(|g| {
                let m = if g.contains('/') {
                    let glob = GlobBuilder::new(&g.to_lowercase())
                        .case_insensitive(true)
                        .literal_separator(true)
                        .build()
                        .map_err(|e| Error::Config(crate::t!("mask.bad", mask = g, detail = e)))?;
                    SkipGlob::Path(glob.compile_matcher())
                } else {
                    SkipGlob::Name(Mask::parse(g)?)
                };
                Ok((g.clone(), m))
            })
            .collect::<Result<Vec<_>>>()
            .map(SkipSet)
    }

    /// The first glob that skips the entry `name` at `rel` (`/`-separated).
    fn hit(&self, name: &str, rel: &str) -> Option<&str> {
        self.0.iter().find_map(|(g, m)| {
            let hit = match m {
                SkipGlob::Name(m) => m.matches(name),
                SkipGlob::Path(p) => p.is_match(rel.to_lowercase()),
            };
            hit.then_some(g.as_str())
        })
    }
}

fn rel_text(rel: &Path) -> String {
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// Why a scan of a source never lists a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejection {
    /// A path part starts with a dot.
    Hidden(String),
    /// A path part matches a `skip` glob of the source.
    Skip { part: String, glob: String },
    /// The source is not recursive and the file lies in a subfolder.
    NotRecursive,
}

/// Whether a scan of `src` skips the file at `rel` (relative to the source
/// root), and why: the rules of [`scan_sources_with`].
pub fn rejection(src: &Source, rel: &Path) -> Result<Option<Rejection>> {
    let skip = SkipSet::new(&src.skip)?;
    if !src.recursive && rel.components().count() > 1 {
        return Ok(Some(Rejection::NotRecursive));
    }
    let mut upto = PathBuf::new();
    for c in rel.components() {
        let name = c.as_os_str().to_string_lossy();
        upto.push(c.as_os_str());
        if name.starts_with('.') {
            return Ok(Some(Rejection::Hidden(name.into_owned())));
        }
        if let Some(glob) = skip.hit(&name, &rel_text(&upto)) {
            let part = if glob.contains('/') {
                rel_text(&upto)
            } else {
                name.into_owned()
            };
            return Ok(Some(Rejection::Skip {
                part,
                glob: glob.to_string(),
            }));
        }
    }
    Ok(None)
}

/// List regular files of all sources, sorted by path. Hidden entries and
/// entries matching a source's `skip` globs are ignored, both as files and as
/// directories. Symlinks are neither followed nor listed.
pub fn scan_sources(sources: &[Source]) -> Result<Vec<FileEntry>> {
    scan_sources_with(sources, &NoProgress, &Cancel::new())
}

/// [`scan_sources`] with a [`Stage::Scan`] event per file found (total 0:
/// not known yet), then one with the final count as `done` and `total`.
/// A cancel ends the scan with [`Error::Cancelled`].
pub fn scan_sources_with(
    sources: &[Source],
    progress: &dyn Progress,
    cancel: &Cancel,
) -> Result<Vec<FileEntry>> {
    let mut out = Vec::new();
    for src in sources {
        let skip = SkipSet::new(&src.skip)?;
        let root = expand_tilde(&src.path);
        let walk = WalkDir::new(&root)
            .max_depth(if src.recursive { usize::MAX } else { 1 })
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| {
                if e.depth() == 0 {
                    return true;
                }
                let name = e.file_name().to_string_lossy();
                if name.starts_with('.') {
                    return false;
                }
                let rel = e.path().strip_prefix(&root).unwrap_or(e.path());
                skip.hit(&name, &rel_text(rel)).is_none()
            });
        walk_files_into(&src.id, &root, walk, &mut out, progress, cancel)?;
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    progress.step(ProgressEvent {
        stage: Stage::Scan,
        done: out.len(),
        total: out.len(),
        current: None,
    });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn touch(root: &Path, rel: &str, body: &str) {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body).unwrap();
    }

    fn source(root: &Path, recursive: bool) -> Source {
        Source {
            id: "s".into(),
            path: root.to_path_buf(),
            server_path: None,
            recursive,
            skip: vec![".stfolder".into(), ".gallery-tmp-*".into()],
            ai_prefix: None,
            ai_prefix_file: None,
            ai_suffix: None,
            ai_suffix_file: None,
        }
    }

    fn rels(v: &[FileEntry]) -> Vec<String> {
        v.iter()
            .map(|e| e.rel.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn scan_top_level_only() {
        let d = tempfile::tempdir().unwrap();
        touch(d.path(), "b.jpg", "x");
        touch(d.path(), "a.jpg", "x");
        touch(d.path(), "sub/c.jpg", "x");
        let r = scan_sources(&[source(d.path(), false)]).unwrap();
        assert_eq!(rels(&r), ["a.jpg", "b.jpg"]);
        assert_eq!(r[0].source_id, "s");
        assert_eq!(r[0].path, d.path().join("a.jpg"));
    }

    #[test]
    fn scan_recursive() {
        let d = tempfile::tempdir().unwrap();
        touch(d.path(), "b.jpg", "x");
        touch(d.path(), "sub/deep/c.jpg", "x");
        let r = scan_sources(&[source(d.path(), true)]).unwrap();
        assert_eq!(rels(&r), ["b.jpg", "sub/deep/c.jpg"]);
    }

    #[test]
    fn scan_skips_hidden_and_skip_globs() {
        let d = tempfile::tempdir().unwrap();
        touch(d.path(), "ok.jpg", "x");
        touch(d.path(), ".stfolder/x.jpg", "x");
        touch(d.path(), ".hidden.jpg", "x");
        touch(d.path(), ".gallery-tmp-1", "x");
        touch(d.path(), "sub/.gallery-tmp-2", "x");
        touch(d.path(), "sub/.hid/y.jpg", "x");
        touch(d.path(), "sub/z.jpg", "x");
        let r = scan_sources(&[source(d.path(), true)]).unwrap();
        assert_eq!(rels(&r), ["ok.jpg", "sub/z.jpg"]);
    }

    #[test]
    fn path_skip_glob_is_relative_to_the_source_and_stays_in_one_folder() {
        let d = tempfile::tempdir().unwrap();
        touch(d.path(), "Shots/a.webm", "x");
        touch(d.path(), "Shots/a.jpg", "x");
        touch(d.path(), "Shots/deep/b.webm", "x");
        touch(d.path(), "Other/c.webm", "x");
        let mut src = source(d.path(), true);
        src.skip.push("shots/*.WEBM".into());
        let r = scan_sources(&[src.clone()]).unwrap();
        assert_eq!(
            rels(&r),
            ["Other/c.webm", "Shots/a.jpg", "Shots/deep/b.webm"]
        );
        let why = rejection(&src, Path::new("Shots/a.webm")).unwrap();
        assert_eq!(
            why,
            Some(Rejection::Skip {
                part: "Shots/a.webm".into(),
                glob: "shots/*.WEBM".into()
            })
        );
        assert_eq!(rejection(&src, Path::new("Shots/a.jpg")).unwrap(), None);
    }

    #[test]
    fn scan_records_size_mtime() {
        let d = tempfile::tempdir().unwrap();
        touch(d.path(), "a.jpg", "12345");
        let r = scan_sources(&[source(d.path(), false)]).unwrap();
        assert_eq!(r[0].size, 5);
        let ns = fs::metadata(d.path().join("a.jpg"))
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as i128;
        assert_eq!(r[0].mtime_ns, ns);
    }

    #[cfg(unix)]
    #[test]
    fn scan_ignores_symlinks() {
        let d = tempfile::tempdir().unwrap();
        touch(d.path(), "real.jpg", "x");
        std::os::unix::fs::symlink(d.path().join("real.jpg"), d.path().join("link.jpg")).unwrap();
        let r = scan_sources(&[source(d.path(), false)]).unwrap();
        assert_eq!(rels(&r), ["real.jpg"]);
    }

    #[test]
    fn files_vanishing_mid_scan_are_skipped() {
        // Syncthing may remove a file or a directory between the directory
        // listing and the metadata read (or the descent).
        let d = tempfile::tempdir().unwrap();
        touch(d.path(), "a/x.jpg", "x");
        touch(d.path(), "b.jpg", "x");
        touch(d.path(), "c.jpg", "x");
        let mut walk = WalkDir::new(d.path()).sort_by_file_name().into_iter();
        // The root is yielded and listed; then its entries disappear.
        assert_eq!(walk.next().unwrap().unwrap().depth(), 0);
        fs::remove_dir_all(d.path().join("a")).unwrap();
        fs::remove_file(d.path().join("b.jpg")).unwrap();
        let r = walk_files("s", d.path(), walk).unwrap();
        assert_eq!(rels(&r), ["c.jpg"]);
        // A missing source root is still an error.
        assert!(scan_sources(&[source(&d.path().join("gone"), false)]).is_err());
    }

    #[test]
    fn scan_expands_tilde() {
        let home = std::env::var("HOME").unwrap();
        let d = tempfile::tempdir_in(&home).unwrap();
        touch(d.path(), "a.jpg", "x");
        let name = d.path().file_name().unwrap().to_str().unwrap();
        let mut s = source(d.path(), false);
        s.path = PathBuf::from(format!("~/{name}"));
        let r = scan_sources(&[s]).unwrap();
        assert_eq!(rels(&r), ["a.jpg"]);
        assert_eq!(r[0].path, d.path().join("a.jpg"));
    }

    #[test]
    fn expand_tilde_cases() {
        use crate::config::expand_tilde;
        let home = PathBuf::from(std::env::var("HOME").unwrap());
        assert_eq!(expand_tilde(Path::new("~")), home);
        assert_eq!(expand_tilde(Path::new("~/a/b")), home.join("a/b"));
        assert_eq!(expand_tilde(Path::new("/x/~/y")), Path::new("/x/~/y"));
        assert_eq!(expand_tilde(Path::new("~user/x")), Path::new("~user/x"));
    }

    #[test]
    fn scan_reports_files_and_stops_on_cancel() {
        use crate::progress::testing::Recorder;
        use crate::progress::{Cancel, NoProgress, Stage};
        let d = tempfile::tempdir().unwrap();
        touch(d.path(), "a.jpg", "x");
        touch(d.path(), "b.jpg", "x");
        touch(d.path(), "sub/c.jpg", "x");
        let rec = Recorder::default();
        let r = scan_sources_with(&[source(d.path(), true)], &rec, &Cancel::new()).unwrap();
        assert_eq!(r.len(), 3);
        let evs = rec.events(Stage::Scan);
        // One per file while the total is unknown, then the final count.
        let pairs: Vec<(usize, usize)> = evs.iter().map(|e| (e.done, e.total)).collect();
        assert_eq!(pairs, [(1, 0), (2, 0), (3, 0), (3, 3)]);
        assert!(evs[..3].iter().all(|e| e.current.is_some()));

        let cancel = Cancel::new();
        cancel.cancel();
        let r = scan_sources_with(&[source(d.path(), true)], &NoProgress, &cancel);
        assert!(matches!(r, Err(Error::Cancelled)), "{r:?}");
    }
}
