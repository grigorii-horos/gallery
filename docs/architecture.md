# Architecture

Gallery is a Cargo workspace plus a React front end. The GUI and the CLI are thin shells over one
Rust core, so they share plans, reports, history, locks and caches.

## Crates

| Path | What it is |
|---|---|
| `crates/gallery-core` | Everything that matters: config, masks, scanning, routing, the AI client, plan building, apply, verify, undo, explain, estimates, Syncthing pause, triage, learning. No UI. |
| `crates/gallery-cli` | The `gallery` binary (clap): `check`, `scan`, `apply`, `undo`, `explain`, … See [cli.md](cli.md). |
| `crates/gallery-gui` | The Tauri v2 app (`gallery-gui`): Rust commands that call the core, a background "pending" watcher, the tray, notifications, the single-instance guard. |
| `ui/` | The React + TypeScript front end (Vite, Vitest). Talks to the Rust side only through Tauri commands and events. |

External tools do the media work and are found in `GALLERY_TOOLS_DIR`, then `PATH`: `ffmpeg`/`ffprobe`
(SVT-AV1, Opus), `exiftool`, `avifenc`, ImageMagick `magick`, `optipng`.

## Data flow

```
sources ──scan──▶ file list ──EXIF──▶ route ──▶ plan ──▶ apply ──▶ report ──▶ undo
                                       │          │         │
                       before_ai masks │          │         ├─ stage outputs (.gallery-tmp-*)
                       AI (cached)     │          │         ├─ verify each result against its source
                       after_ai masks  │          │         ├─ place outputs (never overwrite)
                       manual decisions┘          │         └─ move the source to the trash
                                      plan.json: items, review, unmatched, warnings
```

1. **Scan.** Walk each source (`recursive`, `skip`), collecting path, size, mtime. EXIF is read in
   batches with `exiftool`.
2. **Route.** Three first-match-wins stages: `before_ai` name masks, then one Claude request per
   still-unrouted file that fits an AI target (a 512 px preview, or 3 frames for video), then
   `after_ai` masks. Answers come from the cache when the same file (path, size, mtime), model and
   prompt were seen before. Unsure answers become *review* items; your manual decisions override
   everything. Nothing is written.
3. **Plan.** Routing results become a JSON plan: for each file its target, how it was decided, and
   for each output the content steps (bundles expanded), the final name and folder (capture date
   from EXIF or the file name), plus the review list, unmatched files, warnings and the "unchanged"
   list. The plan also embeds the content presets it uses, so a server can apply it **without the
   config**. A plan built with `--server` uses `server_path` / `server_dir`.
4. **Apply.** For each item, in parallel: run the content pipe on a staged copy, verify it (below),
   link/rename it into place without overwriting, and only after *all* outputs are in place move
   the source to the trash (or, for in-place targets, rename/move it within its source root).
   Every finished item is appended to a journal; the report is refreshed as the run goes.
5. **Undo.** From the report: restore sources from the trash, then remove the outputs this run
   created, refusing to overwrite or delete anything that was changed since.

A *pilot* is a normal apply of a sub-plan (up to N files per target, different extensions
preferred); the remaining files are a second plan. **Retry failed** builds a plan of the failed
files of a report, after removing the partial outputs they left.

### The GUI's "one current plan"

The Sort screen works on one *current plan* per config. It is rebuilt automatically, **without any
AI call**, whenever the config, your decisions, the files in the sources or the AI cache change
(tracked by a fingerprint). Files that need a fresh AI answer wait; the *Ask AI* button runs the
only step that spends money and fills the cache. A run never works on the current plan directly: it
is first copied into the `plans/` archive and the copy runs, so the report lands next to it and
History reads from there. A plan that went stale between the check and the run is refused and
rebuilt, so you only ever run exactly the plan you confirmed.

## Safety guarantees

These are enforced by the core (`apply`, `verify`, `undo`, `lock`), not by the UI:

- **Sources are only read** until every output of their item is in place and verified.
- **Nothing is overwritten.** Outputs are staged as `.gallery-tmp-<run>-*` in their destination
  folder and placed with a no-overwrite link/rename; a taken name gets `name_1.ext`, `name_2.ext`…
  Undo never overwrites an occupied place either.
- **Originals go to the trash only after verification.** Each staged result is compared with its
  source before it is placed: capture date, mtime, GPS, image dimensions (aspect ratio) and
  audio/video duration. A mismatch is an *anomaly*: the output is not placed and the source stays
  untouched, unless you pass `--allow-anomalies` (CLI) or tick the box in the run dialog. A run that
  keeps hitting anomalies stops itself.
- **Changed files are skipped.** If a source changed (or vanished) since the scan, its item is
  skipped, not processed.
- **Pilot runs.** Try N files per target first; look at the before → after table; continue with the
  rest or undo the pilot.
- **Undo and retry.** Every real run writes a report. Undo restores sources and removes only outputs
  that still match what was written (size and BLAKE3 hash); a report can be undone once. An
  interrupted undo can be re-run. **Retry failed** re-plans only the failed files.
- **Crash safety.** A per-item journal is written and synced as the run goes, so even a power cut
  between two report writes leaves undo knowing every source that was trashed.
- **Locks.** Only one apply/undo/retry runs per machine (`apply.lock`, an advisory `flock`: the
  kernel releases it when the process dies, so there is no stale lock to clean up), and each plan
  has its own `<plan>.lock`. The GUI also allows a single instance.
- **Syncthing pause.** A run can pause the local Syncthing folders it touches, so a bad result does
  not spread to your phones before you have looked. Only the local instance is contacted (address
  and key from its own `config.xml`); only folders that were not already paused are paused, and
  only those are resumed. The pause record is written *before* the first pause, and the tray shows
  and reminds you of a pause.
- **Atomic writes.** Plan files, the current-plan state, the pending index and your config (when edited in the GUI) are written through a temp file and a rename, so a reader never sees half a file.
- **Nothing leaves your machine except thumbnails.** The AI step uploads a small preview
  (512 px photo / three 400 px video frames) and your category text; never the original and never
  the file name. `ai_prefix`, `ai_suffix`, category descriptions and your recent corrections (as
  text) are part of the prompt.

## Files on disk

Paths are for a normal (release) build on Linux; `XDG_*` variables are honoured. On Windows and
macOS the same names live under `dirs`' per-OS folders (`%APPDATA%\gallery`, `%LOCALAPPDATA%\gallery`;
`~/Library/Application Support/gallery`, `~/Library/Caches/gallery`). In **debug builds
only**, `GALLERY_DATA_DIR` redirects the data and cache directories (used for screenshots and tests;
release builds ignore it).

| Where | File | What |
|---|---|---|
| `~/.config/gallery/` | `gallery.toml` | Your config (the GUI's default). |
| | `api-key` | Optional AI key (must be owner-only). |
| `~/.local/share/gallery/` | `current/<config-hash>.plan.json`, `.state.json` | The GUI's current plan and the fingerprint it was built from. |
| | `plans/<YYYYMMDD-HHMMSS>.plan.json` | Archive of executed plans (the GUI's runs). |
| | `plans/<…>.plan.report.json` | The run's report: every item, its outputs, trash location and verification results. Undo and History read it. |
| | `plans/<…>.plan.report.jsonl` | The journal of an unfinished run; deleted when the run finishes. |
| | `plans/<…>.plan.lock` | Per-plan lock (persistent, harmless when left behind). |
| | `overrides.json` | Your manual decisions per source file. |
| | `ai-examples.jsonl` | Your corrections, shown to the AI (text only). |
| | `sync-paused.json` | Which Syncthing folders Gallery paused (present only while paused). |
| | `settings.json`, `notify-state.json` | Tray/check settings and notification state. |
| | `apply.lock` | The machine-wide apply/undo lock. |
| `~/.cache/gallery/` | `ai-cache.jsonl` | AI answers (append-only). Safe to delete; it only costs money to rebuild. |
| | `ai-usage.jsonl` | Token usage per AI scan, for cost history. |
| | `pending-index.json` | Per-file size/mtime/EXIF index for quick pending counts. |
| | `thumbs/` | Thumbnails shown in the UI. |
| next to a plan (CLI) | `<plan>.report.json`, `<plan>.jsonl`, `<plan>.lock` | Names are built from the plan name minus a trailing `.json` (`trip.2026.json` → `trip.2026.report.json`). |
| inside a source | `.gallery-trash/<run>/` | Where trashed originals go when `GALLERY_TRASH=folder` is set. |
| output folders | `.gallery-tmp-*`, `.gallery-undo-*` | Staging and undo scratch names; never a folder a template can create. |

## Server model

`gallery scan --server` (or the GUI's *export plan*) builds the same plan with server paths and
writes it where Syncthing will carry it. The server needs only the `gallery` binary and the
external tools, not the config, because the plan carries its own presets. `gallery apply` there
writes `<plan>.report.json` next to the plan, which comes back through Syncthing; `gallery undo`
works from any machine that can reach the files. The server's `gallery` must be at least as new as
the desktop's: a plan containing manual decisions needs a binary of the same version or newer, and
an older one refuses to load it with a hint to update.

## Development

```sh
cargo test --workspace            # tests that need external tools skip themselves without them
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
pnpm -C ui typecheck && pnpm -C ui lint && pnpm -C ui test
```

Set `XDG_CONFIG_HOME`, `XDG_DATA_HOME` and `XDG_CACHE_HOME` to a temporary directory when running
the tests, so they cannot touch your real state.

## Cross-platform notes

- **Time zones.** Nothing is left to the OS's `TZ` (Windows' exiftool cannot read IANA names).
  `Tools::tz` holds the plan's zone; QuickTime UTC values, file-time offsets and the read-back of
  QuickTime dates are converted in Rust with chrono-tz (`content::metadata::date_args`,
  `exif::quicktime_to_local`). On unix `TZ` is still exported to the children.
- **Placing files never overwrites.** Hard link + unlink first (all unix, NTFS); where links are
  unavailable, `renameat2(RENAME_NOREPLACE)` on Linux and `MoveFileExW` without
  `MOVEFILE_REPLACE_EXISTING` on Windows; elsewhere an existence check plus rename.
- **Tools.** Windows children run at idle priority, in their own process group, without a console
  window; unix ones under `nice`/`ionice` in their own process group.
