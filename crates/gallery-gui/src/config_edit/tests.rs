use std::path::{Path, PathBuf};

use super::model::{FilterDraft, OutputDraft, SourceDraft, StepDraft, TargetDraft, TargetKind};
use super::*;

/// A hand-written config: comments everywhere, mixed spacing, inline
/// comments, single-string and list masks.
const COMMENTED: &str = r#"# My gallery — hand-written, keep my notes!

# The camera.
[[sources]]
id = "camera"   # phone
path = "/tmp/gallery-edit-test/camera"
ai_prefix_file = "prompts/camera.md"

[[sources]]
id = "shots"
path = "/tmp/gallery-edit-test/shots"
skip = [".stfolder", "*.md"]

[ai]
model = "claude-sonnet-5-5"   # cheap enough

# ---- by name ----

# Screenshots: renamed in place.
[[targets]]
name = "Screenshots"
[[targets.filters]]
sources = ["shots"]
match = "*"
exclude = ["@dated"]
[[targets.outputs]]
dir = "/tmp/gallery-edit-test/shots"
rename = ["capture-date"]

# TikTok junk.
[[targets]]
name = "TikTok"
[[targets.filters]]
sources = ["camera"]
match = "@tiktok"   # 32 hex digits
[[targets.outputs]]
dir = "/tmp/gallery-edit-test/out/tiktok"

# ---- AI ----

[[targets]]
name = "Documents"
[[targets.filters]]
sources = ["camera"]
match = "@photo"
ai = "Photos of documents."
[[targets.outputs]]
dir = "/tmp/gallery-edit-test/out/docs"
rename = ["capture-date"]

# ---- the rest ----

# The timeline: everything else.
[[targets]]
name = "Timeline"
[[targets.filters]]
sources = ["camera"]
match = ["@photo", "@video"]   # photos and videos only
[[targets.outputs]]
dir = "/tmp/gallery-edit-test/out/timeline/{YYYY}"
content = ["meta-full", "avif-high", "av1-high"]
rename = ["capture-date"]

# Backup copy.
[[targets.outputs]]
dir = "/tmp/gallery-edit-test/backup"

# trailing note
"#;

/// The same config in the old three-stage shape.
const STAGED: &str = r#"# My gallery — hand-written, keep my notes!

# The camera.
[[sources]]
id = "camera"   # phone
path = "/tmp/gallery-edit-test/camera"
ai_prefix_file = "prompts/camera.md"

[[sources]]
id = "shots"
path = "/tmp/gallery-edit-test/shots"
skip = [".stfolder", "*.md"]

[ai]
model = "claude-sonnet-5-5"   # cheap enough

# ---- stage 1: masks before AI ----

# Screenshots: renamed in place.
[[targets.before_ai]]
name = "Screenshots"
[[targets.before_ai.filters]]
sources = ["shots"]
match = "*"
exclude = ["@dated"]
[[targets.before_ai.outputs]]
dir = "/tmp/gallery-edit-test/shots"
rename = ["capture-date"]

# TikTok junk.
[[targets.before_ai]]
name = "TikTok"
[[targets.before_ai.filters]]
sources = ["camera"]
match = "@tiktok"   # 32 hex digits
[[targets.before_ai.outputs]]
dir = "/tmp/gallery-edit-test/out/tiktok"

# ---- stage 2: AI ----

[[targets.ai]]
name = "Documents"
sources = ["camera"]
# What the AI is told.
description = "Photos of documents."
[targets.ai.prefilter]
match = "@photo"
[[targets.ai.outputs]]
dir = "/tmp/gallery-edit-test/out/docs"
rename = ["capture-date"]

# ---- stage 3: after AI ----

# The timeline: everything else.
[[targets.after_ai]]
name = "Timeline"
[[targets.after_ai.filters]]
sources = ["camera"]
match = ["@photo", "@video"]   # photos and videos only
[[targets.after_ai.outputs]]
dir = "/tmp/gallery-edit-test/out/timeline/{YYYY}"
content = ["meta-full", "avif-high", "av1-high"]
rename = ["capture-date"]

# Backup copy.
[[targets.after_ai.outputs]]
dir = "/tmp/gallery-edit-test/backup"

# trailing note
"#;

struct Fx {
    _dir: tempfile::TempDir,
    path: PathBuf,
}

fn fixture(text: &str) -> Fx {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("prompts")).unwrap();
    std::fs::write(dir.path().join("prompts/camera.md"), "Phone shots.\n").unwrap();
    let path = dir.path().join("gallery.toml");
    std::fs::write(&path, text).unwrap();
    Fx { _dir: dir, path }
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap()
}

fn version(p: &Path) -> String {
    file::version_of(&std::fs::read(p).unwrap())
}

fn target(fx: &Fx, i: usize) -> TargetDraft {
    draft(&fx.path, None, &[]).unwrap().draft.targets[i].clone()
}

fn update(index: usize, target: TargetDraft) -> EditOp {
    EditOp::UpdateTarget { index, target }
}

/// The lines of `a` and `b` that differ, after the common head and tail.
fn diff(a: &str, b: &str) -> (Vec<String>, Vec<String>) {
    let (a, b): (Vec<&str>, Vec<&str>) = (a.lines().collect(), b.lines().collect());
    let head = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let tail = a[head..]
        .iter()
        .rev()
        .zip(b[head..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let cut = |v: &[&str]| {
        v[head..v.len() - tail]
            .iter()
            .map(|s| s.to_string())
            .collect()
    };
    (cut(&a), cut(&b))
}

fn save_ok(fx: &Fx, ops: &[EditOp]) -> EditOutcome {
    let out = save(&fx.path, &version(&fx.path), ops).unwrap();
    assert!(out.error.is_none(), "{:?}", out.error);
    assert!(out.saved);
    out
}

fn names(cfg: &Config) -> Vec<&str> {
    cfg.targets.iter().map(|t| t.name.as_str()).collect()
}

fn condition(key: &str, value: serde_json::Value) -> ConditionValue {
    ConditionValue::from_json(key, &value).unwrap()
}

#[test]
fn reads_the_draft() {
    let fx = fixture(COMMENTED);
    let out = draft(&fx.path, None, &[]).unwrap();
    assert!(out.error.is_none(), "{:?}", out.error);
    assert!(!out.changed && !out.old_shape);
    let d = out.draft;
    assert_eq!(d.sources.len(), 2);
    assert_eq!(
        d.sources[0].ai_prefix_file.as_deref(),
        Some("prompts/camera.md")
    );
    assert_eq!(d.sources[0].skip, None);
    assert_eq!(
        d.sources[1].skip.as_deref(),
        Some(&[".stfolder".to_string(), "*.md".into()][..])
    );
    let names: Vec<&str> = d.targets.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["Screenshots", "TikTok", "Documents", "Timeline"]);
    let b = &d.targets[0];
    assert_eq!(b.kind, TargetKind::Folders);
    assert_eq!(
        b.filters[0].matches.as_deref(),
        Some(&["*".to_string()][..])
    );
    assert_eq!(b.filters[0].exclude, vec!["@dated".to_string()]);
    let a = &d.targets[2].filters[0];
    assert_eq!(a.ai.as_deref(), Some("Photos of documents."));
    assert_eq!(a.matches.as_deref(), Some(&["@photo".to_string()][..]));
    let t = &d.targets[3];
    assert_eq!(t.outputs.len(), 2);
    assert_eq!(t.outputs[0].content.len(), 3);
    assert_eq!(out.version, version(&fx.path));
}

#[test]
fn reads_kinds_conditions_and_places() {
    let fx = fixture(
        r#"[[sources]]
id = "camera"
path = "/tmp/gallery-edit-test/camera"

[places]
home = { lat = 47.0245, lon = 28.8323, km = 2 }

[places.dacha]
lat = 46.98
lon = 28.7
km = 1.5

[[targets]]
name = "Night at home"
kind = "keep"
[[targets.filters]]
sources = ["camera"]
geo = "@home"
date = { time = "22:00..05:00", not = true }
expand = "event"
"#,
    );
    let out = draft(&fx.path, None, &[]).unwrap();
    assert!(out.error.is_none(), "{:?}", out.error);
    let d = out.draft;
    assert_eq!(d.places.len(), 2);
    assert_eq!(d.places[0].name, "home");
    assert_eq!(d.places[0].km, 2.0);
    assert_eq!(d.places[1].lon, 28.7);
    let t = &d.targets[0];
    assert_eq!(t.kind, TargetKind::Keep);
    let f = &t.filters[0];
    assert_eq!(f.expand.as_deref(), Some("event"));
    assert_eq!(f.conditions["geo"], serde_json::json!("@home"));
    assert_eq!(
        f.conditions["date"],
        serde_json::json!({ "time": "22:00..05:00", "not": true })
    );
}

#[test]
fn editing_one_target_changes_only_its_line() {
    let fx = fixture(COMMENTED);
    let mut t = target(&fx, 1);
    t.outputs[0].dir = "/tmp/gallery-edit-test/out/tt".into();
    save_ok(&fx, &[update(1, t)]);
    let after = read(&fx.path);
    let (old, new) = diff(COMMENTED, &after);
    assert_eq!(old, vec![r#"dir = "/tmp/gallery-edit-test/out/tiktok""#]);
    assert_eq!(new, vec![r#"dir = "/tmp/gallery-edit-test/out/tt""#]);
}

#[test]
fn changed_value_keeps_its_inline_comment_and_list_form() {
    let fx = fixture(COMMENTED);
    let mut t = target(&fx, 3);
    t.filters[0].matches = Some(vec!["@photo".into()]);
    save_ok(&fx, &[update(3, t)]);
    let (old, new) = diff(COMMENTED, &read(&fx.path));
    assert_eq!(
        old,
        vec![r#"match = ["@photo", "@video"]   # photos and videos only"#]
    );
    assert_eq!(
        new,
        vec![r#"match = ["@photo"]   # photos and videos only"#]
    );
}

#[test]
fn no_op_edit_does_not_write() {
    let fx = fixture(COMMENTED);
    let t = target(&fx, 3);
    let out = save(&fx.path, &version(&fx.path), &[update(3, t)]).unwrap();
    assert!(!out.changed && !out.saved);
    assert!(!file::backup_path(&fx.path).exists());
    assert_eq!(read(&fx.path), COMMENTED);
}

#[test]
fn adds_filters_outputs_and_masked_steps() {
    let fx = fixture(COMMENTED);
    let mut t = target(&fx, 1);
    t.filters.push(FilterDraft {
        sources: vec!["shots".into()],
        matches: Some(vec!["@tiktok".into(), "*.webm".into()]),
        ..Default::default()
    });
    t.outputs.push(OutputDraft {
        dir: "/tmp/gallery-edit-test/out/tt2".into(),
        server_dir: Some("/srv/tt".into()),
        content: vec![StepDraft {
            preset: "avif-high".into(),
            mask: Some(vec!["*.png".into()]),
        }],
        rename: vec![StepDraft {
            preset: "capture-date".into(),
            mask: None,
        }],
    });
    let mut ai = target(&fx, 2);
    ai.filters[0].exclude = vec!["@screenshot".into()];
    ai.filters[0].ai = Some("Documents and forms.".into());
    save_ok(&fx, &[update(1, t.clone()), update(2, ai.clone())]);
    let text = read(&fx.path);
    let cfg = Config::load(&fx.path).unwrap();
    cfg.validate().unwrap();
    let tt = &cfg.targets[1];
    assert_eq!(tt.filters.len(), 2);
    assert_eq!(tt.outputs.len(), 2);
    assert_eq!(tt.outputs[1].server_dir.as_deref(), Some("/srv/tt"));
    assert_eq!(tt.outputs[1].content[0].preset, "avif-high");
    assert!(tt.outputs[1].content[0].mask.is_some());
    assert!(
        text.contains(r#"content = [{ mask = "*.png", preset = "avif-high" }]"#),
        "{text}"
    );
    // The new filter sits after the old one, before the outputs.
    let f2 = text.find(r#"match = ["@tiktok", "*.webm"]"#).unwrap();
    let o1 = text
        .find(r#"dir = "/tmp/gallery-edit-test/out/tiktok""#)
        .unwrap();
    let o2 = text.find("out/tt2").unwrap();
    assert!(
        text.find(r#"match = "@tiktok"   # 32 hex digits"#).unwrap() < f2 && f2 < o1 && o1 < o2,
        "{text}"
    );
    let docs = &cfg.targets[2].filters[0];
    assert_eq!(docs.ai.as_deref(), Some("Documents and forms."));
    assert!(!docs.exclude.is_empty());
    // And the draft reads back what was written.
    let d = draft(&fx.path, None, &[]).unwrap().draft;
    assert_eq!(d.targets[1], t);
    assert_eq!(d.targets[2], ai);
}

#[test]
fn new_filter_goes_before_outputs() {
    let text = COMMENTED.replace(
        "[[targets.filters]]\nsources = [\"camera\"]\nmatch = \"@photo\"\nai = \"Photos of documents.\"\n",
        "",
    );
    let fx = fixture(&text);
    let mut t = target(&fx, 2);
    assert!(t.filters.is_empty());
    t.filters.push(FilterDraft {
        sources: vec!["camera".into()],
        ai: Some("Receipts.".into()),
        ..Default::default()
    });
    save_ok(&fx, &[update(2, t)]);
    let (old, new) = diff(&text, &read(&fx.path));
    assert!(old.is_empty(), "{old:?}");
    assert_eq!(
        new,
        vec![
            "[[targets.filters]]",
            r#"sources = ["camera"]"#,
            r#"ai = "Receipts.""#
        ]
    );
}

#[test]
fn add_update_remove_source() {
    let fx = fixture(COMMENTED);
    let src = SourceDraft {
        id: "photos".into(),
        path: "/tmp/gallery-edit-test/photos".into(),
        recursive: true,
        ..Default::default()
    };
    save_ok(&fx, &[EditOp::AddSource { source: src }]);
    let (old, new) = diff(COMMENTED, &read(&fx.path));
    assert!(old.is_empty());
    assert_eq!(
        new,
        // (The blank line before the new block, aligned after it by `diff`.)
        vec![
            "[[sources]]",
            r#"id = "photos""#,
            r#"path = "/tmp/gallery-edit-test/photos""#,
            "recursive = true",
            ""
        ]
    );

    // Renaming an id renames it in every target that names it.
    let before = read(&fx.path);
    let mut cam = draft(&fx.path, None, &[]).unwrap().draft.sources[0].clone();
    cam.id = "phone".into();
    save_ok(
        &fx,
        &[EditOp::UpdateSource {
            id: "camera".into(),
            source: cam,
        }],
    );
    let after = read(&fx.path);
    assert!(after.contains(r#"id = "phone"   # phone"#), "{after}");
    assert!(!after.contains(r#""camera""#), "{after}");
    assert_eq!(after.lines().count(), before.lines().count());
    assert!(after.contains("match = \"@tiktok\"   # 32 hex digits"));

    // Removing drops it from the targets' source lists, comments stay.
    save_ok(&fx, &[EditOp::RemoveSource { id: "shots".into() }]);
    let after = read(&fx.path);
    assert!(!after.contains("id = \"shots\""));
    assert!(after.contains("sources = []"), "{after}");
    assert!(after.contains("# The camera.\n[[sources]]"));
    let out = draft(&fx.path, None, &[]).unwrap();
    assert!(
        out.warnings
            .iter()
            .any(|w| w.path == "targets[0].filters[0].sources"),
        "{:?}",
        out.warnings
    );
}

/// Two sources and two targets, for the conditions.
const TWO_SOURCES: &str = r#"[[sources]]
id = "camera"
path = "/tmp/gallery-edit-test/camera"

[[sources]]
id = "partner"
path = "/tmp/gallery-edit-test/partner"

# Shared with Partner.
[[targets]]
name = "Together"
[[targets.filters]]
sources = ["camera"]   # mine
match = "@media"
[[targets.outputs]]
dir = "/tmp/gallery-edit-test/shared"

[[targets]]
name = "Timeline"
[[targets.filters]]
sources = ["camera", "partner"]
[[targets.outputs]]
dir = "/tmp/gallery-edit-test/timeline"
"#;

#[test]
fn set_condition_writes_an_inline_table_in_place() {
    let fx = fixture(TWO_SOURCES);
    let near = condition(
        "near",
        serde_json::json!({ "km": 1.0, "time": "2h", "sources": ["partner"] }),
    );
    save_ok(
        &fx,
        &[EditOp::SetCondition {
            target: "Together".into(),
            filter: 0,
            condition: near,
        }],
    );
    let (old, new) = diff(TWO_SOURCES, &read(&fx.path));
    assert!(old.is_empty(), "{old:?}");
    assert_eq!(
        new,
        vec![r#"near = { sources = ["partner"], time = "2h", km = 1.0 }"#]
    );
    let cfg = Config::load(&fx.path).unwrap();
    cfg.validate().unwrap();
    assert!(cfg.targets[0].filters[0].near.is_some());

    // Replaced in place, its line kept.
    let near = condition(
        "near",
        serde_json::json!({ "sources": ["partner"], "time": "30m", "not": true }),
    );
    save_ok(
        &fx,
        &[EditOp::SetCondition {
            target: "Together".into(),
            filter: 0,
            condition: near,
        }],
    );
    assert!(
        read(&fx.path).contains(
            "match = \"@media\"\nnear = { sources = [\"partner\"], time = \"30m\", not = true }\n"
        ),
        "{}",
        read(&fx.path)
    );

    // A source renamed or removed follows into `near`.
    let mut partner = draft(&fx.path, None, &[]).unwrap().draft.sources[1].clone();
    partner.id = "wife".into();
    save_ok(
        &fx,
        &[EditOp::UpdateSource {
            id: "partner".into(),
            source: partner,
        }],
    );
    assert!(read(&fx.path).contains("near = { sources = [\"wife\"], time = \"30m\", not = true }"));
}

#[test]
fn remove_condition_leaves_the_filter_valid() {
    let fx = fixture(TWO_SOURCES);
    let ops = [
        EditOp::SetCondition {
            target: "Together".into(),
            filter: 0,
            condition: condition("geo", serde_json::json!({ "has_gps": false })),
        },
        EditOp::SetCondition {
            target: "Together".into(),
            filter: 0,
            condition: condition("duplicate", serde_json::json!({})),
        },
    ];
    save_ok(&fx, &ops);
    assert!(read(&fx.path).contains("geo = { has_gps = false }\nduplicate = {}\n"));
    save_ok(
        &fx,
        &[
            EditOp::RemoveCondition {
                target: "Together".into(),
                filter: 0,
                key: "geo".into(),
            },
            EditOp::RemoveCondition {
                target: "Together".into(),
                filter: 0,
                key: "duplicate".into(),
            },
        ],
    );
    assert_eq!(read(&fx.path), TWO_SOURCES);
    let cfg = Config::load(&fx.path).unwrap();
    cfg.validate().unwrap();

    // A key that is not a condition, or not set, or a filter that is not there.
    for (key, filter) in [("name", 0), ("near", 0), ("geo", 3)] {
        let e = draft(
            &fx.path,
            None,
            &[EditOp::RemoveCondition {
                target: "Together".into(),
                filter,
                key: key.into(),
            }],
        )
        .unwrap_err();
        assert_eq!(e.kind, FailureKind::Op, "{key}");
    }
    let e = draft(
        &fx.path,
        None,
        &[EditOp::SetTargetKind {
            name: "Nope".into(),
            kind: TargetKind::Trash,
        }],
    )
    .unwrap_err();
    assert!(e.message.contains("Nope"), "{e:?}");
}

#[test]
fn set_place_writes_places_after_the_sources() {
    let fx = fixture(TWO_SOURCES);
    let home = gallery_core::config::Place {
        lat: 47.0245,
        lon: 28.8323,
        km: 2.0,
    };
    save_ok(
        &fx,
        &[
            EditOp::SetPlace {
                name: "home".into(),
                place: home,
            },
            EditOp::SetCondition {
                target: "Together".into(),
                filter: 0,
                condition: condition("geo", serde_json::json!("@home")),
            },
        ],
    );
    let after = read(&fx.path);
    assert!(
        after.contains(
            "path = \"/tmp/gallery-edit-test/partner\"\n\n[places]\nhome = { lat = 47.0245, lon = 28.8323, km = 2.0 }\n\n# Shared with Partner.\n[[targets]]"
        ),
        "{after}"
    );
    let cfg = Config::load(&fx.path).unwrap();
    cfg.validate().unwrap();
    assert_eq!(cfg.places["home"], home);
    let d = draft(&fx.path, None, &[]).unwrap().draft;
    assert_eq!(d.places[0].name, "home");

    // Replaced in place; another one added after it.
    save_ok(
        &fx,
        &[
            EditOp::SetPlace {
                name: "home".into(),
                place: gallery_core::config::Place { km: 3.0, ..home },
            },
            EditOp::SetPlace {
                name: "dacha".into(),
                place: gallery_core::config::Place {
                    lat: 46.98,
                    lon: 28.7,
                    km: 1.0,
                },
            },
        ],
    );
    assert!(read(&fx.path).contains(
        "[places]\nhome = { lat = 47.0245, lon = 28.8323, km = 3.0 }\ndacha = { lat = 46.98, lon = 28.7, km = 1.0 }\n"
    ));

    // A place still named by a condition cannot go: the check says why.
    let out = draft(
        &fx.path,
        None,
        &[EditOp::RemovePlace {
            name: "home".into(),
        }],
    )
    .unwrap();
    let e = out.error.expect("unknown place");
    assert_eq!(
        e.path.as_deref(),
        Some("targets[0].filters[0].geo"),
        "{e:?}"
    );
    assert!(e.message.contains("@home"), "{e:?}");
    // The last place gone: `[places]` too.
    save_ok(
        &fx,
        &[
            EditOp::RemoveCondition {
                target: "Together".into(),
                filter: 0,
                key: "geo".into(),
            },
            EditOp::RemovePlace {
                name: "home".into(),
            },
            EditOp::RemovePlace {
                name: "dacha".into(),
            },
        ],
    );
    assert_eq!(read(&fx.path), TWO_SOURCES);
}

#[test]
fn set_target_kind_and_its_check() {
    let fx = fixture(TWO_SOURCES);
    let trash = EditOp::SetTargetKind {
        name: "Together".into(),
        kind: TargetKind::Trash,
    };
    // A trash target keeps no outputs.
    let out = draft(&fx.path, None, std::slice::from_ref(&trash)).unwrap();
    let e = out.error.expect("outputs of a trash target");
    assert_eq!(e.path.as_deref(), Some("targets[0].outputs"), "{e:?}");
    assert_eq!(out.draft.targets[0].kind, TargetKind::Trash);
    let mut t = out.draft.targets[0].clone();
    t.outputs.clear();
    save_ok(&fx, &[trash, update(0, t)]);
    let after = read(&fx.path);
    assert!(
        after.contains("[[targets]]\nname = \"Together\"\nkind = \"trash\"\n[[targets.filters]]"),
        "{after}"
    );
    assert!(!after.contains("/shared"));
    // No warning about missing outputs for it.
    let out = draft(&fx.path, None, &[]).unwrap();
    assert!(
        out.warnings.iter().all(|w| w.path != "targets[0].outputs"),
        "{:?}",
        out.warnings
    );
    // Back to folders: the key goes (it is the default).
    save_ok(
        &fx,
        &[EditOp::SetTargetKind {
            name: "Together".into(),
            kind: TargetKind::Folders,
        }],
    );
    assert!(!read(&fx.path).contains("kind ="));
}

#[test]
fn checks_name_the_field() {
    let fx = fixture(TWO_SOURCES);
    // `expand` with `ai` in one filter.
    let mut t = target(&fx, 1);
    t.filters[0].ai = Some("Documents.".into());
    t.filters[0].expand = Some("event".into());
    let e = draft(&fx.path, None, &[update(1, t)])
        .unwrap()
        .error
        .expect("expand with ai");
    assert_eq!(
        e.path.as_deref(),
        Some("targets[1].filters[0].expand"),
        "{e:?}"
    );
    assert!(e.message.contains("`expand`"), "{e:?}");

    // An unknown `@place`.
    let e = draft(
        &fx.path,
        None,
        &[EditOp::SetCondition {
            target: "Timeline".into(),
            filter: 0,
            condition: condition("geo", serde_json::json!(["@home", "!@dacha"])),
        }],
    )
    .unwrap()
    .error
    .expect("unknown place");
    assert_eq!(
        e.path.as_deref(),
        Some("targets[1].filters[0].geo"),
        "{e:?}"
    );

    // `series` with other parameters than the first one.
    let series = |gap: &str| condition("series", serde_json::json!({ "gap": gap }));
    let e = draft(
        &fx.path,
        None,
        &[
            EditOp::SetCondition {
                target: "Together".into(),
                filter: 0,
                condition: series("5s"),
            },
            EditOp::SetCondition {
                target: "Timeline".into(),
                filter: 0,
                condition: series("10s"),
            },
        ],
    )
    .unwrap()
    .error
    .expect("series mismatch");
    assert_eq!(
        e.path.as_deref(),
        Some("targets[1].filters[0].series"),
        "{e:?}"
    );

    // An empty AI description.
    let mut t = target(&fx, 0);
    t.filters[0].ai = Some(" ".into());
    let e = draft(&fx.path, None, &[update(0, t)])
        .unwrap()
        .error
        .unwrap();
    assert_eq!(e.path.as_deref(), Some("targets[0].filters[0].ai"));
}

#[test]
fn a_bad_condition_value_is_shown_and_named() {
    let fx = fixture(TWO_SOURCES);
    let mut t = target(&fx, 0);
    t.filters[0]
        .conditions
        .insert("date".into(), serde_json::json!({ "range": "soon" }));
    let out = draft(&fx.path, None, &[update(0, t.clone())]).unwrap();
    assert!(out.error.is_some());
    // The draft still holds it, to be fixed.
    assert_eq!(out.draft.targets[0], t);
}

#[test]
fn move_target_reorders_the_list() {
    let fx = fixture(COMMENTED);
    save_ok(&fx, &[EditOp::MoveTarget { from: 3, to: 0 }]);
    let cfg = Config::load(&fx.path).unwrap();
    assert_eq!(
        names(&cfg),
        ["Timeline", "Screenshots", "TikTok", "Documents"]
    );
    let after = read(&fx.path);
    // The section heading stays in its place; the comment goes with its block.
    assert!(
        after.contains("# ---- by name ----\n\n# The timeline: everything else.\n[[targets]]\nname = \"Timeline\""),
        "{after}"
    );
    assert!(after.ends_with("# trailing note\n"), "{after}");
    // And back: the original text.
    save_ok(&fx, &[EditOp::MoveTarget { from: 0, to: 3 }]);
    assert_eq!(read(&fx.path), COMMENTED);
}

#[test]
fn reorder_keeps_section_heading_in_place_and_comment_with_block() {
    let fx = fixture(COMMENTED);
    save_ok(&fx, &[EditOp::MoveTarget { from: 1, to: 0 }]);
    let after = read(&fx.path);
    let expect = COMMENTED.replace(
        "# ---- by name ----\n\n# Screenshots: renamed in place.\n[[targets]]\nname = \"Screenshots\"\n[[targets.filters]]\nsources = [\"shots\"]\nmatch = \"*\"\nexclude = [\"@dated\"]\n[[targets.outputs]]\ndir = \"/tmp/gallery-edit-test/shots\"\nrename = [\"capture-date\"]\n\n# TikTok junk.\n[[targets]]\nname = \"TikTok\"\n[[targets.filters]]\nsources = [\"camera\"]\nmatch = \"@tiktok\"   # 32 hex digits\n[[targets.outputs]]\ndir = \"/tmp/gallery-edit-test/out/tiktok\"\n",
        "# ---- by name ----\n\n# TikTok junk.\n[[targets]]\nname = \"TikTok\"\n[[targets.filters]]\nsources = [\"camera\"]\nmatch = \"@tiktok\"   # 32 hex digits\n[[targets.outputs]]\ndir = \"/tmp/gallery-edit-test/out/tiktok\"\n\n# Screenshots: renamed in place.\n[[targets]]\nname = \"Screenshots\"\n[[targets.filters]]\nsources = [\"shots\"]\nmatch = \"*\"\nexclude = [\"@dated\"]\n[[targets.outputs]]\ndir = \"/tmp/gallery-edit-test/shots\"\nrename = [\"capture-date\"]\n",
    );
    assert_ne!(expect, COMMENTED);
    assert_eq!(after, expect);
}

#[test]
fn move_to_a_missing_position_is_refused() {
    let fx = fixture(COMMENTED);
    for op in [
        EditOp::MoveTarget { from: 0, to: 4 },
        EditOp::MoveTarget { from: 4, to: 0 },
    ] {
        let e = save(&fx.path, &version(&fx.path), &[op]).unwrap_err();
        assert_eq!(e.kind, FailureKind::Op);
        assert_eq!(e.op, Some(0));
    }
    assert_eq!(read(&fx.path), COMMENTED);
}

#[test]
fn add_and_remove_targets() {
    let fx = fixture(COMMENTED);
    let new = TargetDraft {
        name: "Receipts".into(),
        filters: vec![FilterDraft {
            sources: vec!["camera".into()],
            ai: Some("Receipts.".into()),
            ..Default::default()
        }],
        outputs: vec![OutputDraft {
            dir: "/tmp/gallery-edit-test/out/receipts".into(),
            ..Default::default()
        }],
        ..Default::default()
    };
    save_ok(
        &fx,
        &[EditOp::AddTarget {
            kind: TargetKind::Folders,
            target: new,
        }],
    );
    let after = read(&fx.path);
    assert!(
        after.ends_with("dir = \"/tmp/gallery-edit-test/backup\"\n\n[[targets]]\nname = \"Receipts\"\n[[targets.filters]]\nsources = [\"camera\"]\nai = \"Receipts.\"\n[[targets.outputs]]\ndir = \"/tmp/gallery-edit-test/out/receipts\"\n\n# trailing note\n"),
        "{after}"
    );
    assert_eq!(
        names(&Config::load(&fx.path).unwrap()),
        ["Screenshots", "TikTok", "Documents", "Timeline", "Receipts"]
    );
    save_ok(&fx, &[EditOp::RemoveTarget { index: 4 }]);
    assert_eq!(read(&fx.path), COMMENTED);

    // A trash target: no outputs, `kind` written.
    save_ok(
        &fx,
        &[EditOp::AddTarget {
            kind: TargetKind::Trash,
            target: TargetDraft {
                name: "Junk".into(),
                ..Default::default()
            },
        }],
    );
    assert!(read(&fx.path).contains("\n[[targets]]\nname = \"Junk\"\nkind = \"trash\"\n"));
    save_ok(&fx, &[EditOp::RemoveTarget { index: 4 }]);

    // Removing the first target: the section heading stays for the next one.
    save_ok(&fx, &[EditOp::RemoveTarget { index: 0 }]);
    let after = read(&fx.path);
    assert!(
        after.contains("# ---- by name ----\n\n# TikTok junk.\n[[targets]]\nname = \"TikTok\""),
        "{after}"
    );
    assert!(!after.contains("Screenshots"));
}

#[test]
fn adds_the_first_target_after_the_sources() {
    let text =
        "[[sources]]\nid = \"s\"\npath = \"/tmp/gallery-edit-test/s\"\n\n[ai]\nmodel = \"m\"\n";
    let fx = fixture(text);
    let t = TargetDraft {
        name: "First".into(),
        filters: vec![FilterDraft {
            sources: vec!["s".into()],
            matches: Some(vec!["@photo".into()]),
            ..Default::default()
        }],
        ..Default::default()
    };
    save_ok(
        &fx,
        &[EditOp::AddTarget {
            kind: TargetKind::Keep,
            target: t,
        }],
    );
    let after = read(&fx.path);
    let i = after.find("[[targets]]").unwrap();
    assert!(i > after.find("[[sources]]").unwrap(), "{after}");
    assert!(after.contains("[[targets]]\nname = \"First\"\nkind = \"keep\"\n\n[[targets.filters]]\nsources = [\"s\"]\nmatch = \"@photo\"\n"), "{after}");
    Config::load(&fx.path).unwrap().validate().unwrap();
}

#[test]
fn new_subtables_follow_the_files_spacing() {
    // COMMENTED has no blank line before sub-tables: new ones neither.
    let fx = fixture(COMMENTED);
    let mut t = target(&fx, 3);
    t.outputs.push(OutputDraft {
        dir: "/tmp/gallery-edit-test/third".into(),
        ..Default::default()
    });
    save_ok(&fx, &[update(3, t)]);
    assert!(read(&fx.path).contains("dir = \"/tmp/gallery-edit-test/backup\"\n[[targets.outputs]]\ndir = \"/tmp/gallery-edit-test/third\"\n"));
}

#[test]
fn invalid_edit_is_not_written() {
    let fx = fixture(COMMENTED);
    let mut t = target(&fx, 3);
    t.outputs[0].content[1].preset = "avif-nope".into();
    let out = save(&fx.path, &version(&fx.path), &[update(3, t)]).unwrap();
    assert!(!out.saved);
    let e = out.error.unwrap();
    assert!(e.message.contains("avif-nope"), "{e:?}");
    assert_eq!(e.path.as_deref(), Some("targets[3]"));
    assert_eq!(read(&fx.path), COMMENTED);
    assert!(!file::backup_path(&fx.path).exists());
    // The draft still shows the bad value, to be fixed.
    assert_eq!(
        out.draft.targets[3].outputs[0].content[1].preset,
        "avif-nope"
    );
}

#[test]
fn bad_mask_names_its_field() {
    let fx = fixture(COMMENTED);
    let mut t = target(&fx, 0);
    t.filters[0].exclude = vec!["^(".into()];
    let out = draft(&fx.path, None, &[update(0, t)]).unwrap();
    let e = out.error.unwrap();
    assert_eq!(
        e.path.as_deref(),
        Some("targets[0].filters[0].exclude"),
        "{e:?}"
    );

    let mut t = target(&fx, 0);
    t.filters[0].matches = Some(vec!["@nope".into()]);
    let e = draft(&fx.path, None, &[update(0, t)])
        .unwrap()
        .error
        .unwrap();
    assert!(e.message.contains("@nope"), "{e:?}");
    assert_eq!(e.path.as_deref(), Some("targets[0]"));
}

#[test]
fn duplicate_source_id_is_an_error() {
    let fx = fixture(COMMENTED);
    let src = SourceDraft {
        id: "shots".into(),
        path: "/x".into(),
        ..Default::default()
    };
    let e = draft(&fx.path, None, &[EditOp::AddSource { source: src }])
        .unwrap()
        .error
        .unwrap();
    assert_eq!(e.path.as_deref(), Some("sources[2].id"));
}

#[test]
fn external_change_is_refused() {
    let fx = fixture(COMMENTED);
    let v = version(&fx.path);
    std::fs::write(&fx.path, format!("{COMMENTED}# edited elsewhere\n")).unwrap();
    let mut t = target(&fx, 1);
    t.name = "TT".into();
    let e = save(&fx.path, &v, &[update(1, t)]).unwrap_err();
    assert_eq!(e.kind, FailureKind::Changed);
    assert!(read(&fx.path).ends_with("# edited elsewhere\n"));
}

#[test]
fn save_keeps_a_backup_and_returns_the_new_version() {
    let fx = fixture(COMMENTED);
    let mut t = target(&fx, 1);
    t.name = "TT".into();
    let out = save_ok(&fx, &[update(1, t)]);
    assert_eq!(read(&file::backup_path(&fx.path)), COMMENTED);
    assert_eq!(out.backup.as_deref(), Some("gallery.toml.bak"));
    assert_eq!(out.version, version(&fx.path));
    // The returned version saves again.
    let mut t = target(&fx, 1);
    t.name = "TT2".into();
    let out2 = save(&fx.path, &out.version, &[update(1, t)]).unwrap();
    assert!(out2.saved);
    assert!(read(&file::backup_path(&fx.path)).contains("name = \"TT\""));
}

#[cfg(unix)]
#[test]
fn save_through_a_symlink_writes_its_target() {
    let fx = fixture(COMMENTED);
    let link = fx.path.with_file_name("link.toml");
    std::os::unix::fs::symlink(&fx.path, &link).unwrap();
    let mut t = target(&fx, 1);
    t.name = "TT".into();
    let out = save(&link, &version(&link), &[update(1, t)]).unwrap();
    assert!(out.saved, "{:?}", out.error);
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(read(&fx.path).contains("name = \"TT\""));
}

#[test]
fn warnings_with_paths() {
    let fx = fixture(COMMENTED);
    let w = draft(&fx.path, None, &[]).unwrap().warnings;
    // The screenshots target writes into its own source.
    assert!(
        w.iter()
            .any(|w| w.path == "targets[0].outputs[0].dir" && w.message.contains("“shots”")),
        "{w:?}"
    );
    assert!(!w.iter().any(|w| w.path.starts_with("targets[3]")), "{w:?}");

    let mut t = target(&fx, 3);
    t.outputs[0].content = vec![
        StepDraft {
            preset: "avif-high".into(),
            mask: None,
        },
        StepDraft {
            preset: "webp-high".into(),
            mask: Some(vec!["*.avif".into()]),
        },
    ];
    t.outputs.truncate(1);
    let mut b = target(&fx, 1);
    b.outputs.clear();
    let w = draft(&fx.path, None, &[update(3, t), update(1, b)])
        .unwrap()
        .warnings;
    let double = w
        .iter()
        .find(|w| w.path == "targets[3].outputs[0].content")
        .expect("double encode");
    assert!(double.message.contains("*.jpg"), "{double:?}");
    assert!(w.iter().any(|w| w.path == "targets[1].outputs"), "{w:?}");
}

#[test]
fn prompt_preview_uses_the_draft() {
    let fx = fixture(COMMENTED);
    let p = preview_prompt(&fx.path, &[], "camera", None).unwrap();
    assert!(p.prompt.starts_with("Phone shots."), "{}", p.prompt);
    assert!(p.prompt.contains("- Documents — Photos of documents."));
    assert_eq!(p.categories, vec!["Documents".to_string()]);

    let mut s = draft(&fx.path, None, &[]).unwrap().draft.sources[0].clone();
    s.ai_prefix_file = None;
    s.ai_suffix = Some("Receipt — into Receipts.".into());
    let p = preview_prompt(
        &fx.path,
        &[EditOp::UpdateSource {
            id: "camera".into(),
            source: s,
        }],
        "camera",
        None,
    )
    .unwrap();
    assert!(!p.prompt.contains("Phone shots."));
    assert!(
        p.prompt
            .contains("Receipt — into Receipts.\n\nANSWER FORMAT"),
        "{}",
        p.prompt
    );
    assert!(preview_prompt(&fx.path, &[], "nope", None).is_err());
}

#[test]
fn prompt_preview_shows_the_examples_section() {
    use gallery_core::learn::{Correction, Store};
    let fx = fixture(COMMENTED);
    let ex = fx.path.with_file_name("examples.jsonl");
    let store = Store::new(&ex);
    let c = Correction::new(
        "camera",
        "a.jpg",
        Some("Documents".into()),
        "passport scan",
        gallery_core::overrides::Action::Keep,
        "now",
    );
    store.append(&[c]).unwrap();
    let p = preview_prompt(&fx.path, &[], "camera", Some(&ex)).unwrap();
    assert!(
        p.prompt.contains("The user's past corrections:"),
        "{}",
        p.prompt
    );
    assert!(p.prompt.contains("passport scan"));
    let none = preview_prompt(&fx.path, &[], "camera", None).unwrap();
    assert!(!none.prompt.contains("The user's past corrections"));
}

#[test]
fn mask_tester_uses_user_and_builtin_masks() {
    let fx = fixture(&format!("[masks]\nmine = [\"*.xyz\"]\n\n{COMMENTED}"));
    let r = mask_test(
        &fx.path,
        &[],
        &MaskTest {
            matches: Some(vec!["@photo".into(), "@mine".into()]),
            exclude: vec!["@screenshot".into()],
            names: vec![
                "IMG_1.jpg".into(),
                "a.xyz".into(),
                "Screenshot_1.png".into(),
                "x.mp4".into(),
                " ".into(),
            ],
        },
    )
    .unwrap();
    let taken: Vec<(&str, bool)> = r.iter().map(|x| (x.name.as_str(), x.taken)).collect();
    assert_eq!(
        taken,
        vec![
            ("IMG_1.jpg", true),
            ("a.xyz", true),
            ("Screenshot_1.png", false),
            ("x.mp4", false)
        ]
    );
    assert!(
        mask_test(
            &fx.path,
            &[],
            &MaskTest {
                matches: Some(vec!["^(".into()]),
                exclude: vec![],
                names: vec![]
            }
        )
        .is_err()
    );
}

#[test]
fn example_config_round_trips() {
    round_trip(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/gallery.toml"));
}

/// The same for any config (a copy of a real one):
/// `GALLERY_ROUND_TRIP=/copy/gallery.toml cargo test -- --ignored round_trip_env`.
#[test]
#[ignore]
fn round_trip_env() {
    round_trip(Path::new(
        &std::env::var_os("GALLERY_ROUND_TRIP").expect("GALLERY_ROUND_TRIP"),
    ));
}

fn round_trip(p: &Path) {
    let text = read(p);
    let mut doc: toml_edit::DocumentMut = text.parse().unwrap();
    assert!(!migrate::to_ordered(&mut doc).unwrap(), "the new shape");
    let d = doc::read_draft(&doc).unwrap();
    assert!(!d.targets.is_empty() && !d.places.is_empty());
    // Rewriting every target and source with its own values changes nothing.
    for (i, t) in d.targets.iter().enumerate() {
        ops::apply(&mut doc, &update(i, t.clone())).unwrap();
    }
    for s in &d.sources {
        ops::apply(
            &mut doc,
            &EditOp::UpdateSource {
                id: s.id.clone(),
                source: s.clone(),
            },
        )
        .unwrap();
    }
    assert_eq!(doc.to_string(), text);
    // Moving every target to the end and back, too.
    let n = d.targets.len();
    for _ in 0..n {
        ops::apply(&mut doc, &EditOp::MoveTarget { from: 0, to: n - 1 }).unwrap();
    }
    assert_eq!(doc.to_string(), text);
}

// ---------- the old three-stage shape ----------

#[test]
fn old_shape_is_edited_as_the_ordered_list() {
    let fx = fixture(STAGED);
    let out = draft(&fx.path, None, &[]).unwrap();
    assert!(out.error.is_none(), "{:?}", out.error);
    assert!(out.old_shape && !out.changed);
    assert!(
        out.warnings
            .first()
            .is_some_and(|w| w.path.is_empty() && w.message.contains(".bak")),
        "{:?}",
        out.warnings
    );
    let names: Vec<&str> = out.draft.targets.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["Screenshots", "TikTok", "Documents", "Timeline"]);
    // Nothing is written without a change.
    let out = save(&fx.path, &version(&fx.path), &[]).unwrap();
    assert!(!out.saved);
    assert_eq!(read(&fx.path), STAGED);
    assert!(!file::backup_path(&fx.path).exists());
}

#[test]
fn old_shape_is_migrated_on_the_first_save() {
    let fx = fixture(STAGED);
    let before = Config::load(&fx.path).unwrap();
    assert!(before.was_staged);
    let mut t = target(&fx, 1);
    t.outputs[0].dir = "/tmp/gallery-edit-test/out/tt".into();
    let out = save_ok(&fx, &[update(1, t)]);
    assert_eq!(out.backup.as_deref(), Some("gallery.toml.bak"));
    assert!(!out.old_shape);
    // The original bytes are kept.
    assert_eq!(read(&file::backup_path(&fx.path)), STAGED);
    let after = read(&fx.path);
    let cfg = Config::load(&fx.path).unwrap();
    assert!(!cfg.was_staged);
    cfg.validate().unwrap();
    assert_eq!(names(&cfg), names(&before));
    // The same filters as the migration on load.
    for (a, b) in before.targets.iter().zip(&cfg.targets) {
        assert_eq!(
            serde_json::to_value(&a.filters).unwrap(),
            serde_json::to_value(&b.filters).unwrap(),
            "{}",
            a.name
        );
    }
    // The text: headers renamed, the AI target's filter built in the
    // prefilter's place, comments and the rest kept.
    let expect = COMMENTED
        .replace(
            "# ---- by name ----",
            "# ---- stage 1: masks before AI ----",
        )
        .replace("# ---- AI ----", "# ---- stage 2: AI ----")
        .replace("# ---- the rest ----", "# ---- stage 3: after AI ----")
        .replace(
            "ai = \"Photos of documents.\"",
            "# What the AI is told.\nai = \"Photos of documents.\"",
        )
        .replace("out/tiktok", "out/tt");
    assert_eq!(after, expect);

    // A later save keeps the old-shape backup and uses the next name.
    let mut t = target(&fx, 1);
    t.name = "TT".into();
    let out = save_ok(&fx, &[update(1, t)]);
    assert_eq!(out.backup.as_deref(), Some("gallery.toml.bak.1"));
    assert_eq!(read(&file::backup_path(&fx.path)), STAGED);
    assert_eq!(read(&fx.path.with_file_name("gallery.toml.bak.1")), after);
}

#[test]
fn migration_never_replaces_an_existing_backup() {
    let fx = fixture(STAGED);
    let bak = file::backup_path(&fx.path);
    std::fs::write(&bak, "older backup\n").unwrap();
    std::fs::write(
        fx.path.with_file_name("gallery.toml.bak.1"),
        "older still\n",
    )
    .unwrap();
    let out = save_ok(
        &fx,
        &[EditOp::SetPlace {
            name: "home".into(),
            place: gallery_core::config::Place {
                lat: 1.0,
                lon: 2.0,
                km: 3.0,
            },
        }],
    );
    assert_eq!(out.backup.as_deref(), Some("gallery.toml.bak.2"));
    assert_eq!(read(&bak), "older backup\n");
    assert_eq!(read(&fx.path.with_file_name("gallery.toml.bak.2")), STAGED);
}

#[test]
fn staged_example_is_migrated_on_the_first_save() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let fixture_text = read(&repo.join("crates/gallery-core/tests/fixtures/staged-example.toml"));
    // The prompt files, found from the copy.
    let ai = std::fs::canonicalize(repo.join("examples/ai")).unwrap();
    let text = fixture_text.replace(
        "\"../../../../examples/ai/",
        &format!("\"{}/", ai.display().to_string().replace('\\', "/")),
    );
    let fx = fixture(&text);
    let before = Config::load(&fx.path).unwrap();
    assert!(before.was_staged);
    save_ok(
        &fx,
        &[EditOp::SetPlace {
            name: "home".into(),
            place: gallery_core::config::Place {
                lat: 47.0,
                lon: 28.8,
                km: 2.0,
            },
        }],
    );
    assert_eq!(read(&file::backup_path(&fx.path)), text);
    let after = read(&fx.path);
    let cfg = Config::load(&fx.path).unwrap();
    assert!(!cfg.was_staged);
    cfg.validate().unwrap();
    assert_eq!(names(&cfg), names(&before));
    for (a, b) in before.targets.iter().zip(&cfg.targets) {
        assert_eq!(
            serde_json::to_value(a).unwrap(),
            serde_json::to_value(b).unwrap(),
            "{}",
            a.name
        );
    }
    // Comments of untouched tables stay.
    for line in fixture_text.lines().filter(|l| l.starts_with('#')).skip(1) {
        assert!(after.contains(line), "lost: {line}");
    }
    assert!(!after.contains("[[targets.before_ai") && !after.contains("[[targets.after_ai"));
    assert!(!after.contains("[[targets.ai") && !after.contains("[targets.ai.prefilter]"));
}

#[test]
fn old_inline_stages_are_migrated_too() {
    let text = r#"[[sources]]
id = "camera"
path = "/tmp/gallery-edit-test/camera"

[targets]
after_ai = [{ name = "X", filters = [{ sources = ["camera"] }], outputs = [{ dir = "/tmp/gallery-edit-test/x" }] }]
ai = [{ name = "D", sources = ["camera"], description = "Docs.", prefilter = { match = "@photo" } }]
"#;
    let fx = fixture(text);
    let before = Config::load(&fx.path).unwrap();
    save_ok(
        &fx,
        &[EditOp::AddTarget {
            kind: TargetKind::Keep,
            target: TargetDraft {
                name: "Y".into(),
                ..Default::default()
            },
        }],
    );
    let cfg = Config::load(&fx.path).unwrap();
    assert!(!cfg.was_staged);
    assert_eq!(names(&cfg), ["D", "X", "Y"]);
    for (a, b) in before.targets.iter().zip(&cfg.targets) {
        assert_eq!(
            serde_json::to_value(a).unwrap(),
            serde_json::to_value(b).unwrap()
        );
    }
}

const MULTILINE: &str = r#"[[sources]]
id = "camera"
path = "/tmp/gallery-edit-test/camera"

[[targets.ai]]
name = "Docs"
sources = ["camera"]
description = """
Photos of documents.
Receipts too.
"""
[[targets.ai.outputs]]
dir = "/tmp/gallery-edit-test/docs"
"#;

#[test]
fn multi_line_description_stays_multi_line() {
    let fx = fixture(MULTILINE);
    let mut t = target(&fx, 0);
    assert_eq!(
        t.filters[0].ai.as_deref(),
        Some("Photos of documents.\nReceipts too.\n")
    );
    t.filters[0].ai = Some("Photos of documents.\nReceipts, \"tickets\" and \\ more.\n".into());
    save_ok(&fx, &[update(0, t.clone())]);
    let after = read(&fx.path);
    assert!(
        after.contains("[[targets.filters]]\nsources = [\"camera\"]\nai = \"\"\"\nPhotos of documents.\nReceipts, \\\"tickets\\\" and \\\\ more.\n\"\"\"\n"),
        "{after}"
    );
    assert_eq!(target(&fx, 0), t);
    // A single-line replacement is still one line.
    t.filters[0].ai = Some("One line.".into());
    save_ok(&fx, &[update(0, t)]);
    assert!(read(&fx.path).contains("ai = \"One line.\"\n"));
}

// ---------- refusals ----------

const THREE: &str = r#"[[sources]]
id = "camera"
path = "/tmp/gallery-edit-test/camera"

# == A ==

# first
[[targets]]
name = "A"
[[targets.outputs]]
dir = "/tmp/gallery-edit-test/a"

# == B ==

# second
[[targets]]
name = "B"
[[targets.outputs]]
dir = "/tmp/gallery-edit-test/b"

# == C ==

# third
[[targets]]
name = "C"
[[targets.outputs]]
dir = "/tmp/gallery-edit-test/c"

# == end ==
"#;

fn remove_target(fx: &Fx, index: usize) -> String {
    save_ok(fx, &[EditOp::RemoveTarget { index }]);
    read(&fx.path)
}

#[test]
fn removing_a_block_moves_its_heading_to_the_next_block() {
    // Middle: B's heading joins C's, C and its heading stay together.
    let fx = fixture(THREE);
    let after = remove_target(&fx, 1);
    assert!(
        after.contains("# == B ==\n\n# == C ==\n\n# third\n[[targets]]\nname = \"C\""),
        "{after}"
    );
    assert!(
        after.contains("# == A ==\n\n# first\n[[targets]]\nname = \"A\""),
        "{after}"
    );
    assert!(!after.contains("second"));
    assert!(after.contains("# == end =="));

    // First: A's heading joins B's.
    let fx = fixture(THREE);
    let after = remove_target(&fx, 0);
    assert!(
        after.contains("# == A ==\n\n# == B ==\n\n# second\n[[targets]]\nname = \"B\""),
        "{after}"
    );
    assert!(
        after.contains("# == C ==\n\n# third\n[[targets]]\nname = \"C\""),
        "{after}"
    );

    // Last: its heading is kept, in front of whatever follows.
    let fx = fixture(THREE);
    let after = remove_target(&fx, 2);
    assert!(
        after.contains("# == B ==\n\n# second\n[[targets]]\nname = \"B\""),
        "{after}"
    );
    assert!(
        after.contains("# == C ==") && after.contains("# == end =="),
        "{after}"
    );
    assert!(!after.contains("third"));
}

const INLINE_LIST: &str = r#"targets = [{ name = "X", filters = [{ sources = ["camera"] }], outputs = [{ dir = "/tmp/gallery-edit-test/x" }] }]

[[sources]]
id = "camera"
path = "/tmp/gallery-edit-test/camera"
"#;

const INLINE_SOURCES: &str = r#"sources = [{ id = "camera", path = "/tmp/gallery-edit-test/camera" }]

[[targets]]
name = "A"
[[targets.filters]]
sources = ["camera"]
[[targets.outputs]]
dir = "/tmp/gallery-edit-test/a"
"#;

const INLINE_TARGETS: &str = r#"targets = [{ name = "X", outputs = [{ dir = "/tmp/gallery-edit-test/x" }] }, { name = "Y", kind = "keep" }]

[[sources]]
id = "camera"
path = "/tmp/gallery-edit-test/camera"
"#;

fn a_target(name: &str) -> TargetDraft {
    TargetDraft {
        name: name.into(),
        outputs: vec![OutputDraft {
            dir: format!("/tmp/gallery-edit-test/{name}"),
            ..Default::default()
        }],
        ..Default::default()
    }
}

/// Every op in `ops` is refused on `text` with a clear error and the file
/// is not touched.
fn refused(text: &str, ops: &[EditOp]) {
    for op in ops {
        let fx = fixture(text);
        let before = draft(&fx.path, None, &[]).unwrap();
        let err = draft(&fx.path, None, std::slice::from_ref(op)).unwrap_err();
        assert_eq!(err.kind, FailureKind::Op, "{op:?}: {err:?}");
        assert!(err.message.contains("in the file"), "{}", err.message);
        let err = save(&fx.path, &version(&fx.path), std::slice::from_ref(op)).unwrap_err();
        assert_eq!(err.kind, FailureKind::Op);
        assert_eq!(read(&fx.path), text);
        assert_eq!(
            draft(&fx.path, None, &[]).unwrap().draft.targets.len(),
            before.draft.targets.len()
        );
    }
}

#[test]
fn adding_to_an_inline_list_is_refused_not_wiped() {
    refused(
        INLINE_LIST,
        &[
            EditOp::AddTarget {
                kind: TargetKind::Folders,
                target: a_target("Y"),
            },
            EditOp::RemoveTarget { index: 0 },
            EditOp::SetTargetKind {
                name: "X".into(),
                kind: TargetKind::Keep,
            },
        ],
    );
    refused(
        INLINE_SOURCES,
        &[EditOp::AddSource {
            source: SourceDraft {
                id: "new".into(),
                path: "/tmp/gallery-edit-test/new".into(),
                ..Default::default()
            },
        }],
    );
    refused(
        INLINE_TARGETS,
        &[
            EditOp::AddTarget {
                kind: TargetKind::Folders,
                target: a_target("Z"),
            },
            EditOp::MoveTarget { from: 1, to: 0 },
        ],
    );
}

#[test]
fn draft_refuses_a_changed_file() {
    let fx = fixture(COMMENTED);
    let v = version(&fx.path);
    assert!(draft(&fx.path, Some(&v), &[]).is_ok());
    std::fs::write(&fx.path, format!("{COMMENTED}# outside\n")).unwrap();
    let err = draft(&fx.path, Some(&v), &[]).unwrap_err();
    assert_eq!(err.kind, FailureKind::Changed);
    assert!(draft(&fx.path, Some(&version(&fx.path)), &[]).is_ok());
}

// ---------- renames and overrides ----------

fn rename_op(fx: &Fx, to: &str) -> EditOp {
    let mut t = target(fx, 2);
    t.name = to.into();
    update(2, t)
}

fn overrides_file(fx: &Fx, names: &[&str]) -> PathBuf {
    use gallery_core::overrides::{Action, Overrides};
    let file = fx.path.with_file_name("overrides.json");
    let mut ov = Overrides::default();
    for (i, n) in names.iter().enumerate() {
        let p = PathBuf::from(format!("/tmp/gallery-edit-test/camera/{i}.jpg"));
        ov.set(p, 1, 1, Action::Target { name: (*n).into() });
    }
    ov.set(
        PathBuf::from("/tmp/gallery-edit-test/camera/keep.jpg"),
        1,
        1,
        Action::Keep,
    );
    ov.save(&file).unwrap();
    file
}

#[test]
fn renaming_a_target_warns_about_overrides_and_can_update_them() {
    let fx = fixture(COMMENTED);
    let file = overrides_file(&fx, &["Documents", "Documents", "Timeline"]);
    let op = rename_op(&fx, "Papers");

    // Before the save: a warning on the name, the rename is listed.
    let mut out = draft(&fx.path, None, std::slice::from_ref(&op)).unwrap();
    assert_eq!(out.renames.len(), 1);
    with_override_warnings(&mut out, &file);
    assert_eq!(out.renames[0].overrides, 2);
    let w = out
        .warnings
        .iter()
        .find(|w| w.path == "targets[2].name")
        .expect("warning on the name");
    assert!(
        w.message.contains("Documents") && w.message.contains('2'),
        "{}",
        w.message
    );

    // After the save: the same rename is reported, overrides still old.
    let mut out = save_ok(&fx, std::slice::from_ref(&op));
    with_override_warnings(&mut out, &file);
    assert_eq!(out.renames[0].from, "Documents");
    assert_eq!(out.renames[0].to, "Papers");
    assert_eq!(out.renames[0].overrides, 2);

    // The UI offers the update; it moves only the matching overrides.
    let map = [("Documents".to_string(), "Papers".to_string())];
    assert_eq!(
        crate::overrides::retarget_in(&file, &fx.path, &out.version, &map).unwrap(),
        2
    );
    let ov = gallery_core::overrides::Overrides::load(&file).unwrap();
    let names: Vec<String> = ov
        .entries()
        .filter_map(|e| match &e.action {
            gallery_core::overrides::Action::Target { name } => Some(name.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(names.iter().filter(|n| *n == "Papers").count(), 2);
    assert_eq!(names.iter().filter(|n| *n == "Timeline").count(), 1);
    assert_eq!(ov.len(), 4);
    assert_eq!(
        crate::overrides::retarget_in(&file, &fx.path, &out.version, &map).unwrap(),
        0
    );

    // No overrides on the old name: no warning.
    let fx = fixture(COMMENTED);
    let file = overrides_file(&fx, &["Timeline"]);
    let mut out = draft(&fx.path, None, &[rename_op(&fx, "Papers")]).unwrap();
    with_override_warnings(&mut out, &file);
    assert!(out.warnings.iter().all(|w| w.path != "targets[2].name"));
    assert_eq!(out.renames[0].overrides, 0);
}

fn update_named(fx: &Fx, index: usize, to: &str) -> EditOp {
    let mut t = target(fx, index);
    t.name = to.into();
    update(index, t)
}

fn named_overrides(file: &Path) -> Vec<String> {
    use gallery_core::overrides::{Action, Overrides};
    let mut v: Vec<String> = Overrides::load(file)
        .unwrap()
        .entries()
        .filter_map(|e| match &e.action {
            Action::Target { name } => Some(name.clone()),
            _ => None,
        })
        .collect();
    v.sort();
    v
}

const TWO: &str = "[[sources]]\nid = \"s\"\npath = \"/tmp/gallery-edit-test/camera\"\n\n[[targets]]\nname = \"A\"\n\n[[targets]]\nname = \"B\"\n";

#[test]
fn retarget_refuses_a_name_missing_from_the_config() {
    let fx = fixture(TWO);
    let file = overrides_file(&fx, &["A"]);
    let version = crate::config_edit::file::read(&fx.path).unwrap().1;
    let map = [("A".to_string(), "No such target".to_string())];
    let err = crate::overrides::retarget_in(&file, &fx.path, &version, &map).unwrap_err();
    assert!(err.contains("No such target"), "{err}");
    assert_eq!(named_overrides(&file), vec!["A"]);
}

#[test]
fn retarget_applies_a_swap_simultaneously() {
    let fx = fixture(TWO);
    let file = overrides_file(&fx, &["A", "A", "B"]);
    let version = crate::config_edit::file::read(&fx.path).unwrap().1;
    let map = [
        ("A".to_string(), "B".to_string()),
        ("B".to_string(), "A".to_string()),
    ];
    assert_eq!(
        crate::overrides::retarget_in(&file, &fx.path, &version, &map).unwrap(),
        3
    );
    assert_eq!(named_overrides(&file), vec!["A", "B", "B"]);
}

#[test]
fn retarget_refuses_a_changed_config() {
    let fx = fixture(TWO);
    let file = overrides_file(&fx, &["A"]);
    let map = [("A".to_string(), "B".to_string())];
    let err = crate::overrides::retarget_in(&file, &fx.path, "stale", &map).unwrap_err();
    assert!(err.contains("changed"), "{err}");
    assert_eq!(named_overrides(&file), vec!["A"]);
}

#[test]
fn swap_warning_counts_each_old_name_once_and_does_not_claim_a_missing_target() {
    let fx = fixture(TWO);
    let file = overrides_file(&fx, &["A", "A", "B"]);
    let mut out = draft(
        &fx.path,
        None,
        &[update_named(&fx, 0, "B"), update_named(&fx, 1, "A")],
    )
    .unwrap();
    with_override_warnings(&mut out, &file);
    let counts: Vec<_> = out
        .renames
        .iter()
        .map(|r| (r.from.as_str(), r.overrides))
        .collect();
    assert_eq!(counts, vec![("A", 2), ("B", 1)]);
    let names: Vec<_> = out
        .warnings
        .iter()
        .filter(|w| w.path.ends_with(".name"))
        .collect();
    assert_eq!(names.len(), 2);
    assert!(names.iter().all(|w| w.message.contains("will belong")));
}

#[test]
fn preview_of_an_old_shape_file_reports_its_rewrite_as_a_change() {
    let fx = fixture(
        "[[sources]]\nid = \"s\"\npath = \"/tmp/gallery-edit-test/s\"\n\n[[targets.before_ai]]\nname = \"A\"\n",
    );
    // No op changes anything, yet saving would rewrite the file.
    let p = preview_change(&fx.path, None, &[]).unwrap();
    assert!(!p.diff.is_empty());
    assert!(p.changed, "{:?}", p.diff);
    // A file already in the new shape with no op: no change.
    let new = fixture(
        "[[sources]]\nid = \"s\"\npath = \"/tmp/gallery-edit-test/s\"\n\n[[targets]]\nname = \"A\"\n",
    );
    let p = preview_change(&new.path, None, &[]).unwrap();
    assert!(!p.changed, "{p:?}");
}
