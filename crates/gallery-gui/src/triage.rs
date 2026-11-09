//! "Unmatched triage": the plan's unmatched files in groups, each with
//! the config edits that would deal with it. The edits are ordinary
//! `config_edit` operations: the UI previews their exact TOML change, then
//! saves them with the file version the groups were read at, so a config
//! changed meanwhile is never overwritten.

use std::path::Path;

use gallery_core::config::{Config, expand_tilde};
use gallery_core::explain::ExplainOptions;
use gallery_core::mask::MaskItem;
use gallery_core::plan::Plan;
use gallery_core::presets::builtin_masks;
use gallery_core::triage::{Group, group_unmatched_with};
use serde::Serialize;

use crate::config_edit::EditOp;
use crate::config_edit::doc::read_draft;
use crate::config_edit::file;
use crate::config_edit::model::{
    ConfigDraft, FilterDraft, OutputDraft, SourceDraft, TargetDraft, TargetKind,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    /// Add the extension to a target's filter `match`.
    AddToTarget,
    /// A `skip` glob of the source for the group's folder.
    SkipFolder,
    /// A `skip` glob of the source for the extension everywhere in it.
    SkipSource,
    /// A new target at the end of the list for the group.
    CreateTarget,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SuggestedAction {
    pub kind: ActionKind,
    /// Short Russian label for the button or the list entry.
    pub label: String,
    /// `AddToTarget`: the target; `CreateTarget`: the proposed name.
    pub target: Option<String>,
    /// `CreateTarget`: the proposed output folder.
    pub dir: Option<String>,
    pub ops: Vec<EditOp>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GroupView {
    #[serde(flatten)]
    pub group: Group,
    pub actions: Vec<SuggestedAction>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TriageView {
    /// Version of the config the actions were made for; save with it.
    pub version: String,
    pub total: usize,
    pub groups: Vec<GroupView>,
}

/// The pattern for the group's extension: a built-in mask that is exactly
/// `*.ext` (`@png`), else `*.ext`. `None` for files without an extension.
pub fn ext_item(ext: &str) -> Option<String> {
    if ext.is_empty() {
        return None;
    }
    let pat = format!("*.{}", escape_glob(ext));
    let exact = builtin_masks().iter().find(|(_, e)| {
        matches!(e.0.as_slice(), [MaskItem::Pattern(m)] if m.as_str().eq_ignore_ascii_case(&pat))
    });
    Some(match exact {
        Some((name, _)) => format!("@{name}"),
        None => pat,
    })
}

/// A path part as a literal in a glob.
fn escape_glob(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if matches!(c, '*' | '?' | '[' | ']' | '{' | '}' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Suggested actions for `g`.
pub fn actions_for(g: &Group, draft: &ConfigDraft, cfg: &Config) -> Vec<SuggestedAction> {
    let Some(item) = ext_item(&g.ext) else {
        return Vec::new();
    };
    let mut out = add_to_target(g, &item, draft);
    out.extend(skip_actions(g, draft, cfg));
    out.push(create_target(g, &item, draft, cfg, None, None));
    out
}

fn add_to_target(g: &Group, item: &str, draft: &ConfigDraft) -> Vec<SuggestedAction> {
    let mut listing = Vec::new();
    let mut others = Vec::new();
    for (index, t) in draft.targets.iter().enumerate() {
        // Only targets that place files by their names.
        if !t.kind.is_folders() || t.has_ai() {
            continue;
        }
        let mut new = t.clone();
        let hit = new
            .filters
            .iter_mut()
            .find(|f| f.sources.contains(&g.source) && f.matches.is_some());
        let (label, bucket) = match hit {
            Some(f) => {
                let m = f.matches.as_mut().expect("checked");
                if m.iter().any(|x| x == item) {
                    continue;
                }
                m.push(item.to_string());
                (
                    gallery_core::t!("triage.add_to_match", item = item, target = t.name),
                    &mut listing,
                )
            }
            None if t.filters.iter().all(|f| !f.sources.contains(&g.source)) => {
                new.filters.push(FilterDraft {
                    sources: vec![g.source.clone()],
                    matches: Some(vec![item.to_string()]),
                    ..FilterDraft::default()
                });
                (
                    gallery_core::t!(
                        "triage.add_filter",
                        target = t.name,
                        source = g.source,
                        item = item
                    ),
                    &mut others,
                )
            }
            // Only filters without `match` list the source: they take any
            // file already.
            None => continue,
        };
        bucket.push(SuggestedAction {
            kind: ActionKind::AddToTarget,
            label,
            target: Some(t.name.clone()),
            dir: None,
            ops: vec![EditOp::UpdateTarget { index, target: new }],
        });
    }
    listing.extend(others);
    listing
}

fn skip_op(draft: &ConfigDraft, cfg: &Config, id: &str, glob: String) -> Option<EditOp> {
    let src = draft.sources.iter().find(|s| s.id == id)?;
    let mut skip = cfg
        .sources
        .iter()
        .find(|s| s.id == id)
        .map(|s| s.skip.clone())
        .or_else(|| src.skip.clone())?;
    if skip.contains(&glob) {
        return None;
    }
    skip.push(glob);
    let source = SourceDraft {
        skip: Some(skip),
        ..src.clone()
    };
    Some(EditOp::UpdateSource {
        id: id.to_string(),
        source,
    })
}

fn skip_actions(g: &Group, draft: &ConfigDraft, cfg: &Config) -> Vec<SuggestedAction> {
    let ext = escape_glob(&g.ext);
    let mut out = Vec::new();
    if !g.dir.is_empty() {
        let glob = format!("{}/*.{ext}", escape_glob(&g.dir));
        if let Some(op) = skip_op(draft, cfg, &g.source, glob.clone()) {
            out.push(SuggestedAction {
                kind: ActionKind::SkipFolder,
                label: gallery_core::t!("triage.ignore_folder", dir = g.dir, glob = glob),
                target: None,
                dir: None,
                ops: vec![op],
            });
        }
    }
    let glob = format!("*.{ext}");
    if let Some(op) = skip_op(draft, cfg, &g.source, glob.clone()) {
        out.push(SuggestedAction {
            kind: ActionKind::SkipSource,
            label: gallery_core::t!("triage.ignore_source", source = g.source, glob = glob),
            target: None,
            dir: None,
            ops: vec![op],
        });
    }
    out
}

/// "Create target…": a new target (at the end) taking the group's extension
/// from its source. `name` and `dir` default to a free name and the group's
/// own folder (files stay where they are; only renamed).
pub fn create_target(
    g: &Group,
    item: &str,
    draft: &ConfigDraft,
    cfg: &Config,
    name: Option<&str>,
    dir: Option<&str>,
) -> SuggestedAction {
    let taken: Vec<&str> = draft.targets.iter().map(|t| t.name.as_str()).collect();
    let name = match name {
        Some(n) => n.to_string(),
        None => free_name(&format!("{} .{}", g.source, g.ext), &taken),
    };
    let dir = match dir {
        Some(d) => d.to_string(),
        None => {
            let root = cfg
                .sources
                .iter()
                .find(|s| s.id == g.source)
                .map(|s| expand_tilde(&s.path))
                .unwrap_or_default();
            let at = if g.dir.is_empty() {
                root
            } else {
                root.join(&g.dir)
            };
            at.display().to_string()
        }
    };
    let target = TargetDraft {
        name: name.clone(),
        filters: vec![FilterDraft {
            sources: vec![g.source.clone()],
            matches: Some(vec![item.to_string()]),
            ..FilterDraft::default()
        }],
        outputs: vec![OutputDraft {
            dir: dir.clone(),
            rename: vec![crate::config_edit::model::StepDraft {
                preset: "capture-date".into(),
                mask: None,
            }],
            ..OutputDraft::default()
        }],
        ..TargetDraft::default()
    };
    SuggestedAction {
        kind: ActionKind::CreateTarget,
        label: gallery_core::t!("triage.create_target"),
        target: Some(name),
        dir: Some(dir),
        ops: vec![EditOp::AddTarget {
            kind: TargetKind::Folders,
            target,
        }],
    }
}

fn free_name(base: &str, taken: &[&str]) -> String {
    if !taken.contains(&base) {
        return base.to_string();
    }
    (2..)
        .map(|n| format!("{base} {n}"))
        .find(|n| !taken.contains(&n.as_str()))
        .expect("a free name")
}

struct Loaded {
    version: String,
    draft: ConfigDraft,
    cfg: Config,
}

fn load(config: &Path) -> Result<Loaded, String> {
    let (text, version) = file::read(config).map_err(|e| format!("{}: {e}", config.display()))?;
    // An old-shape file is offered the edits of its migrated list.
    let (doc, _) = crate::config_edit::ordered_doc(&text).map_err(|e| e.message)?;
    let draft = read_draft(&doc)?;
    let dir = config.parent().unwrap_or(Path::new(""));
    let label = config.file_name().map(Path::new).unwrap_or(config);
    let cfg = Config::from_str_at(&text, dir, label).map_err(|e| e.to_string())?;
    cfg.validate().map_err(|e| e.to_string())?;
    Ok(Loaded {
        version,
        draft,
        cfg,
    })
}

/// The groups of `plan`'s unmatched files under the config at `config`,
/// with their actions. Read-only.
pub fn view(config: &Path, plan: &Plan) -> Result<TriageView, String> {
    let l = load(config)?;
    let groups = group_unmatched_with(plan, &l.cfg, &ExplainOptions::default());
    Ok(TriageView {
        version: l.version,
        total: plan.unmatched.len(),
        groups: groups
            .into_iter()
            .map(|group| GroupView {
                actions: actions_for(&group, &l.draft, &l.cfg),
                group,
            })
            .collect(),
    })
}

/// "Create target…" for the group (`source`, folder `rel_dir` in it,
/// extension `ext`) with the name and output folder the user typed: the
/// operation and nothing else; the config at `config` must still have
/// `version`.
pub fn create_ops(
    config: &Path,
    version: &str,
    (source, rel_dir, ext): (&str, &str, &str),
    name: &str,
    dir: &str,
) -> Result<SuggestedAction, String> {
    let l = load(config)?;
    if l.version != version {
        return Err(gallery_core::t!("triage.changed"));
    }
    let item = ext_item(ext).ok_or_else(|| gallery_core::t!("triage.no_extension"))?;
    let g = Group {
        source: source.into(),
        dir: rel_dir.into(),
        ext: ext.into(),
        count: 0,
        size: 0,
        samples: Vec::new(),
        reason: String::new(),
    };
    Ok(create_target(
        &g,
        &item,
        &l.draft,
        &l.cfg,
        Some(name),
        Some(dir),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config_edit::{FailureKind, preview_change, save};
    use gallery_core::overrides::Overrides;
    use gallery_core::pipeline::NoAi;
    use gallery_core::plan::{Side, build_plan};
    use gallery_core::progress::{Cancel, NoProgress};
    use gallery_core::route::{RouteCtx, route_with_overrides};
    use gallery_core::scan::scan_sources;
    use std::collections::HashMap;
    use std::path::PathBuf;

    struct Fx {
        dir: tempfile::TempDir,
        cfg: PathBuf,
    }

    const FILES: [&str; 6] = [
        "shots/Screenshots/a.webm",
        "shots/Screenshots/b.webm",
        "shots/Screenshots/c.avif",
        "shots/Screenshots/ok.jpg",
        "shots/Other/d.webm",
        "lone/x.png",
    ];

    fn fixture() -> Fx {
        let dir = tempfile::tempdir().unwrap();
        let r = dir.path();
        for f in FILES {
            let p = r.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, b"x").unwrap();
        }
        let toml = format!(
            "# my notes\n[[sources]]\nid = \"shots\"   # phone\npath = \"{r}/shots\"\nrecursive = true\n\n\
             [[sources]]\nid = \"lone\"\npath = \"{r}/lone\"\n\n\
             # pics\n[[targets]]\nname = \"pics\"\n[[targets.filters]]\n\
             sources = [\"shots\"]\nmatch = \"*.jpg\"   # only jpg\n\
             [[targets.outputs]]\ndir = \"{r}/out\"\n",
            r = r.to_string_lossy().replace('\\', "/")
        );
        let cfg = r.join("gallery.toml");
        std::fs::write(&cfg, toml).unwrap();
        Fx { dir, cfg }
    }

    fn plan_of(cfg: &Path) -> Plan {
        let c = Config::load(cfg).unwrap();
        let files = scan_sources(&c.sources).unwrap();
        let routed = route_with_overrides(
            &c,
            &files,
            &RouteCtx::empty(&files),
            &NoAi,
            false,
            &Overrides::default(),
            &NoProgress,
            &Cancel::new(),
        )
        .unwrap();
        build_plan(&c, routed.decisions, &HashMap::new(), Side::Local).unwrap()
    }

    fn names(paths: &[PathBuf]) -> Vec<String> {
        let mut v: Vec<_> = paths
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    fn webm(v: &TriageView) -> &GroupView {
        v.groups
            .iter()
            .find(|g| g.group.ext == "webm" && g.group.dir == "Screenshots")
            .unwrap()
    }

    fn kind(g: &GroupView, k: ActionKind) -> &SuggestedAction {
        g.actions.iter().find(|a| a.kind == k).unwrap()
    }

    #[test]
    fn ext_item_uses_an_exact_builtin_mask() {
        assert_eq!(ext_item("png").as_deref(), Some("@png"));
        assert_eq!(ext_item("webm").as_deref(), Some("*.webm"));
        assert_eq!(ext_item(""), None);
        assert_eq!(ext_item("a[b").as_deref(), Some("*.a\\[b"));
    }

    #[test]
    fn view_groups_and_offers_every_action() {
        let fx = fixture();
        let v = view(&fx.cfg, &plan_of(&fx.cfg)).unwrap();
        assert_eq!(v.total, 5);
        let g = webm(&v);
        assert_eq!(g.group.count, 2);
        let kinds: Vec<_> = g.actions.iter().map(|a| a.kind).collect();
        assert_eq!(
            kinds,
            [
                ActionKind::AddToTarget,
                ActionKind::SkipFolder,
                ActionKind::SkipSource,
                ActionKind::CreateTarget
            ]
        );
        // The lone source has no target: the target is offered with a new filter.
        let lone = v.groups.iter().find(|g| g.group.source == "lone").unwrap();
        assert!(lone.actions[0].label.contains("new filter"));
        // The source root has no folder to skip separately.
        let root_less = v.groups.iter().find(|g| g.group.dir == "Other").unwrap();
        assert!(
            root_less
                .actions
                .iter()
                .any(|a| a.kind == ActionKind::SkipFolder)
        );
    }

    #[test]
    fn add_to_match_changes_only_that_line_and_plans_the_files() {
        let fx = fixture();
        let before = std::fs::read_to_string(&fx.cfg).unwrap();
        let v = view(&fx.cfg, &plan_of(&fx.cfg)).unwrap();
        let a = kind(webm(&v), ActionKind::AddToTarget);
        let p = preview_change(&fx.cfg, Some(&v.version), &a.ops).unwrap();
        assert!(p.error.is_none() && p.changed);
        let del: Vec<_> = p.diff.iter().filter(|l| l.kind == "del").collect();
        let add: Vec<_> = p.diff.iter().filter(|l| l.kind == "add").collect();
        assert_eq!(del.len(), 1);
        assert!(add[0].text.contains("*.jpg") && add[0].text.contains("*.webm"));
        assert_eq!(
            std::fs::read_to_string(&fx.cfg).unwrap(),
            before,
            "preview writes nothing"
        );

        let out = save(&fx.cfg, &v.version, &a.ops).unwrap();
        assert!(out.saved && out.error.is_none());
        let after = std::fs::read_to_string(&fx.cfg).unwrap();
        for keep in ["# my notes", "# phone", "# pics", "recursive = true"] {
            assert!(after.contains(keep), "{keep} lost:\n{after}");
        }
        let (b, a2): (Vec<_>, Vec<_>) = (before.lines().collect(), after.lines().collect());
        assert_eq!(b.len(), a2.len());
        assert_eq!(b.iter().zip(&a2).filter(|(x, y)| x != y).count(), 1);

        let plan = plan_of(&fx.cfg);
        assert!(!names(&plan.unmatched).contains(&"a.webm".to_string()));
        let planned: Vec<_> = plan.items.iter().map(|i| i.source.clone()).collect();
        assert!(names(&planned).contains(&"a.webm".to_string()));
        // The same webm in another folder follows: the filter is per source.
        assert!(names(&planned).contains(&"d.webm".to_string()));
    }

    #[test]
    fn folder_skip_glob_removes_only_that_folders_files_from_the_next_scan() {
        let fx = fixture();
        let v = view(&fx.cfg, &plan_of(&fx.cfg)).unwrap();
        let a = kind(webm(&v), ActionKind::SkipFolder);
        let out = save(&fx.cfg, &v.version, &a.ops).unwrap();
        assert!(out.saved, "{:?}", out.error);
        let text = std::fs::read_to_string(&fx.cfg).unwrap();
        assert!(text.contains("Screenshots/*.webm"), "{text}");
        assert!(text.contains(".stfolder"), "defaults kept:\n{text}");
        let c = Config::load(&fx.cfg).unwrap();
        let listed = scan_sources(&c.sources).unwrap();
        let rels: Vec<_> = listed
            .iter()
            .map(|f| f.rel.display().to_string().replace('\\', "/"))
            .collect();
        assert!(
            !rels.contains(&"Screenshots/a.webm".to_string()),
            "{rels:?}"
        );
        assert!(rels.contains(&"Other/d.webm".to_string()));
        assert!(rels.contains(&"Screenshots/c.avif".to_string()));
        let plan = plan_of(&fx.cfg);
        assert!(!names(&plan.unmatched).contains(&"a.webm".to_string()));
        assert!(names(&plan.unmatched).contains(&"d.webm".to_string()));
    }

    #[test]
    fn source_skip_glob_removes_the_extension_everywhere_in_the_source() {
        let fx = fixture();
        let v = view(&fx.cfg, &plan_of(&fx.cfg)).unwrap();
        let a = kind(webm(&v), ActionKind::SkipSource);
        save(&fx.cfg, &v.version, &a.ops).unwrap();
        let plan = plan_of(&fx.cfg);
        let u = names(&plan.unmatched);
        assert!(!u.iter().any(|n| n.ends_with(".webm")), "{u:?}");
        assert!(u.contains(&"c.avif".to_string()));
    }

    #[test]
    fn create_target_with_typed_name_and_folder_then_validated() {
        let fx = fixture();
        let v = view(&fx.cfg, &plan_of(&fx.cfg)).unwrap();
        let d = fx.dir.path().display();
        let ops = |name: &str| {
            create_ops(
                &fx.cfg,
                &v.version,
                ("shots", "Screenshots", "avif"),
                name,
                &format!("{d}/shots/Screenshots"),
            )
            .unwrap()
        };
        // Default proposal: a free name, the group's own folder.
        let dflt = kind(
            v.groups.iter().find(|g| g.group.ext == "avif").unwrap(),
            ActionKind::CreateTarget,
        );
        assert!(
            dflt.dir
                .as_deref()
                .unwrap()
                .replace('\\', "/")
                .ends_with("shots/Screenshots")
        );

        // An empty name does not validate: nothing is written.
        let bad = ops("");
        let out = save(&fx.cfg, &v.version, &bad.ops).unwrap();
        assert!(out.error.is_some() && !out.saved);

        let good = ops("Avatifs");
        let p = preview_change(&fx.cfg, Some(&v.version), &good.ops).unwrap();
        assert!(p.diff.iter().all(|l| l.kind != "del"));
        assert!(p.diff.iter().any(|l| l.text.contains("Avatifs")));
        assert!(save(&fx.cfg, &v.version, &good.ops).unwrap().saved);
        let plan = plan_of(&fx.cfg);
        assert!(!names(&plan.unmatched).contains(&"c.avif".to_string()));
        // Routed to the new target: planned, or already as it should be.
        let routed = plan.items.iter().any(|i| i.source.ends_with("c.avif"))
            || plan.unchanged.iter().any(|p| p.ends_with("c.avif"));
        assert!(routed, "{plan:?}");
    }

    #[test]
    fn a_changed_config_is_refused() {
        let fx = fixture();
        let v = view(&fx.cfg, &plan_of(&fx.cfg)).unwrap();
        let a = kind(webm(&v), ActionKind::AddToTarget).clone();
        let mut text = std::fs::read_to_string(&fx.cfg).unwrap();
        text.push_str("\n# edited outside\n");
        std::fs::write(&fx.cfg, &text).unwrap();
        let e = save(&fx.cfg, &v.version, &a.ops).unwrap_err();
        assert_eq!(e.kind, FailureKind::Changed);
        let e = preview_change(&fx.cfg, Some(&v.version), &a.ops).unwrap_err();
        assert_eq!(e.kind, FailureKind::Changed);
        assert!(create_ops(&fx.cfg, &v.version, ("shots", "", "avif"), "x", "/x").is_err());
        assert_eq!(std::fs::read_to_string(&fx.cfg).unwrap(), text);
    }

    #[test]
    fn no_actions_for_files_without_extension() {
        let fx = fixture();
        let c = Config::load(&fx.cfg).unwrap();
        let (text, _) = file::read(&fx.cfg).unwrap();
        let draft = read_draft(&text.parse().unwrap()).unwrap();
        let g = Group {
            source: "shots".into(),
            dir: String::new(),
            ext: String::new(),
            count: 1,
            size: 0,
            samples: vec![],
            reason: String::new(),
        };
        assert!(actions_for(&g, &draft, &c).is_empty());
    }
}
