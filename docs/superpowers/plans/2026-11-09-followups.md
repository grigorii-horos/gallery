# Follow-ups after ordered targets — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Persistent EXIF cache, AI tags/description written into metadata, place picked from a photo's GPS, per-file/per-target undo in History, and the deferred review minors.

**Architecture:** Each task is independent and touches its own area; all follow the existing patterns of the crates they change.

**Tech Stack:** Rust (gallery-core, gallery-cli, gallery-gui/Tauri v2), React+TS (ui/), exiftool.

**Spec:** `docs/superpowers/specs/2026-11-09-ordered-targets-and-filters-design.md` (for the filter/route model) plus the decisions below (user decisions 2026-11-09).

## User decisions (binding)

- EXIF cache lives in the system cache dir (`crate::paths::cache_dir()`: `~/.cache/gallery` on Linux, `~/Library/Caches/gallery` on macOS, `%LOCALAPPDATA%\gallery\cache` on Windows).
- AI tags and a one-sentence description are written into metadata **only for files routed by the AI** (`Via::Ai`), in the language the AI answers in (`[ai] language`, blank = interface language). They come from the same AI request (no extra request).
- Places: besides pasting "lat, lon", the user can pick a photo and take its GPS.
- History: undo a single file or all files of one target from a finished run, not only the whole run.

## Global Constraints

- Linux/macOS/Windows; CI gates: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` (XDG_DATA_HOME/XDG_CACHE_HOME at temp dirs; do NOT set GALLERY_DATA_DIR), `pnpm -C ui typecheck lint test`.
- Never touch `~/.config/gallery`, `~/.local/share/gallery`, `~/.cache/gallery`, real media, the real Syncthing or the Anthropic API; don't launch the GUI; don't build release / touch `target/release`; no push.
- Safety model unchanged (verify-before-place-before-trash, journal/undo, last-copy guard, plan identity). Byte-for-byte outputs (empty content pipe) are never modified.
- Every user-facing string in all 11 Rust and/or UI catalogs, real translations.
- Commits conventional, ending with a blank line and `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. Branch `feat/followups`.

## Review Focus

1. A file edited in place (same size, new mtime) must never get a stale cached EXIF record.
2. AI tags must not be written into byte-for-byte backup outputs, and must not make verification report an anomaly.
3. A cached AI answer from before this change (no tags) must not trigger a new paid request.
4. Partial undo of one target must not touch other targets' outputs or the sources that other items still need.
5. Picking a photo without GPS gives a clear message, no crash.

---

### Task 1: Persistent EXIF cache

**Files:** Modify `crates/gallery-core/src/exif.rs` (and its tests).

**Interfaces:** `exif::read_with(tools, paths, progress, cancel)` keeps its signature and result; it consults a cache at `cache_dir()/exif-cache.json` (or a small sharded dir if large) keyed by canonical path + size + mtime_ns + exiftool version + the argument set; records not in the cache are read in one batched exiftool call and stored. `None` cache dir → no persistence. Concurrent writers: write to a temp file and rename; a corrupt cache is ignored and rebuilt. Prune entries whose files no longer exist when saving.

- [ ] Tests: second read of unchanged files does not run exiftool (inject a counting tools stub or check a call counter); a file rewritten with the same size and a new mtime is re-read; corrupt cache file is ignored; no cache dir → works without writing.
- [ ] Implement, gates, commit `feat(exif): persistent EXIF cache keyed by path, size and mtime`.

### Task 2: AI tags and description into metadata

**Files:** `crates/gallery-core/src/ai/client.rs` (schema, prompt, parsing, cache), `crates/gallery-core/src/route.rs` (`AiAnswer`), `crates/gallery-core/src/plan.rs` (`PlanItem.ai_meta: Option<AiMeta { tags: Vec<String>, description: String }>`, serde default + skip if none; `needed_version()` returns 2 when any item has it), `crates/gallery-core/src/apply.rs` (write after the content pipe, before verification/placement), `crates/gallery-core/src/verify.rs` if needed, docs (`docs/configuration.md` AI section), locales.

**Decisions:** schema adds optional `tags` (3–10 short keywords, lowercase, in the answer language) and `description` (one sentence). Written with exiftool as `XMP-dc:Subject` (list), `XMP-dc:Description`, plus `IPTC:Keywords` for JPEG; for video containers exiftool's XMP in MP4/MOV. Only for items with `via = Ai` and only for outputs whose content pipe is non-empty. Old cached answers without tags are used as is (no tags, no new request). Tags never influence routing.

- [ ] Tests: schema contains tags/description and parsing fills them; cached answer without tags → no request, no tags; plan item for an AI-routed file carries ai_meta, plan version 2; apply writes `XMP-dc:Subject` and `XMP-dc:Description` into a processed output (require_tool exiftool) and leaves a byte-copy output byte-identical; verification passes.
- [ ] Implement, gates, commit `feat(ai): write AI tags and description into metadata of AI-routed outputs`.

### Task 3: Place from a photo's GPS

**Files:** `crates/gallery-gui/src/lib.rs` + a small command module (e.g. `places.rs`): command `gps_of(path) -> Result<{lat, lon}, String>` via `exif::read` + `filter::facts` GPS parsing (reuse, (0,0) = no fix); `ui/src/editor/PlacesPanel.tsx` button "From a photo…" using `@tauri-apps/plugin-dialog` open (image/video filters) → fills lat/lon of the row; locales.

- [ ] Tests: Rust command on a fixture with GPS (require_tool exiftool) and without GPS (error text); UI test with mocked dialog + invoke fills the fields; no GPS shows the message.
- [ ] Implement, gates, commit `feat(gui): take a place's coordinates from a photo`.

### Task 4: Undo one file or one target from History

**Files:** `crates/gallery-gui/src/history.rs` (commands `undo_files(report, sources)` and `undo_target(report, target)` built on `gallery_core::undo::undo_items`, with the same lock/guards as the full undo), `ui/src/screens/HistoryScreen.tsx` (per-item undo button, per-target undo button with confirm and preview counts), `crates/gallery-cli/src/main.rs` (`gallery undo <report> [--file PATH]... [--target NAME]`), `docs/cli.md`, locales.

- [ ] Tests: undo of one target restores only that target's items (outputs removed, trashed sources restored) and leaves another target's outputs; a later full undo skips the already-undone items; CLI flags; UI buttons call the commands and refresh; server-side reports stay refused as today.
- [ ] Implement, gates, commit `feat(history): undo one file or one target of a run`.

### Task 5: Deferred review minors — core and CLI

**Files:** as named per item.

- `tools.rs`: install hints per Linux distro (detect `pacman`/`dnf`/`zypper`/`apt` on PATH; Arch: `sudo pacman -S perl-image-exiftool optipng ffmpeg imagemagick libavif`).
- `filter/range.rs`: `TimeOfDay` with equal ends (`"09:00..09:00"`) is a config error.
- `filter/facts.rs`: geo `{has_gps=false, lat/lon/km}` together is a config error; video rotation (QuickTime `Rotation` 90/270) swaps width/height for `orientation`; tests for `facts_of` fallbacks (ExifImageWidth, MediaDuration, CodecID) and `not=true` on props and path.
- `filter/groups.rs`: `near_count` stops at `min`; `Groups::event` lookup without cloning `EventKey` per call; file under both a source and an output root hashed once; `MetricsCache` prunes entries of missing files on save.
- `route.rs`/`effect.rs`: effect diff passes tools when available and computes groups once when `needs` are equal for old and new config; `resolve_expand` skips events already expanded; `suggested_best` does not hold a one-shot series (no other shot held at that target).
- `plan.rs`: guard-held review items carry `captured` from the plan item; trash items journaled once.
- `explain`/UI DTO: rename leftover `stages` to `targets`.

- [ ] Tests for each behavioural change (one focused test each), gates, commit(s) `fix: …` per area.

### Task 6: Deferred review minors — GUI and UI

- `config_edit/file.rs`: Keep backup copies the checked original bytes, not the live file.
- `config_edit/migrate.rs`: `is_old_shape` false when all stage lists are empty (match core).
- `config_edit/checks.rs`: `unknown_place` points at the filter that names the place.
- `summary.rs`: doc of `conditions` (expand) fixed; `preview_change` reports `changed = true` when the diff is non-empty for an old-shape file (or the UI explains the rewrite).
- `overrides.rs`: `confirm_series` cross-checks `members` against the current plan's `ReviewItem.series` and refuses others; stamps compared inside the batch writer (no TOCTOU); test for a stale `decide_event` member.
- UI: `OptNumber` without `min` for lat/lon; Places invalid row shows an error; geo form keeps its mode when the circle is cleared; duplicate scope untoggle does not snap back; TargetsScreen formatting; ConditionMenu Escape + focus; series cards in arrow-key navigation (radiogroup arrows); a series card is shown in every event its members belong to and event counts match what is rendered; an event-wide target decision excludes held series members (they keep the confirm-best flow); `decide_head` counts series cards.

- [ ] Tests for each behavioural change, gates, commit(s) `fix(gui): …` / `fix(ui): …`.
