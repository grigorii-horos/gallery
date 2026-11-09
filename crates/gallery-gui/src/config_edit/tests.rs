use std::path::{Path, PathBuf};

use super::model::{
    FilterDraft, OutputDraft, PrefilterDraft, SourceDraft, Stage, StepDraft, TargetDraft,
};
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

fn target(fx: &Fx, stage: Stage, i: usize) -> TargetDraft {
    draft(&fx.path, None, &[])
        .unwrap()
        .draft
        .targets
        .stage(stage)[i]
        .clone()
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

#[test]
fn reads_the_draft() {
    let fx = fixture(COMMENTED);
    let out = draft(&fx.path, None, &[]).unwrap();
    assert!(out.error.is_none(), "{:?}", out.error);
    assert!(!out.changed);
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
    let b = &d.targets.before_ai[0];
    assert_eq!(
        b.filters[0].matches.as_deref(),
        Some(&["*".to_string()][..])
    );
    assert_eq!(b.filters[0].exclude, vec!["@dated".to_string()]);
    let a = &d.targets.ai[0];
    assert_eq!(
        a.prefilter.as_ref().unwrap().matches.as_deref(),
        Some(&["@photo".to_string()][..])
    );
    let t = &d.targets.after_ai[0];
    assert_eq!(t.outputs.len(), 2);
    assert_eq!(t.outputs[0].content.len(), 3);
    assert_eq!(out.version, version(&fx.path));
}

#[test]
fn editing_one_target_changes_only_its_line() {
    let fx = fixture(COMMENTED);
    let mut t = target(&fx, Stage::BeforeAi, 1);
    t.outputs[0].dir = "/tmp/gallery-edit-test/out/tt".into();
    save_ok(
        &fx,
        &[EditOp::UpdateTarget {
            stage: Stage::BeforeAi,
            index: 1,
            target: t,
        }],
    );
    let after = read(&fx.path);
    let (old, new) = diff(COMMENTED, &after);
    assert_eq!(old, vec![r#"dir = "/tmp/gallery-edit-test/out/tiktok""#]);
    assert_eq!(new, vec![r#"dir = "/tmp/gallery-edit-test/out/tt""#]);
}

#[test]
fn changed_value_keeps_its_inline_comment_and_list_form() {
    let fx = fixture(COMMENTED);
    let mut t = target(&fx, Stage::AfterAi, 0);
    t.filters[0].matches = Some(vec!["@photo".into()]);
    save_ok(
        &fx,
        &[EditOp::UpdateTarget {
            stage: Stage::AfterAi,
            index: 0,
            target: t,
        }],
    );
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
    let t = target(&fx, Stage::AfterAi, 0);
    let out = save(
        &fx.path,
        &version(&fx.path),
        &[EditOp::UpdateTarget {
            stage: Stage::AfterAi,
            index: 0,
            target: t,
        }],
    )
    .unwrap();
    assert!(!out.changed && !out.saved);
    assert!(!file::backup_path(&fx.path).exists());
    assert_eq!(read(&fx.path), COMMENTED);
}

#[test]
fn adds_filter_prefilter_outputs_and_masked_steps() {
    let fx = fixture(COMMENTED);
    let mut t = target(&fx, Stage::BeforeAi, 1);
    t.filters.push(FilterDraft {
        sources: vec!["shots".into()],
        matches: Some(vec!["@tiktok".into(), "*.webm".into()]),
        exclude: vec![],
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
    let mut ai = target(&fx, Stage::Ai, 0);
    ai.prefilter = Some(PrefilterDraft {
        matches: Some(vec!["@photo".into()]),
        exclude: vec!["@screenshot".into()],
    });
    ai.description = Some("Documents and forms.".into());
    save_ok(
        &fx,
        &[
            EditOp::UpdateTarget {
                stage: Stage::BeforeAi,
                index: 1,
                target: t.clone(),
            },
            EditOp::UpdateTarget {
                stage: Stage::Ai,
                index: 0,
                target: ai.clone(),
            },
        ],
    );
    let text = read(&fx.path);
    let cfg = Config::load(&fx.path).unwrap();
    cfg.validate().unwrap();
    let tt = stage(&cfg, Stage::BeforeAi)[1];
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
    let docs = &stage(&cfg, Stage::Ai)[0].filters[0];
    assert_eq!(docs.ai.as_deref(), Some("Documents and forms."));
    assert!(!docs.exclude.is_empty());
    // And the draft reads back what was written.
    let d = draft(&fx.path, None, &[]).unwrap().draft;
    assert_eq!(d.targets.before_ai[1], t);
    assert_eq!(d.targets.ai[0], ai);
}

#[test]
fn new_prefilter_goes_before_outputs() {
    let text = COMMENTED.replace("[targets.ai.prefilter]\nmatch = \"@photo\"\n", "");
    let fx = fixture(&text);
    let mut ai = target(&fx, Stage::Ai, 0);
    assert!(ai.prefilter.is_none());
    ai.prefilter = Some(PrefilterDraft {
        matches: Some(vec!["@video".into()]),
        exclude: vec![],
    });
    save_ok(
        &fx,
        &[EditOp::UpdateTarget {
            stage: Stage::Ai,
            index: 0,
            target: ai,
        }],
    );
    let (old, new) = diff(&text, &read(&fx.path));
    assert!(old.is_empty(), "{old:?}");
    assert_eq!(new, vec!["[targets.ai.prefilter]", r#"match = "@video""#]);
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
            .any(|w| w.path == "targets.before_ai[0].filters[0].sources"),
        "{:?}",
        out.warnings
    );
}

#[test]
fn reorder_keeps_section_heading_in_place_and_comment_with_block() {
    let fx = fixture(COMMENTED);
    save_ok(
        &fx,
        &[EditOp::MoveTarget {
            from: Stage::BeforeAi,
            index: 1,
            to: Stage::BeforeAi,
            to_index: 0,
        }],
    );
    let after = read(&fx.path);
    let expect = COMMENTED.replace(
        "# ---- stage 1: masks before AI ----\n\n# Screenshots: renamed in place.\n[[targets.before_ai]]\nname = \"Screenshots\"\n[[targets.before_ai.filters]]\nsources = [\"shots\"]\nmatch = \"*\"\nexclude = [\"@dated\"]\n[[targets.before_ai.outputs]]\ndir = \"/tmp/gallery-edit-test/shots\"\nrename = [\"capture-date\"]\n\n# TikTok junk.\n[[targets.before_ai]]\nname = \"TikTok\"\n[[targets.before_ai.filters]]\nsources = [\"camera\"]\nmatch = \"@tiktok\"   # 32 hex digits\n[[targets.before_ai.outputs]]\ndir = \"/tmp/gallery-edit-test/out/tiktok\"\n",
        "# ---- stage 1: masks before AI ----\n\n# TikTok junk.\n[[targets.before_ai]]\nname = \"TikTok\"\n[[targets.before_ai.filters]]\nsources = [\"camera\"]\nmatch = \"@tiktok\"   # 32 hex digits\n[[targets.before_ai.outputs]]\ndir = \"/tmp/gallery-edit-test/out/tiktok\"\n\n# Screenshots: renamed in place.\n[[targets.before_ai]]\nname = \"Screenshots\"\n[[targets.before_ai.filters]]\nsources = [\"shots\"]\nmatch = \"*\"\nexclude = [\"@dated\"]\n[[targets.before_ai.outputs]]\ndir = \"/tmp/gallery-edit-test/shots\"\nrename = [\"capture-date\"]\n",
    );
    assert_ne!(expect, COMMENTED);
    assert_eq!(after, expect);
    // And back: the original text.
    save_ok(
        &fx,
        &[EditOp::MoveTarget {
            from: Stage::BeforeAi,
            index: 0,
            to: Stage::BeforeAi,
            to_index: 1,
        }],
    );
    assert_eq!(read(&fx.path), COMMENTED);
}

#[test]
fn move_between_mask_stages_and_back() {
    let fx = fixture(COMMENTED);
    save_ok(
        &fx,
        &[EditOp::MoveTarget {
            from: Stage::BeforeAi,
            index: 1,
            to: Stage::AfterAi,
            to_index: 1,
        }],
    );
    let after = read(&fx.path);
    let cfg = Config::load(&fx.path).unwrap();
    assert_eq!(stage(&cfg, Stage::BeforeAi).len(), 1);
    assert_eq!(stage(&cfg, Stage::AfterAi)[1].name, "TikTok");
    assert_eq!(stage(&cfg, Stage::AfterAi)[1].filters.len(), 1);
    assert!(
        after.contains(
            "# TikTok junk.\n[[targets.after_ai]]\nname = \"TikTok\"\n[[targets.after_ai.filters]]"
        ),
        "{after}"
    );
    assert!(
        after.ends_with("dir = \"/tmp/gallery-edit-test/out/tiktok\"\n\n# trailing note\n"),
        "{after}"
    );
    save_ok(
        &fx,
        &[EditOp::MoveTarget {
            from: Stage::AfterAi,
            index: 1,
            to: Stage::BeforeAi,
            to_index: 1,
        }],
    );
    assert_eq!(read(&fx.path), COMMENTED);
}

#[test]
fn ai_target_cannot_move_to_a_mask_stage() {
    let fx = fixture(COMMENTED);
    let e = save(
        &fx.path,
        &version(&fx.path),
        &[EditOp::MoveTarget {
            from: Stage::Ai,
            index: 0,
            to: Stage::AfterAi,
            to_index: 0,
        }],
    )
    .unwrap_err();
    assert_eq!(e.kind, FailureKind::Op);
    assert_eq!(e.op, Some(0));
    assert_eq!(read(&fx.path), COMMENTED);
}

#[test]
fn add_and_remove_targets() {
    let fx = fixture(COMMENTED);
    let new = TargetDraft {
        name: "Receipts".into(),
        sources: vec!["camera".into()],
        description: Some("Receipts.".into()),
        outputs: vec![OutputDraft {
            dir: "/tmp/gallery-edit-test/out/receipts".into(),
            ..Default::default()
        }],
        ..Default::default()
    };
    save_ok(
        &fx,
        &[EditOp::AddTarget {
            stage: Stage::Ai,
            target: new,
        }],
    );
    let (old, added) = diff(COMMENTED, &read(&fx.path));
    assert!(old.is_empty());
    assert_eq!(
        added,
        vec![
            "[[targets.ai]]",
            r#"name = "Receipts""#,
            r#"sources = ["camera"]"#,
            r#"description = "Receipts.""#,
            "[[targets.ai.outputs]]",
            r#"dir = "/tmp/gallery-edit-test/out/receipts""#,
            "",
        ]
    );
    save_ok(
        &fx,
        &[EditOp::RemoveTarget {
            stage: Stage::Ai,
            index: 1,
        }],
    );
    assert_eq!(read(&fx.path), COMMENTED);

    // Removing the first target: the stage heading stays for the next one.
    save_ok(
        &fx,
        &[EditOp::RemoveTarget {
            stage: Stage::BeforeAi,
            index: 0,
        }],
    );
    let after = read(&fx.path);
    assert!(after.contains("# ---- stage 1: masks before AI ----\n\n# TikTok junk.\n[[targets.before_ai]]\nname = \"TikTok\""), "{after}");
    assert!(!after.contains("Screenshots"));
}

#[test]
fn adds_to_an_empty_stage_after_the_earlier_stages() {
    let text = "[[sources]]\nid = \"s\"\npath = \"/tmp/gallery-edit-test/s\"\n\n# AI\n[[targets.ai]]\nname = \"A\"\nsources = [\"s\"]\ndescription = \"a\"\n\n# after\n[[targets.after_ai]]\nname = \"B\"\n";
    let fx = fixture(text);
    let t = TargetDraft {
        name: "First".into(),
        filters: vec![FilterDraft {
            sources: vec!["s".into()],
            matches: Some(vec!["@photo".into()]),
            exclude: vec![],
        }],
        ..Default::default()
    };
    save_ok(
        &fx,
        &[EditOp::AddTarget {
            stage: Stage::BeforeAi,
            target: t,
        }],
    );
    let after = read(&fx.path);
    let i = after.find("[[targets.before_ai]]").unwrap();
    assert!(i < after.find("[[targets.ai]]").unwrap(), "{after}");
    assert!(i > after.find("[[sources]]").unwrap(), "{after}");
    assert!(after.contains("[[targets.before_ai]]\nname = \"First\"\n\n[[targets.before_ai.filters]]\nsources = [\"s\"]\nmatch = \"@photo\"\n"), "{after}");
    Config::load(&fx.path).unwrap().validate().unwrap();
}

#[test]
fn new_subtables_follow_the_files_spacing() {
    // COMMENTED has no blank line before sub-tables: new ones neither.
    let fx = fixture(COMMENTED);
    let mut t = target(&fx, Stage::AfterAi, 0);
    t.outputs.push(OutputDraft {
        dir: "/tmp/gallery-edit-test/third".into(),
        ..Default::default()
    });
    save_ok(
        &fx,
        &[EditOp::UpdateTarget {
            stage: Stage::AfterAi,
            index: 0,
            target: t,
        }],
    );
    assert!(read(&fx.path).contains("dir = \"/tmp/gallery-edit-test/backup\"\n[[targets.after_ai.outputs]]\ndir = \"/tmp/gallery-edit-test/third\"\n"));
}

#[test]
fn invalid_edit_is_not_written() {
    let fx = fixture(COMMENTED);
    let mut t = target(&fx, Stage::AfterAi, 0);
    t.outputs[0].content[1].preset = "avif-nope".into();
    let out = save(
        &fx.path,
        &version(&fx.path),
        &[EditOp::UpdateTarget {
            stage: Stage::AfterAi,
            index: 0,
            target: t,
        }],
    )
    .unwrap();
    assert!(!out.saved);
    let e = out.error.unwrap();
    assert!(e.message.contains("avif-nope"), "{e:?}");
    assert_eq!(e.path.as_deref(), Some("targets.after_ai[0]"));
    assert_eq!(read(&fx.path), COMMENTED);
    assert!(!file::backup_path(&fx.path).exists());
    // The draft still shows the bad value, to be fixed.
    assert_eq!(
        out.draft.targets.after_ai[0].outputs[0].content[1].preset,
        "avif-nope"
    );
}

#[test]
fn bad_mask_names_its_field() {
    let fx = fixture(COMMENTED);
    let mut t = target(&fx, Stage::BeforeAi, 0);
    t.filters[0].exclude = vec!["^(".into()];
    let out = draft(
        &fx.path,
        None,
        &[EditOp::UpdateTarget {
            stage: Stage::BeforeAi,
            index: 0,
            target: t,
        }],
    )
    .unwrap();
    let e = out.error.unwrap();
    assert_eq!(
        e.path.as_deref(),
        Some("targets.before_ai[0].filters[0].exclude"),
        "{e:?}"
    );

    let mut t = target(&fx, Stage::BeforeAi, 0);
    t.filters[0].matches = Some(vec!["@nope".into()]);
    let e = draft(
        &fx.path,
        None,
        &[EditOp::UpdateTarget {
            stage: Stage::BeforeAi,
            index: 0,
            target: t,
        }],
    )
    .unwrap()
    .error
    .unwrap();
    assert!(e.message.contains("@nope"), "{e:?}");
    assert_eq!(e.path.as_deref(), Some("targets.before_ai[0]"));
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
    let mut t = target(&fx, Stage::BeforeAi, 1);
    t.name = "TT".into();
    let e = save(
        &fx.path,
        &v,
        &[EditOp::UpdateTarget {
            stage: Stage::BeforeAi,
            index: 1,
            target: t,
        }],
    )
    .unwrap_err();
    assert_eq!(e.kind, FailureKind::Changed);
    assert!(read(&fx.path).ends_with("# edited elsewhere\n"));
}

#[test]
fn save_keeps_a_backup_and_returns_the_new_version() {
    let fx = fixture(COMMENTED);
    let mut t = target(&fx, Stage::BeforeAi, 1);
    t.name = "TT".into();
    let out = save_ok(
        &fx,
        &[EditOp::UpdateTarget {
            stage: Stage::BeforeAi,
            index: 1,
            target: t,
        }],
    );
    assert_eq!(read(&file::backup_path(&fx.path)), COMMENTED);
    assert_eq!(out.version, version(&fx.path));
    // The returned version saves again.
    let mut t = target(&fx, Stage::BeforeAi, 1);
    t.name = "TT2".into();
    let out2 = save(
        &fx.path,
        &out.version,
        &[EditOp::UpdateTarget {
            stage: Stage::BeforeAi,
            index: 1,
            target: t,
        }],
    )
    .unwrap();
    assert!(out2.saved);
    assert!(read(&file::backup_path(&fx.path)).contains("name = \"TT\""));
}

#[cfg(unix)]
#[test]
fn save_through_a_symlink_writes_its_target() {
    let fx = fixture(COMMENTED);
    let link = fx.path.with_file_name("link.toml");
    std::os::unix::fs::symlink(&fx.path, &link).unwrap();
    let mut t = target(&fx, Stage::BeforeAi, 1);
    t.name = "TT".into();
    let out = save(
        &link,
        &version(&link),
        &[EditOp::UpdateTarget {
            stage: Stage::BeforeAi,
            index: 1,
            target: t,
        }],
    )
    .unwrap();
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
            .any(|w| w.path == "targets.before_ai[0].outputs[0].dir"
                && w.message.contains("“shots”")),
        "{w:?}"
    );
    assert!(
        !w.iter().any(|w| w.path.starts_with("targets.after_ai")),
        "{w:?}"
    );

    let mut t = target(&fx, Stage::AfterAi, 0);
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
    let mut b = target(&fx, Stage::BeforeAi, 1);
    b.outputs.clear();
    let w = draft(
        &fx.path,
        None,
        &[
            EditOp::UpdateTarget {
                stage: Stage::AfterAi,
                index: 0,
                target: t,
            },
            EditOp::UpdateTarget {
                stage: Stage::BeforeAi,
                index: 1,
                target: b,
            },
        ],
    )
    .unwrap()
    .warnings;
    let double = w
        .iter()
        .find(|w| w.path == "targets.after_ai[0].outputs[0].content")
        .expect("double encode");
    assert!(double.message.contains("*.jpg"), "{double:?}");
    assert!(
        w.iter().any(|w| w.path == "targets.before_ai[1].outputs"),
        "{w:?}"
    );
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
    // The GUI editor is still stage-based (Task 8): it round-trips the old
    // three-stage example, kept as a fixture of the core crate. TODO(T8):
    // point it back at examples/gallery.toml once the editor reads the new shape.
    round_trip(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../gallery-core/tests/fixtures/staged-example.toml"),
    );
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
    let d = doc::read_draft(&doc).unwrap();
    // Rewriting every target and source with its own values changes nothing.
    for (stage, i, t) in d.targets.all() {
        ops::apply(
            &mut doc,
            &EditOp::UpdateTarget {
                stage,
                index: i,
                target: t.clone(),
            },
        )
        .unwrap();
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
    // Moving every mask target to the end of its stage and back, too.
    for stage in [Stage::BeforeAi, Stage::AfterAi] {
        let n = d.targets.stage(stage).len();
        for _ in 0..n {
            ops::apply(
                &mut doc,
                &EditOp::MoveTarget {
                    from: stage,
                    index: 0,
                    to: stage,
                    to_index: n - 1,
                },
            )
            .unwrap();
        }
    }
    assert_eq!(doc.to_string(), text);
}

const THREE: &str = r#"[[sources]]
id = "camera"
path = "/tmp/gallery-edit-test/camera"

# == A ==

# first
[[targets.after_ai]]
name = "A"
[[targets.after_ai.outputs]]
dir = "/tmp/gallery-edit-test/a"

# == B ==

# second
[[targets.after_ai]]
name = "B"
[[targets.after_ai.outputs]]
dir = "/tmp/gallery-edit-test/b"

# == C ==

# third
[[targets.after_ai]]
name = "C"
[[targets.after_ai.outputs]]
dir = "/tmp/gallery-edit-test/c"

# == end ==
"#;

fn remove_after_ai(fx: &Fx, index: usize) -> String {
    save_ok(
        fx,
        &[EditOp::RemoveTarget {
            stage: Stage::AfterAi,
            index,
        }],
    );
    read(&fx.path)
}

#[test]
fn removing_a_block_moves_its_heading_to_the_next_block() {
    // Middle: B's heading joins C's, C and its heading stay together.
    let fx = fixture(THREE);
    let after = remove_after_ai(&fx, 1);
    assert!(
        after.contains("# == B ==\n\n# == C ==\n\n# third\n[[targets.after_ai]]\nname = \"C\""),
        "{after}"
    );
    assert!(
        after.contains("# == A ==\n\n# first\n[[targets.after_ai]]\nname = \"A\""),
        "{after}"
    );
    assert!(!after.contains("second"));
    assert!(after.contains("# == end =="));

    // First: A's heading joins B's.
    let fx = fixture(THREE);
    let after = remove_after_ai(&fx, 0);
    assert!(
        after.contains("# == A ==\n\n# == B ==\n\n# second\n[[targets.after_ai]]\nname = \"B\""),
        "{after}"
    );
    assert!(
        after.contains("# == C ==\n\n# third\n[[targets.after_ai]]\nname = \"C\""),
        "{after}"
    );

    // Last: its heading is kept, in front of whatever follows.
    let fx = fixture(THREE);
    let after = remove_after_ai(&fx, 2);
    assert!(
        after.contains("# == B ==\n\n# second\n[[targets.after_ai]]\nname = \"B\""),
        "{after}"
    );
    assert!(
        after.contains("# == C ==") && after.contains("# == end =="),
        "{after}"
    );
    assert!(!after.contains("third"));
}

const INLINE_BEFORE: &str = r#"[[sources]]
id = "camera"
path = "/tmp/gallery-edit-test/camera"

[targets]
before_ai = [{ name = "X", filters = [{ sources = ["camera"] }], outputs = [{ dir = "/tmp/gallery-edit-test/x" }] }]
"#;

const INLINE_SOURCES: &str = r#"sources = [{ id = "camera", path = "/tmp/gallery-edit-test/camera" }]

[[targets.after_ai]]
name = "A"
sources = ["camera"]
[[targets.after_ai.outputs]]
dir = "/tmp/gallery-edit-test/a"
"#;

const INLINE_TARGETS: &str = r#"targets = { after_ai = [{ name = "X", outputs = [{ dir = "/tmp/gallery-edit-test/x" }] }] }

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
            draft(&fx.path, None, &[])
                .unwrap()
                .draft
                .targets
                .all()
                .count(),
            before.draft.targets.all().count()
        );
    }
}

#[test]
fn adding_to_an_inline_list_is_refused_not_wiped() {
    refused(
        INLINE_BEFORE,
        &[
            EditOp::AddTarget {
                stage: Stage::BeforeAi,
                target: a_target("Y"),
            },
            EditOp::MoveTarget {
                from: Stage::BeforeAi,
                index: 0,
                to: Stage::AfterAi,
                to_index: 0,
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
                stage: Stage::AfterAi,
                target: a_target("Y"),
            },
            EditOp::AddTarget {
                stage: Stage::BeforeAi,
                target: a_target("Y"),
            },
        ],
    );
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
    let mut t = target(&fx, Stage::Ai, 0);
    assert_eq!(
        t.description.as_deref(),
        Some("Photos of documents.\nReceipts too.\n")
    );
    t.description = Some("Photos of documents.\nReceipts, \"tickets\" and \\ more.\n".into());
    save_ok(
        &fx,
        &[EditOp::UpdateTarget {
            stage: Stage::Ai,
            index: 0,
            target: t.clone(),
        }],
    );
    let after = read(&fx.path);
    assert!(
        after.contains("description = \"\"\"\nPhotos of documents.\nReceipts, \\\"tickets\\\" and \\\\ more.\n\"\"\"\n"),
        "{after}"
    );
    assert_eq!(target(&fx, Stage::Ai, 0), t);
    // A single-line replacement is still one line.
    t.description = Some("One line.".into());
    save_ok(
        &fx,
        &[EditOp::UpdateTarget {
            stage: Stage::Ai,
            index: 0,
            target: t,
        }],
    );
    assert!(read(&fx.path).contains("description = \"One line.\"\n"));
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

fn rename_op(fx: &Fx, to: &str) -> EditOp {
    let mut t = target(fx, Stage::Ai, 0);
    t.name = to.into();
    EditOp::UpdateTarget {
        stage: Stage::Ai,
        index: 0,
        target: t,
    }
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
        .find(|w| w.path == "targets.ai[0].name")
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
    assert!(out.warnings.iter().all(|w| w.path != "targets.ai[0].name"));
    assert_eq!(out.renames[0].overrides, 0);
}

fn update_named(fx: &Fx, index: usize, to: &str) -> EditOp {
    let mut t = target(fx, Stage::BeforeAi, index);
    t.name = to.into();
    EditOp::UpdateTarget {
        stage: Stage::BeforeAi,
        index,
        target: t,
    }
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

const TWO: &str = "[[sources]]\nid = \"s\"\npath = \"/tmp/gallery-edit-test/camera\"\n\n[[targets.before_ai]]\nname = \"A\"\n\n[[targets.before_ai]]\nname = \"B\"\n";

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

/// The loaded config's targets of one old stage, in order.
fn stage(cfg: &Config, s: Stage) -> Vec<&gallery_core::config::Target> {
    super::model::staged(cfg)
        .filter(|(st, _, _)| *st == s)
        .map(|(_, _, t)| t)
        .collect()
}
