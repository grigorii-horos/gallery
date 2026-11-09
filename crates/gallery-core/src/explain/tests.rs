use super::*;
use crate::ai::cache::Stored;
use crate::overrides::Action;
use crate::pipeline::NoAi;
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
    assert!(e.stages.iter().any(|t| t.name == "pics" && t.taken));
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
    let pics = e.stages.iter().find(|t| t.name == "pics").unwrap();
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
    let f = &e.stages.iter().find(|t| t.name == "pics").unwrap().filters[0];
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
    let m = e.stages.iter().find(|t| t.name == "edits").unwrap().filters[0]
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
    let system = system_prompt(&cats, &crate::route::SourcePrompt::default());
    let key = cache::key(&entry, &fx.cfg.ai.model, &cache::prompt_hash(&system));
    Cache::load(&fx.p("ai-cache.jsonl"))
        .put(
            key,
            Stored {
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
            Decision::Target { name, via } => {
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
    let names: Vec<_> = e.stages.iter().map(|t| t.name.as_str()).collect();
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
    let m = e.stages[0].filters[0].matches.as_ref().unwrap();
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
