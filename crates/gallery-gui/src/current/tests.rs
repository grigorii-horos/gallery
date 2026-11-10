use super::*;
use gallery_core::overrides::Action;
use gallery_core::trash::TrashMode;

struct Fx {
    dir: tempfile::TempDir,
    config: PathBuf,
    tools: Tools,
}

impl Fx {
    fn new() -> Fx {
        let dir = tempfile::tempdir().unwrap();
        let config = crate::scan::tests::fixture(dir.path());
        Fx {
            dir,
            config,
            tools: Tools::discover().unwrap(),
        }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn current(&self) -> PathBuf {
        self.root().join("data/current")
    }

    fn plans(&self) -> PathBuf {
        self.root().join("data/plans")
    }

    fn overrides(&self) -> PathBuf {
        self.root().join("data/overrides.json")
    }

    fn cache(&self) -> PathBuf {
        self.root().join("cache/ai.jsonl")
    }

    fn examples(&self) -> PathBuf {
        self.root().join("data/ai-examples.jsonl")
    }

    fn index(&self) -> PathBuf {
        self.root().join("cache/index.json")
    }

    fn env<'a>(&'a self, paths: &'a Paths) -> Env<'a> {
        Env {
            dir: &paths.current,
            config: &self.config,
            overrides: Some(&paths.overrides),
            ai_cache: &paths.cache,
            examples: Some(&paths.examples),
            index: Some(&paths.index),
            tools: Some(&self.tools),
        }
    }

    fn paths(&self) -> Paths {
        Paths {
            current: self.current(),
            overrides: self.overrides(),
            cache: self.cache(),
            examples: self.examples(),
            index: self.index(),
        }
    }

    fn src(&self, name: &str) -> PathBuf {
        self.root().join("src").join(name)
    }
}

struct Paths {
    current: PathBuf,
    overrides: PathBuf,
    cache: PathBuf,
    examples: PathBuf,
    index: PathBuf,
}

fn now() -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 11, 7)
        .unwrap()
        .and_hms_opt(10, 0, 0)
        .unwrap()
}

fn fp(fx: &Fx, paths: &Paths) -> Fingerprint {
    let env = fx.env(paths);
    let c = check(&env, &NoProgress, &Cancel::new()).unwrap();
    c.fingerprint
}

fn confirmed(env: &Env) -> Confirmed {
    Confirmed {
        plan_id: plan_id(env),
        pilot_id: None,
    }
}

fn with_pilot(env: &Env, per_target: usize) -> Confirmed {
    let plan = Plan::load(&env.plan_path()).unwrap();
    let (pilot, _) = crate::pilot::choose(&plan, per_target).unwrap();
    Confirmed {
        plan_id: plan_id(env),
        pilot_id: Some(slice_id(&pilot)),
    }
}

fn plan_json(plan: &Plan) -> serde_json::Value {
    let mut v = serde_json::to_value(plan).unwrap();
    v["created"] = serde_json::Value::Null;
    v
}

#[test]
fn the_fingerprint_changes_with_config_decisions_files_and_cache_and_not_otherwise() {
    let fx = Fx::new();
    let p = fx.paths();
    let base = fp(&fx, &p);
    assert_eq!(fp(&fx, &p), base, "reading again changes nothing");

    // The same text written again (new mtime) is the same config.
    let text = std::fs::read_to_string(&fx.config).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(&fx.config, &text).unwrap();
    assert_eq!(fp(&fx, &p), base);

    std::fs::write(&fx.config, format!("{text}\n# edited\n")).unwrap();
    let edited = fp(&fx, &p);
    assert_ne!(edited.config, base.config);
    assert_eq!(
        (&edited.overrides, &edited.sources, &edited.ai_cache),
        (&base.overrides, &base.sources, &base.ai_cache)
    );
    std::fs::write(&fx.config, &text).unwrap();
    assert_eq!(fp(&fx, &p), base, "back to the text, back to the print");

    // A manual decision.
    std::fs::create_dir_all(fx.overrides().parent().unwrap()).unwrap();
    let mut ov = Overrides::default();
    ov.set(fx.src("note.txt"), 1, 2, Action::Keep);
    ov.save(&fx.overrides()).unwrap();
    let with_ov = fp(&fx, &p);
    assert_ne!(with_ov.overrides, base.overrides);
    assert_eq!(with_ov.sources, base.sources);

    // A new file, then a changed one.
    std::fs::write(fx.src("new.txt"), "n").unwrap();
    let added = fp(&fx, &p);
    assert_ne!(added.sources, with_ov.sources);
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(fx.src("new.txt"), "nn").unwrap();
    assert_ne!(fp(&fx, &p).sources, added.sources);

    // AI answers added to the cache.
    let before = fp(&fx, &p).ai_cache;
    std::fs::create_dir_all(fx.cache().parent().unwrap()).unwrap();
    std::fs::write(fx.cache(), "{}\n").unwrap();
    assert_ne!(fp(&fx, &p).ai_cache, before);

    // A run recorded files their pipe cannot shrink: no source file changed,
    // yet the plan must be built again so those files drop out of it.
    let before = fp(&fx, &p).incompressible;
    let store = gallery_core::paths::cache_dir()
        .unwrap()
        .join("incompressible.json");
    std::fs::create_dir_all(store.parent().unwrap()).unwrap();
    let old = std::fs::read(&store).ok();
    std::fs::write(
        &store,
        format!(
            "{{\"version\":1,\"files\":{{}},\"pad\":\"{}\"}}",
            "x".repeat(before.len() + 7)
        ),
    )
    .unwrap();
    let after = fp(&fx, &p).incompressible;
    match old {
        Some(b) => std::fs::write(&store, b).unwrap(),
        None => std::fs::remove_file(&store).unwrap(),
    }
    assert_ne!(after, before);
}

#[test]
fn a_new_ai_example_leaves_the_plan_fresh_and_keeps_its_files() {
    use gallery_core::learn::{Correction, Store};
    let fx = Fx::new();
    let p = fx.paths();
    let env = fx.env(&p);
    refresh(&env, false, &NoProgress, &Cancel::new()).unwrap();
    assert!(check(&env, &NoProgress, &Cancel::new()).unwrap().fresh);
    let before = load(&env).unwrap().unwrap();

    let c = Correction::new(
        "s",
        "cat.jpg",
        Some("cats".into()),
        "grey cat",
        Action::Keep,
        "now",
    );
    Store::new(&p.examples).append(&[c]).unwrap();
    // The examples do not touch the cache key: the plan is still current and
    // no file starts to wait for the AI because of the correction.
    assert!(check(&env, &NoProgress, &Cancel::new()).unwrap().fresh);
    assert_eq!(
        refresh(&env, false, &NoProgress, &Cancel::new()).unwrap(),
        Refreshed::Unchanged
    );
    let after = load(&env).unwrap().unwrap();
    assert_eq!(after.waiting_ai, before.waiting_ai);
    assert_eq!(after.plan.items.len(), before.plan.items.len());
}

#[test]
fn a_rebuild_is_the_plan_of_a_scan_without_ai_minus_files_waiting_for_the_ai() {
    let fx = Fx::new();
    let p = fx.paths();
    let env = fx.env(&p);
    assert!(load(&env).unwrap().is_none());
    assert_eq!(
        refresh(&env, false, &NoProgress, &Cancel::new()).unwrap(),
        Refreshed::Rebuilt
    );
    let view = load(&env).unwrap().unwrap();
    // cat.jpg is an AI candidate without a cached answer: it waits.
    assert_eq!(view.waiting_ai, 1);

    let cfg = Config::load(&fx.config).unwrap();
    let mut base = pipeline::scan_plan(
        &cfg,
        &fx.tools,
        pipeline::ScanOptions {
            no_ai: true,
            ..Default::default()
        },
        &Overrides::default(),
        &NoProgress,
        &Cancel::new(),
    )
    .unwrap()
    .plan;
    base.items.retain(|i| !i.source.ends_with("cat.jpg"));
    assert_eq!(plan_json(&view.plan), plan_json(&base));
    assert_eq!(view.plan.items.len(), 2);
    assert_eq!(pending_of(&view).total(), 3);
}

#[test]
fn nothing_is_rebuilt_until_something_changes() {
    let fx = Fx::new();
    let p = fx.paths();
    let env = fx.env(&p);
    let go = || refresh(&env, false, &NoProgress, &Cancel::new()).unwrap();
    assert_eq!(go(), Refreshed::Rebuilt);
    let built = load(&env).unwrap().unwrap().built_at;
    assert_eq!(go(), Refreshed::Unchanged);
    assert_eq!(load(&env).unwrap().unwrap().built_at, built);

    std::fs::write(fx.src("more.txt"), "m").unwrap();
    assert_eq!(go(), Refreshed::Rebuilt);
    assert_eq!(load(&env).unwrap().unwrap().plan.items.len(), 3);

    // A decision is seen by the next pass and routes the file.
    std::fs::create_dir_all(fx.overrides().parent().unwrap()).unwrap();
    let meta = std::fs::metadata(fx.src("more.txt")).unwrap();
    let mut ov = Overrides::default();
    ov.set(
        fx.src("more.txt"),
        meta.len(),
        gallery_core::scan::mtime_ns(&meta).unwrap(),
        Action::Keep,
    );
    ov.save(&fx.overrides()).unwrap();
    assert_eq!(go(), Refreshed::Rebuilt);
    let view = load(&env).unwrap().unwrap();
    assert_eq!(view.plan.items.len(), 2);
    assert_eq!(view.overrides.len(), 1);

    // A plan file lost: built again.
    std::fs::remove_file(env.plan_path()).unwrap();
    assert_eq!(go(), Refreshed::Rebuilt);
}

#[test]
fn another_config_has_its_own_current_plan() {
    let fx = Fx::new();
    let p = fx.paths();
    let env = fx.env(&p);
    refresh(&env, false, &NoProgress, &Cancel::new()).unwrap();
    let other_cfg = fx.root().join("other.toml");
    std::fs::copy(&fx.config, &other_cfg).unwrap();
    let other = Env {
        config: &other_cfg,
        ..fx.env(&p)
    };
    assert_ne!(other.plan_path(), env.plan_path());
    assert!(load(&other).unwrap().is_none());
}

#[test]
fn a_run_on_a_stale_plan_is_refused_and_the_plan_is_rebuilt() {
    let fx = Fx::new();
    let p = fx.paths();
    let env = fx.env(&p);
    let cancel = Cancel::new();
    // No plan yet: nothing to run.
    assert_eq!(
        prepare_run(
            &env,
            &fx.plans(),
            None,
            false,
            &confirmed(&env),
            now(),
            &cancel
        ),
        Err(Refusal::Stale)
    );
    // Fresh now.
    let ok = prepare_run(
        &env,
        &fx.plans(),
        None,
        false,
        &confirmed(&env),
        now(),
        &cancel,
    )
    .unwrap();
    assert_eq!(ok.files, 2);

    // A file appears after the user looked: refused once, rebuilt, then fine.
    std::fs::write(fx.src("late.txt"), "l").unwrap();
    let e = prepare_run(
        &env,
        &fx.plans(),
        None,
        false,
        &confirmed(&env),
        now(),
        &cancel,
    )
    .unwrap_err();
    assert_eq!(e, Refusal::Stale);
    assert!(e.message().contains("refreshed"));
    assert_eq!(load(&env).unwrap().unwrap().plan.items.len(), 3);
    assert_eq!(
        prepare_run(
            &env,
            &fx.plans(),
            None,
            false,
            &confirmed(&env),
            now(),
            &cancel
        )
        .unwrap()
        .files,
        3
    );

    // Same for a manual decision made meanwhile.
    std::fs::create_dir_all(fx.overrides().parent().unwrap()).unwrap();
    let mut ov = Overrides::default();
    ov.set(fx.src("late.txt"), 1, 1, Action::Keep);
    ov.save(&fx.overrides()).unwrap();
    assert_eq!(
        prepare_run(
            &env,
            &fx.plans(),
            None,
            false,
            &confirmed(&env),
            now(),
            &cancel
        ),
        Err(Refusal::Stale)
    );
}

#[test]
fn the_executed_plan_is_archived_and_history_and_undo_keep_working() {
    let fx = Fx::new();
    let p = fx.paths();
    let env = fx.env(&p);
    let cancel = Cancel::new();
    refresh(&env, false, &NoProgress, &cancel).unwrap();

    // A dry run uses the current plan in place and writes nothing.
    let dry = prepare_run(
        &env,
        &fx.plans(),
        None,
        true,
        &confirmed(&env),
        now(),
        &cancel,
    )
    .unwrap();
    assert_eq!(dry.plan_path, env.plan_path());
    assert!(!fx.plans().exists());

    let prep = prepare_run(
        &env,
        &fx.plans(),
        None,
        false,
        &confirmed(&env),
        now(),
        &cancel,
    )
    .unwrap();
    assert!(prep.plan_path.starts_with(fx.plans()));
    assert!(prep.plan_path.ends_with("20261107-100000.plan.json"));
    let req = crate::apply::ApplyRequest {
        plan_path: prep.plan_path.display().to_string(),
        dry_run: false,
        jobs: Some(2),
        allow_anomalies: false,
        pause_sync: false,
    };
    let done = crate::apply::run(&req, &fx.tools, TrashMode::Folder, &|_| {}, &cancel).unwrap();
    assert_eq!((done.summary.done, done.summary.failed), (2, 0));
    let report = PathBuf::from(done.report_path.clone().unwrap());
    assert_eq!(report.parent().unwrap(), fx.plans());

    // History lists it and the undo is allowed on the archived plan.
    let listed = crate::history::list(Some(&fx.plans()), None);
    assert_eq!(listed.len(), 1);
    assert_eq!(Path::new(&listed[0].path), report);
    crate::history::guard_undo(&report, Some(&fx.plans())).unwrap();
    let undone = crate::history::undo(&report, Some(&fx.plans()), &|_| {}, &cancel).unwrap();
    assert!(undone.undone);
    assert!(fx.src("note.txt").exists());

    // The current plan itself never got a report: it is not "already run".
    assert_eq!(crate::apply::already_applied(&env.plan_path()), None);
}

#[test]
fn a_pilot_run_archives_only_the_pilot_and_the_rest_comes_from_the_rebuild() {
    let fx = Fx::new();
    for i in 0..4 {
        std::fs::write(fx.src(&format!("n{i}.txt")), "x").unwrap();
    }
    let p = fx.paths();
    let env = fx.env(&p);
    let cancel = Cancel::new();
    refresh(&env, false, &NoProgress, &cancel).unwrap();
    let total = load(&env).unwrap().unwrap().plan.items.len();
    assert_eq!(total, 6);
    let prep = prepare_run(
        &env,
        &fx.plans(),
        Some(2),
        false,
        &with_pilot(&env, 2),
        now(),
        &cancel,
    )
    .unwrap();
    assert_eq!(prep.files, 2);
    assert_eq!(Plan::load(&prep.plan_path).unwrap().items.len(), 2);
    let req = crate::apply::ApplyRequest {
        plan_path: prep.plan_path.display().to_string(),
        dry_run: false,
        jobs: Some(2),
        allow_anomalies: false,
        pause_sync: false,
    };
    crate::apply::run(&req, &fx.tools, TrashMode::Folder, &|_| {}, &cancel).unwrap();
    // The files moved: the current plan is stale and what it becomes is the rest.
    assert_eq!(
        refresh(&env, false, &NoProgress, &cancel).unwrap(),
        Refreshed::Rebuilt
    );
    assert_eq!(load(&env).unwrap().unwrap().plan.items.len(), total - 2);
}

#[test]
fn a_cancelled_refresh_keeps_the_stored_plan() {
    let fx = Fx::new();
    let p = fx.paths();
    let env = fx.env(&p);
    refresh(&env, false, &NoProgress, &Cancel::new()).unwrap();
    std::fs::write(fx.src("x.txt"), "x").unwrap();
    let c = Cancel::new();
    c.cancel();
    assert!(refresh(&env, false, &NoProgress, &c).is_err());
    assert_eq!(load(&env).unwrap().unwrap().plan.items.len(), 2);
}

#[test]
fn an_ai_scan_returns_its_cost_and_leaves_the_current_plan_rebuilt() {
    use gallery_core::route::{AiAnswer, AiRequest, Confidence};
    struct Cats;
    impl Classifier for Cats {
        fn classify(&self, batch: Vec<AiRequest>) -> gallery_core::Result<Vec<AiAnswer>> {
            Ok(batch
                .iter()
                .map(|_| AiAnswer {
                    category: Some("cats".into()),
                    reason: "a cat".into(),
                    confidence: Confidence::Ok,
                    meta: None,
                })
                .collect())
        }
    }
    let fx = Fx::new();
    let p = fx.paths();
    let env = fx.env(&p);
    let done = ai_scan(&env, &fx.tools, Some(&Cats), &NoProgress, &Cancel::new()).unwrap();
    assert_eq!(done.plan_path, env.plan_path().display().to_string());
    // The test classifier writes no cache, so cat.jpg still waits; the real
    // one fills the cache and the rebuild then plans it.
    let view = load(&env).unwrap().unwrap();
    assert_eq!((view.plan.items.len(), view.waiting_ai), (2, 1));
    // The plans folder got nothing: an AI scan does not make plan files.
    assert!(!fx.plans().exists());
}

#[test]
fn a_plan_rebuilt_after_the_confirmation_is_not_the_one_that_runs() {
    let fx = Fx::new();
    let p = fx.paths();
    let env = fx.env(&p);
    let cancel = Cancel::new();
    refresh(&env, false, &NoProgress, &cancel).unwrap();
    // The user looked at this plan and opened the dialog.
    let seen = confirmed(&env);
    let before = std::fs::read_dir(fx.src("")).unwrap().count();
    // A rebuild produces a fresh plan before "Run" is pressed.
    std::fs::write(fx.src("late.txt"), "l").unwrap();
    refresh(&env, false, &NoProgress, &cancel).unwrap();
    assert_ne!(seen.plan_id, plan_id(&env));
    // It is fresh, but not the confirmed one: refused, nothing archived.
    let e = prepare_run(&env, &fx.plans(), None, false, &seen, now(), &cancel).unwrap_err();
    assert_eq!(e, Refusal::Stale);
    assert!(!fx.plans().exists());
    assert_eq!(std::fs::read_dir(fx.src("")).unwrap().count(), before + 1);
    // Confirming the new plan runs it.
    let ok = prepare_run(
        &env,
        &fx.plans(),
        None,
        false,
        &confirmed(&env),
        now(),
        &cancel,
    )
    .unwrap();
    assert_eq!(ok.files, 3);
    // No identity at all is no confirmation.
    let e = prepare_run(
        &env,
        &fx.plans(),
        None,
        false,
        &Confirmed::default(),
        now(),
        &cancel,
    );
    assert_eq!(e, Err(Refusal::Stale));
}

#[test]
fn a_pilot_runs_only_the_slice_that_was_previewed() {
    let fx = Fx::new();
    for i in 0..4 {
        std::fs::write(fx.src(&format!("n{i}.txt")), "x").unwrap();
    }
    let p = fx.paths();
    let env = fx.env(&p);
    let cancel = Cancel::new();
    refresh(&env, false, &NoProgress, &cancel).unwrap();
    let split = crate::pilot::preview(&env.plan_path(), 2).unwrap();
    let want = Confirmed {
        plan_id: Some(split.plan_id.clone()),
        pilot_id: Some(split.pilot_id.clone()),
    };
    let ok = prepare_run(&env, &fx.plans(), Some(2), false, &want, now(), &cancel).unwrap();
    assert_eq!(ok.files, split.pilot_files);
    // Another slice (or none named) is refused.
    let other = Confirmed {
        pilot_id: Some("0".into()),
        ..want.clone()
    };
    assert_eq!(
        prepare_run(&env, &fx.plans(), Some(2), false, &other, now(), &cancel),
        Err(Refusal::Stale)
    );
    let none = Confirmed {
        pilot_id: None,
        ..want
    };
    assert_eq!(
        prepare_run(&env, &fx.plans(), Some(2), false, &none, now(), &cancel),
        Err(Refusal::Stale)
    );
}

#[test]
fn every_run_gets_a_new_archive_copy_without_a_report() {
    let fx = Fx::new();
    let p = fx.paths();
    let env = fx.env(&p);
    let cancel = Cancel::new();
    refresh(&env, false, &NoProgress, &cancel).unwrap();
    let a = prepare_run(
        &env,
        &fx.plans(),
        None,
        false,
        &confirmed(&env),
        now(),
        &cancel,
    )
    .unwrap();
    let req = crate::apply::ApplyRequest {
        plan_path: a.plan_path.display().to_string(),
        dry_run: false,
        jobs: Some(2),
        allow_anomalies: false,
        pause_sync: false,
    };
    crate::apply::run(&req, &fx.tools, TrashMode::Folder, &|_| {}, &cancel).unwrap();
    assert!(crate::apply::already_applied(&a.plan_path).is_some());
    // The files moved, so the fingerprint no longer matches: the finished
    // plan cannot be run again, it is refused and rebuilt.
    assert_eq!(
        prepare_run(
            &env,
            &fx.plans(),
            None,
            false,
            &confirmed(&env),
            now(),
            &cancel
        ),
        Err(Refusal::Stale)
    );
    // And a copy never reuses a name: the same plan in the same second gets
    // a new file, which has no report.
    std::fs::write(fx.src("again.txt"), "a").unwrap();
    refresh(&env, false, &NoProgress, &cancel).unwrap();
    let c = prepare_run(
        &env,
        &fx.plans(),
        None,
        false,
        &confirmed(&env),
        now(),
        &cancel,
    )
    .unwrap();
    let d = prepare_run(
        &env,
        &fx.plans(),
        None,
        false,
        &confirmed(&env),
        now(),
        &cancel,
    )
    .unwrap();
    assert_ne!(c.plan_path, d.plan_path);
    assert_ne!(a.plan_path, c.plan_path);
    assert_eq!(crate::apply::already_applied(&d.plan_path), None);
}

#[test]
fn a_config_saved_during_the_check_does_not_get_the_old_plan_under_its_hash() {
    let fx = Fx::new();
    let p = fx.paths();
    let env = fx.env(&p);
    let old = std::fs::read_to_string(&fx.config).unwrap();
    let saved = std::sync::atomic::AtomicBool::new(false);
    // The user saves the config while the sources are being scanned.
    let save_midway = |_: gallery_core::progress::ProgressEvent| {
        if !saved.swap(true, std::sync::atomic::Ordering::SeqCst) {
            std::fs::write(&fx.config, format!("{old}\n# edited\n")).unwrap();
        }
    };
    let c = check(&env, &save_midway, &Cancel::new()).unwrap();
    assert!(
        saved.load(std::sync::atomic::Ordering::SeqCst),
        "the save happened"
    );
    build_and_store(&env, c, &Cancel::new()).unwrap();
    // The stored state is of the config that was parsed, not the new text:
    // the next pass sees the difference and rebuilds.
    assert_eq!(
        read_meta(&env).unwrap().fingerprint.config,
        hash_bytes(old.as_bytes())
    );
    assert_eq!(
        refresh(&env, false, &NoProgress, &Cancel::new()).unwrap(),
        Refreshed::Rebuilt
    );
    assert_eq!(
        refresh(&env, false, &NoProgress, &Cancel::new()).unwrap(),
        Refreshed::Unchanged
    );
}

#[test]
fn the_view_and_its_plan_id_come_from_the_same_read() {
    let fx = Fx::new();
    let p = fx.paths();
    let env = fx.env(&p);
    refresh(&env, false, &NoProgress, &Cancel::new()).unwrap();
    let view = load(&env).unwrap().unwrap();
    let on_disk = std::fs::read(env.plan_path()).unwrap();
    assert_eq!(view.plan_id, hash_bytes(&on_disk));
    assert_eq!(Some(view.plan_id.clone()), plan_id(&env));
    // The shown plan is the parsed bytes whose hash is the id.
    let again: Plan = serde_json::from_slice(&on_disk).unwrap();
    assert_eq!(plan_json(&view.plan), plan_json(&again));
}

#[test]
fn writes_leave_no_temp_files_behind_and_never_share_a_name() {
    let fx = Fx::new();
    let p = fx.paths();
    let env = fx.env(&p);
    refresh(&env, true, &NoProgress, &Cancel::new()).unwrap();
    refresh(&env, true, &NoProgress, &Cancel::new()).unwrap();
    let mut names: Vec<String> = std::fs::read_dir(&p.current)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names.len(), 2, "{names:?}");
    assert!(names[0].ends_with(".plan.json") && names[1].ends_with(".state.json"));
}
