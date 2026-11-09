# Architecture

Gallery is a Cargo workspace plus a React front end. The GUI and the CLI are thin shells over one
Rust core, so they share plans, reports, history, locks and caches.

## Crates

| Path | What it is |
|---|---|
| `crates/gallery-core` | Everything that matters: config, masks, scanning, conditions (`filter`), routing, the AI client, plan building, apply, verify, undo, explain, estimates, Syncthing pause, triage, learning. No UI. |
| `crates/gallery-cli` | The `gallery` binary (clap): `check`, `scan`, `apply`, `undo`, `explain`, … See [cli.md](cli.md). |
| `crates/gallery-gui` | The Tauri v2 app (`gallery-gui`): Rust commands that call the core, a background "pending" watcher, the tray, notifications, the single-instance guard. |
| `ui/` | The React + TypeScript front end (Vite, Vitest). Talks to the Rust side only through Tauri commands and events. |

External tools do the media work and are found in `GALLERY_TOOLS_DIR`, then `PATH`: `ffmpeg`/`ffprobe`
(SVT-AV1, Opus), `exiftool`, `avifenc`, ImageMagick `magick`, `optipng`.

## Data flow

```
sources ──scan──▶ file list ──EXIF──▶ groups ──▶ route ──▶ plan ──▶ apply ──▶ report ──▶ undo
                  (frozen)    facts    events      │          │         │
                                       near        │          │         ├─ stage outputs (.gallery-tmp-*)
                       manual decisions first      │          │         ├─ verify each result against its source
                       targets walked in order     │          │         ├─ place outputs (never overwrite)
                       AI: pause, ask once, resume │          │         └─ move the source to the trash
                                                   ┘   plan.json: items (folders / trash), kept, review,
                                                       unmatched, warnings; last-copy guard
```

1. **Scan.** Walk each source (`recursive`, `skip`), collecting path, size, mtime. The list is
   *frozen* for the run. EXIF is read in batches with `exiftool` when some condition needs it, and
   cached by path, size and mtime.
2. **Facts and groups.** From the metadata `filter::facts` builds per-file facts (capture date in
   the plan's time zone, GPS, device, properties). `filter::groups` then computes, only for the
   conditions the config uses and on the whole frozen list: *events* (files of some sources sorted
   by capture time, split by a gap or a jump in place), *neighbours* for `near` (per-source time
   index, a time window search), *duplicate groups* (size, then a hash; the original is chosen by
   source order, capture date, path; hashes and also the output folders' files are cached), and
   *series* (consecutive shots of one source within a gap and visually similar by a perceptual hash
   of a thumbnail; the best shot by sharpness, size or resolution). Sharpness (variance of the
   Laplacian on a downscaled decode) and hashes are cached in the user cache dir. All of this is
   independent of the routing order.
3. **Route.** Manual decisions apply first. Then every file walks `[[targets]]` top-down: the first
   target with a filter that matches (all the filter's conditions hold, `first_miss` reports the
   first one that does not) takes it, and later targets never see it. A `keep` target takes it and
   does nothing; a `trash` target marks it for the trash; a `folders` target plans its outputs.
   `expand = "event"` filters are barriers (below). Nothing is written.

   **The AI is a condition, walked in strict order with pause/resume.** A file walks until it
   reaches a target whose matching filter has `ai`. It then pauses. All paused files are collected
   and one batch of requests is sent (a 512 px preview, or 3 frames for video, per file), each
   with every AI category the file can still reach: the AI filters of this and later targets whose
   non-AI conditions hold for it. Requests are grouped by identical category set; answers come from
   the cache when the same file (path, size, mtime), model, prompt and category set were seen
   before. Then each file resumes its walk: an `ai` condition holds only for the category the model
   chose, and a non-AI target between two AI targets still gets the file first if it matches. This
   gives the same result as a plain strict walk. An unsure answer becomes a *review* item where the
   file would be taken through it.

   **`expand`.** A target with an `expand = "event"` filter resolves only when every undecided file
   stands at it. The seeds are undecided files for which the filter's other conditions hold, and
   the filter then also takes every undecided file of their events that is in its `sources`
   (still subject to `match` / `exclude`).

   **Series review.** A file matched through `series` is not sent to the target: it is held for
   review together with its series and a suggested best shot (`ReviewItem.series`). The user's
   confirmation is a manual decision per series, after which the others go to the target.
4. **Plan.** Routing results become a JSON plan: for each file its target, how it was decided, and
   for each output the content steps (bundles expanded), the final name and folder (capture date
   from EXIF or the file name), plus the review list, unmatched files, warnings, the "unchanged"
   list and the files a `keep` target holds (`kept`). A file a `trash` target takes is an item of
   kind `trash` with no outputs. Then the **last-copy guard** runs (below). The plan also embeds the content presets it uses, so a server can apply it **without the
   config**. A plan built with `--server` uses `server_path` / `server_dir`.
5. **Apply.** For each item, in parallel: run the content pipe on a staged copy, verify it (below),
   link/rename it into place without overwriting, and only after *all* outputs are in place move
   the source to the trash (or, for in-place targets, rename/move it within its source root).
   `trash` items are moved to the trash after all placements, with the same journal, and undo
   restores them.
   Every finished item is appended to a journal; the report is refreshed as the run goes.
6. **Undo.** From the report: restore sources from the trash, then remove the outputs this run
   created, refusing to overwrite or delete anything that was changed since.

A *pilot* is a normal apply of a sub-plan (up to N files per target, different extensions
preferred); the remaining files are a second plan. **Retry failed** builds a plan of the failed
files of a report, after removing the partial outputs they left.

### Plan versions

`Plan.version` is the lowest format the plan needs. A plan with only `folders` items is **version 1**
and has exactly the shape it had before targets could be `trash` or `keep` (no `kind` on items, no
`kept`), so an older `gallery apply` still runs it. A plan with a `trash` item (`PlanItem.kind =
"trash"`) or a kept file (`Plan.kept`) is **version 2**. `Plan::load` accepts 1 and 2; a newer
version is refused before anything is touched with an "update gallery on the server" message, so a
server running an older binary never applies a plan it does not understand. Plan identity (the
`plan_id` and a pilot's slice id) is a hash of the saved bytes, so it follows the version.

### The last-copy guard

After routing, `guard_last_copies(plan, groups, files)` looks at every set of files that must not
all go to the trash: each group of byte-identical files (`Groups.same_content`, whatever scope the
`duplicate` conditions use, `outputs` alone included) and each series. If **every** member is a
`trash` item, the original (duplicate group) or the best shot (series) is moved from the plan's
items to its review list with the reason "every other copy goes to the trash too: this one is
kept for review". It runs in the scan pipeline right after the plan is built, and `explain` runs
the same guard, so the explanation never disagrees with a real scan. Items are matched by source
path locally, and by path below the source root, size and mtime for a server plan.

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
| | `incompressible.json` | Files whose content pipe could not make them smaller (path, size, mtime, pipe fingerprint): planning keeps them in place instead of encoding them again. Safe to delete; it only costs one more encode per file. |
| | `metrics.json` | Per-file sharpness, perceptual hash and content hash used by `series` and `duplicate` conditions (safe to delete). |
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
