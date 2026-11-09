# Ordered Targets and Rich Filters Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the three routing stages with one user-ordered `[[targets]]` list whose filters combine general conditions (date, place, event, near, device, props, duplicate, series, path, AI), plus `trash` and `keep` target kinds.

**Architecture:** Config gets `Vec<Target>` (old stage shape migrated on load). Per-file *facts* are derived from the exiftool records the pipeline already reads; *groups* (events, neighbours, duplicates, series) are computed once over the frozen scan list; routing walks targets in order, pausing files at their first AI need, sending one AI batch, then resuming. Plans gain a per-item kind (folders/trash/keep) and version 2.

**Tech Stack:** Rust (gallery-core, gallery-cli, gallery-gui/Tauri v2), React+TS (ui/, pnpm, vitest), exiftool, ffmpeg/magick thumbnails, `image` crate (jpeg only) for sharpness/dHash.

**Spec:** `docs/superpowers/specs/2026-11-09-ordered-targets-and-filters-design.md`

## Global Constraints

- Linux, macOS, Windows; CI (`.github/workflows/ci.yml`) stays green: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, `pnpm -C ui typecheck lint test`.
- Run tests with `XDG_DATA_HOME`/`XDG_CACHE_HOME` (and `GALLERY_DATA_DIR`) pointing at temp dirs. Never touch `~/.config/gallery`, `~/.local/share/gallery`, `~/.cache/gallery`, real media, the real Syncthing or the Anthropic API. Don't launch the GUI; don't touch `target/release`.
- Safety model unchanged: verify-before-place-before-trash, no-replace placement, journal/report/undo/retry, pilot, plan identity (plan_id/pilot_id), mtime preservation.
- Every user-facing string goes to `locales/en.json` (Rust) or `ui/src/i18n/locales/en.json` (UI) and to the other 10 locales (en, ru, uk, ro, de, fr, es, pt-BR, it, pl, zh-CN) — real translations; parity tests must pass.
- Old configs (`targets.before_ai` / `targets.ai` / `targets.after_ai`) load unchanged in meaning: concatenated in that order.
- `expand` together with `ai` in one filter is a config error.
- Missing data → condition false; `not = true` negates the whole condition table.
- Series matches always go to review first (never straight to the target).
- Never trash every copy of a duplicate group or every shot of a series.
- Commits: conventional, each ending with a blank line and `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. Work on branch `feat/ordered-targets`. zsh noclobber (`>|`); `cp/mv/rm` are aliased `-i`.

## Review Focus

1. Photos without EXIF date or GPS (screenshots, messenger files) under `date`/`geo`/`near`/`event` → never match (and match with `not`), never panic; the explain panel says "no capture date"/"no GPS".
2. Mixed time zones / files from two phones with different zones in `near`/`event` → times compared as the plan's local zone, same as name presets; a pair 30 min apart across zones is not mis-grouped by hours.
3. An old-shape config edited and saved from the GUI → `.bak` written first, saved file is new shape, routing identical (same decisions on the fixture library).
4. A duplicate group where the "original" is taken by an earlier `trash` target, or both copies hit trash targets → one copy is held in review, nothing is lost.
5. A server running an older `gallery` given a version-2 plan → refuses with the "update gallery" message, touches nothing.

---

## File Structure

- `crates/gallery-core/src/config.rs` — `Target`, `TargetKind`, `Filter`, `Place`, migration (`RawTargets`), validation.
- `crates/gallery-core/src/filter/mod.rs` — `Conditions` structs (serde), `Facts`, `Needs`, `check_filter`.
- `crates/gallery-core/src/filter/range.rs` — value/duration/size/time-of-day/date-range parsers.
- `crates/gallery-core/src/filter/facts.rs` — `facts_of` from an exiftool record.
- `crates/gallery-core/src/filter/groups.rs` — events, near index, duplicate groups, series.
- `crates/gallery-core/src/filter/image_metrics.rs` — sharpness and dHash from cached thumbnails.
- `crates/gallery-core/src/route.rs` — ordered walk, AI pause/resume, expand, kinds, series review.
- `crates/gallery-core/src/plan.rs`, `apply.rs`, `undo.rs`, `estimate.rs`, `effect.rs`, `explain.rs` — kinds, version 2, guards, reasons.
- `crates/gallery-core/src/overrides.rs` — `Action::SeriesBest`.
- `crates/gallery-gui/src/config_edit/*` — new model/ops, places, migration save.
- `ui/src/screens/TargetsScreen.tsx`, `ui/src/editor/conditions/*.tsx` (new), `ui/src/screens/SettingsScreen.tsx` (Places), `ui/src/flow/DecideStep.tsx` (series/event review).
- `examples/gallery.toml`, `docs/configuration.md`, `docs/architecture.md`, `README.md`.

---

### Task 1: Ordered targets in config, with migration

**Files:**
- Modify: `crates/gallery-core/src/config.rs` (Targets/MaskTarget/AiTarget/InputFilter/Prefilter → new types), every compile site in `crates/gallery-core`, `crates/gallery-cli`, `crates/gallery-gui` (mechanical: iterate `cfg.targets`).
- Modify: `crates/gallery-core/src/route.rs` (keep today's behaviour: walk in order; AI as today but per target position — full AI semantics land in Task 5).
- Test: `crates/gallery-core/src/config.rs` tests module.

**Interfaces:**
- Produces:
  - `pub enum TargetKind { Folders, Trash, Keep }` (serde snake_case, default `Folders`).
  - `pub struct Target { pub name: String, pub kind: TargetKind, pub filters: Vec<Filter>, pub outputs: Vec<Output> }`.
  - `pub struct Filter { pub sources: Vec<String>, pub matches: Option<MaskExpr> /* "match" */, pub exclude: MaskExpr, pub conditions: crate::filter::Conditions /* #[serde(flatten)] */ }` — Task 1 defines `Conditions` with only `ai: Option<String>` and `expand: Option<Expand>`; Task 3–4 add fields.
  - `pub struct Place { pub lat: f64, pub lon: f64, pub km: f64 }`; `Config.places: BTreeMap<String, Place>`.
  - `Config.targets: Vec<Target>`; `Config::target(&self, name) -> Option<&Target>`; `Config::was_staged: bool` (`#[serde(skip)]`, true when loaded from the old shape).
- [ ] **Step 1: Write failing tests** `old_stage_shape_migrates_in_order` (before_ai `A`, ai `D` with prefilter `@photo` and description `"docs"`, after_ai `B` → names `["A","D","B"]`, `D.filters[0].matches == Some(@photo)`, `D.filters[0].conditions.ai == Some("docs")`, `was_staged == true`), `new_shape_parses_kinds` (`kind = "trash"` with no outputs ok; `kind = "trash"` with outputs → error containing `outputs`), `expand_with_ai_is_an_error`, `unknown_place_ref_is_an_error` (geo added in Task 3 — leave this test for Task 3), `mixing_old_and_new_shape_is_an_error`.
- [ ] **Step 2:** `cargo test -p gallery-core config::` → FAIL (types missing).
- [ ] **Step 3: Implement** a private `RawConfig` deserialized first; `targets` is `#[serde(untagged)] enum RawTargets { List(Vec<Target>), Staged { before_ai, ai, after_ai } }`; convert to `Vec<Target>`. Validation in `Config::validate` (existing entry point). Update all call sites; `route_with_overrides` walks `cfg.targets` in order — mask-only targets decide immediately, AI targets keep today's single batch (temporary).
- [ ] **Step 4:** `cargo test --workspace` (existing routing/e2e tests must pass unchanged in meaning; update only struct-literal construction in tests) → PASS. clippy/fmt clean.
- [ ] **Step 5: Commit** `refactor(config): one ordered [[targets]] list; old stages migrate on load`.

### Task 2: Value parsers

**Files:**
- Create: `crates/gallery-core/src/filter/mod.rs` (module root), `crates/gallery-core/src/filter/range.rs`.

**Interfaces:**
- Produces:
  - `pub struct NumRange { pub min: Option<f64>, pub max: Option<f64>, pub min_incl: bool, pub max_incl: bool }`, `NumRange::parse(s: &str, unit: Unit) -> Result<NumRange>`, `fn contains(&self, v: f64) -> bool`. `pub enum Unit { Plain, Bytes, Seconds }`.
  - `pub fn parse_duration(s: &str) -> Result<chrono::Duration>` (`"5s" "2h" "30d" "1y"=365d "1w"`).
  - `pub struct TimeOfDay { from: NaiveTime, to: NaiveTime }` (wraps midnight), `parse`, `contains(NaiveTime)`.
  - `pub struct DateRange { from: Option<NaiveDate>, to: Option<NaiveDate> }` (`"2026-08-12..2026-08-18"`, open ends), `parse`, `contains(NaiveDate)` inclusive.
- [ ] **Step 1: Tests:** `"..500KB"` contains 500·1024 and not 500·1024+1 (KB = 1024; MB, GB likewise); `">=60"` contains 60; `"1MB..20MB"`; `"<40"` excludes 40; `"..10s"` seconds; `"16:9"` rejected by NumRange (aspect parsed in Task 3); duration `"1y"` = 365 days; `TimeOfDay("22:00..05:00")` contains 23:30 and 04:59, not 05:00, not 12:00; `DateRange("2026-01-01..")` contains 2030-01-01; malformed → `Err` with the input text in the message.
- [ ] **Step 2:** run → FAIL. **Step 3:** implement. **Step 4:** run → PASS.
- [ ] **Step 5: Commit** `feat(filter): range, duration, time and date parsers`.

### Task 3: Facts and per-file conditions (date, geo, device, props, path)

**Files:**
- Create: `crates/gallery-core/src/filter/facts.rs`.
- Modify: `crates/gallery-core/src/filter/mod.rs`, `crates/gallery-core/src/config.rs` (validate place refs).

**Interfaces:**
- Consumes: Task 2 parsers; `crate::datetime::{capture_date, file_created, from_name}`; exiftool records (`HashMap<PathBuf, serde_json::Value>`, keys as `exif::read` produces).
- Produces:
  - `pub struct Facts { pub capture: Option<NaiveDateTime>, pub mtime: NaiveDateTime, pub name_date: Option<NaiveDateTime>, pub gps: Option<(f64, f64)>, pub make: Option<String>, pub model: Option<String>, pub software: Option<String>, pub width: Option<u32>, pub height: Option<u32>, pub duration_s: Option<f64>, pub fps: Option<f64>, pub video_codec: Option<String>, pub audio_codec: Option<String>, pub has_audio: Option<bool>, pub container: String /* lowercase ext */, pub size: u64 }`.
  - `pub fn facts_of(entry: &FileEntry, exif: &serde_json::Value, tz: Option<Tz>) -> Facts`.
  - Condition structs in `Conditions` (all `Option`, each with `#[serde(default)] not: bool`): `DateCond { source: DateSource /* capture|file_mtime|name */, range, older_than, newer_than, months: Vec<u32>, weekdays: Vec<Weekday>, time }`, `GeoCond` (string `"@x"`/`"!@x"`, list, or inline `{lat, lon, km}`; plus `has_gps: Option<bool>`), `DeviceCond { make, model, software: Option<MaskExpr> /* case-insensitive */ }`, `PropsCond { size, width, height, megapixels, duration, fps, sharpness /* Task 4 fills */, orientation: Option<Orientation>, aspect: Option<String>, video_codec, audio_codec, container: Option<Vec<String>>, has_audio: Option<bool>, has_tag: Vec<String>, missing_tag: Vec<String> }`, `path: Option<MaskExpr>` (matched against `rel` with `/` separators).
  - `pub fn check_file(cfg: &Config, c: &Conditions, entry: &FileEntry, facts: &Facts, exif: &Value, now: NaiveDateTime) -> Result<(), Miss>` where `pub struct Miss { pub condition: &'static str, pub detail: String }` (detail is a translated short reason, e.g. `t!("filter.miss.no_gps")`).
- [ ] **Step 1: Tests** (JSON fixtures inline): Pixel record (`Make "Google"`, `Model "Pixel 8"`, `GPSLatitude 47.02`, `GPSLongitude 28.83`, `ImageWidth 4000`, `ImageHeight 3000`) → `device {model="pixel*"}` hits (case-insensitive), `geo "@home"` (home 47.0245, 28.8323, km 2) hits, `"!@home"` misses, `props {megapixels=">=12", orientation="landscape"}` hits; screenshot without GPS → `geo "@home"` miss with reason `no_gps`, `{has_gps=false}` hit; `date {time="22:00..05:00"}` with capture 23:10 hit; `date {older_than="30d"}` with `now` fixed; `date {source="name"}` on `20260716_211554.png`; video record (`CompressorID "hvc1"`/`VideoCodec`, `AudioFormat "mp4a"`, `Duration 7.5`, `VideoFrameRate 59.94`) → `props {video_codec="hevc", duration="..10s", fps=">=50", has_audio=true}` hit (map `hvc1|hev1→hevc`, `avc1→h264`, `av01→av1`, `mp4a→aac`); `has_tag=["GPSLatitude"]`; `path="WhatsApp/**"` on rel `WhatsApp/Media/x.jpg` (Windows-style `\` in rel also matches); `not=true` on a missing-data condition → hit; config `geo="@nowhere"` → load error naming `nowhere`.
- [ ] **Step 2:** FAIL. **Step 3:** implement; aspect `"16:9"` matches within 1% either orientation; megapixels = w·h/1e6; haversine km. **Step 4:** PASS.
- [ ] **Step 5: Commit** `feat(filter): per-file conditions date, geo, device, props, path`.

### Task 4: Groups over the frozen list (event, near, duplicate, series, sharpness)

**Files:**
- Create: `crates/gallery-core/src/filter/groups.rs`, `crates/gallery-core/src/filter/image_metrics.rs`.
- Modify: `crates/gallery-core/Cargo.toml` (add `image = { version = "0.25", default-features = false, features = ["jpeg"] }`), `crates/gallery-core/src/filter/mod.rs`.

**Interfaces:**
- Consumes: `Facts` (Task 3), `crate::thumbs::thumb_for_in(tools, path, dir) -> Result<PathBuf>` (512 px JPEG), `blake3`.
- Produces:
  - Conditions: `EventCond { sources: Vec<String>, gap: Option<String> /* default "4h" */, km: Option<f64>, min_files: Option<usize>, min_duration, max_duration: Option<String>, away_from: Option<String> /* "@place" */, not }`, `Expand::Event` (`expand = "event"`), `NearCond { sources, time /* default "2h" */, km: Option<f64>, min: usize /* default 1 */, require_gps: bool, not }`, `DuplicateCond { of: Vec<DupScope /* sources|outputs */>, not }`, `SeriesCond { gap /* "5s" */, similar: bool /* true */, keep: Keep /* best */, by: By /* sharpness|size|resolution */, not }`.
  - `pub struct Needs { exif_all: bool, events: Vec<EventKey>, near: bool, duplicates: Vec<DupScope>, series: bool, sharpness: bool }`, `pub fn needs(cfg: &Config) -> Needs`.
  - `pub struct Groups { pub event_of: HashMap<(EventKey, usize), usize> /* (params, file idx) → event id */, pub events: HashMap<EventKey, Vec<Event>>, pub dup_original: HashMap<usize, usize> /* dup idx → original idx */, pub dup_in_outputs: HashSet<usize>, pub series_of: HashMap<usize, usize>, pub series: Vec<Series { members: Vec<usize>, best: usize }>, pub sharpness: HashMap<usize, f32> }`; `pub struct Event { pub members: Vec<usize>, pub start: NaiveDateTime, pub end: NaiveDateTime }`; `EventKey { sources: Vec<String>, gap_s: i64, km_milli: Option<u64> }` (hashable).
  - `pub fn compute(cfg: &Config, files: &[FileEntry], facts: &[Facts], needs: &Needs, tools: Option<&Tools>, cache_dir: &Path, progress: &dyn Progress, cancel: &Cancel) -> Result<Groups>`.
  - `pub fn near_count(groups_index: &NearIndex, i: usize, c: &NearCond) -> usize` (time-sorted per source, binary search window).
  - `image_metrics::{sharpness(jpeg: &Path) -> Result<f32> /* 0..100 */, dhash(jpeg: &Path) -> Result<u64>}`; cached in `cache_dir/metrics.json` keyed by `thumbs::cache_name(path, size, mtime_ns)`.
- [ ] **Step 1: Tests:** events — capture times 10:00, 10:30, 16:00 with gap 4h → 2 events; with `km=30` and a 100 km jump between 10:00 and 10:30 → 3; file without capture date → in no event (event conditions miss). near — camera 12:00 at (47.02,28.83), partner 13:30 at 0.5 km → `time="2h", km=1` hit; partner at 5 km → miss; partner without GPS → hit, with `require_gps` → miss. duplicate — two byte-identical files in sources `camera` (listed first) and `partner` → the partner one is the duplicate, camera never matches; same content in an output dir of a folders target → `of=["outputs"]` matches the source file; a source file inside that output dir is not its own duplicate. series — three shots 2 s apart, synthetic JPEGs: one sharp checkerboard, two Gaussian-blurred → series of 3, best = sharp one, `sharpness(sharp) > sharpness(blurred) + 20`; a 4th shot 30 s later → not in the series; with `similar=true`, a totally different image 2 s later → not in the series (dHash Hamming > 10).
- [ ] **Step 2:** FAIL. **Step 3:** implement; duplicate hashing: group by size, then blake3 of full content (rayon). Outputs scan: prefix of each folders-target output `dir` template before the first `{`, `~`-expanded, walked recursively, only sizes matching some source size are hashed. dHash: 9×8 grayscale of the thumbnail. Sharpness: grayscale thumbnail, 3×3 Laplacian variance, `min(100, 100·ln(1+var)/ln(1+2000))`. **Step 4:** PASS.
- [ ] **Step 5: Commit** `feat(filter): events, neighbours, duplicates, series and sharpness`.

### Task 5: Ordered routing with AI pause/resume, expand, kinds and series review

**Files:**
- Modify: `crates/gallery-core/src/route.rs`, `crates/gallery-core/src/pipeline.rs` (build facts + groups between exif read and route), `crates/gallery-core/src/overrides.rs`, `crates/gallery-core/src/learn.rs` (categories from targets).
- Test: `crates/gallery-core/src/route.rs` tests, `crates/gallery-core/src/pipeline.rs` tests.

**Interfaces:**
- Consumes: Tasks 1–4.
- Produces:
  - `pub fn route_with_overrides(cfg, files, ctx: &RouteCtx, ai, accept_low, overrides, progress, cancel) -> Result<Routed>` where `pub struct RouteCtx<'a> { pub facts: &'a [Facts], pub exif: &'a HashMap<PathBuf, Value>, pub groups: &'a Groups, pub now: NaiveDateTime }`; `RouteCtx::empty(files)` for callers without metadata (tests).
  - `Decision::Target { name, via }` for all kinds (plan reads the kind); `Decision::Review { suggested, reason, series: Option<SeriesReview> }` with `pub struct SeriesReview { pub members: Vec<PathBuf>, pub best: PathBuf }`.
  - `overrides::Action::SeriesBest` — "this file is the confirmed best of its series": series conditions never hold for it.
  - `pub fn first_miss(cfg, target: &Target, i, ctx) -> Option<Miss>` for explain.
- Algorithm (not determined by signatures):
  1. Overrides first (as today; `SeriesBest` only marks).
  2. Each file has a cursor at target 0. Loop: advance each undecided file's cursor; a filter matches when all non-AI conditions hold and (if `ai`) the AI answer equals this target. A file whose cursor reaches a filter with `ai` (non-AI conditions hold) and has no answer yet is *paused*.
  3. When no file can advance: build one `AiRequest` per paused file with categories = every `(target.name, description)` of this and later targets whose filter has `ai` and whose non-AI conditions hold for the file (descriptions of several matching filters of one target joined with "; "); send the batch; resume. One AI round per file, at most.
  4. `expand = "event"`: before the walk reaches target T, seeds are the files still undecided at T whose filter's non-expand conditions hold; every undecided file of those seeds' events (event params from the filter's `event` table or defaults), in the filter's sources and passing `match`/`exclude`, matches T.
  5. `series` match → `Decision::Review` with `suggested = target.name` and the `SeriesReview` (whole series, best marked), unless overrides decided the file.
  6. `kind = keep` → `Decision::Target { via: Mask|Ai }` on a keep target (plan turns it into a kept item).
- [ ] **Step 1: Tests:** order — targets: 1 `match="*.png"` (folders), 2 `ai="screens"`, 3 `match="*.jpg"`, 4 `ai="docs"`. A png → target 1, no AI request. A heic → one request with categories `["2","4"]`; answer `"4"` → target 4; answer NONE → unmatched. A jpg → one request with categories `["2","4"]` (non-AI targets never prune the category list) → answer `"4"` → target 3 takes it (order), answer `"2"` → target 2. Mask target between AI targets is respected. `expand` — camera 10:00 near partner, camera 10:30 alone, same event → both to "Together"; camera file of another day → not. keep — keep target first with `geo "@home"`: home files decided `Kept`-kind, never reach later targets. series — 3 shots → 3 review items with the same `SeriesReview`; with `SeriesBest` on the best and `Target{trash}` overrides on the others → best walks to the next target, others to trash. Cancellation during AI → `Error::Cancelled`. Old-shape config gives identical decisions to the pre-change router on the existing route tests.
- [ ] **Step 2:** FAIL. **Step 3:** implement; pipeline: `facts_of` for every file, `groups::compute` with `needs(cfg)`, cache dir `crate::paths::cache_dir()`; `learn::render_examples` gets categories from the request. **Step 4:** `cargo test --workspace` PASS.
- [ ] **Step 5: Commit** `feat(route): ordered targets, AI pause/resume, expand, keep/trash, series review`.

### Task 6: Plan v2, apply/undo for trash and keep, last-copy guard

**Files:**
- Modify: `crates/gallery-core/src/plan.rs`, `apply.rs`, `undo.rs`, `estimate.rs`, `effect.rs`, `explain.rs`; `locales/*.json`.

**Interfaces:**
- Consumes: Task 5 decisions; `trash::move_to_trash(path, source_root, run_id)`.
- Produces:
  - `PLAN_VERSION = 2`; `Plan::load` accepts 1 and 2, rejects >2 with the existing `plan.newer_version` message.
  - `PlanItem.kind: TargetKind` (`#[serde(default)]`, folders); trash/keep items have `outputs: []`.
  - `Plan.kept: Vec<KeptItem { source: PathBuf, target: String }>` for keep-target files (not in `items`).
  - `fn guard_last_copies(plan: &mut Plan, groups: &Groups, files: &[FileEntry])` in `plan.rs`: for each duplicate group / series whose every member is a trash item, move the best (series) or original (duplicate) to `review` with reason `t!("plan.last_copy_kept")`.
- [ ] **Step 1: Tests:** trash item applies: file moved to trash, journal entry, report lists it, `undo` restores it byte-identical with mtime; keep item: apply touches nothing, report counts it as kept; version-1 plan still applies; version-3 plan refused with `plan.newer_version` and no file touched; guard: duplicate pair where both route to trash targets → one stays in review; series of 3 all to trash via overrides → best in review; estimate/effect count trash items as "to trash" and keep items as "kept"; pilot split keeps trash items in pilot proportionally like other targets.
- [ ] **Step 2:** FAIL. **Step 3:** implement; apply processes trash items after all placements of the run (same phase as trashing originals). **Step 4:** PASS.
- [ ] **Step 5: Commit** `feat(plan): plan v2 with trash and keep targets; never trash the last copy`.

### Task 7: CLI, explain, example config and docs

**Files:**
- Modify: `crates/gallery-cli/src/*` (check/explain/presets output), `examples/gallery.toml` (new shape, includes places, a duplicates trash target, the together example, a blurry-series trash target), `docs/configuration.md`, `docs/architecture.md`, `docs/cli.md`, `README.md`; `locales/*.json`.
- Test: `crates/gallery-cli/tests/*.rs`, `crates/gallery-core/src/explain/tests.rs`.

**Interfaces:**
- Consumes: `first_miss` (Task 5), `Config::was_staged` (Task 1).
- [ ] **Step 1: Tests:** `gallery check` on an old-shape config prints the one-line migration notice (`config.old_shape`) and exits 0; `gallery explain <file>` prints per target the first miss (e.g. `near: no partner file within 2h / 1 km`); `examples/gallery.toml` loads and `gallery check` is clean; config round-trip test (gui) still byte-identical for the new example.
- [ ] **Step 2:** FAIL. **Step 3:** implement + rewrite docs sections "Targets", "Filters" (one subsection per condition with the spec's examples), "Migration". **Step 4:** PASS.
- [ ] **Step 5: Commit** `feat(cli): explain misses, migration notice; docs for ordered targets and filters`.

### Task 8: GUI backend — config editing for the new model

**Files:**
- Modify: `crates/gallery-gui/src/config_edit/{model.rs,ops.rs,order.rs,checks.rs,doc.rs,file.rs,mod.rs,tests.rs}`, `crates/gallery-gui/src/settings.rs` (places), `crates/gallery-gui/src/lib.rs` (commands).

**Interfaces:**
- Produces (Tauri commands / `EditOp` variants, TS mirrors in `ui/src/api.ts`):
  - `EditOp::{AddTarget { kind }, MoveTarget { from, to }, SetTargetKind { name, kind }, SetCondition { target, filter, condition: ConditionValue }, RemoveCondition { target, filter, key }, SetPlace { name, place }, RemovePlace { name }}`; `ConditionValue` = tagged union of the Task 3–4 condition structs.
  - Saving an old-shape file: write `<config>.bak` (no-replace; numbered if exists), then the new shape; comments of untouched tables kept (toml_edit).
- [ ] **Step 1: Tests:** old-shape fixture → any edit → `.bak` equals the original bytes, saved file loads with `was_staged == false` and identical `targets` names/order; `MoveTarget` 3→0 reorders; `SetCondition near` writes `near = { sources = ["partner"], time = "2h", km = 1.0 }` inline; `RemoveCondition` leaves the filter valid; `SetPlace` writes `[places]`; checks: `expand`+`ai` reported, unknown `@place` reported, trash target with outputs reported.
- [ ] **Step 2:** FAIL. **Step 3:** implement. **Step 4:** `cargo test -p gallery-gui` PASS.
- [ ] **Step 5: Commit** `feat(gui): edit ordered targets, kinds, conditions and places`.

### Task 9: UI — target list, condition editor, places

**Files:**
- Modify: `ui/src/screens/TargetsScreen.tsx`, `ui/src/editor/model.ts`, `ui/src/editor/fields.tsx`, `ui/src/screens/SettingsScreen.tsx`, `ui/src/api.ts`, `ui/src/i18n/locales/*.json`.
- Create: `ui/src/editor/conditions/{ConditionMenu.tsx, DateCondition.tsx, GeoCondition.tsx, EventCondition.tsx, NearCondition.tsx, DeviceCondition.tsx, PropsCondition.tsx, DuplicateCondition.tsx, SeriesCondition.tsx, PathCondition.tsx, AiCondition.tsx}`, tests next to them.

**Interfaces:**
- Consumes: Task 8 commands.
- [ ] **Step 1: Tests (vitest + Testing Library):** one target list without stage headers, kind badge text (`Trash`, `Keep`) and `AI` badge; drag/up/down calls `MoveTarget`; "+ condition" menu lists the 10 conditions and adds one; each condition form edits its sub-options and the `not` toggle; `expand` checkbox disabled with a hint when the filter has AI; Settings → Places add/edit/remove, paste `"47.0245, 28.8323"` fills lat/lon; ru locale renders translated labels.
- [ ] **Step 2:** FAIL. **Step 3:** implement in the existing design language (reuse `fields.tsx` inputs). **Step 4:** `pnpm -C ui typecheck lint test` PASS.
- [ ] **Step 5: Commit** `feat(ui): ordered targets, condition editor, places`.

### Task 10: UI — series and event review

**Files:**
- Modify: `ui/src/flow/DecideStep.tsx`, `ui/src/plan/ExplainPanel.tsx`, `crates/gallery-gui/src/overrides.rs`, `crates/gallery-gui/src/explain.rs`, locales.

**Interfaces:**
- Consumes: `ReviewItem.series` (Task 5/6), `Action::SeriesBest`, `first_miss`.
- Produces: GUI command `confirm_series { members, best, target }` → overrides: `SeriesBest` for best, `Target{target}` for others; `decide_event { members, action }` applies one action to all files of an event.
- [ ] **Step 1: Tests:** series review card shows all members side by side (thumbnails), suggested best marked; clicking another member then "Confirm" writes `SeriesBest` for it and `Target` for the rest; "Group by event" toggle groups review items; one event-level decision writes an override per member; explain panel shows the first miss text.
- [ ] **Step 2:** FAIL. **Step 3:** implement. **Step 4:** `pnpm -C ui test` and `cargo test -p gallery-gui` PASS.
- [ ] **Step 5: Commit** `feat(ui): review series side by side; decide whole events`.

### Task 11: Whole-branch verification

- [ ] **Step 1:** `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace` (temp XDG dirs) → all pass.
- [ ] **Step 2:** `pnpm -C ui typecheck && pnpm -C ui lint && pnpm -C ui test` → all pass.
- [ ] **Step 3:** Load a copy of the user-shaped example (old shape, 4 sources) with `gallery scan --no-ai -o <plan>` on a fixture library → same decisions as `main` for every file (script compares plan items by source → target).
- [ ] **Step 4:** Push `feat/ordered-targets`, open a PR, CI green on all three OS.
