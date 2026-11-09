use super::*;

struct Fx {
    dir: tempfile::TempDir,
    cfg: Config,
}

impl Fx {
    fn src(&self) -> PathBuf {
        self.dir.path().join("src")
    }

    fn env<'a>(&'a self, tools: &'a Tools, index: &'a Path, cache: &'a Path) -> Env<'a> {
        Env {
            tools: Some(tools),
            ai_cache: cache,
            index: Some(index),
        }
    }
}

/// Source `s`: `*.png` go to `pics` (before the AI), `*.jpg` to the AI
/// target `cats`, the rest to `rest` after it.
fn fx() -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir(&src).unwrap();
    let out = dir.path().join("out");
    let toml = format!(
        r#"
[[sources]]
id = "s"
path = "{src}"

[[targets.before_ai]]
name = "pics"

[[targets.before_ai.filters]]
sources = ["s"]
match = "*.png"

[[targets.before_ai.outputs]]
dir = "{out}/pics"

[[targets.ai]]
name = "cats"
sources = ["s"]
description = "cats"
prefilter = {{ match = "*.jpg" }}

[[targets.ai.outputs]]
dir = "{out}/cats"

[[targets.after_ai]]
name = "rest"

[[targets.after_ai.filters]]
sources = ["s"]
match = "*.txt"

[[targets.after_ai.outputs]]
dir = "{out}/rest"
"#,
        src = src.to_string_lossy().replace('\\', "/"),
        out = out.to_string_lossy().replace('\\', "/")
    );
    let path = dir.path().join("gallery.toml");
    std::fs::write(&path, toml).unwrap();
    let cfg = Config::load(&path).unwrap();
    cfg.validate().unwrap();
    Fx { dir, cfg }
}

fn run(fx: &Fx, tools: &Tools) -> Pending {
    let index = fx.dir.path().join("index.json");
    let cache = fx.dir.path().join("ai.jsonl");
    count(
        &fx.cfg,
        &Overrides::default(),
        &fx.env(tools, &index, &cache),
        &Cancel::new(),
    )
    .unwrap()
}

#[test]
fn a_new_file_grows_the_count_and_only_it_is_read() {
    let fx = fx();
    let tools = Tools::discover().unwrap();
    std::fs::write(fx.src().join("a.png"), b"x").unwrap();
    let first = run(&fx, &tools);
    assert_eq!(first.to_process, 1);
    assert_eq!(first.by_target.get("pics"), Some(&1));
    assert_eq!(first.exif_reads, 1);

    std::fs::write(fx.src().join("b.png"), b"y").unwrap();
    std::fs::write(fx.src().join("n.txt"), b"z").unwrap();
    let second = run(&fx, &tools);
    assert_eq!(second.to_process, 3);
    assert_eq!(second.by_target.get("pics"), Some(&2));
    assert_eq!(second.by_target.get("rest"), Some(&1));
    assert_eq!(second.exif_reads, 2, "a.png is known from the index");
    assert!(!second.fast_path);
}

#[test]
fn nothing_changed_takes_the_fast_path() {
    let fx = fx();
    let tools = Tools::discover().unwrap();
    std::fs::write(fx.src().join("a.png"), b"x").unwrap();
    let first = run(&fx, &tools);
    assert!(!first.fast_path);
    let again = run(&fx, &tools);
    assert!(again.fast_path);
    assert_eq!(again.exif_reads, 0);
    assert_eq!(again.to_process, first.to_process);
    assert_eq!(again.by_target, first.by_target);
}

#[test]
fn a_changed_config_recounts_without_reading_exif_again() {
    let mut fx = fx();
    let tools = Tools::discover().unwrap();
    std::fs::write(fx.src().join("a.png"), b"x").unwrap();
    assert_eq!(run(&fx, &tools).to_process, 1);
    fx.cfg.targets.retain(|t| t.has_ai());
    let after = run(&fx, &tools);
    assert!(!after.fast_path);
    assert_eq!(after.exif_reads, 0);
    assert_eq!(after.to_process, 0);
}

#[test]
fn files_waiting_for_the_ai_are_counted_apart() {
    let fx = fx();
    let tools = Tools::discover().unwrap();
    std::fs::write(fx.src().join("cat.jpg"), b"x").unwrap();
    std::fs::write(fx.src().join("dog.jpg"), b"y").unwrap();
    std::fs::write(fx.src().join("a.png"), b"x").unwrap();
    let p = run(&fx, &tools);
    assert_eq!(p.need_ai, 2);
    assert_eq!(p.to_process, 1);
    assert_eq!(p.total(), 3);
}

#[test]
fn no_index_means_every_call_reads_everything() {
    let fx = fx();
    let tools = Tools::discover().unwrap();
    std::fs::write(fx.src().join("a.png"), b"x").unwrap();
    let cache = fx.dir.path().join("ai.jsonl");
    let env = Env {
        tools: Some(&tools),
        ai_cache: &cache,
        index: None,
    };
    for _ in 0..2 {
        let p = count(&fx.cfg, &Overrides::default(), &env, &Cancel::new()).unwrap();
        assert_eq!((p.to_process, p.exif_reads, p.fast_path), (1, 1, false));
    }
}

#[test]
fn a_cancel_ends_the_count() {
    let fx = fx();
    let cancel = Cancel::new();
    cancel.cancel();
    let cache = fx.dir.path().join("ai.jsonl");
    let env = Env {
        tools: None,
        ai_cache: &cache,
        index: None,
    };
    assert!(matches!(
        count(&fx.cfg, &Overrides::default(), &env, &cancel),
        Err(crate::Error::Cancelled)
    ));
}

fn plan_json(plan: &Plan) -> serde_json::Value {
    let mut v = serde_json::to_value(plan).unwrap();
    v["created"] = serde_json::Value::Null;
    v
}

#[test]
fn a_built_plan_is_the_plan_of_a_scan_without_ai_and_leaves_waiting_files_out() {
    let fx = fx();
    let tools = Tools::discover().unwrap();
    std::fs::write(fx.src().join("a.png"), b"x").unwrap();
    std::fs::write(fx.src().join("n.txt"), b"z").unwrap();
    std::fs::write(fx.src().join("cat.jpg"), b"y").unwrap();
    std::fs::write(fx.src().join("u.bin"), b"y").unwrap();
    let cache = fx.dir.path().join("ai.jsonl");
    let files = scan::scan_sources_with(&fx.cfg.sources, &NoProgress, &Cancel::new()).unwrap();
    let env = Env {
        tools: Some(&tools),
        ai_cache: &cache,
        index: None,
    };
    let built = build(&fx.cfg, &Overrides::default(), &env, &files, &Cancel::new()).unwrap();
    assert_eq!(built.waiting.len(), 1);
    assert!(built.waiting.iter().all(|p| p.ends_with("cat.jpg")));

    let ov = Overrides::default();
    let mut base = crate::pipeline::scan_plan(
        &fx.cfg,
        &tools,
        crate::pipeline::ScanOptions {
            no_ai: true,
            ..Default::default()
        },
        &ov,
        &NoProgress,
        &Cancel::new(),
    )
    .unwrap()
    .plan;
    // No AI answers cached: the same plan, minus the file that waits.
    strip_waiting(&mut base, &built.waiting);
    assert_eq!(plan_json(&built.plan), plan_json(&base));
    assert!(
        built
            .plan
            .items
            .iter()
            .all(|i| !i.source.ends_with("cat.jpg"))
    );
    assert_eq!(built.plan.items.len(), 2);
    assert_eq!(built.plan.unmatched.len(), 1);

    // The count is the same answer.
    let p = count(&fx.cfg, &Overrides::default(), &env, &Cancel::new()).unwrap();
    assert_eq!((p.to_process, p.need_ai), (2, 1));
}
