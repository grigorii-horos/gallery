//! The server export: the plan rebuilt with the server's paths (same config,
//! same manual decisions) and saved to a folder the user chose — usually one
//! Syncthing carries to the server. The GUI never runs it and never touches
//! the server; it shows the command to run there.

use std::path::Path;

use std::cell::RefCell;
use std::collections::{BTreeSet, HashSet};

use gallery_core::Error;
use gallery_core::ai::cache::Cache;
use gallery_core::config::Config;
use gallery_core::effect::CachedOnly;
use gallery_core::overrides::Overrides;
use gallery_core::pending;
use gallery_core::pipeline::{self, ScanOptions};
use gallery_core::progress::{Cancel, Progress};
use gallery_core::route::Classifier;
use gallery_core::tools::Tools;
use serde::{Deserialize, Serialize};

use crate::scan::{ScanDone, ScanError, ScanSummary};

pub const PROGRESS_EVENT: &str = "export-progress";

/// What "Export plan" asks for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportRequest {
    pub dest_dir: String,
}

/// The result of an export.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ExportDone {
    pub plan_path: String,
    pub file_name: String,
    /// The command to run on the server; the plan's path there is unknown,
    /// so its folder is a placeholder.
    pub command: String,
    /// Files per target, AI cost (cache misses are paid).
    pub summary: ScanSummary,
}

/// `gallery apply <path to the plan on the server>/<file_name>`.
pub fn command_line(file_name: &str) -> String {
    gallery_core::t!("export.command", file = file_name)
}

/// A missing server path is the user's to configure: say what and where.
fn explain(mut e: ScanError) -> ScanError {
    if e.cancelled {
        e.message = gallery_core::t!("export.cancelled");
        return e;
    }
    if let Some(list) = gallery_core::i18n::extract("plan.server_needs_paths", "list", &e.message) {
        let lines: Vec<String> = list.split("; ").map(|w| format!("  • {w}")).collect();
        e.message = gallery_core::t!("export.missing_paths", lines = lines.join("\n"));
    }
    e
}

/// Rebuild the plan for the server and save it as a new
/// `YYYYMMDD-HHMMSS.plan.json` in `req.dest_dir` (made if missing). It is
/// the current plan with the server's paths: routed with the cached AI
/// answers only (nothing is paid for here) and without the files that wait
/// for the AI. `classifier` replaces the cache (tests).
#[allow(clippy::too_many_arguments)]
pub fn run(
    config: &Path,
    overrides: Option<&Path>,
    ai_cache: &Path,
    tools: &Tools,
    req: &ExportRequest,
    classifier: Option<&dyn Classifier>,
    progress: &dyn Progress,
    cancel: &Cancel,
) -> Result<ExportDone, ScanError> {
    run_with_examples(
        config, overrides, ai_cache, None, tools, req, classifier, progress, cancel,
    )
}

/// [`run`] where the AI prompts carry the user's corrections from the
/// `examples` file, as the current plan's do (the cache keys must agree).
#[allow(clippy::too_many_arguments)]
pub fn run_with_examples(
    config: &Path,
    overrides: Option<&Path>,
    ai_cache: &Path,
    examples: Option<&Path>,
    tools: &Tools,
    req: &ExportRequest,
    classifier: Option<&dyn Classifier>,
    progress: &dyn Progress,
    cancel: &Cancel,
) -> Result<ExportDone, ScanError> {
    let dest = gallery_core::config::expand_tilde(Path::new(req.dest_dir.trim()));
    if req.dest_dir.trim().is_empty() || !dest.is_absolute() {
        return Err(ScanError {
            message: gallery_core::t!("export.choose_folder"),
            cancelled: false,
        });
    }
    let done = build(
        config, overrides, ai_cache, examples, tools, &dest, classifier, progress, cancel,
    )
    .map_err(explain)?;
    let file_name = Path::new(&done.plan_path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(ExportDone {
        command: command_line(&file_name),
        plan_path: done.plan_path,
        file_name,
        summary: done.summary,
    })
}

/// The waiting files under the names the server plan uses for them (the
/// source's `server_path` plus the path inside the source), with the local
/// names too.
fn on_server(cfg: &Config, local: HashSet<std::path::PathBuf>) -> BTreeSet<std::path::PathBuf> {
    let mut all = BTreeSet::new();
    for p in local {
        for s in &cfg.sources {
            if let (Some(root), Ok(rel)) = (
                &s.server_path,
                p.strip_prefix(gallery_core::config::expand_tilde(&s.path)),
            ) {
                all.insert(root.join(rel));
            }
        }
        all.insert(p);
    }
    all
}

#[allow(clippy::too_many_arguments)]
fn build(
    config: &Path,
    overrides: Option<&Path>,
    ai_cache: &Path,
    examples: Option<&Path>,
    tools: &Tools,
    dest: &Path,
    classifier: Option<&dyn Classifier>,
    progress: &dyn Progress,
    cancel: &Cancel,
) -> Result<ScanDone, ScanError> {
    let mut cfg = Config::load(config)?;
    cfg.validate()?;
    if let Some(f) = examples {
        cfg.attach_examples(&gallery_core::learn::Store::new(f));
    }
    let overrides = match overrides {
        Some(f) => Overrides::load(f)?,
        None => Overrides::default(),
    };
    let cache = Cache::load(ai_cache);
    let cached = CachedOnly {
        cache: &cache,
        model: &cfg.ai.model,
        pending: RefCell::new(HashSet::new()),
    };
    let classifier: &dyn Classifier = classifier.unwrap_or(&cached);
    let opts = ScanOptions {
        server: true,
        accept_low: false,
        no_ai: false,
    };
    let mut out = pipeline::scan_plan_using(
        &cfg,
        tools,
        opts,
        &overrides,
        Some(classifier),
        progress,
        cancel,
    )?;
    let waiting = on_server(&cfg, cached.pending.into_inner());
    pending::strip_waiting(&mut out.plan, &waiting);
    std::fs::create_dir_all(dest).map_err(Error::from)?;
    let path = crate::plans::new_plan_path(dest, chrono::Local::now().naive_local());
    out.plan.save(&path)?;
    Ok(ScanDone {
        plan_path: path.display().to_string(),
        summary: ScanSummary::new(&out, &cfg.ai.model),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gallery_core::plan::{Plan, Side};
    use gallery_core::progress::ProgressEvent;

    fn req(dest: &Path) -> ExportRequest {
        ExportRequest {
            dest_dir: dest.display().to_string(),
        }
    }

    /// The scan fixture with server paths for the source and both outputs.
    fn server_fixture(dir: &Path) -> std::path::PathBuf {
        let cfg = crate::scan::tests::fixture(dir);
        let text = std::fs::read_to_string(&cfg).unwrap();
        let text = text
            .replacen(
                "id = \"s\"\n",
                "id = \"s\"\nserver_path = \"/srv/photos\"\n",
                1,
            )
            .replace("/cats\"\n", "/cats\"\nserver_dir = \"/srv/lib/cats\"\n")
            .replace("/rest\"\n", "/rest\"\nserver_dir = \"/srv/lib/rest\"\n");
        std::fs::write(&cfg, text).unwrap();
        cfg
    }

    #[test]
    fn command_names_the_file_and_leaves_the_server_folder_to_fill() {
        assert_eq!(
            command_line("20261105-101500.plan.json"),
            "gallery apply <path to the plan on the server>/20261105-101500.plan.json"
        );
    }

    #[test]
    fn export_saves_a_server_plan_named_by_time_in_the_chosen_folder() {
        let d = tempfile::tempdir().unwrap();
        let config = server_fixture(d.path());
        let dest = d.path().join("sync/plans");
        let tools = Tools::discover().unwrap();
        let noop = |_: ProgressEvent| {};
        let a = run(
            &config,
            None,
            &d.path().join("no-ai-cache"),
            &tools,
            &req(&dest),
            None,
            &noop,
            &Cancel::new(),
        )
        .unwrap();
        let b = run(
            &config,
            None,
            &d.path().join("no-ai-cache"),
            &tools,
            &req(&dest),
            None,
            &noop,
            &Cancel::new(),
        )
        .unwrap();
        for done in [&a, &b] {
            let p = Path::new(&done.plan_path);
            assert_eq!(p.parent().unwrap(), dest);
            let name = &done.file_name;
            // YYYYMMDD-HHMMSS(.-N).plan.json
            assert!(name.ends_with(".plan.json"), "{name}");
            let stem = name.trim_end_matches(".plan.json");
            assert!(stem.len() >= 15, "{name}");
            assert!(stem[..8].chars().all(|c| c.is_ascii_digit()), "{name}");
            assert_eq!(&stem[8..9], "-");
            assert!(stem[9..15].chars().all(|c| c.is_ascii_digit()), "{name}");
            assert!(done.command.ends_with(name.as_str()));
            let plan = Plan::load(p).unwrap();
            assert_eq!(plan.side, Side::Server);
            assert!(
                plan.items
                    .iter()
                    .all(|i| i.source.starts_with("/srv/photos"))
            );
            assert_eq!(done.summary.files, 2);
        }
        // Two exports in one second never overwrite each other.
        assert_ne!(a.plan_path, b.plan_path);
        // The local sources are not touched by an export.
        assert!(d.path().join("src/cat.jpg").exists());
    }

    #[test]
    fn missing_server_paths_say_what_to_configure() {
        let d = tempfile::tempdir().unwrap();
        let config = crate::scan::tests::fixture(d.path());
        let dest = d.path().join("out-plans");
        let tools = Tools::discover().unwrap();
        let e = run(
            &config,
            None,
            &d.path().join("no-ai-cache"),
            &tools,
            &req(&dest),
            None,
            &|_: ProgressEvent| {},
            &Cancel::new(),
        )
        .unwrap_err();
        assert!(!e.cancelled);
        assert!(
            e.message.contains("source 's': no server_path"),
            "{}",
            e.message
        );
        assert!(e.message.contains("server_dir"), "{}", e.message);
        assert!(e.message.contains("server_path on these sources"));
        assert!(
            !dest.exists() || std::fs::read_dir(&dest).unwrap().count() == 0,
            "no plan written"
        );
    }

    #[test]
    fn a_cancelled_export_writes_nothing() {
        let d = tempfile::tempdir().unwrap();
        let config = server_fixture(d.path());
        let dest = d.path().join("x");
        let tools = Tools::discover().unwrap();
        let c = Cancel::new();
        c.cancel();
        let e = run(
            &config,
            None,
            &d.path().join("no-ai-cache"),
            &tools,
            &req(&dest),
            None,
            &|_: ProgressEvent| {},
            &c,
        )
        .unwrap_err();
        assert!(e.cancelled);
        assert_eq!(e.message, "Export cancelled");
        assert!(!dest.exists());
    }

    #[test]
    fn export_needs_an_absolute_folder() {
        let d = tempfile::tempdir().unwrap();
        let config = server_fixture(d.path());
        let tools = Tools::discover().unwrap();
        for dest in ["", "  ", "relative/dir"] {
            let r = ExportRequest {
                dest_dir: dest.into(),
            };
            let e = run(
                &config,
                None,
                &d.path().join("no-ai-cache"),
                &tools,
                &r,
                None,
                &|_: ProgressEvent| {},
                &Cancel::new(),
            )
            .unwrap_err();
            assert!(e.message.contains("folder"), "{dest}: {}", e.message);
        }
    }
}
