//! Edit operations on the user's presets and masks.

use std::path::{Path, PathBuf};

use super::model::{ContentPresetDraft, MaskDraft, NamePresetDraft, PresetKind};
use super::*;

/// A hand-written config with presets and masks of every form.
const PRESETS: &str = r#"# My gallery

[[sources]]
id = "camera"
path = "/tmp/gallery-preset-test/camera"

# ---- my masks ----

[masks]
# phone shots
phone = ["@photo", "@video"]   # both
docs = "*scan*"
phone-docs = ["@phone", "@docs"]

# ---- my presets ----

# Short names.
[name_presets.short]
template = "{date:YYYYMMDD}_{stem}.{ext}"   # day only
date = "capture"
mask = "@phone"

[name_presets."TikTok"]
template = "tiktok_{date}_{stem}.{ext}"
date = "file_created"

# Small photos.
[content_presets.tiny]
kind = "photo"
format = "avif"
quality = 40   # small
speed = 8

[content_presets.voice]
kind = "audio"
codec = "opus"
kbps = 32
mask = "*.m4a"

# ---- targets ----

[[targets]]
name = "Timeline"
[[targets.filters]]
sources = ["camera"]
match = "@phone"
exclude = ["@docs"]
[[targets.outputs]]
dir = "/tmp/gallery-preset-test/out"
content = ["tiny", { mask = "@phone-docs", preset = "voice" }]
rename = ["short", "capture-date"]

# trailing note
"#;

struct Fx {
    _dir: tempfile::TempDir,
    path: PathBuf,
}

fn fixture(text: &str) -> Fx {
    let dir = tempfile::tempdir().unwrap();
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

fn save_ok(fx: &Fx, ops: &[EditOp]) -> EditOutcome {
    let out = save(&fx.path, &version(&fx.path), ops).unwrap();
    assert!(out.error.is_none(), "{:?}", out.error);
    assert!(out.saved);
    out
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

fn current(fx: &Fx) -> model::ConfigDraft {
    draft(&fx.path, None, &[]).unwrap().draft
}

fn op_error(fx: &Fx, ops: &[EditOp]) -> String {
    let before = read(&fx.path);
    let e = save(&fx.path, &version(&fx.path), ops).unwrap_err();
    assert_eq!(e.kind, FailureKind::Op, "{e:?}");
    assert_eq!(read(&fx.path), before, "the file must not change");
    e.message
}

#[test]
fn reads_presets_and_masks() {
    let fx = fixture(PRESETS);
    let out = draft(&fx.path, None, &[]).unwrap();
    assert!(out.error.is_none(), "{:?}", out.error);
    let d = out.draft;
    assert_eq!(
        d.mask_defs,
        vec![
            MaskDraft {
                name: "phone".into(),
                items: vec!["@photo".into(), "@video".into()]
            },
            MaskDraft {
                name: "docs".into(),
                items: vec!["*scan*".into()]
            },
            MaskDraft {
                name: "phone-docs".into(),
                items: vec!["@phone".into(), "@docs".into()]
            },
        ]
    );
    assert_eq!(d.masks, vec!["phone", "docs", "phone-docs"]);
    assert_eq!(
        d.name_presets[0],
        NamePresetDraft {
            name: "short".into(),
            mask: Some(vec!["@phone".into()]),
            template: "{date:YYYYMMDD}_{stem}.{ext}".into(),
            date: Some("capture".into()),
            skip_if: None,
            normalize_ext: None,
        }
    );
    assert_eq!(d.name_presets[1].name, "TikTok");
    let tiny = &d.content_presets[0];
    assert_eq!((tiny.name.as_str(), tiny.kind.as_str()), ("tiny", "photo"));
    assert_eq!(
        (tiny.quality, tiny.speed, tiny.format.as_deref()),
        (Some(40), Some(8), Some("avif"))
    );
    assert_eq!(tiny.ladder, None);
    let voice = &d.content_presets[1];
    assert_eq!(
        (voice.codec.as_deref(), voice.kbps),
        (Some("opus"), Some(32))
    );
    assert_eq!(voice.mask, Some(vec!["*.m4a".to_string()]));
}

#[test]
fn updating_a_content_preset_changes_only_its_line() {
    let fx = fixture(PRESETS);
    let mut tiny = current(&fx).content_presets[0].clone();
    tiny.quality = Some(45);
    save_ok(
        &fx,
        &[EditOp::UpdateContentPreset {
            name: "tiny".into(),
            preset: tiny,
        }],
    );
    let (old, new) = diff(PRESETS, &read(&fx.path));
    assert_eq!(old, vec!["quality = 40   # small"]);
    assert_eq!(new, vec!["quality = 45   # small"]);
}

#[test]
fn updating_a_name_preset_and_a_mask_touches_only_their_lines() {
    let fx = fixture(PRESETS);
    let d = current(&fx);
    let mut short = d.name_presets[0].clone();
    short.normalize_ext = Some(true);
    let mut docs = d.mask_defs[1].clone();
    docs.items = vec!["*scan*".into(), "*doc*".into()];
    save_ok(
        &fx,
        &[
            EditOp::UpdateNamePreset {
                name: "short".into(),
                preset: short,
            },
            EditOp::UpdateMask {
                name: "docs".into(),
                mask: docs,
            },
        ],
    );
    let (old, new) = diff(PRESETS, &read(&fx.path));
    assert_eq!(old[0], r#"docs = "*scan*""#);
    assert_eq!(new[0], r#"docs = ["*scan*", "*doc*"]"#);
    let after = read(&fx.path);
    assert!(
        after.contains("mask = \"@phone\"\nnormalize_ext = true\n"),
        "{after}"
    );
    // Everything else as written.
    assert!(after.contains("phone = [\"@photo\", \"@video\"]   # both"));
    assert!(after.contains("# trailing note"));
}

#[test]
fn changing_the_kind_drops_the_old_kinds_keys() {
    let fx = fixture(PRESETS);
    let mut tiny = current(&fx).content_presets[0].clone();
    tiny.kind = "png".into();
    tiny.level = Some(5);
    // The form keeps the photo values around; they are not written.
    let out = save_ok(
        &fx,
        &[EditOp::UpdateContentPreset {
            name: "tiny".into(),
            preset: tiny,
        }],
    );
    let after = read(&fx.path);
    assert!(
        after.contains("[content_presets.tiny]\nkind = \"png\"\nlevel = 5\n"),
        "{after}"
    );
    let t = &out.draft.content_presets[0];
    assert_eq!((t.quality, t.format.as_deref()), (None, None));
}

#[test]
fn adds_presets_and_masks_next_to_their_kind() {
    let fx = fixture(PRESETS);
    let out = save_ok(
        &fx,
        &[
            EditOp::AddNamePreset {
                preset: NamePresetDraft {
                    name: "day".into(),
                    template: "{date:YYYYMMDD}.{ext}".into(),
                    date: Some("capture".into()),
                    ..Default::default()
                },
            },
            EditOp::AddContentPreset {
                preset: ContentPresetDraft {
                    name: "slow-av1".into(),
                    kind: "video".into(),
                    crf: Some(28),
                    preset: Some(2),
                    ..Default::default()
                },
            },
            EditOp::AddMask {
                mask: MaskDraft {
                    name: "scans".into(),
                    items: vec!["*scan*".into()],
                },
            },
        ],
    );
    let after = read(&fx.path);
    // Untouched text stays byte for byte: without the added lines it is the original.
    let stripped = after
        .replace("scans = \"*scan*\"\n", "")
        .replace(
            "\n[name_presets.day]\ntemplate = \"{date:YYYYMMDD}.{ext}\"\ndate = \"capture\"\n",
            "",
        )
        .replace(
            "\n[content_presets.slow-av1]\nkind = \"video\"\ncrf = 28\npreset = 2\n",
            "",
        );
    assert_eq!(stripped, PRESETS);
    let pos = |s: &str| after.find(s).unwrap_or_else(|| panic!("{s} in {after}"));
    assert!(pos("phone-docs = ") < pos("scans = \"*scan*\""));
    assert!(pos("scans = ") < pos("# ---- my presets"));
    assert!(pos("[name_presets.\"TikTok\"]") < pos("[name_presets.day]"));
    assert!(pos("[content_presets.voice]") < pos("[content_presets.slow-av1]"));
    assert!(pos("[content_presets.slow-av1]") < pos("# ---- targets"));
    assert!(after.contains("[content_presets.slow-av1]\nkind = \"video\"\ncrf = 28\npreset = 2\n"));
    let d = out.draft;
    assert_eq!(d.name_presets.len(), 3);
    assert_eq!(d.content_presets.len(), 3);
    assert_eq!(d.mask_defs.len(), 4);
}

#[test]
fn adds_to_a_config_without_presets_before_the_targets() {
    let text = r#"[[sources]]
id = "camera"
path = "/tmp/gallery-preset-test/camera"

# ---- targets ----

[[targets]]
name = "T"
[[targets.filters]]
sources = ["camera"]
"#;
    let fx = fixture(text);
    save_ok(
        &fx,
        &[
            EditOp::AddMask {
                mask: MaskDraft {
                    name: "scans".into(),
                    items: vec!["*scan*".into()],
                },
            },
            EditOp::AddContentPreset {
                preset: ContentPresetDraft {
                    name: "small".into(),
                    kind: "png".into(),
                    level: Some(2),
                    ..Default::default()
                },
            },
        ],
    );
    let after = read(&fx.path);
    let pos = |s: &str| after.find(s).unwrap_or_else(|| panic!("{s} in {after}"));
    assert!(
        pos("[masks]\nscans = \"*scan*\"") < pos("# ---- targets"),
        "{after}"
    );
    assert!(
        pos("[content_presets.small]") < pos("# ---- targets"),
        "{after}"
    );
    assert!(after.starts_with(
        "[[sources]]\nid = \"camera\"\npath = \"/tmp/gallery-preset-test/camera\"\n\n"
    ));
}

#[test]
fn removes_an_unused_preset_and_mask() {
    let fx = fixture(&PRESETS.replace(
        r#"{ mask = "@phone-docs", preset = "voice" }"#,
        r#""voice""#,
    ));
    let out = save_ok(
        &fx,
        &[
            EditOp::RemoveNamePreset {
                name: "TikTok".into(),
            },
            EditOp::RemoveMask {
                name: "phone-docs".into(),
            },
        ],
    );
    assert!(out.error.is_none());
    let after = read(&fx.path);
    assert!(!after.contains("TikTok") && !after.contains("tiktok_"));
    assert!(!after.contains("phone-docs ="), "{after}");
    // Neighbours untouched.
    assert!(after.contains(
        "[name_presets.short]\ntemplate = \"{date:YYYYMMDD}_{stem}.{ext}\"   # day only\n"
    ));
    assert!(after.contains("# Small photos.\n[content_presets.tiny]"));
}

#[test]
fn removing_the_last_mask_removes_the_empty_masks_table() {
    let before = PRESETS
        .replace(
            "[masks]\n# phone shots\nphone = [\"@photo\", \"@video\"]   # both\ndocs = \"*scan*\"\nphone-docs = [\"@phone\", \"@docs\"]\n",
            "[masks]\nsolo = \"*.png\"\n",
        )
        .replace("@phone-docs", "*.jpg")
        .replace("\"@phone\"", "\"*.jpg\"")
        .replace("[\"@docs\"]", "[\"*scan*\"]");
    assert!(before.contains("[masks]\nsolo"), "fixture");
    let fx = fixture(&before);
    save_ok(
        &fx,
        &[EditOp::RemoveMask {
            name: "solo".into(),
        }],
    );
    let after = read(&fx.path);
    assert!(!after.contains("[masks]"), "{after}");
    // Only the table (with its section heading) is gone.
    assert_eq!(
        after,
        before.replace("# ---- my masks ----\n\n[masks]\nsolo = \"*.png\"\n\n", "")
    );
}

#[test]
fn removal_of_a_used_preset_or_mask_is_refused_with_the_references() {
    let fx = fixture(PRESETS);
    let m = op_error(
        &fx,
        &[EditOp::RemoveContentPreset {
            name: "voice".into(),
        }],
    );
    assert!(m.contains("voice") && m.contains("Timeline"), "{m}");
    let m = op_error(
        &fx,
        &[EditOp::RemoveMask {
            name: "phone".into(),
        }],
    );
    // Used by a filter, a name preset and another mask.
    assert!(m.contains("Timeline"), "{m}");
    assert!(m.contains("short"), "{m}");
    assert!(m.contains("phone-docs"), "{m}");
    let m = op_error(
        &fx,
        &[EditOp::RemoveNamePreset {
            name: "short".into(),
        }],
    );
    assert!(m.contains("Timeline"), "{m}");
}

#[test]
fn renaming_updates_references_in_the_same_edit() {
    let fx = fixture(PRESETS);
    save_ok(
        &fx,
        &[
            EditOp::RenamePreset {
                kind: PresetKind::Mask,
                from: "phone".into(),
                to: "mobile".into(),
                update_refs: true,
            },
            EditOp::RenamePreset {
                kind: PresetKind::Content,
                from: "voice".into(),
                to: "speech".into(),
                update_refs: true,
            },
            EditOp::RenamePreset {
                kind: PresetKind::Name,
                from: "short".into(),
                to: "by-day".into(),
                update_refs: true,
            },
        ],
    );
    let after = read(&fx.path);
    assert!(!after.contains("@phone\""), "{after}");
    assert!(
        after.contains("mobile = [\"@photo\", \"@video\"]   # both"),
        "{after}"
    );
    assert!(
        after.contains("phone-docs = [\"@mobile\", \"@docs\"]"),
        "{after}"
    );
    assert!(after.contains("match = \"@mobile\""), "{after}");
    assert!(after.contains("[name_presets.by-day]\ntemplate"), "{after}");
    assert!(after.contains("mask = \"@mobile\"\n"), "{after}");
    assert!(after.contains("[content_presets.speech]"), "{after}");
    assert!(
        after.contains(r#"content = ["tiny", { mask = "@phone-docs", preset = "speech" }]"#),
        "{after}"
    );
    assert!(
        after.contains(r#"rename = ["by-day", "capture-date"]"#),
        "{after}"
    );
    // The renamed ones keep their place and comments.
    let pos = |s: &str| after.find(s).unwrap();
    assert!(pos("# phone shots\nmobile = ") < pos("docs = "));
    assert!(pos("# Short names.\n[name_presets.by-day]") < pos("[name_presets.\"TikTok\"]"));
    assert!(pos("[content_presets.tiny]") < pos("[content_presets.speech]"));
}

#[test]
fn renaming_without_reference_update_is_invalid_and_not_written() {
    let fx = fixture(PRESETS);
    let before = read(&fx.path);
    let out = save(
        &fx.path,
        &version(&fx.path),
        &[EditOp::RenamePreset {
            kind: PresetKind::Content,
            from: "voice".into(),
            to: "speech".into(),
            update_refs: false,
        }],
    )
    .unwrap();
    assert!(!out.saved);
    let e = out.error.unwrap();
    assert!(e.message.contains("voice"), "{e:?}");
    assert_eq!(read(&fx.path), before);
}

#[test]
fn renaming_onto_a_taken_name_is_refused() {
    let fx = fixture(PRESETS);
    let m = op_error(
        &fx,
        &[EditOp::RenamePreset {
            kind: PresetKind::Mask,
            from: "docs".into(),
            to: "phone".into(),
            update_refs: true,
        }],
    );
    assert!(m.contains("phone"), "{m}");
    let m = op_error(
        &fx,
        &[EditOp::RenamePreset {
            kind: PresetKind::Mask,
            from: "docs".into(),
            to: "photo".into(),
            update_refs: true,
        }],
    );
    assert!(m.contains("built-in"), "{m}");
}

#[test]
fn copy_as_own_writes_the_builtin_fields_under_a_new_name() {
    let fx = fixture(PRESETS);
    let out = save_ok(
        &fx,
        &[
            EditOp::CopyBuiltin {
                kind: PresetKind::Content,
                from: "avif-high".into(),
                to: "my-avif".into(),
            },
            EditOp::CopyBuiltin {
                kind: PresetKind::Name,
                from: "capture-date".into(),
                to: "my-date".into(),
            },
            EditOp::CopyBuiltin {
                kind: PresetKind::Mask,
                from: "media".into(),
                to: "my-media".into(),
            },
        ],
    );
    let d = out.draft;
    let avif = d
        .content_presets
        .iter()
        .find(|p| p.name == "my-avif")
        .unwrap();
    assert_eq!(avif.kind, "photo");
    assert_eq!((avif.quality, avif.speed), (Some(72), Some(6)));
    assert_eq!(avif.format.as_deref(), Some("avif"));
    assert_eq!(avif.ladder, Some(true));
    let date = d.name_presets.iter().find(|p| p.name == "my-date").unwrap();
    assert_eq!(date.template, "{date:YYYYMMDD_HHMMSS}.{ext}");
    assert_eq!(date.skip_if, Some(vec!["@dated".to_string()]));
    let m = d.mask_defs.iter().find(|m| m.name == "my-media").unwrap();
    assert_eq!(m.items, vec!["@photo", "@video", "@png"]);
    // The copy behaves as the built-in.
    let cfg = gallery_core::config::Config::load(&fx.path).unwrap();
    assert_eq!(
        cfg.content_presets["my-avif"].output_ext("jpg"),
        "avif".to_string()
    );
}

#[test]
fn copy_as_own_refuses_a_clashing_name() {
    let fx = fixture(PRESETS);
    for (kind, from, to) in [
        (PresetKind::Content, "avif-high", "avif-max"),
        (PresetKind::Content, "avif-high", "tiny"),
        (PresetKind::Name, "capture-date", "short"),
        (PresetKind::Name, "capture-date", "file-date"),
        (PresetKind::Mask, "media", "photo"),
        (PresetKind::Mask, "media", "docs"),
        (PresetKind::Mask, "media", ""),
        (PresetKind::Content, "no-such", "fresh"),
    ] {
        let m = op_error(
            &fx,
            &[EditOp::CopyBuiltin {
                kind,
                from: from.into(),
                to: to.into(),
            }],
        );
        assert!(!m.is_empty());
    }
}

#[test]
fn adding_a_taken_name_is_refused() {
    let fx = fixture(PRESETS);
    op_error(
        &fx,
        &[EditOp::AddMask {
            mask: MaskDraft {
                name: "docs".into(),
                items: vec!["*".into()],
            },
        }],
    );
    op_error(
        &fx,
        &[EditOp::AddContentPreset {
            preset: ContentPresetDraft {
                name: "av1-high".into(),
                kind: "video".into(),
                ..Default::default()
            },
        }],
    );
}

#[test]
fn out_of_range_values_name_their_field() {
    let fx = fixture(PRESETS);
    let d = current(&fx);
    let mut tiny = d.content_presets[0].clone();
    tiny.speed = Some(11);
    let out = draft(
        &fx.path,
        None,
        &[EditOp::UpdateContentPreset {
            name: "tiny".into(),
            preset: tiny.clone(),
        }],
    )
    .unwrap();
    let e = out.error.unwrap();
    assert_eq!(e.path.as_deref(), Some("content_presets.tiny.speed"));
    assert!(e.message.contains("0") && e.message.contains("10"), "{e:?}");
    // WebP's method goes to 6 only.
    tiny.speed = Some(7);
    tiny.format = Some("webp".into());
    let out = draft(
        &fx.path,
        None,
        &[EditOp::UpdateContentPreset {
            name: "tiny".into(),
            preset: tiny,
        }],
    )
    .unwrap();
    assert_eq!(
        out.error.unwrap().path.as_deref(),
        Some("content_presets.tiny.speed")
    );
    let mut short = d.name_presets[0].clone();
    short.template = " ".into();
    let out = draft(
        &fx.path,
        None,
        &[EditOp::UpdateNamePreset {
            name: "short".into(),
            preset: short,
        }],
    )
    .unwrap();
    assert_eq!(
        out.error.unwrap().path.as_deref(),
        Some("name_presets.short.template")
    );
}

#[test]
fn a_mask_cycle_is_located_on_a_mask() {
    let fx = fixture(PRESETS);
    let out = draft(
        &fx.path,
        None,
        &[EditOp::UpdateMask {
            name: "docs".into(),
            mask: MaskDraft {
                name: "docs".into(),
                items: vec!["@phone-docs".into()],
            },
        }],
    )
    .unwrap();
    let e = out.error.unwrap();
    assert!(e.message.contains("cycle"), "{e:?}");
    assert!(e.path.unwrap().starts_with("masks."));
}

#[test]
fn usage_lists_where_each_user_item_is_used() {
    let fx = fixture(PRESETS);
    let out = draft(&fx.path, None, &[]).unwrap();
    let u = &out.usage;
    assert_eq!(u.content["voice"].len(), 1);
    assert!(u.content["voice"][0].contains("Timeline"));
    assert!(u.name.get("TikTok").is_none_or(Vec::is_empty));
    assert!(u.masks["phone"].len() >= 3, "{:?}", u.masks["phone"]);
    // Own items with descriptions and expanded masks.
    let own = out.own.as_ref().unwrap();
    let tiny = own.content.iter().find(|p| p.name == "tiny").unwrap();
    assert!(tiny.description.contains("AVIF"), "{}", tiny.description);
    let phone = own.masks.iter().find(|m| m.name == "phone").unwrap();
    assert!(!phone.expanded.contains('@'));
}

#[test]
fn name_preview_runs_the_core_pipe() {
    let fx = fixture(PRESETS);
    let names = vec![
        "IMG_0001.JPEG".to_string(),
        "20260102_030405.jpg".into(),
        "song.mp3".into(),
    ];
    let p = NamePresetDraft {
        name: "x".into(),
        template: "{date:YYYY}/{date:YYYYMMDD_HHMMSS}.{ext}".into(),
        date: Some("capture".into()),
        skip_if: Some(vec!["@dated".into()]),
        normalize_ext: Some(true),
        mask: Some(vec!["@photo".into()]),
    };
    let r = preview_name(
        &fx.path,
        &[],
        &NameSample {
            preset: PresetRef::Draft(p.clone()),
            names: names.clone(),
            date: Some("2026-11-05 10:15:00".into()),
            source: None,
        },
    )
    .unwrap();
    assert_eq!(r[0].result, "2026/20261105_101500.jpg");
    assert_eq!(r[0].status, NameStatus::Renamed);
    assert_eq!(r[1].status, NameStatus::Skipped);
    assert_eq!(r[1].result, "20260102_030405.jpg");
    assert_eq!(r[2].status, NameStatus::NotMatched);
    // No date: the template needs one, nothing changes.
    let r = preview_name(
        &fx.path,
        &[],
        &NameSample {
            preset: PresetRef::Draft(p),
            names: names.clone(),
            date: None,
            source: None,
        },
    )
    .unwrap();
    assert_eq!(r[0].status, NameStatus::NoDate);
    // A preset by name: a built-in, or the user's own (masks of the config).
    let r = preview_name(
        &fx.path,
        &[],
        &NameSample {
            preset: PresetRef::Named("short".into()),
            names,
            date: Some("2026-11-05 10:15:00".into()),
            source: None,
        },
    )
    .unwrap();
    assert_eq!(r[0].result, "20261105_IMG_0001.JPEG");
    assert_eq!(r[2].status, NameStatus::NotMatched);
}

#[test]
fn name_preview_reports_a_bad_date_and_unsafe_template() {
    let fx = fixture(PRESETS);
    let e = preview_name(
        &fx.path,
        &[],
        &NameSample {
            preset: PresetRef::Named("capture-date".into()),
            names: vec!["a.jpg".into()],
            date: Some("yesterday".into()),
            source: None,
        },
    )
    .unwrap_err();
    assert!(e.contains("date"), "{e}");
    let e = preview_name(
        &fx.path,
        &[],
        &NameSample {
            preset: PresetRef::Draft(NamePresetDraft {
                name: "x".into(),
                template: "../{stem}.{ext}".into(),
                ..Default::default()
            }),
            names: vec!["a.jpg".into()],
            date: None,
            source: None,
        },
    )
    .unwrap_err();
    assert!(e.contains(".."), "{e}");
}

#[test]
fn limits_cover_every_numeric_field() {
    let l = limits::all();
    let has = |kind: &str, key: &str| l.iter().any(|r| r.kind == kind && r.key == key);
    for (kind, key) in [
        ("photo", "quality"),
        ("photo", "speed"),
        ("photo", "max_short_side"),
        ("photo", "min_saving_pct"),
        ("video", "crf"),
        ("video", "preset"),
        ("video", "gop"),
        ("video", "audio_kbps"),
        ("video", "max_short_side"),
        ("audio", "kbps"),
        ("png", "level"),
    ] {
        assert!(has(kind, key), "{kind}.{key}");
    }
    let crf = l.iter().find(|r| r.key == "crf").unwrap();
    assert_eq!((crf.min, crf.max), (0, 63));
}

#[test]
fn removing_the_first_preset_keeps_the_section_heading() {
    let text = PRESETS.replace(
        r#"rename = ["short", "capture-date"]"#,
        r#"rename = ["capture-date"]"#,
    );
    let fx = fixture(&text);
    save_ok(
        &fx,
        &[EditOp::RemoveNamePreset {
            name: "short".into(),
        }],
    );
    let after = read(&fx.path);
    assert!(!after.contains("# Short names."), "{after}");
    assert!(
        after.contains("\n# ---- my presets ----\n\n[name_presets.\"TikTok\"]\n"),
        "{after}"
    );
}

#[test]
fn inline_preset_entries_stay_inline() {
    let text = r#"[[sources]]
id = "camera"
path = "/tmp/gallery-preset-test/camera"

[name_presets]
day = { template = "{date:YYYYMMDD}.{ext}" }   # mine
"#;
    let fx = fixture(text);
    let d = current(&fx);
    assert_eq!(d.name_presets[0].template, "{date:YYYYMMDD}.{ext}");
    let mut day = d.name_presets[0].clone();
    day.date = Some("file_created".into());
    save_ok(
        &fx,
        &[
            EditOp::UpdateNamePreset {
                name: "day".into(),
                preset: day,
            },
            EditOp::AddNamePreset {
                preset: NamePresetDraft {
                    name: "plain".into(),
                    template: "{stem}.{ext}".into(),
                    ..Default::default()
                },
            },
        ],
    );
    let after = read(&fx.path);
    assert!(
        after.contains(
            r#"day = { template = "{date:YYYYMMDD}.{ext}", date = "file_created" }   # mine"#
        ),
        "{after}"
    );
    assert!(
        after.contains(r#"plain = { template = "{stem}.{ext}" }"#),
        "{after}"
    );
    assert!(!after.contains("[name_presets."), "{after}");
}

#[test]
fn ops_and_samples_come_in_the_ui_shape() {
    let op: EditOp = serde_json::from_value(serde_json::json!({
        "op": "rename_preset", "kind": "mask", "from": "a", "to": "b", "update_refs": true
    }))
    .unwrap();
    assert!(matches!(
        op,
        EditOp::RenamePreset {
            kind: PresetKind::Mask,
            ..
        }
    ));
    let op: EditOp = serde_json::from_value(serde_json::json!({
        "op": "update_content_preset", "name": "x",
        "preset": { "name": "x", "kind": "photo", "mask": null, "quality": 50, "format": "webp" }
    }))
    .unwrap();
    assert!(matches!(op, EditOp::UpdateContentPreset { preset, .. } if preset.quality == Some(50)));
    let s: NameSample = serde_json::from_value(serde_json::json!({
        "preset": "capture-date", "names": ["a.jpg"], "date": null
    }))
    .unwrap();
    assert!(matches!(s.preset, PresetRef::Named(n) if n == "capture-date"));
    let s: NameSample = serde_json::from_value(serde_json::json!({
        "preset": { "name": "x", "template": "{stem}.{ext}", "mask": null, "date": null, "skip_if": null, "normalize_ext": null },
        "names": []
    }))
    .unwrap();
    assert!(matches!(s.preset, PresetRef::Draft(_)));
    let out = serde_json::to_value(
        preview_name(
            Path::new("/nonexistent/gallery.toml"),
            &[],
            &NameSample {
                preset: PresetRef::Named("ext-lower".into()),
                names: vec!["A.JPEG".into()],
                date: None,
                source: None,
            },
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(out[0]["result"], "A.jpg");
    assert_eq!(out[0]["status"], "renamed");
}

use super::model::StepDraft;

fn step(preset: &str, mask: Option<&str>) -> StepDraft {
    StepDraft {
        preset: preset.into(),
        mask: mask.map(|m| vec![m.to_string()]),
    }
}

fn bundle(name: &str, steps: Vec<StepDraft>) -> ContentPresetDraft {
    ContentPresetDraft {
        name: name.into(),
        kind: "bundle".into(),
        steps: Some(steps),
        ..Default::default()
    }
}

const WITH_BUNDLE: &str = r#"[[sources]]
id = "camera"
path = "/tmp/gallery-preset-test/camera"

# Small photos.
[content_presets.tiny]
kind = "photo"
quality = 40   # small

# My pack.
[content_presets.pack]
kind = "bundle"
steps = ["tiny",   { mask = "*.png", preset = "png-avif" }]   # order matters

[[targets]]
name = "t"
[[targets.outputs]]
dir = "/tmp/gallery-preset-test/out"
content = ["pack"]
"#;

#[test]
fn bundles_read_add_and_update_leaving_the_rest_byte_identical() {
    let fx = fixture(WITH_BUNDLE);
    let d = current(&fx);
    let pack = d.content_presets.iter().find(|p| p.name == "pack").unwrap();
    assert_eq!(pack.kind, "bundle");
    assert_eq!(
        pack.steps,
        Some(vec![step("tiny", None), step("png-avif", Some("*.png"))])
    );
    assert_eq!(pack.quality, None);

    // Reorder: only the steps line changes.
    let mut p = pack.clone();
    p.steps.as_mut().unwrap().reverse();
    save_ok(
        &fx,
        &[EditOp::UpdateContentPreset {
            name: "pack".into(),
            preset: p,
        }],
    );
    let (old, new) = diff(WITH_BUNDLE, &read(&fx.path));
    assert_eq!(old.len(), 1, "{old:?}");
    assert!(old[0].starts_with("steps = ["), "{old:?}");
    assert!(new[0].starts_with("steps = [{ mask = \"*.png\", preset = \"png-avif\" }, \"tiny\"]"));
    assert!(new[0].ends_with("# order matters"), "{new:?}");

    // Add a second bundle next to the first.
    save_ok(
        &fx,
        &[EditOp::AddContentPreset {
            preset: bundle("pack2", vec![step("pack", None), step("av1-high", None)]),
        }],
    );
    let text = read(&fx.path);
    assert!(
        text.contains(
            "[content_presets.pack2]\nkind = \"bundle\"\nsteps = [\"pack\", \"av1-high\"]\n"
        ),
        "{text}"
    );
    assert!(
        gallery_core::config::Config::load(&fx.path)
            .unwrap()
            .validate()
            .is_ok()
    );
}

#[test]
fn bundle_references_block_removal_and_follow_a_rename() {
    let fx = fixture(WITH_BUNDLE);
    // `tiny` is used by the bundle; `pack` by the target.
    let m = op_error(
        &fx,
        &[EditOp::RemoveContentPreset {
            name: "tiny".into(),
        }],
    );
    assert!(m.contains("set “pack”"), "{m}");
    let m = op_error(
        &fx,
        &[EditOp::RemoveContentPreset {
            name: "pack".into(),
        }],
    );
    assert!(m.contains("target “t”"), "{m}");

    save_ok(
        &fx,
        &[EditOp::RenamePreset {
            kind: PresetKind::Content,
            from: "tiny".into(),
            to: "small".into(),
            update_refs: true,
        }],
    );
    let text = read(&fx.path);
    assert!(
        text.contains("steps = [\"small\", { mask = \"*.png\""),
        "{text}"
    );
    assert!(text.contains("# order matters"));
    assert!(text.contains("[content_presets.small]"));
}

#[test]
fn copy_builtin_bundle_as_own_and_bad_bundles_are_refused() {
    let fx = fixture(WITH_BUNDLE);
    let out = save_ok(
        &fx,
        &[EditOp::CopyBuiltin {
            kind: PresetKind::Content,
            from: "compress-all".into(),
            to: "my-all".into(),
        }],
    );
    let mine = out
        .draft
        .content_presets
        .iter()
        .find(|p| p.name == "my-all")
        .unwrap();
    assert_eq!(mine.kind, "bundle");
    let names: Vec<_> = mine
        .steps
        .iter()
        .flatten()
        .map(|s| s.preset.as_str())
        .collect();
    assert_eq!(names, ["avif-high", "png-avif", "webp-avif", "av1-high"]);

    let refused = |p: ContentPresetDraft| {
        let out = save(
            &fx.path,
            &version(&fx.path),
            &[EditOp::AddContentPreset { preset: p }],
        )
        .unwrap();
        assert!(!out.saved);
        out.error.unwrap().message
    };
    let mut with_mask = bundle("bad", vec![step("tiny", None)]);
    with_mask.mask = Some(vec!["*.png".into()]);
    assert!(refused(with_mask).contains("no mask of its own"));
    let m = refused(bundle("loop", vec![step("loop", None)]));
    assert!(m.contains("bundle cycle"), "{m}");
    assert!(refused(bundle("empty", vec![])).contains("no steps"));
}
