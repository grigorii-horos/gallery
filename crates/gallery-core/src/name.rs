//! Name presets: template rendering and the name pipe.

use std::collections::HashSet;
use std::path::PathBuf;

use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};

use crate::config::{DateSource, NamePreset};
use crate::mask::{MaskExpr, MaskScope};
use crate::media::split_name;

#[derive(Debug, Clone, Default)]
pub struct NameCtx {
    pub capture: Option<NaiveDateTime>,
    pub created: Option<NaiveDateTime>,
    pub source_id: String,
}

/// One pipe step. `matched` is whether the step's mask matched the current
/// file name; `after` is the relative path after the step (`/`-separated).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceStep {
    pub index: usize,
    pub preset: String,
    pub matched: bool,
    pub after: String,
    /// The pipe of the step; `None` in plans written before it was recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipe: Option<Pipe>,
    /// The bundle this content step was expanded from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle: Option<String>,
}

/// The two pipes of an output: content first, then the name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pipe {
    Content,
    Name,
}

/// Result of the name pipe: folders below the output directory, then the
/// file name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameTrace {
    pub subdir: Vec<String>,
    pub final_name: String,
    pub steps: Vec<TraceStep>,
}

impl NameTrace {
    /// `subdir/final_name`, built from components (no separator in strings).
    pub fn rel_path(&self) -> PathBuf {
        self.subdir.iter().chain([&self.final_name]).collect()
    }
}

/// Prefix of names Gallery itself uses inside output folders (staged
/// outputs, trash, undo leftovers): never a folder made by a template.
const RESERVED_PREFIX: &str = ".gallery-";

/// Split a rendered template at `/` into folders and a file name. Every part
/// must be a plain name: not empty (no leading `/`, no `//`, no trailing
/// `/`), not `.` or `..`; folders may not hold `\` or start with `.gallery-`.
fn split_rel(rendered: &str) -> std::result::Result<(Vec<String>, String), String> {
    let mut parts: Vec<String> = rendered.split('/').map(String::from).collect();
    let name = parts.pop().unwrap_or_default();
    for (i, p) in parts.iter().chain([&name]).enumerate() {
        let folder = i < parts.len();
        let bad = if p.is_empty() {
            Some("empty path component (leading, doubled or trailing '/')")
        } else if p == "." || p == ".." {
            Some("'.' or '..' path component")
        } else if folder && p.contains('\\') {
            Some("'\\' in a folder name; use '/' between folders")
        } else if folder && p.starts_with(RESERVED_PREFIX) {
            Some("folder name starting with '.gallery-'")
        } else {
            None
        };
        if let Some(why) = bad {
            return Err(format!("{why}: '{rendered}'"));
        }
    }
    Ok((parts, name))
}

/// Check a name preset's template at config load: rendered for a sample file
/// it must be a safe relative path, and it may not hold `\` at all (`/` is
/// the only folder separator). The error names the preset.
pub fn check_template(preset_name: &str, preset: &NamePreset) -> std::result::Result<(), String> {
    let err = |why: String| format!("name preset '{preset_name}': {why}");
    if preset.template.contains('\\') {
        return Err(err(format!(
            "'\\' in template '{}'; use '/' between folders",
            preset.template
        )));
    }
    let date = NaiveDateTime::parse_from_str("2026-01-02 03:04:05", "%Y-%m-%d %H:%M:%S").ok();
    let ctx = NameCtx {
        capture: date,
        created: date,
        source_id: "source".into(),
    };
    match render(preset, "IMG_0001.jpg", &ctx) {
        Some(r) => split_rel(&r).map(|_| ()).map_err(err),
        // Needs a date it never has: the step never changes anything.
        None => Ok(()),
    }
}

/// Translate `YYYYMMDD_HHMMSS`-style tokens (YYYY MM DD HH mm SS) to strftime.
/// `MM` is months, except right after `HH` (optionally past separators), where
/// it is minutes, so `HHMMSS` and `HH-MM-SS` read as time.
fn to_strftime(fmt: &str) -> String {
    let mut out = String::new();
    let mut rest = fmt;
    let mut after_hour = false;
    while let Some(c) = rest.chars().next() {
        let token = [
            ("YYYY", "%Y"),
            ("MM", if after_hour { "%M" } else { "%m" }),
            ("DD", "%d"),
            ("HH", "%H"),
            ("mm", "%M"),
            ("SS", "%S"),
        ]
        .into_iter()
        .find(|(tok, _)| rest.starts_with(tok));
        if let Some((tok, repl)) = token {
            out.push_str(repl);
            after_hour = tok == "HH";
            rest = &rest[tok.len()..];
            continue;
        }
        if c == '%' {
            out.push_str("%%");
        } else {
            out.push(c);
        }
        if c.is_alphanumeric() {
            after_hour = false;
        }
        rest = &rest[c.len_utf8()..];
    }
    out
}

fn normalize_ext(ext: &str) -> String {
    let e = ext.to_lowercase();
    if e == "jpeg" { "jpg".into() } else { e }
}

/// Render one preset; `None` when the template needs a date and there is none.
fn render(preset: &NamePreset, current: &str, ctx: &NameCtx) -> Option<String> {
    let date = match preset.date {
        DateSource::Capture => ctx.capture,
        DateSource::FileCreated => ctx.created,
        DateSource::None => None,
    };
    let (stem, ext) = split_name(current);
    let ext = if preset.normalize_ext {
        normalize_ext(&ext)
    } else {
        ext
    };
    let mut out = String::new();
    let mut rest = preset.template.as_str();
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        let Some(j) = rest[i..].find('}') else {
            rest = &rest[i..];
            break;
        };
        let key = &rest[i + 1..i + j];
        match key {
            "stem" => out.push_str(&stem),
            "ext" => out.push_str(&ext),
            "source" => out.push_str(&ctx.source_id),
            "date" => out.push_str(&date?.format("%Y%m%d_%H%M%S").to_string()),
            _ => match key.strip_prefix("date:") {
                Some(fmt) => out.push_str(&date?.format(&to_strftime(fmt)).to_string()),
                None => out.push_str(&rest[i..=i + j]),
            },
        }
        rest = &rest[i + j + 1..];
    }
    out.push_str(rest);
    Some(out)
}

/// Why a name step whose mask matched still changes nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameSkip {
    /// The preset's `skip_if` matches the current name.
    SkipIf,
    /// The template needs a date the file does not have.
    NoDate,
}

/// Whether the name step `preset` (its mask already matched) is skipped for
/// `name`; the same checks as [`apply_name_pipe`] makes.
pub fn name_skip(
    scope: &(impl MaskScope + ?Sized),
    preset: &NamePreset,
    name: &str,
    ctx: &NameCtx,
) -> Option<NameSkip> {
    if preset
        .skip_if
        .as_ref()
        .is_some_and(|s| s.matches_veto(name, scope))
    {
        return Some(NameSkip::SkipIf);
    }
    render(preset, name, ctx)
        .is_none()
        .then_some(NameSkip::NoDate)
}

/// Run the name pipe. Its state is (folders, file name), starting with no
/// folders and `current`. Each step applies when its mask matches the
/// *current file name*; `skip_if`, `{stem}` and `{ext}` see the file name too.
/// A rendered result with `/` sets both: its folders replace the current
/// ones; without `/` only the file name changes. A step changes nothing when
/// `skip_if` matches or when the template needs a date that is unavailable.
/// Steps are (mask, preset name, preset); the trace records the preset name.
/// A rendered path that is not a safe relative path is an error naming the
/// preset.
pub fn apply_name_pipe(
    scope: &(impl MaskScope + ?Sized),
    steps: &[(MaskExpr, &str, &NamePreset)],
    current: &str,
    ctx: &NameCtx,
) -> std::result::Result<NameTrace, String> {
    let mut subdir: Vec<String> = Vec::new();
    let mut name = current.to_string();
    let mut trace = Vec::with_capacity(steps.len());
    for (index, (mask, preset_name, preset)) in steps.iter().enumerate() {
        let matched = mask.matches(&name, scope);
        if matched
            && !preset
                .skip_if
                .as_ref()
                .is_some_and(|s| s.matches_veto(&name, scope))
            && let Some(new) = render(preset, &name, ctx)
        {
            let (dirs, file) =
                split_rel(&new).map_err(|why| format!("name preset '{preset_name}': {why}"))?;
            if new.contains('/') {
                subdir = dirs;
            }
            name = file;
        }
        let mut after = subdir.join("/");
        if !after.is_empty() {
            after.push('/');
        }
        after.push_str(&name);
        trace.push(TraceStep {
            index,
            preset: preset_name.to_string(),
            matched,
            after,
            pipe: Some(Pipe::Name),
            bundle: None,
        });
    }
    Ok(NameTrace {
        subdir,
        final_name: name,
        steps: trace,
    })
}

/// Pick a free name in a directory: `wanted`, else `{stem}_{n}.{ext}` with n
/// from 1. Comparison is case-insensitive (macOS/Windows), so `dir_names`
/// holds lowercase names; `existing` is asked about the candidate as written.
/// The chosen name is added to `dir_names`.
pub fn reserve(
    dir_names: &mut HashSet<String>,
    existing: impl Fn(&str) -> bool,
    wanted: &str,
) -> String {
    let taken = |set: &HashSet<String>, n: &str| set.contains(&n.to_lowercase()) || existing(n);
    let mut candidate = wanted.to_string();
    let (stem, ext) = split_name(wanted);
    let mut n = 0;
    while taken(dir_names, &candidate) {
        n += 1;
        candidate = if ext.is_empty() {
            format!("{stem}_{n}")
        } else {
            format!("{stem}_{n}.{ext}")
        };
    }
    dir_names.insert(candidate.to_lowercase());
    candidate
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DateSource;
    use std::path::Path;

    fn preset(t: &str, date: DateSource, skip: Option<&str>, norm: bool) -> NamePreset {
        NamePreset {
            mask: None,
            template: t.into(),
            date,
            skip_if: skip.map(|s| MaskExpr::parse(s).unwrap()),
            normalize_ext: norm,
        }
    }
    fn ctx() -> NameCtx {
        NameCtx {
            capture: Some(
                NaiveDateTime::parse_from_str("2026-04-12 10:33:14", "%Y-%m-%d %H:%M:%S").unwrap(),
            ),
            created: None,
            source_id: "phone".into(),
        }
    }
    fn m(s: &str) -> MaskExpr {
        MaskExpr::parse(s).unwrap()
    }

    #[test]
    fn name_pipe_trace() {
        let date = preset(
            "{date:YYYYMMDD_HHMMSS}.{ext}",
            DateSource::Capture,
            None,
            false,
        );
        let ext = preset("{stem}.{ext}", DateSource::None, None, true);
        let t = apply_name_pipe(
            &(),
            &[(m("*"), "date", &date), (m("*.jpeg"), "ext", &ext)],
            "IMG_20260412_103314.JPEG",
            &ctx(),
        )
        .unwrap();
        assert_eq!(t.final_name, "20260412_103314.jpg");
        assert_eq!(t.steps.len(), 2);
        assert_eq!(t.steps[0].after, "20260412_103314.JPEG");
        assert!(t.steps[1].matched);
        assert_eq!(t.steps[1].index, 1);
        assert_eq!(t.steps[0].preset, "date");
        assert_eq!(t.steps[1].preset, "ext");
    }

    #[test]
    fn name_pipe_no_date_keeps_name() {
        let date = preset(
            "{date:YYYYMMDD}_{stem}.{ext}",
            DateSource::Capture,
            None,
            false,
        );
        let c = NameCtx {
            capture: None,
            ..ctx()
        };
        let t = apply_name_pipe(&(), &[(m("*"), "date", &date)], "a.jpg", &c).unwrap();
        assert_eq!(t.final_name, "a.jpg");
        let none = preset("{date:YYYY}.{ext}", DateSource::None, None, false);
        assert_eq!(
            apply_name_pipe(&(), &[(m("*"), "none", &none)], "a.jpg", &ctx())
                .unwrap()
                .final_name,
            "a.jpg"
        );
    }

    #[test]
    fn name_pipe_mask_miss_skips() {
        let p = preset("x.{ext}", DateSource::None, None, false);
        let t = apply_name_pipe(&(), &[(m("*.png"), "p", &p)], "a.jpg", &ctx()).unwrap();
        assert_eq!(t.final_name, "a.jpg");
        assert!(!t.steps[0].matched);
        assert_eq!(t.steps[0].after, "a.jpg");
    }

    #[test]
    fn name_pipe_unresolved_skip_if_ref_skips() {
        let p = preset("x.{ext}", DateSource::None, Some("@nope"), false);
        let t = apply_name_pipe(&(), &[(m("*"), "p", &p)], "a.jpg", &ctx()).unwrap();
        assert_eq!(t.final_name, "a.jpg");
    }

    #[test]
    fn name_pipe_skip_if_and_source() {
        let p = preset(
            "{source}_{stem}.{ext}",
            DateSource::None,
            Some("^\\d{8}_"),
            false,
        );
        assert_eq!(
            apply_name_pipe(&(), &[(m("*"), "p", &p)], "20260412_x.jpg", &ctx())
                .unwrap()
                .final_name,
            "20260412_x.jpg"
        );
        assert_eq!(
            apply_name_pipe(&(), &[(m("*"), "p", &p)], "x.jpg", &ctx())
                .unwrap()
                .final_name,
            "phone_x.jpg"
        );
    }

    #[test]
    fn name_pipe_file_created() {
        let p = preset(
            "{date:YYYY-MM-DD}.{ext}",
            DateSource::FileCreated,
            None,
            false,
        );
        let c = NameCtx {
            created: ctx().capture,
            capture: None,
            ..ctx()
        };
        assert_eq!(
            apply_name_pipe(&(), &[(m("*"), "p", &p)], "a.jpg", &c)
                .unwrap()
                .final_name,
            "2026-04-12.jpg"
        );
    }

    fn folders() -> NamePreset {
        preset(
            "{date:YYYY}/{date:MM}/{date:YYYYMMDD_HHMMSS}.{ext}",
            DateSource::Capture,
            None,
            false,
        )
    }

    #[test]
    fn name_pipe_template_with_slash_makes_subdirs() {
        let ext = preset("{stem}.{ext}", DateSource::None, None, true);
        let f = folders();
        let t = apply_name_pipe(
            &(),
            &[(m("*"), "folders", &f), (m("*.jpeg"), "ext", &ext)],
            "Screenshot_x.JPEG",
            &ctx(),
        )
        .unwrap();
        // A step without `/` changes the file name only: the folders stay.
        assert_eq!(t.subdir, ["2026", "04"]);
        assert_eq!(t.final_name, "20260412_103314.jpg");
        assert_eq!(
            t.rel_path(),
            Path::new("2026").join("04").join("20260412_103314.jpg")
        );
        assert_eq!(t.steps[0].after, "2026/04/20260412_103314.JPEG");
        assert_eq!(t.steps[1].after, "2026/04/20260412_103314.jpg");
    }

    #[test]
    fn name_pipe_masks_and_stem_see_the_file_name_only() {
        let f = folders();
        let tag = preset("{stem}_x.{ext}", DateSource::None, None, false);
        let t = apply_name_pipe(
            &(),
            &[
                (m("*"), "folders", &f),
                (m("^2026/"), "path", &tag),
                (m("^20260412_103314\\.jpg$"), "tag", &tag),
            ],
            "a.jpg",
            &ctx(),
        )
        .unwrap();
        assert!(!t.steps[1].matched, "masks never see the folders");
        assert!(t.steps[2].matched);
        assert_eq!(t.final_name, "20260412_103314_x.jpg");
        assert_eq!(t.subdir, ["2026", "04"]);
    }

    #[test]
    fn name_pipe_slash_step_replaces_subdir() {
        let f = folders();
        let other = preset("x/{stem}.{ext}", DateSource::None, None, false);
        let t = apply_name_pipe(
            &(),
            &[(m("*"), "folders", &f), (m("*"), "other", &other)],
            "a.jpg",
            &ctx(),
        )
        .unwrap();
        assert_eq!(t.subdir, ["x"]);
        assert_eq!(t.final_name, "20260412_103314.jpg");
        assert_eq!(t.steps[1].after, "x/20260412_103314.jpg");
    }

    #[test]
    fn name_pipe_dateless_step_keeps_subdir() {
        let f = folders();
        let created = preset(
            "y/{date:YYYY}/{stem}.{ext}",
            DateSource::FileCreated,
            None,
            false,
        );
        let skip = preset("z/{stem}.{ext}", DateSource::None, Some("^2026"), false);
        let t = apply_name_pipe(
            &(),
            &[
                (m("*"), "folders", &f),
                (m("*"), "created", &created),
                (m("*"), "skip", &skip),
            ],
            "a.jpg",
            &ctx(),
        )
        .unwrap();
        assert_eq!(t.subdir, ["2026", "04"]);
        assert_eq!(t.steps[2].after, "2026/04/20260412_103314.jpg");
    }

    #[test]
    fn name_pipe_bad_rendered_path_names_the_preset() {
        let p = preset("{source}/{stem}.{ext}", DateSource::None, None, false);
        let c = NameCtx {
            source_id: String::new(),
            ..ctx()
        };
        let e = apply_name_pipe(&(), &[(m("*"), "Empty folder", &p)], "a.jpg", &c).unwrap_err();
        assert!(e.contains("Empty folder"), "{e}");
    }

    #[test]
    fn check_template_rejects_unsafe_paths() {
        for t in [
            "../x.{ext}",
            "/abs/{stem}.{ext}",
            "a//b.{ext}",
            "a\\b.{ext}",
            "./a.{ext}",
            "{stem}/",
        ] {
            let e =
                check_template("Bad", &preset(t, DateSource::Capture, None, false)).unwrap_err();
            assert!(e.contains("Bad"), "{t}: {e}");
        }
        check_template("ok", &folders()).unwrap();
        check_template("ok", &preset("{stem}.{ext}", DateSource::None, None, true)).unwrap();
    }

    #[test]
    fn reserve_suffixes() {
        let mut set = HashSet::new();
        let none = |_: &str| false;
        assert_eq!(reserve(&mut set, none, "a.jpg"), "a.jpg");
        assert_eq!(reserve(&mut set, none, "a.jpg"), "a_1.jpg");
        assert_eq!(reserve(&mut set, none, "a.jpg"), "a_2.jpg");
        assert_eq!(reserve(&mut set, none, "A.JPG"), "A_3.JPG");
        let on_disk = |n: &str| n == "b.jpg";
        assert_eq!(reserve(&mut set, on_disk, "b.jpg"), "b_1.jpg");
    }
}
