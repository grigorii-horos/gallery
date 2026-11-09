use super::*;
use crate::ai::cache::{self, Stored};
use crate::overrides::Action;
use crate::pipeline::NoAi;
use crate::plan::build_plan;
use crate::scan::scan_sources;

struct Fx {
    dir: tempfile::TempDir,
    cfg: Config,
}

impl Fx {
    fn p(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }
    fn explain(&self, rel: &str) -> Explanation {
        self.explain_with(rel, &Overrides::default())
    }
    fn explain_with(&self, rel: &str, o: &Overrides) -> Explanation {
        let opts = ExplainOptions {
            tools: None,
            ai_cache: Some(self.p("ai-cache.jsonl")),
        };
        explain(&self.cfg, &self.p(rel), o, &opts).unwrap()
    }
}

fn touch(p: &Path) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, b"x").unwrap();
}

fn fixture() -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for f in [
        "src/20260101_120000.png",
        "src/IMG_1.jpg",
        "src/a_thumb.jpg",
        "src/x_edit.gif",
        "src/clip.mp4",
        "src/cat.webp",
        "src/skip.tmp",
        "src/.hid.jpg",
        "flat/sub/a.jpg",
        "flat/b.jpg",
        "other/x.jpg",
    ] {
        touch(&root.join(f));
    }
    let mut noise = String::new();
    for i in 0..23 {
        noise.push_str(&format!(
            "\n[[targets.before_ai]]\nname = \"flat{i}\"\n[[targets.before_ai.filters]]\nsources = [\"flat\"]\nmatch = \"*.jpg\"\n[[targets.before_ai.outputs]]\ndir = \"{r}/out/f{i}\"\n",
            r = root.to_string_lossy().replace('\\', "/")
        ));
    }
    let toml = format!(
        r#"
[[sources]]
id = "s"
path = "{r}/src"
skip = ["*.tmp"]

[[sources]]
id = "flat"
path = "{r}/flat"

[name_presets.date-name]
template = "{{date:YYYYMMDD_HHMMSS}}.{{ext}}"

[[targets.before_ai]]
name = "pics"
[[targets.before_ai.filters]]
sources = ["s"]
match = ["@photo-original", "@png"]
exclude = "*_thumb*"
[[targets.before_ai.outputs]]
dir = "{r}/src"
content = ["avif-high"]
rename = ["date-name"]

[[targets.before_ai]]
name = "edits"
[[targets.before_ai.filters]]
sources = ["s"]
match = ["*.gif", "!*_edit*"]
[[targets.before_ai.outputs]]
dir = "{r}/out/gifs"

[[targets.ai]]
name = "cats"
sources = ["s"]
description = "cats"
prefilter = {{ match = "*.webp" }}
[[targets.ai.outputs]]
dir = "{r}/out/cats"
{noise}"#,
        r = root.to_string_lossy().replace('\\', "/")
    );
    let path = root.join("gallery.toml");
    std::fs::write(&path, toml).unwrap();
    let cfg = Config::load(&path).unwrap();
    cfg.validate().unwrap();
    Fx { dir, cfg }
}

#[test]
fn processed_file_shows_steps_and_destination() {
    let fx = fixture();
    let e = fx.explain("src/IMG_1.jpg");
    assert_eq!(e.outcome, Outcome::Process);
    let d = e.decision.as_ref().unwrap();
    assert_eq!(d.target.as_deref(), Some("pics"));
    let out = &e.outputs[0];
    assert!(out.content[0].matched);
    assert_eq!(out.content[0].after, "IMG_1.avif");
    assert_eq!(out.op, OutputOp::Write);
    assert!(out.rename[0].matched);
    assert_eq!(
        out.rename[0].skipped.as_deref(),
        Some("no date for the template")
    );
    assert!(e.verdict.starts_with("Will be processed"), "{}", e.verdict);
    assert!(e.targets.iter().any(|t| t.name == "pics" && t.taken));
}

#[test]
fn wrong_extension_names_the_match() {
    let fx = fixture();
    let e = fx.explain("src/clip.mp4");
    assert_eq!(e.decision.as_ref().unwrap().kind, DecisionKind::Unmatched);
    assert!(
        e.verdict.starts_with(
            "Extension .mp4 is not in the match [@photo-original, @png] of target pics"
        ),
        "{}",
        e.verdict
    );
    let pics = e.targets.iter().find(|t| t.name == "pics").unwrap();
    assert_eq!(pics.filters[0].fit, Fit::No);
    assert!(!pics.filters[0].matches.as_ref().unwrap().matched);
}

#[test]
fn no_content_step_applies() {
    let fx = fixture();
    let e = fx.explain("src/20260101_120000.png");
    assert_eq!(e.outcome, Outcome::Unchanged);
    let out = &e.outputs[0];
    assert!(!out.content[0].matched);
    assert!(out.content[0].mask_expanded.contains("heic"));
    assert_eq!(out.op, OutputOp::Keep);
    assert!(out.rename[0].matched && out.rename[0].before == out.rename[0].after);
    assert!(
        e.verdict.starts_with("The file passes the filter of target pics, but no content step fits: mask avif-high = @photo-original ("),
        "{}",
        e.verdict
    );
    assert!(
        e.verdict.contains("does not take .png. Add png-avif."),
        "{}",
        e.verdict
    );
}

#[test]
fn exclude_hit() {
    let fx = fixture();
    let e = fx.explain("src/a_thumb.jpg");
    assert_eq!(e.decision.as_ref().unwrap().kind, DecisionKind::Excluded);
    let f = &e.targets.iter().find(|t| t.name == "pics").unwrap().filters[0];
    assert_eq!(f.fit, Fit::Excluded);
    assert!(f.exclude.as_ref().unwrap().matched);
    assert!(
        e.verdict
            .contains("excluded by the exclude mask (*_thumb*)"),
        "{}",
        e.verdict
    );
}

#[test]
fn negated_item_hit() {
    let fx = fixture();
    let e = fx.explain("src/x_edit.gif");
    assert_eq!(e.decision.as_ref().unwrap().kind, DecisionKind::Unmatched);
    let m = e
        .targets
        .iter()
        .find(|t| t.name == "edits")
        .unwrap()
        .filters[0]
        .matches
        .as_ref()
        .unwrap();
    assert!(m.items.iter().any(|i| i.negated && i.hit));
    assert!(
        e.verdict.contains("cuts off item !*_edit*"),
        "{}",
        e.verdict
    );
}

#[test]
fn override_keep_and_target() {
    let fx = fixture();
    let mut o = Overrides::default();
    let keep = fx.p("src/IMG_1.jpg");
    let (sz, mt) = stat(&keep);
    o.set(keep, sz, mt, Action::Keep);
    let e = fx.explain_with("src/IMG_1.jpg", &o);
    assert_eq!(e.decision.as_ref().unwrap().kind, DecisionKind::Kept);
    assert!(e.verdict.starts_with("Manual decision"));

    let clip = fx.p("src/clip.mp4");
    let (sz, mt) = stat(&clip);
    o.set(
        clip,
        sz,
        mt,
        Action::Target {
            name: "edits".into(),
        },
    );
    let e = fx.explain_with("src/clip.mp4", &o);
    let d = e.decision.as_ref().unwrap();
    assert_eq!(d.via, Some(Via::Manual));
    assert_eq!(e.outcome, Outcome::Process);
    assert!(e.verdict.contains("manual decision"), "{}", e.verdict);
}

fn stat(p: &Path) -> (u64, i128) {
    let m = std::fs::metadata(p).unwrap();
    (m.len(), mtime_ns(&m).unwrap())
}

#[test]
fn outside_sources() {
    let fx = fixture();
    assert_eq!(fx.explain("other/x.jpg").scope, Scope::NoSource);
    assert_eq!(fx.explain("nope.jpg").scope, Scope::Missing);
    assert_eq!(fx.explain("src").scope, Scope::NotFile);
    assert!(matches!(
        fx.explain("src/.hid.jpg").scope,
        Scope::Hidden { .. }
    ));
    let e = fx.explain("src/skip.tmp");
    assert!(matches!(e.scope, Scope::Skip { ref glob, .. } if glob == "*.tmp"));
    let e = fx.explain("flat/sub/a.jpg");
    assert!(matches!(e.scope, Scope::NotRecursive { .. }));
    assert!(e.verdict.contains("recursive = true"));
    assert_eq!(e.outcome, Outcome::NotScanned);
}

#[test]
fn ai_not_asked_then_cached() {
    let fx = fixture();
    let e = fx.explain("src/cat.webp");
    assert_eq!(
        e.ai,
        AiState::NotAsked {
            candidates: vec!["cats".into()]
        }
    );
    assert!(e.verdict.contains("the AI was not asked"), "{}", e.verdict);

    // Store an answer the way the classifier keys it.
    let path = fx.p("src/cat.webp");
    let (size, mtime_ns) = stat(&path);
    let entry = FileEntry {
        source_id: "s".into(),
        path: path.clone(),
        rel: PathBuf::from("cat.webp"),
        size,
        mtime_ns,
    };
    let cats = vec![("cats".to_string(), "cats".to_string())];
    let system = crate::ai::cache_prompt(&cats, &crate::route::SourcePrompt::default());
    let key = cache::key(&entry, &fx.cfg.ai.model, &cache::prompt_hash(&system));
    Cache::load(&fx.p("ai-cache.jsonl"))
        .put(
            key,
            Stored {
                tags: Vec::new(),
                description: String::new(),
                category: "cats".into(),
                reason: "a cat".into(),
                confidence: "OK".into(),
            },
        )
        .unwrap();
    let e = fx.explain("src/cat.webp");
    assert!(matches!(&e.ai, AiState::Cached { category: Some(c), .. } if c == "cats"));
    let d = e.decision.as_ref().unwrap();
    assert_eq!(d.target.as_deref(), Some("cats"));
    assert!(matches!(d.via, Some(Via::Ai { .. })));
    assert_eq!(e.outcome, Outcome::Process);
    assert!(e.to_text().contains("AI (from cache): cats"));
}

/// The explanation's decision and outcome equal what a scan does.
#[test]
fn agrees_with_build_plan() {
    let fx = fixture();
    let mut o = Overrides::default();
    let clip = fx.p("src/clip.mp4");
    let (sz, mt) = stat(&clip);
    o.set(
        clip,
        sz,
        mt,
        Action::Target {
            name: "edits".into(),
        },
    );
    let keep = fx.p("src/IMG_1.jpg");
    let (sz, mt) = stat(&keep);
    o.set(keep, sz, mt, Action::Keep);

    let files = scan_sources(&fx.cfg.sources).unwrap();
    let routed = route_with_overrides(
        &fx.cfg,
        &files,
        &RouteCtx::empty(&files),
        &NoAi,
        false,
        &o,
        &NoProgress,
        &Cancel::new(),
    )
    .unwrap();
    let plan = build_plan(
        &fx.cfg,
        routed.decisions.clone(),
        &HashMap::new(),
        Side::Local,
    )
    .unwrap();
    assert!(files.len() >= 6);
    for (entry, decision) in &routed.decisions {
        let rel = entry.path.strip_prefix(fx.dir.path()).unwrap();
        let e = fx.explain_with(rel.to_str().unwrap(), &o);
        let d = e.decision.as_ref().expect("scanned file has a decision");
        match decision {
            Decision::Target { name, via, .. } => {
                assert_eq!(d.kind, DecisionKind::Target, "{rel:?}");
                assert_eq!(d.target.as_ref(), Some(name), "{rel:?}");
                // A cached AI answer would differ; the fixture has none.
                assert_eq!(d.via.as_ref(), Some(via), "{rel:?}");
            }
            Decision::Review { .. } => assert_eq!(d.kind, DecisionKind::Review),
            Decision::Excluded => assert_eq!(d.kind, DecisionKind::Excluded, "{rel:?}"),
            Decision::Kept => assert_eq!(d.kind, DecisionKind::Kept, "{rel:?}"),
            Decision::Unmatched => assert_eq!(d.kind, DecisionKind::Unmatched, "{rel:?}"),
        }
        let item = plan.items.iter().find(|i| i.source == entry.path);
        match item {
            Some(it) => {
                assert_eq!(e.outcome, Outcome::Process, "{rel:?}");
                let want: Vec<_> = it.outputs.iter().map(|o| o.dest.clone()).collect();
                let got: Vec<_> = e.outputs.iter().map(|o| o.dest.clone()).collect();
                assert_eq!(got, want, "{rel:?}");
                let ops: Vec<_> = it.outputs.iter().map(|o| o.op).collect();
                assert_eq!(e.outputs.iter().map(|o| o.op).collect::<Vec<_>>(), ops);
            }
            None if plan.unmatched.contains(&entry.path) => {
                assert_eq!(e.outcome, Outcome::Unmatched, "{rel:?}")
            }
            None if plan.review.iter().any(|r| r.source == entry.path) => {
                assert_eq!(e.outcome, Outcome::Review, "{rel:?}")
            }
            None => {
                assert!(plan.unchanged.contains(&entry.path), "{rel:?}");
                assert_eq!(e.outcome, Outcome::Unchanged, "{rel:?}");
            }
        }
    }
    // Files a scan does not list are reported as not scanned.
    for rel in ["src/skip.tmp", "src/.hid.jpg", "flat/sub/a.jpg"] {
        assert!(!files.iter().any(|f| f.path == fx.p(rel)));
        assert_eq!(fx.explain(rel).outcome, Outcome::NotScanned);
    }
}

#[test]
fn text_is_a_tree_ending_in_the_verdict() {
    let fx = fixture();
    let e = fx.explain("src/clip.mp4");
    let t = e.to_text();
    assert!(t.contains("├─ Source: s"));
    assert!(t.contains("pics — no"));
    assert!(t.trim_end().ends_with(&e.verdict));
}

#[test]
fn targets_for_other_sources_are_collapsed() {
    let fx = fixture();
    let e = fx.explain("src/clip.mp4");
    let names: Vec<_> = e.targets.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["pics", "edits", "cats"]);
    assert_eq!(e.other_targets.len(), 23);
    assert_eq!(other_targets_text(&e), "23 more targets not for source s");
    let t = e.to_text();
    assert!(t.contains("23 more targets not for source s"));
    assert!(!t.contains("flat0"));
}

#[test]
fn av1_file_is_excluded_by_the_video_mask_with_the_reason() {
    let dir = tempfile::tempdir().unwrap();
    let r = dir.path();
    touch(&r.join("src/20260727_200006.av1.mp4"));
    let toml = format!(
        "[[sources]]\nid = \"s\"\npath = \"{r}/src\"\n\n\
         [[targets.before_ai]]\nname = \"video\"\n[[targets.before_ai.filters]]\n\
         sources = [\"s\"]\nmatch = [\"@photo-original\", \"@png\", \"@video-original\"]\n\
         [[targets.before_ai.outputs]]\ndir = \"{r}/out\"\n",
        r = r.to_string_lossy().replace('\\', "/")
    );
    std::fs::write(r.join("g.toml"), toml).unwrap();
    let cfg = Config::load(&r.join("g.toml")).unwrap();
    let opts = ExplainOptions {
        tools: None,
        ai_cache: Some(r.join("c.jsonl")),
    };
    let e = explain(
        &cfg,
        &r.join("src/20260727_200006.av1.mp4"),
        &Overrides::default(),
        &opts,
    )
    .unwrap();
    let m = e.targets[0].filters[0].matches.as_ref().unwrap();
    assert_eq!(m.items[0].reason, "extension .mp4 does not match");
    assert_eq!(m.items[1].reason, "extension .mp4 does not match");
    let v = &m.items[2];
    assert!(v.expanded.contains("!*.av1.mp4"), "{}", v.expanded);
    assert!(v.reason.starts_with("matched *.{mp4"), "{}", v.reason);
    assert!(
        v.reason.ends_with("but excluded by !*.av1.mp4"),
        "{}",
        v.reason
    );
    assert!(
        e.verdict.starts_with(
            "The file is already compressed to AV1 (.av1.mp4): @video-original excludes it on purpose."
        ),
        "{}",
        e.verdict
    );
    assert!(e.verdict.contains("add \"*.av1.mp4\" to the match"));
    assert!(e.to_text().contains("@video-original = "));
}

#[test]
fn missing_file_hints_at_a_converted_sibling() {
    let fx = fixture();
    touch(&fx.p("src/20260716_211554.avif"));
    let e = fx.explain("src/20260716_211554.heic");
    assert_eq!(e.scope, Scope::Missing);
    assert!(e.verdict.starts_with("File not found"));
    assert!(
        e.verdict
            .ends_with("20260716_211554.avif is nearby — maybe already processed"),
        "{}",
        e.verdict
    );
    assert!(!fx.explain("src/nothing.heic").verdict.contains("nearby"));
}

#[test]
fn bundle_step_shows_its_expansion_and_which_steps_applied() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    touch(&root.join("src/a.png"));
    let toml = format!(
        r#"
[[sources]]
id = "s"
path = "{r}/src"

[[targets.before_ai]]
name = "all"
[[targets.before_ai.filters]]
sources = ["s"]
match = "*"
[[targets.before_ai.outputs]]
dir = "{r}/out"
content = ["compress-all"]
"#,
        r = root.to_string_lossy().replace('\\', "/")
    );
    let path = root.join("gallery.toml");
    std::fs::write(&path, toml).unwrap();
    let cfg = Config::load(&path).unwrap();
    cfg.validate().unwrap();
    let fx = Fx { dir, cfg };
    let e = fx.explain("src/a.png");
    let steps = &e.outputs[0].content;
    let got: Vec<_> = steps
        .iter()
        .map(|s| (s.preset.as_str(), s.bundle.as_deref(), s.matched))
        .collect();
    assert_eq!(
        got,
        [
            ("avif-high", Some("compress-all"), false),
            ("png-avif", Some("compress-all"), true),
            ("webp-avif", Some("compress-all"), false),
            ("av1-high", Some("compress-all"), false),
        ]
    );
    assert_eq!(e.outputs[0].dest.file_name().unwrap(), "a.avif");
    let text = e.to_text();
    assert!(text.contains("compress-all › png-avif: applied"), "{text}");
    assert!(text.contains("compress-all › avif-high: mask did not match"));
}

#[test]
fn trash_and_keep_targets_say_what_happens() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    touch(&root.join("src/junk.gif"));
    touch(&root.join("src/keep.jpg"));
    let toml = format!(
        r#"
[[sources]]
id = "s"
path = "{r}/src"

[[targets]]
name = "Junk"
kind = "trash"
[[targets.filters]]
sources = ["s"]
match = "*.gif"

[[targets]]
name = "Stay"
kind = "keep"
[[targets.filters]]
sources = ["s"]
match = "*.jpg"
"#,
        r = root.to_string_lossy().replace('\\', "/")
    );
    let cfg = Config::from_str_at(&toml, root, Path::new("t.toml")).unwrap();
    let fx = Fx { dir, cfg };
    let ex = fx.explain("src/junk.gif");
    assert_eq!(ex.outcome, Outcome::Trash);
    assert!(ex.outputs.is_empty());
    assert_eq!(
        ex.verdict,
        crate::t!(
            "explain.will_trash",
            target = "Junk",
            via = crate::t!("explain.via_mask")
        )
    );
    let ex = fx.explain("src/keep.jpg");
    assert_eq!(ex.outcome, Outcome::Kept);
    assert_eq!(
        ex.verdict,
        crate::t!(
            "explain.will_keep",
            target = "Stay",
            via = crate::t!("explain.via_mask")
        )
    );
}

fn fixture_with(files: &[(&str, &[u8])], toml_tail: &str) -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for (f, content) in files {
        let p = root.join(f);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }
    let toml = toml_tail.replace("{r}", &root.to_string_lossy().replace('\\', "/"));
    let cfg = Config::from_str_at(&toml, root, Path::new("t.toml")).unwrap();
    Fx { dir, cfg }
}

#[test]
fn a_duplicate_goes_to_the_trash_and_the_original_names_the_miss() {
    let fx = fixture_with(
        &[
            ("s/a.jpg", b"same"),
            ("s/b.jpg", b"same"),
            ("s/c.jpg", b"other"),
        ],
        r#"
[[sources]]
id = "s"
path = "{r}/s"

[[targets]]
name = "Dups"
kind = "trash"
[[targets.filters]]
sources = ["s"]
duplicate = { of = ["sources"] }
"#,
    );
    let (a, b) = (fx.explain("s/a.jpg"), fx.explain("s/b.jpg"));
    // Exactly one of the two copies is the original.
    let (dup, orig) = if a.outcome == Outcome::Trash {
        (a, b)
    } else {
        (b, a)
    };
    assert_eq!(dup.outcome, Outcome::Trash, "{}", dup.to_text());
    assert!(dup.targets[0].taken && dup.targets[0].miss.is_none());
    assert_ne!(orig.outcome, Outcome::Trash);
    let miss = orig.targets[0].miss.as_ref().expect("the original misses");
    assert_eq!(miss.condition, "duplicate");
    assert_eq!(miss.detail, crate::t!("filter.miss.no_duplicate"));
    assert!(orig.to_text().contains("duplicate: "), "{}", orig.to_text());
    // A file with no twin misses the same way.
    let c = fx.explain("s/c.jpg");
    assert_eq!(c.targets[0].miss.as_ref().unwrap().condition, "duplicate");
}

#[test]
fn near_misses_read_like_the_condition() {
    let fx = fixture_with(
        &[
            ("cam/20260101_120000.jpg", b"1"),
            ("partner/20260101_200000.jpg", b"2"),
            ("partner/20260101_130000.jpg", b"3"),
        ],
        r#"
[[sources]]
id = "cam"
path = "{r}/cam"
[[sources]]
id = "partner"
path = "{r}/partner"

[[targets]]
name = "Together"
[[targets.filters]]
sources = ["cam"]
near = { sources = ["partner"], time = "30m", km = 1 }
[[targets.outputs]]
dir = "{r}/out"
"#,
    );
    // The only partner file within 30 minutes is none: 13:00 is an hour away.
    let ex = fx.explain("cam/20260101_120000.jpg");
    let miss = ex.targets[0].miss.as_ref().expect("near misses");
    assert_eq!(miss.condition, "near");
    assert!(
        miss.detail.contains("partner")
            && miss.detail.contains("30m")
            && miss.detail.contains("1.0 km"),
        "{}",
        miss.detail
    );
    assert!(ex.to_text().contains("near: "), "{}", ex.to_text());
    assert_eq!(ex.outcome, Outcome::Unmatched);

    // Within two hours it is taken.
    let near = fx.cfg.targets[0].filters[0].near.clone();
    assert!(near.is_some());
    let mut cfg = fx.cfg.clone();
    cfg.targets[0].filters[0].near.as_mut().unwrap().time =
        Some(crate::filter::facts::Spec::parse("2h").unwrap());
    let fx2 = Fx { dir: fx.dir, cfg };
    let ex = fx2.explain("cam/20260101_120000.jpg");
    assert!(ex.targets[0].taken && ex.targets[0].miss.is_none());
    assert_eq!(ex.outcome, Outcome::Process);
}

#[test]
fn explain_holds_the_last_copy_like_a_scan() {
    // Both copies hit a trash target: the scan trashes one and holds the
    // other for review, and so does explain.
    let fx = fixture_with(
        &[("s/a.jpg", b"same"), ("s/b.jpg", b"same")],
        r#"
[[sources]]
id = "s"
path = "{r}/s"

[[targets]]
name = "Dups"
kind = "trash"
[[targets.filters]]
sources = ["s"]
duplicate = { of = ["sources"] }

[[targets]]
name = "Everything"
kind = "trash"
[[targets.filters]]
sources = ["s"]
"#,
    );
    let (a, b) = (fx.explain("s/a.jpg"), fx.explain("s/b.jpg"));
    let mut outcomes = [a.outcome, b.outcome];
    outcomes.sort_by_key(|o| format!("{o:?}"));
    assert_eq!(outcomes, [Outcome::Review, Outcome::Trash]);
    let held = if a.outcome == Outcome::Review { &a } else { &b };
    assert_eq!(
        held.verdict,
        crate::t!("explain.last_copy_held", target = "Everything")
    );
}

#[test]
fn a_review_verdict_reads_this_files_item_not_another() {
    // Two files held for review: README (its name cannot be built) and
    // cat.webp (the AI is unsure). Each verdict is about its own file.
    let fx = fixture_with(
        &[("s/README", b"r"), ("s/cat.webp", b"c")],
        r#"
[[sources]]
id = "s"
path = "{r}/s"

[name_presets."By extension"]
template = "{ext}/{stem}"

[[targets]]
name = "cats"
[[targets.filters]]
sources = ["s"]
match = "*.webp"
ai = "cats"

[[targets]]
name = "plain"
[[targets.filters]]
sources = ["s"]
match = "README"
[[targets.outputs]]
dir = "{r}/out"
rename = ["By extension"]
"#,
    );
    let cats = vec![("cats".to_string(), "cats".to_string())];
    let system = crate::ai::cache_prompt(&cats, &crate::route::SourcePrompt::default());
    let path = fx.p("s/cat.webp");
    let (size, mtime_ns) = stat(&path);
    let entry = FileEntry {
        source_id: "s".into(),
        path,
        rel: PathBuf::from("cat.webp"),
        size,
        mtime_ns,
    };
    let key = cache::key(&entry, &fx.cfg.ai.model, &cache::prompt_hash(&system));
    Cache::load(&fx.p("ai-cache.jsonl"))
        .put(
            key,
            Stored {
                tags: Vec::new(),
                description: String::new(),
                category: "cats".into(),
                reason: "a cat".into(),
                confidence: "LOW".into(),
            },
        )
        .unwrap();
    let readme = fx.explain("s/README");
    assert_eq!(readme.outcome, Outcome::Review);
    assert!(
        readme.verdict.contains("By extension"),
        "{}",
        readme.verdict
    );
    let cat = fx.explain("s/cat.webp");
    assert_eq!(cat.outcome, Outcome::Review);
    assert!(cat.verdict.contains("a cat"), "{}", cat.verdict);
    assert!(!cat.verdict.contains("By extension"), "{}", cat.verdict);
}
