//! "Triage of unmatched": the plan's unmatched files grouped by where they
//! lie and what they are, each group with the reason the scan leaves it
//! alone. Read-only; the reason is the core's `explain` verdict of one file
//! per group, so it cannot drift from what a scan does.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::config::{Config, expand_tilde};
use crate::explain::{ExplainOptions, explain};
use crate::media::split_name;
use crate::overrides::Overrides;
use crate::plan::Plan;

/// How many file names a group shows.
pub const SAMPLES: usize = 8;

/// Unmatched files of one source, folder and extension.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Group {
    pub source: String,
    /// Folder relative to the source root, `/`-separated; empty: the root.
    pub dir: String,
    /// Lower-case extension without the dot; empty: files without one.
    pub ext: String,
    pub count: usize,
    /// Total size in bytes (files that vanished count 0).
    pub size: u64,
    /// The first files by path, at most [`SAMPLES`].
    pub samples: Vec<PathBuf>,
    /// Russian line: why no target takes these files (see `explain`).
    pub reason: String,
}

fn locate<'a>(cfg: &'a Config, path: &Path) -> Option<(&'a str, PathBuf)> {
    cfg.sources
        .iter()
        .filter_map(|s| {
            let root = expand_tilde(&s.path);
            let rel = path.strip_prefix(&root).ok()?;
            Some((root.components().count(), s.id.as_str(), rel.to_path_buf()))
        })
        .max_by_key(|(depth, _, _)| *depth)
        .map(|(_, id, rel)| (id, rel))
}

/// The unmatched files of `plan` in groups, largest first (count, then
/// size). One file per group is explained.
pub fn group_unmatched(plan: &Plan, cfg: &Config) -> Vec<Group> {
    group_unmatched_with(plan, cfg, &ExplainOptions::default())
}

/// [`group_unmatched`] with the explain options (AI cache, tools).
pub fn group_unmatched_with(plan: &Plan, cfg: &Config, opts: &ExplainOptions) -> Vec<Group> {
    type Key = (String, String, String);
    let mut by: BTreeMap<Key, Vec<(&PathBuf, u64)>> = BTreeMap::new();
    for p in &plan.unmatched {
        let (source, rel) = locate(cfg, p).unwrap_or(("", p.clone()));
        let dir = rel
            .parent()
            .map(|d| {
                d.components()
                    .map(|c| c.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/")
            })
            .unwrap_or_default();
        let name = rel.file_name().map(|n| n.to_string_lossy().into_owned());
        let ext = split_name(&name.unwrap_or_default()).1.to_lowercase();
        let size = std::fs::symlink_metadata(p).map_or(0, |m| m.len());
        by.entry((source.to_string(), dir, ext))
            .or_default()
            .push((p, size));
    }
    let mut out: Vec<Group> = by
        .into_iter()
        .map(|((source, dir, ext), mut files)| {
            files.sort();
            let reason = match explain(cfg, files[0].0, &Overrides::default(), opts) {
                Ok(e) => e.verdict,
                Err(e) => format!("could not explain: {e}"),
            };
            Group {
                count: files.len(),
                size: files.iter().map(|(_, s)| s).sum(),
                samples: files
                    .iter()
                    .take(SAMPLES)
                    .map(|(p, _)| (*p).clone())
                    .collect(),
                source,
                dir,
                ext,
                reason,
            }
        })
        .collect();
    out.sort_by(|a, b| {
        (b.count, b.size)
            .cmp(&(a.count, a.size))
            .then_with(|| (&a.source, &a.dir, &a.ext).cmp(&(&b.source, &b.dir, &b.ext)))
    });
    out
}

/// Human-readable size text: `1.5 MB`.
pub fn size_text(bytes: u64) -> String {
    const U: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} {}", U[0])
    } else {
        format!("{v:.1} {}", U[i])
    }
}

/// The group as one table line: `source/dir  .ext  count  size  reason`.
pub fn table_line(g: &Group) -> String {
    let place = if g.dir.is_empty() {
        g.source.clone()
    } else {
        format!("{}/{}", g.source, g.dir)
    };
    let ext = if g.ext.is_empty() {
        "(no extension)".to_string()
    } else {
        format!(".{}", g.ext)
    };
    format!(
        "{place}\t{ext}\t{}\t{}\t{}",
        g.count,
        size_text(g.size),
        g.reason
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::NoAi;
    use crate::plan::{Side, build_plan};
    use crate::progress::{Cancel, NoProgress};
    use crate::route::route_with_overrides;
    use crate::scan::scan_sources;
    use std::collections::HashMap;

    struct Fx {
        dir: tempfile::TempDir,
        cfg: Config,
    }

    fn touch(p: &Path, n: usize) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, vec![b'x'; n]).unwrap();
    }

    fn fixture() -> Fx {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        for (f, n) in [
            ("shots/Screenshots/a.webm", 100),
            ("shots/Screenshots/b.WEBM", 200),
            ("shots/Screenshots/c.avif", 10),
            ("shots/Screenshots/ok.jpg", 5),
            ("shots/notes.txt", 7),
            ("lone/x.mp4", 3),
        ] {
            touch(&r.join(f), n);
        }
        let toml = format!(
            "[[sources]]\nid = \"shots\"\npath = \"{r}/shots\"\nrecursive = true\n\
             [[sources]]\nid = \"lone\"\npath = \"{r}/lone\"\n\
             [[targets.after_ai]]\nname = \"pics\"\n[[targets.after_ai.filters]]\n\
             sources = [\"shots\"]\nmatch = \"*.jpg\"\n[[targets.after_ai.outputs]]\ndir = \"{r}/out\"\n",
            r = r.display()
        );
        let cfg = Config::from_str_at(&toml, r, Path::new("t.toml")).unwrap();
        cfg.validate().unwrap();
        Fx { dir, cfg }
    }

    fn plan_of(fx: &Fx) -> Plan {
        let files = scan_sources(&fx.cfg.sources).unwrap();
        let routed = route_with_overrides(
            &fx.cfg,
            &files,
            &NoAi,
            false,
            &Overrides::default(),
            &NoProgress,
            &Cancel::new(),
        )
        .unwrap();
        build_plan(&fx.cfg, routed.decisions, &HashMap::new(), Side::Local).unwrap()
    }

    fn groups(fx: &Fx) -> Vec<Group> {
        let opts = ExplainOptions {
            tools: None,
            ai_cache: Some(fx.dir.path().join("ai.jsonl")),
        };
        group_unmatched_with(&plan_of(fx), &fx.cfg, &opts)
    }

    #[test]
    fn groups_by_source_folder_and_extension_largest_first() {
        let fx = fixture();
        let g = groups(&fx);
        let key: Vec<_> = g
            .iter()
            .map(|g| (g.source.as_str(), g.dir.as_str(), g.ext.as_str(), g.count))
            .collect();
        assert_eq!(
            key,
            [
                ("shots", "Screenshots", "webm", 2),
                ("shots", "Screenshots", "avif", 1),
                ("shots", "", "txt", 1),
                ("lone", "", "mp4", 1),
            ]
        );
        assert_eq!(g[0].size, 300);
        assert_eq!(g[0].samples.len(), 2);
        assert!(g[0].samples[0].ends_with("Screenshots/a.webm"));
    }

    #[test]
    fn reason_comes_from_explain_of_one_file() {
        let fx = fixture();
        let g = groups(&fx);
        let webm = &g[0].reason;
        assert!(webm.contains(".webm") && webm.contains("pics"), "{webm}");
        let lone = g.iter().find(|g| g.source == "lone").unwrap();
        assert!(lone.reason.contains("lone"), "{}", lone.reason);
    }

    #[test]
    fn empty_plan_has_no_groups() {
        let fx = fixture();
        let mut p = plan_of(&fx);
        p.unmatched.clear();
        assert!(group_unmatched(&p, &fx.cfg).is_empty());
    }

    #[test]
    fn size_and_table_text() {
        assert_eq!(size_text(5), "5 B");
        assert_eq!(size_text(1536), "1.5 KB");
        let fx = fixture();
        let l = table_line(&groups(&fx)[0]);
        assert!(l.starts_with("shots/Screenshots\t.webm\t2\t300 B\t"), "{l}");
    }
}
