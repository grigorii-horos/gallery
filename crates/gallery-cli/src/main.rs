//! `gallery` command line: check tools, scan sources into a plan, apply, undo.

use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

use clap::{ArgAction, Parser, Subcommand};
use gallery_core::apply::{ApplyOptions, ItemResult, ItemStatus, apply_with, report_path};
use gallery_core::config::Config;
use gallery_core::overrides::Overrides;
use gallery_core::pipeline::{self, ScanOptions};
use gallery_core::plan::{Plan, Warning};
use gallery_core::progress::{Cancel, Progress, ProgressEvent, Stage};
use gallery_core::syncthing::SyncPause;
use gallery_core::tools::Tools;
use gallery_core::trash::TrashMode;
use gallery_core::{Error, Result, undo};

#[derive(Parser)]
#[command(
    name = "gallery",
    about = "Media organizer: scan sources into a plan, apply the plan, undo",
    disable_help_flag = true,
    disable_help_subcommand = true,
    disable_version_flag = true,
    long_about = "Media organizer. Files from the config's sources are sorted into targets \
(masks, then AI, then masks), land in a plan and are processed only after that.\n\n\
Workflow: `gallery scan -c gallery.toml -o plan.json`, review the summary and the plan, \
`gallery apply plan.json`; `gallery undo plan.report.json` undoes a run.\n\n\
Working through a server: `scan --server` builds a plan with server paths; the plan is copied to \
the server, `gallery apply` runs there, and the report comes back via Syncthing next to the plan.\n\n\
Sources are not changed in place: they go to the trash only after all outputs \
of their target are written and verified. If an output lies in the source's own folder and does not \
change the content, the source is not copied but renamed in place (and does not go to the trash); \
a file that already has the right name is left out of the plan (\"unchanged\"). \
GALLERY_TRASH=folder uses a .gallery-trash folder instead of the system trash. The AI key is taken from ANTHROPIC_API_KEY or CLAUDE_API_KEY."
)]
struct Cli {
    /// Short help
    #[arg(short = 'h', global = true, action = ArgAction::HelpShort)]
    help: Option<bool>,
    /// Detailed help
    #[arg(short = 'H', long = "help-full", global = true, action = ArgAction::HelpLong)]
    help_full: Option<bool>,
    /// Message language: en, ru, uk, de, fr, es, pt-BR, it, pl, zh-CN
    #[arg(long, global = true, value_name = "CODE")]
    lang: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum SyncCmd {
    /// What is paused and whether Syncthing answers
    Status,
    /// Resume the folders this program paused
    Resume,
}

#[derive(Subcommand)]
enum ExamplesCmd {
    /// Show the corrections the AI sees
    List {
        /// Only this source
        #[arg(long)]
        source: Option<String>,
    },
    /// Delete corrections (all or of one source)
    Clear {
        /// Only this source
        #[arg(long)]
        source: Option<String>,
    },
}

#[derive(Subcommand)]
enum Cmd {
    /// Show tools, versions and capabilities
    #[command(
        long_about = "Shows the external tools (ffmpeg, ffprobe, exiftool, avifenc, magick, \
optipng), their versions and ffmpeg's capabilities. Exit code 1 if a required \
tool or capability is missing. Tools are looked up in GALLERY_TOOLS_DIR, then in PATH."
    )]
    Check,
    /// Scan the sources and write a plan
    #[command(
        long_about = "Scans the sources, reads EXIF, sorts files into targets and writes the \
plan. Prints a summary: files per target, the review list, unmatched files, warnings, \
AI cost. Nothing but the plan file is changed. Unsure AI answers go to \
review unless --accept-low is given."
    )]
    Scan {
        /// Config file (TOML)
        #[arg(short, long)]
        config: PathBuf,
        /// Plan file to write
        #[arg(short, long, required_unless_present = "estimate")]
        output: Option<PathBuf>,
        /// Only estimate the AI cost (how many files would be sent, how many are cached), no calls and no plan
        #[arg(long, conflicts_with_all = ["no_ai", "output"])]
        estimate: bool,
        /// Plan with server paths (server_path of the sources)
        #[arg(long)]
        server: bool,
        /// Accept unsure AI answers
        #[arg(long)]
        accept_low: bool,
        /// Skip the AI stage; files go to after_ai
        #[arg(long)]
        no_ai: bool,
        /// Manual decisions file (default ~/.local/share/gallery/overrides.json, if present)
        #[arg(long)]
        overrides: Option<PathBuf>,
    },
    /// Apply a plan
    #[command(
        long_about = "Applies a plan. Refuses to run if a tool lacks a needed \
capability. Changed or missing sources are skipped. The report is written next to \
the plan as <plan>.report.json. Exit code 2 if any file failed."
    )]
    Apply {
        /// Plan file
        plan: PathBuf,
        /// Only check, write nothing
        #[arg(long)]
        dry_run: bool,
        /// Files in parallel
        #[arg(short, long, default_value_t = 4)]
        jobs: usize,
        /// Pilot: apply only N files per target, write the rest to a separate plan
        #[arg(long, value_name = "N")]
        pilot: Option<usize>,
        /// Place results that differ from the source (date, mtime, GPS, size, duration)
        #[arg(long)]
        allow_anomalies: bool,
        /// Pause the local Syncthing folders the plan touches and do not resume them
        #[arg(long)]
        pause_sync: bool,
    },
    /// Syncthing folders paused by this program
    #[command(
        long_about = "Works only with a local Syncthing (address and key come from its \
config.xml). `status` shows what is paused; `resume` resumes only \
those folders and clears the pause record."
    )]
    Sync {
        #[command(subcommand)]
        action: SyncCmd,
    },
    /// Explain why a file gets the treatment it gets
    #[command(
        long_about = "Shows what a scan does with each file: which source it is in, \
which target filters take it and why not (match, exclude, `!`), what the manual decision \
or the AI decided, which content and name steps fired, where the file will end up, and a \
one-line verdict with a hint. Changes nothing and never calls the AI: a cached answer is used if there is one."
    )]
    Explain {
        /// Config file (TOML)
        #[arg(short, long)]
        config: PathBuf,
        /// Manual decisions file (default ~/.local/share/gallery/overrides.json, if present)
        #[arg(long)]
        overrides: Option<PathBuf>,
        /// Files to ask about
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
    /// Triage "unmatched": which files, and why no target takes them
    #[command(
        long_about = "Groups the plan's unmatched files by source, folder and extension and \
prints for each group the file count, size and reason (as `explain` does). Without --plan \
the plan is built on the fly without AI and EXIF: enough for the unmatched list. Changes \
nothing."
    )]
    Unmatched {
        /// Config file (TOML)
        #[arg(short, long)]
        config: PathBuf,
        /// A finished plan (JSON); without it a new one is built without AI
        #[arg(long)]
        plan: Option<PathBuf>,
        /// Manual decisions file for the on-the-fly plan
        #[arg(long)]
        overrides: Option<PathBuf>,
    },
    /// How many files are pending (for scripts and cron)
    #[command(
        long_about = "Quietly counts how many files a scan would put into targets and how many wait for \
an AI answer (not in the cache). The AI is not called and nothing changes except a small index in the \
user's cache: on a repeated call EXIF is read only for new and changed files. \
The first line is `pending: N`; --json prints everything as one object."
    )]
    Pending {
        /// Config file (TOML)
        #[arg(short, long)]
        config: PathBuf,
        /// Manual decisions file (default ~/.local/share/gallery/overrides.json, if present)
        #[arg(long)]
        overrides: Option<PathBuf>,
        /// Print JSON
        #[arg(long)]
        json: bool,
    },
    /// Write the app's .desktop file (application menu) for gallery-gui
    #[command(
        long_about = "Writes ~/.local/share/applications/gallery.desktop (Exec points to \
gallery-gui next to this program, or to --exe) and an icon to ~/.local/share/icons. \
Never runs on its own."
    )]
    InstallDesktop {
        /// Path to gallery-gui (default: next to this executable)
        #[arg(long)]
        exe: Option<PathBuf>,
    },
    /// AI examples: your corrections of its answers
    #[command(
        long_about = "When you change what the AI decided (or resolve a doubtful answer), \
the correction is remembered as text (no images) and shown to the AI as an example in later \
requests for the same source; how many examples go into a request is set by `[ai] examples` \
(default 20, 0 turns it off). `list` shows them, `clear` deletes them. Examples affect \
only new AI requests: answers already in the cache stay valid."
    )]
    Examples {
        #[command(subcommand)]
        action: ExamplesCmd,
    },
    /// Show the built-in presets
    #[command(
        long_about = "Prints the built-in name and content presets with their key parameters. \
They can be used in the config by name without a definition; a user preset with the \
same name is an error."
    )]
    Presets,
    /// Undo a run from its report
    #[command(
        long_about = "Restores the sources from the trash and deletes the outputs the run created. \
Outputs changed after the run stay. A report can be undone once."
    )]
    Undo {
        /// Report file (<plan>.report.json)
        report: PathBuf,
    },
}

/// Exit code of a run stopped by Ctrl-C (128 + SIGINT).
const CANCELLED: u8 = 130;

/// The `--lang` value in `args` (`--lang ru` or `--lang=ru`), read before
/// clap runs so that `--help` is already in that language.
fn lang_arg(args: &[String]) -> Option<&str> {
    let mut it = args.iter().skip(1);
    while let Some(a) = it.next() {
        if a == "--" {
            break;
        }
        if let Some(v) = a.strip_prefix("--lang=") {
            return Some(v);
        }
        if a == "--lang" {
            return it.next().map(String::as_str);
        }
    }
    None
}

/// The language of this run: `--lang`, else LC_ALL / LC_MESSAGES / LANG, else English.
fn choose_locale(args: &[String]) -> &'static str {
    match lang_arg(args) {
        Some(code) => gallery_core::i18n::resolve(code).unwrap_or(gallery_core::i18n::DEFAULT),
        None => gallery_core::i18n::detect_env(),
    }
}

/// The text of catalog key `key`, when it has one.
fn catalog(key: &str) -> Option<String> {
    gallery_core::i18n::try_translate(key)
}

/// Replaces the help texts of `cmd` (and its subcommands) with the catalog's
/// in the language in force. `path` is the command's key: `cli.scan`,
/// `cli.sync.status`. An argument looks for `<path>.<id>`, then `cli.arg.<id>`;
/// a missing key leaves the English text of the source.
fn localize(mut cmd: clap::Command, path: &str) -> clap::Command {
    if let Some(t) = catalog(&format!("{path}.about")) {
        cmd = cmd.about(t);
    }
    if let Some(t) = catalog(&format!("{path}.long")) {
        cmd = cmd.long_about(t);
    }
    // clap wants 'static headings; the few leaked strings live as long as the process.
    let heading = |k: &str| -> Option<&'static str> {
        catalog(&format!("cli.h.{k}")).map(|h| &*Box::leak(h.into_boxed_str()))
    };
    if let Some(h) = heading("commands") {
        cmd = cmd.subcommand_help_heading(h);
    }
    let ids: Vec<(String, bool)> = cmd
        .get_arguments()
        .map(|a| (a.get_id().to_string(), a.is_positional()))
        .collect();
    for (id, positional) in ids {
        let text = catalog(&format!("{path}.{id}")).or_else(|| catalog(&format!("cli.arg.{id}")));
        let head = heading(if positional { "arguments" } else { "options" });
        cmd = cmd.mut_arg(id, |a| {
            let a = match text {
                Some(t) => a.help(t),
                None => a,
            };
            match head {
                Some(h) => a.help_heading(h),
                None => a,
            }
        });
    }
    let subs: Vec<String> = cmd
        .get_subcommands()
        .map(|s| s.get_name().to_string())
        .collect();
    for name in subs {
        let key = format!("{path}.{}", name.replace('-', "_"));
        cmd = cmd.mut_subcommand(&name, |s| localize(s, &key));
    }
    if let Some(usage) = heading("usage") {
        cmd = cmd.help_template(format!(
            "{{about-with-newline}}\n{usage} {{usage}}\n\n{{all-args}}{{after-help}}"
        ));
    }
    cmd
}

/// The command line definition in the language in force.
fn command() -> clap::Command {
    use clap::CommandFactory;
    localize(Cli::command(), "cli")
}

fn parse_cli(args: &[String]) -> Cli {
    use clap::FromArgMatches;
    let matches = command().get_matches_from(args);
    Cli::from_arg_matches(&matches).unwrap_or_else(|e| e.exit())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    gallery_core::i18n::set_locale(choose_locale(&args));
    match run(parse_cli(&args).cmd) {
        Ok(code) => ExitCode::from(code),
        Err(Error::Cancelled) => {
            eprintln!("{}", gallery_core::t!("error.cancelled"));
            ExitCode::from(CANCELLED)
        }
        Err(e) => {
            eprintln!("{}", gallery_core::t!("cli.error", error = e));
            ExitCode::from(1)
        }
    }
}

/// Single-line progress on stderr (`\r`), drawn only when stderr is a
/// terminal and at most every 100 ms; called from worker threads.
struct Line {
    tty: bool,
    last: Mutex<Option<Instant>>,
}

impl Line {
    fn new() -> Self {
        Line {
            tty: std::io::stderr().is_terminal(),
            last: Mutex::new(None),
        }
    }

    /// Wipe the line before regular output.
    fn clear(&self) {
        if self.tty {
            eprint!("\r\x1b[K");
        }
    }
}

impl Progress for Line {
    fn step(&self, ev: ProgressEvent) {
        if !self.tty {
            return;
        }
        // Another thread is drawing: skip this event rather than wait.
        let Ok(mut last) = self.last.try_lock() else {
            return;
        };
        let now = Instant::now();
        let last_one = ev.total > 0 && ev.done == ev.total;
        if !last_one && last.is_some_and(|t| now - t < Duration::from_millis(100)) {
            return;
        }
        *last = Some(now);
        eprint!("\r\x1b[K{}", progress_text(&ev));
    }
}

fn progress_text(ev: &ProgressEvent) -> String {
    let stage = gallery_core::t!(match ev.stage {
        Stage::Scan => "cli.stage.scan",
        Stage::Exif => "cli.stage.exif",
        Stage::Ai => "cli.stage.ai",
        Stage::Plan => "cli.stage.plan",
        Stage::Apply => "cli.stage.apply",
        Stage::Undo => "cli.stage.undo",
    });
    if ev.total == 0 {
        format!("{stage}: {}", ev.done)
    } else {
        format!("{stage}: {}/{}", ev.done, ev.total)
    }
}

/// What a scan is doing, for Ctrl-C: only while it reads may a second
/// Ctrl-C end it at once; never while the plan file is being written.
static SCAN_PHASE: AtomicU8 = AtomicU8::new(PHASE_OTHER);
const PHASE_OTHER: u8 = 0;
const PHASE_READING: u8 = 1;
const PHASE_WRITING: u8 = 2;

/// What a Ctrl-C does.
#[derive(Debug, PartialEq, Eq)]
enum CtrlC {
    /// Print the notice (a catalog key) and exit at once.
    Exit(&'static str),
    /// Set the cancel flag and print the notice (a catalog key).
    Notice(&'static str),
}

/// The Ctrl-C reaction of a scan (`read_only`) or apply/undo in `phase`,
/// when an earlier Ctrl-C has (`again`) or has not already cancelled.
fn ctrl_c_action(read_only: bool, phase: u8, again: bool) -> CtrlC {
    if !read_only {
        CtrlC::Notice("cli.ctrlc.stopping_files")
    } else if phase == PHASE_WRITING {
        CtrlC::Notice("cli.ctrlc.writing_plan")
    } else if again && phase == PHASE_READING {
        CtrlC::Exit("cli.ctrlc.interrupted")
    } else {
        CtrlC::Notice("cli.ctrlc.stopping")
    }
}

/// A cancel flag set by Ctrl-C. Apply and undo stop between files: the
/// files in work are finished, so a second Ctrl-C only repeats the notice.
/// A scan only reads, so a second Ctrl-C ends it at once.
fn cancel_on_ctrl_c(read_only: bool) -> Cancel {
    let cancel = Cancel::new();
    let flag = cancel.clone();
    let set = ctrlc::set_handler(move || {
        let phase = SCAN_PHASE.load(Ordering::SeqCst);
        match ctrl_c_action(read_only, phase, flag.is_cancelled()) {
            CtrlC::Exit(msg) => {
                eprintln!("\n{}", gallery_core::t!(msg));
                std::process::exit(CANCELLED.into());
            }
            CtrlC::Notice(msg) => {
                flag.cancel();
                eprintln!("\n{}", gallery_core::t!(msg));
            }
        }
    });
    if let Err(e) = set {
        eprintln!(
            "{}",
            gallery_core::t!("cli.ctrlc.not_intercepted", error = e)
        );
    }
    cancel
}

fn run(cmd: Cmd) -> Result<u8> {
    match cmd {
        Cmd::Check => check(),
        Cmd::Scan {
            config,
            output,
            estimate,
            server,
            accept_low,
            no_ai,
            overrides,
        } => {
            let o = overrides_for_scan(overrides.as_deref(), Overrides::default_path())?;
            if estimate {
                return scan_estimate_cmd(&config, &o).map(|()| 0);
            }
            let output = output.expect("clap requires --output without --estimate");
            let opts = ScanOptions {
                server,
                accept_low,
                no_ai,
            };
            scan_cmd(&config, &output, opts, &o).map(|()| 0)
        }
        Cmd::Apply {
            plan,
            dry_run,
            jobs,
            pilot,
            allow_anomalies,
            pause_sync,
        } => apply_cmd(&plan, dry_run, jobs, pilot, allow_anomalies, pause_sync),
        Cmd::Sync { action } => sync_cmd(&action),
        Cmd::Explain {
            config,
            overrides,
            paths,
        } => explain_cmd(&config, overrides.as_deref(), &paths).map(|()| 0),
        Cmd::Unmatched {
            config,
            plan,
            overrides,
        } => unmatched_cmd(&config, plan.as_deref(), overrides.as_deref()).map(|()| 0),
        Cmd::Pending {
            config,
            overrides,
            json,
        } => pending_cmd(&config, overrides.as_deref(), json).map(|()| 0),
        Cmd::InstallDesktop { exe } => install_desktop_cmd(exe).map(|()| 0),
        Cmd::Examples { action } => examples_cmd(&action).map(|()| 0),
        Cmd::Presets => {
            print!("{}", presets_text());
            Ok(0)
        }
        Cmd::Undo { report } => undo_cmd(&report).map(|()| 0),
    }
}

/// The `pending` text: the count first, then ready files per target.
fn pending_text(p: &gallery_core::pending::Pending) -> String {
    use std::fmt::Write;
    // The first line is for scripts: it stays `pending: N` in every language.
    let mut out = format!(
        "pending: {}\n{}\n{}\n",
        p.total(),
        gallery_core::t!("cli.pending_ready", n = p.to_process),
        gallery_core::t!("cli.pending_ai", n = p.need_ai)
    );
    for (name, n) in &p.by_target {
        let _ = writeln!(out, "  {name}: {n}");
    }
    out
}

/// The config with the user's AI examples attached: every command that asks
/// the AI or reads its cached answers needs them, as they are part of the
/// prompt and so of the cache key.
fn load_config(config: &Path) -> Result<Config> {
    let mut cfg = Config::load(config)?;
    if let Some(p) = gallery_core::learn::Store::default_path() {
        cfg.attach_examples(&gallery_core::learn::Store::new(p));
    }
    Ok(cfg)
}

/// `gallery examples list|clear`.
fn examples_cmd(action: &ExamplesCmd) -> Result<()> {
    use gallery_core::learn::Store;
    let path =
        Store::default_path().ok_or_else(|| Error::Config(gallery_core::t!("lock.no_data_dir")))?;
    let store = Store::new(path);
    match action {
        ExamplesCmd::List { source } => {
            print!("{}", examples_text(&store.load(), source.as_deref()));
        }
        ExamplesCmd::Clear { source } => {
            let n = store.clear(source.as_deref())?;
            println!("{}", gallery_core::t!("cli.examples_deleted", n = n));
        }
    }
    Ok(())
}

/// The `examples list` listing: per source, newest first.
fn examples_text(all: &[gallery_core::learn::Correction], source: Option<&str>) -> String {
    use gallery_core::overrides::Action;
    use std::fmt::Write as _;
    let mut ids: Vec<&str> = all
        .iter()
        .map(|c| c.source_id.as_str())
        .filter(|id| source.is_none_or(|s| s == *id))
        .collect();
    ids.sort_unstable();
    ids.dedup();
    if ids.is_empty() {
        return format!("{}\n", gallery_core::t!("cli.no_examples"));
    }
    let mut out = String::new();
    for id in ids {
        let mine: Vec<_> = all.iter().filter(|c| c.source_id == id).collect();
        let _ = writeln!(out, "{id}: {}", mine.len());
        for c in mine.iter().rev() {
            let right = match &c.user_choice {
                Action::Target { name } => name.clone(),
                Action::Keep => gallery_core::t!("cli.leave_in_place"),
            };
            let _ = writeln!(
                out,
                "  {}",
                gallery_core::t!(
                    "cli.example_line",
                    file = c.file,
                    reason = c.ai_reason,
                    chose = c
                        .ai_suggested
                        .clone()
                        .unwrap_or_else(|| gallery_core::t!("cli.nothing")),
                    right = right,
                    id = c.id
                )
            );
        }
    }
    out
}

fn pending_cmd(config: &Path, overrides: Option<&Path>, json: bool) -> Result<()> {
    let cfg = load_config(config)?;
    cfg.validate()?;
    let o = overrides_for_scan(overrides, Overrides::default_path())?;
    let tools = Tools::discover().ok();
    let p = gallery_core::pending::count_default(
        &cfg,
        &o,
        tools.as_ref(),
        &gallery_core::progress::Cancel::new(),
    )?;
    if json {
        println!(
            "{}",
            serde_json::to_string(&p).map_err(|e| Error::Config(e.to_string()))?
        );
    } else {
        print!("{}", pending_text(&p));
    }
    Ok(())
}

/// `gallery-gui` next to the running executable, unless `exe` says otherwise.
fn gui_exe(exe: Option<PathBuf>) -> Result<PathBuf> {
    let path = match exe {
        Some(p) => p,
        None => std::env::current_exe()?.with_file_name("gallery-gui"),
    };
    if !path.is_file() {
        return Err(Error::Config(gallery_core::t!(
            "cli.gui_not_found",
            path = path.display()
        )));
    }
    Ok(std::fs::canonicalize(path)?)
}

fn install_desktop_cmd(exe: Option<PathBuf>) -> Result<()> {
    let exe = gui_exe(exe)?;
    let data_home =
        dirs::data_dir().ok_or_else(|| Error::Config(gallery_core::t!("lock.no_data_dir")))?;
    let icon = include_bytes!("../../gallery-gui/icons/128x128.png");
    for f in gallery_core::desktop::install_menu_entry(&data_home, &exe, icon)? {
        println!("{}", gallery_core::t!("cli.written", path = f.display()));
    }
    Ok(())
}

/// The `presets` listing: built-in presets by kind, one line each.
fn presets_text() -> String {
    use gallery_core::presets::{
        builtin_content_presets, builtin_name_presets, describe_content, describe_name,
    };
    use std::fmt::Write;
    let scope = gallery_core::config::Config::default();
    let mut out = format!("{}\n", gallery_core::t!("cli.presets_name"));
    for (name, p) in builtin_name_presets() {
        let mask = p.mask_or_default();
        let _ = writeln!(
            out,
            "  {name}: {} [{}]",
            describe_name(&p),
            gallery_core::t!("cli.mask_label", mask = mask.expand(&scope))
        );
    }
    out.push_str(&format!("\n{}\n", gallery_core::t!("cli.presets_content")));
    let (bundles, singles): (Vec<_>, Vec<_>) = builtin_content_presets()
        .into_iter()
        .partition(|(_, p)| matches!(p, gallery_core::config::ContentPreset::Bundle(_)));
    for (name, p) in singles {
        let mask = p.mask_or_default();
        let _ = writeln!(
            out,
            "  {name}: {} [{}]",
            describe_content(&p),
            gallery_core::t!("cli.mask_label", mask = mask.expand(&scope))
        );
    }
    out.push_str(&format!("\n{}\n", gallery_core::t!("cli.presets_bundles")));
    for (name, p) in bundles {
        let _ = writeln!(out, "  {name}: {}", describe_content(&p));
    }
    out.push_str(&format!("\n{}\n", gallery_core::t!("cli.presets_masks")));
    for (name, e) in gallery_core::presets::builtin_masks() {
        let (raw, full) = (e.source(), e.expand(&scope));
        if raw == full {
            let _ = writeln!(out, "  @{name}: {raw}");
        } else {
            let _ = writeln!(out, "  @{name}: {raw} = {full}");
        }
    }
    out
}

fn check() -> Result<u8> {
    let mut ok = true;
    for name in [
        "ffmpeg", "ffprobe", "exiftool", "avifenc", "magick", "optipng",
    ] {
        match gallery_core::tools::find(name) {
            Some(p) => println!("{name}: {}", p.display()),
            None => {
                let required = name != "optipng";
                ok &= !required;
                println!(
                    "{name}: {}",
                    gallery_core::t!(if required {
                        "cli.check_missing_required"
                    } else {
                        "cli.check_missing_png"
                    })
                );
            }
        }
    }
    if !ok {
        return Ok(1);
    }
    let caps = Tools::discover()?.capabilities();
    for (tool, version) in &caps.versions {
        println!("{tool}: {version}");
    }
    let yn = |b: bool| gallery_core::t!(if b { "cli.yes" } else { "cli.no" });
    println!(
        "{}",
        gallery_core::t!("cli.check_svtav1", v = yn(caps.svtav1))
    );
    println!("{}", gallery_core::t!("cli.check_opus", v = yn(caps.opus)));
    println!(
        "{}",
        gallery_core::t!("cli.check_vidstab", v = yn(caps.vidstab))
    );
    // svtav1 is needed by the main video presets; the others only by optional presets.
    Ok(if caps.svtav1 { 0 } else { 1 })
}

/// Manual decisions for a scan: the `--overrides` file (which must exist),
/// else the default file when it exists, else none.
fn overrides_for_scan(explicit: Option<&Path>, default: Option<PathBuf>) -> Result<Overrides> {
    match explicit {
        Some(p) if !p.exists() => Err(Error::Config(gallery_core::t!(
            "cli.overrides_not_found",
            path = p.display()
        ))),
        Some(p) => Overrides::load(p),
        None => match default {
            Some(p) if p.exists() => Overrides::load(&p),
            _ => Ok(Overrides::default()),
        },
    }
}

/// The summary line about manual decisions, when any were applied.
fn overrides_line(applied: usize) -> Option<String> {
    (applied > 0).then(|| gallery_core::t!("cli.overrides_applied", n = applied))
}

fn explain_cmd(config: &Path, overrides: Option<&Path>, paths: &[PathBuf]) -> Result<()> {
    use gallery_core::explain::{ExplainOptions, explain};
    let cfg = load_config(config)?;
    cfg.validate()?;
    let o = overrides_for_scan(overrides, Overrides::default_path())?;
    // EXIF improves the capture date; without the tools the name decides.
    let tools = Tools::discover().ok();
    let opts = ExplainOptions {
        tools: tools.as_ref(),
        ai_cache: None,
    };
    for (i, p) in paths.iter().enumerate() {
        if i > 0 {
            println!();
        }
        print!("{}", explain(&cfg, p, &o, &opts)?.to_text());
    }
    Ok(())
}

fn unmatched_cmd(config: &Path, plan: Option<&Path>, overrides: Option<&Path>) -> Result<()> {
    use gallery_core::plan::{Side, build_plan};
    use gallery_core::progress::{Cancel, NoProgress};
    use gallery_core::route::route_with_overrides;
    use gallery_core::triage::{group_unmatched, size_text, table_line};
    use std::collections::HashMap;
    let cfg = load_config(config)?;
    cfg.validate()?;
    let plan = match plan {
        Some(p) => Plan::load(p)?,
        None => {
            let o = overrides_for_scan(overrides, Overrides::default_path())?;
            let files = gallery_core::scan::scan_sources(&cfg.sources)?;
            let routed = route_with_overrides(
                &cfg,
                &files,
                &pipeline::NoAi,
                false,
                &o,
                &NoProgress,
                &Cancel::new(),
            )?;
            build_plan(&cfg, routed.decisions, &HashMap::new(), Side::Local)?
        }
    };
    let groups = group_unmatched(&plan, &cfg);
    if groups.is_empty() {
        println!("{}", gallery_core::t!("cli.no_unmatched"));
        return Ok(());
    }
    let total: u64 = groups.iter().map(|g| g.size).sum();
    println!(
        "{}",
        gallery_core::t!(
            "cli.unmatched_summary",
            files = gallery_core::tn!("count.files", plan.unmatched.len()),
            groups = gallery_core::tn!("count.groups", groups.len()),
            size = size_text(total)
        )
    );
    println!("{}", gallery_core::t!("cli.unmatched_header"));
    for g in &groups {
        println!("{}", table_line(g));
    }
    Ok(())
}

/// `scan --estimate`: what an AI scan would cost, without any API call.
fn scan_estimate_cmd(config: &Path, overrides: &Overrides) -> Result<()> {
    use gallery_core::ai::{AiClassifier, usage_log};
    let cfg = load_config(config)?;
    cfg.validate()?;
    if cfg.targets.ai.is_empty() {
        println!("{}", gallery_core::t!("cli.no_ai_targets"));
        return Ok(());
    }
    let cache = AiClassifier::default_cache_path()
        .ok_or_else(|| Error::Ai(gallery_core::t!("ai.no_cache_dir")))?;
    let cancel = cancel_on_ctrl_c(true);
    let line = Line::new();
    let dry = pipeline::ai_dry_run(&cfg, overrides, &cache, &line, &cancel);
    line.clear();
    let rows = usage_log::default_path()
        .map(|p| usage_log::load(&p))
        .unwrap_or_default();
    let e = gallery_core::estimate::ai_estimate(&dry?, &cfg.ai.model, &rows);
    println!(
        "{}",
        gallery_core::t!("cli.ai_line", label = e.label, source = e.source_label)
    );
    if e.usd.is_some_and(|u| u.mid > 1.0) {
        println!("{}", gallery_core::t!("cli.costs_more"));
    }
    Ok(())
}

/// The estimate of a run for the CLI: per target and in total.
fn print_estimate(plan: &Plan, plan_path: &Path, jobs: usize) {
    use gallery_core::estimate::{History, estimate_with};
    use gallery_core::triage::size_text;
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(d) = plan_path.parent() {
        dirs.push(if d.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            d.to_path_buf()
        });
    }
    dirs.extend(History::gui_dir());
    let dirs: Vec<&Path> = dirs.iter().map(PathBuf::as_path).collect();
    let e = estimate_with(plan, &History::load_dirs(&dirs), jobs);
    let saving = |v: i64| {
        let text = size_text(v.unsigned_abs());
        if v < 0 { format!("-{text}") } else { text }
    };
    println!(
        "{}",
        gallery_core::t!("cli.estimate_title", source = e.source_label)
    );
    for t in e.targets.iter().chain(std::iter::once(&e.total)) {
        let name = if t.name.is_empty() {
            gallery_core::t!("cli.total")
        } else {
            t.name.clone()
        };
        println!(
            "  {name}: {}",
            gallery_core::t!(
                "cli.estimate_line",
                files = gallery_core::tn!("count.files", t.files),
                input = size_text(t.input_bytes),
                low = size_text(t.output_bytes.low),
                high = size_text(t.output_bytes.high),
                save_low = saving(t.saving_bytes.low),
                save_high = saving(t.saving_bytes.high),
                time = t.time_text()
            )
        );
    }
    let t = &e.total;
    println!(
        "  {}",
        gallery_core::t!(
            "cli.estimate_totals",
            trash = t.trash,
            renamed = t.renamed,
            kept = t.kept,
            jobs = jobs
        )
    );
}

fn scan_cmd(config: &Path, output: &Path, opts: ScanOptions, overrides: &Overrides) -> Result<()> {
    let cfg = load_config(config)?;
    cfg.validate()?;
    for w in cfg.warnings() {
        eprintln!("{}", gallery_core::t!("cli.warning", text = w));
    }
    let tools = Tools::discover()?;
    let cancel = cancel_on_ctrl_c(true);
    let line = Line::new();
    SCAN_PHASE.store(PHASE_READING, Ordering::SeqCst);
    let result = pipeline::scan_plan(&cfg, &tools, opts, overrides, &line, &cancel)
        .map(|o| (o.plan, o.ai_usage, o.warnings, o.overrides_applied));
    // The plan is written whole even if Ctrl-C comes now.
    SCAN_PHASE.store(PHASE_WRITING, Ordering::SeqCst);
    line.clear();
    let saved = result.and_then(|(plan, ai, warnings, applied)| {
        plan.save(output).map(|()| (plan, ai, warnings, applied))
    });
    SCAN_PHASE.store(PHASE_OTHER, Ordering::SeqCst);
    let (plan, ai, warnings, applied) = saved?;
    for w in warnings {
        eprintln!("{}", gallery_core::t!("cli.warning", text = w));
    }

    print_summary(&plan, output);
    if let Some(l) = overrides_line(applied) {
        println!("{l}");
    }
    if let Some(u) = ai {
        match u.cost(&cfg.ai.model) {
            Some(c) => println!(
                "{}",
                gallery_core::t!(
                    "cli.ai_cost",
                    usd = gallery_core::i18n::decimal(c, 4),
                    input = u.input,
                    output = u.output
                )
            ),
            None => println!(
                "{}",
                gallery_core::t!("cli.ai_tokens", input = u.input, output = u.output)
            ),
        }
    }
    Ok(())
}

fn print_summary(plan: &Plan, output: &Path) {
    let mut per_target: BTreeMap<&str, usize> = BTreeMap::new();
    for it in &plan.items {
        *per_target.entry(it.target.as_str()).or_default() += 1;
    }
    println!(
        "{}",
        gallery_core::t!(
            "cli.plan_line",
            path = output.display(),
            files = gallery_core::tn!("count.files", plan.items.len())
        )
    );
    for (t, n) in per_target {
        println!("  {t}: {n}");
    }
    if !plan.review.is_empty() {
        println!(
            "{}",
            gallery_core::t!("cli.for_review", n = plan.review.len())
        );
        for r in &plan.review {
            println!("  {} -> {} ({})", r.source.display(), r.suggested, r.reason);
        }
    }
    println!(
        "{}",
        gallery_core::t!("cli.matched_none", n = plan.unmatched.len())
    );
    // Routed, but every output would leave the file as it is; or only an
    // exclude mask turned it away (left alone on purpose).
    println!(
        "{}",
        gallery_core::t!("cli.unchanged", n = plan.unchanged.len())
    );
    if !plan.warnings.is_empty() {
        println!(
            "{}",
            gallery_core::t!("cli.warnings", n = plan.warnings.len())
        );
        for w in &plan.warnings {
            match w {
                Warning::SeriesSplit { a, b } => {
                    println!(
                        "  {}",
                        gallery_core::t!("cli.w_series", a = a.display(), b = b.display())
                    )
                }
                Warning::DoubleEncode { source, dest } => println!(
                    "  {}",
                    gallery_core::t!(
                        "cli.w_double",
                        source = source.display(),
                        dest = dest.display()
                    )
                ),
                Warning::MissingServerPath { what } => {
                    println!("  {}", gallery_core::t!("cli.w_server_path", what = what))
                }
            }
        }
    }
}

/// Paths of the pilot and rest plans written next to `plan`
/// (`x.plan.json` gives `x-pilot.plan.json` and `x-rest.plan.json`; `-2`, ...
/// when an earlier pilot left its plans or report: nothing is overwritten).
fn pilot_paths(plan: &Path) -> (PathBuf, PathBuf) {
    let name = plan
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem = name
        .strip_suffix(".plan.json")
        .or_else(|| name.strip_suffix(".json"))
        .unwrap_or(&name);
    let taken = |p: &Path| {
        std::fs::symlink_metadata(p).is_ok() || std::fs::symlink_metadata(report_path(p)).is_ok()
    };
    (1..)
        .map(|n| {
            let tag = if n == 1 {
                String::new()
            } else {
                format!("-{n}")
            };
            (
                plan.with_file_name(format!("{stem}-pilot{tag}.plan.json")),
                plan.with_file_name(format!("{stem}-rest{tag}.plan.json")),
            )
        })
        .find(|(p, r)| !taken(p) && !taken(r))
        .expect("a free name")
}

/// One line per verification difference of a failed output.
fn anomaly_lines(report: &gallery_core::apply::Report) -> Vec<String> {
    report
        .items
        .iter()
        .flat_map(|it| {
            it.outputs.iter().flat_map(move |o| {
                o.anomalies.iter().map(move |a| {
                    gallery_core::t!("cli.discrepancy", path = it.source.display(), what = a)
                })
            })
        })
        .collect()
}

fn apply_cmd(
    plan: &Path,
    dry_run: bool,
    jobs: usize,
    pilot: Option<usize>,
    allow_anomalies: bool,
    pause_sync: bool,
) -> Result<u8> {
    // One apply/undo at a time on this machine (GUI included). A dry run
    // writes nothing and may run beside another.
    let _global = (!dry_run)
        .then(gallery_core::lock::acquire_apply)
        .transpose()?;
    let tools = Tools::discover()?;
    // Never a reason not to run: without Syncthing the run goes on, with a warning.
    let pause = if pause_sync && !dry_run {
        match SyncPause::discover() {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!(
                    "{}",
                    gallery_core::t!(
                        "cli.warning",
                        text = gallery_core::t!("apply.sync_not_paused", error = e)
                    )
                );
                None
            }
        }
    } else {
        None
    };
    let opts = ApplyOptions {
        dry_run,
        jobs: jobs.max(1),
        trash: TrashMode::from_env(),
        allow_anomalies,
        pause_sync: pause.clone(),
        ..Default::default()
    };
    // The pilot runs its own plan; the rest waits in another file.
    let mut rest_plan = None;
    let pilot_plan;
    // A dry run only needs the pilot plan to read: it lives in a temp folder
    // and nothing is left next to the user's plan.
    let dry_dir;
    let plan = match pilot {
        Some(n) if dry_run => {
            let (pilot, rest) = gallery_core::plan::pilot(&Plan::load(plan)?, n.max(1));
            dry_dir = tempfile::tempdir()?;
            pilot_plan = dry_dir.path().join("pilot.plan.json");
            pilot.save(&pilot_plan)?;
            println!(
                "{}",
                gallery_core::t!(
                    "cli.pilot_check",
                    files = gallery_core::tn!("count.files", pilot.items.len()),
                    rest = rest.items.len()
                )
            );
            pilot_plan.as_path()
        }
        Some(n) => {
            let (p, r) = pilot_paths(plan);
            let (pilot, rest) = gallery_core::plan::pilot(&Plan::load(plan)?, n.max(1));
            pilot.save(&p)?;
            rest.save(&r)?;
            println!(
                "{}",
                gallery_core::t!(
                    "cli.pilot",
                    files = gallery_core::tn!("count.files", pilot.items.len()),
                    pilot_path = p.display(),
                    rest = rest.items.len(),
                    rest_path = r.display()
                )
            );
            rest_plan = Some(r);
            pilot_plan = p;
            pilot_plan.as_path()
        }
        None => plan,
    };
    let cancel = cancel_on_ctrl_c(false);
    let line = Line::new();
    let report = apply_with(plan, &tools, &opts, &line, &cancel);
    line.clear();
    let report = report?;

    let (mut done, mut failed, mut skipped) = (0, 0, 0);
    // Done items whose source was renamed in its folder instead of trashed.
    let renamed = report
        .items
        .iter()
        .filter(|it| matches!(it.status, ItemStatus::Done { .. }))
        .filter(|it| it.outputs.iter().any(|o| o.renamed_from.is_some()))
        .count();
    for it in &report.items {
        match &it.status {
            ItemStatus::Done { .. } | ItemStatus::DryRun => done += 1,
            ItemStatus::Failed { .. } => {
                failed += 1;
                eprintln!(
                    "{}",
                    gallery_core::t!(
                        "cli.failed",
                        path = it.source.display(),
                        reason = failure_reason(it)
                    )
                );
            }
            ItemStatus::Stale | ItemStatus::Missing => {
                skipped += 1;
                let why = match it.status {
                    ItemStatus::Stale => gallery_core::t!("cli.skip_stale"),
                    _ => gallery_core::t!("cli.skip_missing"),
                };
                eprintln!(
                    "{}",
                    gallery_core::t!("cli.skipped", path = it.source.display(), why = why)
                );
            }
        }
    }
    let anomalies = anomaly_lines(&report);
    for a in &anomalies {
        eprintln!("{a}");
    }
    for w in &report.tool_warnings {
        println!("{}", gallery_core::t!("cli.warning", text = w));
    }
    println!(
        "{}",
        gallery_core::t!(
            if dry_run {
                "cli.result_checked"
            } else {
                "cli.result_processed"
            },
            done = done,
            renamed = renamed,
            failed = failed,
            skipped = skipped
        )
    );
    if dry_run && let Ok(p) = Plan::load(plan) {
        print_estimate(&p, plan, opts.jobs);
    }
    if !dry_run {
        println!(
            "{}",
            gallery_core::t!("cli.report", path = report_path(plan).display())
        );
    }
    if let Some(p) = &pause {
        print_pause_note(p);
    }
    if pilot.is_some() && !dry_run {
        print_pilot_summary(&report, anomalies.len(), plan, rest_plan.as_deref());
    }
    if report.stopped_by_anomalies {
        eprintln!(
            "{}",
            gallery_core::t!(
                "cli.stopped_anomalies",
                n = Plan::load(plan)
                    .map_or(0, |p| p.items.len().saturating_sub(report.items.len()))
            )
        );
        return Ok(2);
    }
    if report.cancelled {
        // Not started files are not in the report and were not touched.
        let total = Plan::load(plan).map_or(0, |p| p.items.len());
        eprintln!(
            "{}",
            gallery_core::t!("cli.stopped", n = total.saturating_sub(report.items.len()))
        );
        return Ok(CANCELLED);
    }
    Ok(if failed > 0 { 2 } else { 0 })
}

/// After a run with `--pause-sync`: what is paused and how to resume.
fn print_pause_note(p: &SyncPause) {
    if let Ok(Some(st)) = gallery_core::syncthing::load_state(&p.state_path) {
        let names: Vec<&str> = st.folders.iter().map(|f| f.label.as_str()).collect();
        println!(
            "{}",
            gallery_core::t!("cli.sync_paused_note", names = names.join(", "))
        );
    }
}

fn sync_cmd(action: &SyncCmd) -> Result<u8> {
    let state_path = gallery_core::syncthing::default_state_path()
        .ok_or_else(|| Error::Config(gallery_core::t!("lock.no_data_dir")))?;
    let state = gallery_core::syncthing::load_state(&state_path)?;
    match action {
        SyncCmd::Status => {
            match &state {
                None => println!("{}", gallery_core::t!("cli.sync_nothing_paused")),
                Some(st) => {
                    let names: Vec<&str> = st.folders.iter().map(|f| f.label.as_str()).collect();
                    println!(
                        "{}",
                        gallery_core::t!("cli.sync_paused", names = names.join(", "))
                    );
                    println!(
                        "{}",
                        gallery_core::t!("cli.sync_since", since = st.paused_at, plan = st.plan)
                    );
                    println!("{}", gallery_core::t!("cli.sync_to_resume"));
                }
            }
            match gallery_core::syncthing::Syncthing::discover().and_then(|c| c.health()) {
                Ok(()) => println!("{}", gallery_core::t!("cli.sync_responding")),
                Err(e) => println!("{}", gallery_core::t!("cli.sync_unavailable", error = e)),
            }
            Ok(0)
        }
        SyncCmd::Resume => {
            if state.is_none() {
                println!("{}", gallery_core::t!("cli.sync_nothing_to_resume"));
                return Ok(0);
            }
            let out = SyncPause::discover()?.resume()?;
            if !out.resumed.is_empty() {
                println!(
                    "{}",
                    gallery_core::t!("cli.sync_resumed", names = out.resumed.join(", "))
                );
            }
            if out.failed.is_empty() {
                Ok(0)
            } else {
                eprintln!(
                    "{}",
                    gallery_core::t!("cli.sync_resume_failed", names = out.failed.join(", "))
                );
                Ok(2)
            }
        }
    }
}

/// After a pilot: anomaly count, a few of the new files to look at, and how
/// to go on or back out.
fn print_pilot_summary(
    report: &gallery_core::apply::Report,
    anomalies: usize,
    plan: &Path,
    rest: Option<&Path>,
) {
    println!("{}", gallery_core::t!("cli.pilot_anomalies", n = anomalies));
    let samples: Vec<&Path> = report
        .items
        .iter()
        .flat_map(|it| it.outputs.iter())
        .filter_map(|o| o.actual.as_deref())
        .take(5)
        .collect();
    if !samples.is_empty() {
        println!("{}", gallery_core::t!("cli.pilot_samples"));
        for p in samples {
            println!("  {}", p.display());
        }
    }
    println!(
        "{}",
        gallery_core::t!("cli.pilot_undo", path = report_path(plan).display())
    );
    if let Some(r) = rest {
        println!(
            "{}",
            gallery_core::t!("cli.pilot_continue", path = r.display())
        );
    }
}

/// Why an item failed: its own error, else the first failed output's.
fn failure_reason(it: &ItemResult) -> String {
    if let Some(e) = &it.error {
        return e.clone();
    }
    it.outputs
        .iter()
        .find_map(|o| {
            let e = o.error.as_ref()?;
            Some(format!("{}: {e}", o.planned.display()))
        })
        .unwrap_or_else(|| gallery_core::t!("cli.unknown_error"))
}

fn undo_cmd(report: &Path) -> Result<()> {
    let _global = gallery_core::lock::acquire_apply()?;
    let cancel = cancel_on_ctrl_c(false);
    let line = Line::new();
    let s = undo::undo_with(report, &line, &cancel);
    line.clear();
    let s = s?;
    println!(
        "{}",
        gallery_core::t!(
            "cli.undo_result",
            restored = s.restored,
            removed = s.removed_outputs
        )
    );
    for p in &s.problems {
        eprintln!("{}", gallery_core::t!("cli.problem", text = p));
    }
    if s.cancelled {
        eprintln!("{}", gallery_core::t!("cli.undo_stopped"));
        return Err(Error::Cancelled);
    }
    if s.problems.is_empty() {
        Ok(())
    } else {
        Err(Error::Plan(gallery_core::t!(
            "cli.undo_problems",
            n = s.problems.len()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gallery_core::apply::OutputResult;

    #[test]
    fn pilot_plans_sit_next_to_the_plan() {
        let (p, r) = pilot_paths(Path::new("/d/x.plan.json"));
        assert_eq!(p, Path::new("/d/x-pilot.plan.json"));
        assert_eq!(r, Path::new("/d/x-rest.plan.json"));
        let (p, _) = pilot_paths(Path::new("plan.json"));
        assert_eq!(p, Path::new("plan-pilot.plan.json"));
        // Earlier pilot plans are never overwritten.
        let d = tempfile::tempdir().unwrap();
        let plan = d.path().join("x.plan.json");
        std::fs::write(d.path().join("x-pilot.plan.json"), "{}").unwrap();
        let (p, r) = pilot_paths(&plan);
        assert_eq!(p, d.path().join("x-pilot-2.plan.json"));
        assert_eq!(r, d.path().join("x-rest-2.plan.json"));
    }

    fn output(planned: &str, error: Option<&str>) -> OutputResult {
        OutputResult {
            planned: planned.into(),
            actual: None,
            error: error.map(String::from),
            warnings: vec![],
            fell_back: false,
            size: None,
            hash: None,
            renamed_from: None,
            anomalies: Vec::new(),
        }
    }

    #[test]
    fn overrides_for_scan_picks_explicit_or_existing_default() {
        use gallery_core::overrides::{Action, Overrides};
        let dir = tempfile::tempdir().unwrap();
        let mut o = Overrides::default();
        o.set("/s/a.jpg".into(), 1, 2, Action::Keep);
        let saved = dir.path().join("o.json");
        o.save(&saved).unwrap();
        let missing = dir.path().join("missing.json");

        // Explicit path: loaded; a missing explicit file is an error.
        assert_eq!(overrides_for_scan(Some(&saved), None).unwrap(), o);
        assert!(overrides_for_scan(Some(&missing), None).is_err());
        // Default path: used only when it exists.
        assert_eq!(overrides_for_scan(None, Some(saved.clone())).unwrap(), o);
        assert!(overrides_for_scan(None, Some(missing)).unwrap().is_empty());
        assert!(overrides_for_scan(None, None).unwrap().is_empty());
    }

    #[test]
    fn overrides_line_only_when_some_applied() {
        assert_eq!(overrides_line(0), None);
        assert_eq!(overrides_line(3).as_deref(), Some("overrides: 3 applied"));
    }

    #[test]
    fn presets_listing_shows_bundles_with_their_expansion() {
        let t = presets_text();
        assert!(t.contains("\nBundles"), "{t}");
        assert!(
            t.contains("  compress-all: set: avif-high + png-avif + webp-avif + av1-high\n"),
            "{t}"
        );
        assert!(!t.contains("compress-all: set: avif-high + png-avif + webp-avif + av1-high ["));
    }

    #[test]
    fn presets_listing_has_known_lines() {
        let t = presets_text();
        assert!(t.contains("Name presets\n"), "{t}");
        assert!(t.contains("Content presets\n"), "{t}");
        assert!(
            t.contains("  avif-high: AVIF: quality 72, speed 6 [mask: *.{jpg,jpeg,heic,heif}]\n"),
            "{t}"
        );
        assert!(
            t.contains(
                "  av1-high: AV1: crf 30, preset 3 [mask: *.{mp4,m4v,mov,mkv} & !*.av1.mp4]\n"
            ),
            "{t}"
        );
        assert!(
            t.contains(
                "  opus-128: opus: 128 kbps [mask: *.{mp3,m4a,aac,flac,wav,ogg,opus,wma,ape,alac} & !*.opus]\n"
            ),
            "{t}"
        );
        assert!(
            t.contains(
                "  flac: flac: lossless [mask: *.{mp3,m4a,aac,flac,wav,ogg,opus,wma,ape,alac} & !*.flac]\n"
            ),
            "{t}"
        );
        assert!(
            t.contains("  capture-date-dirs: {date:YYYY}/{date:MM}/{date:YYYYMMDD_HHMMSS}.{ext} ("),
            "{t}"
        );
        assert!(t.contains("  meta-full: metadata:"), "{t}");
        assert!(t.contains(" [mask: *]\n"), "{t}");
        assert!(t.contains("Masks (@name)\n"), "{t}");
        assert!(t.contains("  @dated: ^[0-9]{8}_[0-9]{6}\n"), "{t}");
        assert!(
            t.contains("  @photo-original: *.{jpg,jpeg,heic,heif}\n"),
            "{t}"
        );
        assert!(
            t.contains("  @media: @photo | @video | @png = *.{jpg,"),
            "{t}"
        );
    }

    #[test]
    fn failure_reason_falls_back_to_the_failed_output() {
        let mut it = ItemResult {
            source: "/s/a.jpg".into(),
            status: ItemStatus::Failed { trashed: None },
            outputs: vec![
                output("/o/a.jpg", None),
                output("/o/b.avif", Some("avifenc: boom")),
                output("/o/c.jpg", Some("skipped: an earlier output failed")),
            ],
            error: None,
            elapsed_ms: None,
        };
        assert_eq!(failure_reason(&it), "/o/b.avif: avifenc: boom");
        it.error = Some("trash: denied".into());
        assert_eq!(failure_reason(&it), "trash: denied");
        it.error = None;
        it.outputs.clear();
        assert_eq!(failure_reason(&it), "unknown error");
    }

    #[test]
    fn progress_line_text() {
        let ev = |stage, done, total| ProgressEvent {
            stage,
            done,
            total,
            current: None,
        };
        assert_eq!(progress_text(&ev(Stage::Scan, 12, 0)), "scan: 12");
        assert_eq!(progress_text(&ev(Stage::Exif, 500, 1200)), "EXIF: 500/1200");
        assert_eq!(progress_text(&ev(Stage::Ai, 3, 10)), "AI: 3/10");
        assert_eq!(progress_text(&ev(Stage::Plan, 1, 2)), "plan: 1/2");
        assert_eq!(progress_text(&ev(Stage::Apply, 7, 9)), "apply: 7/9");
        assert_eq!(progress_text(&ev(Stage::Undo, 2, 4)), "undo: 2/4");
    }

    #[test]
    fn ctrl_c_during_scan_stages() {
        use CtrlC::{Exit, Notice};
        // First Ctrl-C while reading: stop; second: exit at once.
        assert_eq!(
            ctrl_c_action(true, PHASE_READING, false),
            Notice("cli.ctrlc.stopping")
        );
        assert_eq!(
            ctrl_c_action(true, PHASE_READING, true),
            Exit("cli.ctrlc.interrupted")
        );
        // While the plan file is written: never exit, say so.
        for again in [false, true] {
            assert_eq!(
                ctrl_c_action(true, PHASE_WRITING, again),
                Notice("cli.ctrlc.writing_plan")
            );
        }
        // Apply and undo never exit on Ctrl-C.
        for again in [false, true] {
            assert_eq!(
                ctrl_c_action(false, PHASE_OTHER, again),
                Notice("cli.ctrlc.stopping_files")
            );
        }
    }
}

#[cfg(test)]
mod lang_tests {
    use super::*;
    use clap::CommandFactory;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    /// Every help text of the clap definition: `(catalog key, argument id, text)`.
    fn help_texts(
        cmd: &clap::Command,
        path: &str,
        out: &mut Vec<(String, Option<String>, String)>,
    ) {
        if let Some(a) = cmd.get_about() {
            out.push((format!("{path}.about"), None, a.to_string()));
        }
        if let Some(a) = cmd.get_long_about() {
            out.push((format!("{path}.long"), None, a.to_string()));
        }
        for a in cmd.get_arguments() {
            if let Some(h) = a.get_help() {
                let id = a.get_id().to_string();
                out.push((format!("{path}.{id}"), Some(id), h.to_string()));
            }
        }
        for s in cmd.get_subcommands() {
            help_texts(
                s,
                &format!("{path}.{}", s.get_name().replace('-', "_")),
                out,
            );
        }
    }

    #[test]
    fn lang_flag_is_read_before_clap() {
        assert_eq!(
            lang_arg(&args(&["gallery", "--lang", "ru", "check"])),
            Some("ru")
        );
        assert_eq!(
            lang_arg(&args(&["gallery", "check", "--lang=de"])),
            Some("de")
        );
        assert_eq!(lang_arg(&args(&["gallery", "check"])), None);
        assert_eq!(lang_arg(&args(&["gallery", "--", "--lang", "ru"])), None);
        assert_eq!(
            choose_locale(&args(&["gallery", "--lang", "pt_PT"])),
            "pt-BR"
        );
        assert_eq!(choose_locale(&args(&["gallery", "--lang", "xx"])), "en");
    }

    #[test]
    fn english_help_is_the_catalog_and_the_source_text() {
        let mut all = Vec::new();
        help_texts(&Cli::command(), "cli", &mut all);
        assert!(all.len() > 50);
        gallery_core::i18n::with_locale("en", || {
            for (key, id, text) in all {
                let found = catalog(&key).or_else(|| catalog(&format!("cli.arg.{}", id?)));
                assert_eq!(found.as_deref(), Some(text.as_str()), "{key}");
            }
            Some(())
        });
    }

    #[test]
    fn every_language_localizes_the_whole_help() {
        for l in gallery_core::i18n::LANGUAGES {
            gallery_core::i18n::with_locale(l.code, || {
                let mut cmd = command();
                cmd.build();
                let help = cmd.render_long_help().to_string();
                assert!(help.contains("--lang"), "{}", l.code);
                // Every subcommand's own help carries its translated text.
                for sub in cmd.get_subcommands() {
                    let key = format!("cli.{}.about", sub.get_name().replace('-', "_"));
                    let text = gallery_core::i18n::translate(&key, &[]);
                    assert!(help.contains(&text), "{} {key}", l.code);
                }
                if l.code != "en" {
                    assert!(
                        !help.contains("Scan the sources and write a plan"),
                        "{}",
                        l.code
                    );
                    assert!(!help.contains("Usage:"), "{}", l.code);
                }
            });
        }
    }
}
