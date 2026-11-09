# Ordered targets and rich filters — design

Status: approved by the user (2026-11-09).

## Intent

Today targets live in three fixed stages (`before_ai` masks → one AI batch →
`after_ai` masks). The user wants one plain, ordered list of targets, where
each target's filter is built from general conditions and the list order alone
decides which target takes a file. The AI becomes just one more condition.

The concrete use case behind it: two people (two phone sources, e.g. `camera`
and `partner`) shoot a lot together. Photos taken together (trips, walks — also
frames where neither person is visible) must go to a shared place, while each
person's private photos stay with their owner. The user explicitly rejected a
special "shared"/"together" concept: the case must be expressible with
general filters (events, neighbours across sources, places, dates, …), which
are also useful on their own.

## Goals

- One ordered list `[[targets]]`; the user sorts targets by hand (GUI
  drag/up/down). No stages.
- Strict first-match semantics in list order, for every kind of condition,
  including AI.
- Input list is frozen at the scan: one scan, then each file walks the target
  list top-down; the first target whose filter matches takes it, and it is not
  seen by later targets (it is also not re-scanned from its new place in the
  same run). Unmatched files stay where they are. (This is already how routing
  works; the design keeps it.)
- New target kinds: **Trash** (the file goes to the trash — like any other
  target, selectable by the user) and **Keep** (takes the file and does
  nothing: protects it from the targets below).
- New filter conditions: date, places/geo, event, neighbours (`near`), device,
  file/media properties (incl. codecs, metadata presence, file size),
  duplicates, path.
- Automatic migration of existing configs.

## Non-goals

- No "shared folder"/"together"/people concept, no face recognition.
- No folder watching (still manual runs).
- No reverse geocoding (country/city names); places are user-defined points.

## Anti-goals (failure even if it "works")

- Any condition that silently behaves differently depending on its position
  in the list (except the documented AI batching, which must give the same
  result as strict order — see below).
- A file processed by two targets in one run.
- Slow re-runs: metadata needed by conditions must be cached; a 5 000-file
  library must not re-read EXIF on every preview.
- Breaking the safety model (verify-before-place-before-trash, journal, undo,
  pilot, plan identity) or the mtime preservation.

## Config model

```toml
# Named points for geo conditions (also editable in Settings → Places).
[places]
home = { lat = 47.0245, lon = 28.8323, km = 2 }
dacha = { lat = 46.98, lon = 28.70, km = 1 }

[[targets]]
name = "Duplicates"
kind = "trash"                     # "folders" (default) | "trash" | "keep"
[[targets.filters]]
sources = ["camera", "partner"]
duplicate = { of = ["sources", "outputs"] }

[[targets]]
name = "Together (mine)"
[[targets.filters]]
sources = ["camera"]
match = "@media"
near = { sources = ["partner"], time = "2h", km = 1 }
expand = "event"
[[targets.outputs]]
dir = "~/Sync/Shared/{YYYY}"
rename = ["capture-date"]

[[targets]]
name = "Documents"
[[targets.filters]]
sources = ["camera"]
match = "@photo"
ai = "Photos of documents: passports, contracts, forms, printed pages."
[[targets.outputs]]
dir = "~/Documents/Scans"

[[targets]]
name = "Night shots at home"
kind = "keep"
[[targets.filters]]
sources = ["camera"]
geo = "@home"
date = { time = "22:00..05:00" }
```

- A target has `name`, optional `kind`, `filters` (OR), and `outputs`
  (only for `kind = "folders"`; `trash`/`keep` targets have none).
- A filter = `sources` + conditions; **all conditions of a filter must hold
  (AND)**. Every condition is optional; a filter with only `sources` matches
  every file of those sources. `match`/`exclude` keep today's mask syntax
  (`@name`, `!`, lists = OR).
- Negation: every condition table accepts `not = true` (e.g. "not near
  partner"); `geo` also accepts `"!@home"`.

### Conditions

Each sub-option is independent and set per filter; a condition holds when all
its given sub-options hold.

**date** — which date and which constraints:
```toml
date = { source = "capture",            # capture (default) | file_mtime | name
         range = "2026-08-12..2026-08-18",  # open ends allowed: "2026-01-01.."
         older_than = "30d", newer_than = "1y",
         months = [6, 7, 8], weekdays = ["sat", "sun"],
         time = "22:00..05:00" }        # may wrap midnight
```
`capture` uses the same capture-date resolution as the name presets (EXIF /
QuickTime / filename / mtime fallback, plan time zone). A file without the
chosen date never matches a date condition (unless `not = true`).

**geo** — `"@place"`, `"!@place"`, a list (OR), or inline
`{ lat, lon, km }`; `has_gps = true|false`. Places are defined in `[places]`
(Settings UI edits them). A file without GPS does not match `geo` (but matches
`has_gps = false`).

**event** — events are computed per source set over the frozen input list:
files sorted by capture time; a gap longer than `gap` (default `"4h"`) starts a
new event. Optional `km` splits also on a jump in place between consecutive
GPS-tagged files. Conditions on the event a file belongs to:
```toml
event = { sources = ["camera", "partner"],  # which files form events (default: the filter's sources)
          gap = "4h", km = 30,
          min_files = 10, min_duration = "1d", max_duration = "30d",
          away_from = "@home" }           # at least one file of the event is outside the place
```
**expand = "event"** (filter-level flag): if the filter's other conditions
hold for at least one file of an event, the filter matches every file of that
event that is in the filter's `sources` (still subject to `match`/`exclude`,
and still only files not taken by an earlier target). Event parameters for
`expand` come from the filter's `event` table if present, else defaults.

**near** — the file has at least one "neighbour" among files of the given
sources in the frozen input list (not only unmatched ones — the whole scan):
```toml
near = { sources = ["partner"], time = "2h", km = 1, min = 1 }
```
`km` is checked only when both files have GPS; if either lacks GPS, time
alone decides (`require_gps = true` makes such pairs not count).

**device** — EXIF/QuickTime make, model, software; each a glob or list,
case-insensitive: `device = { make = "Google", model = "Pixel*",
software = "!*Snapseed*" }`. Missing tag → no match.

**props** — media/file properties; numeric values accept ranges
`"1MB..20MB"`, `"..1080"`, `">=60"`:
```toml
props = { size = "..500KB", width = ">=3000", height = "..1080",
          megapixels = ">=12", orientation = "portrait",   # portrait|landscape|square
          aspect = "16:9", duration = "..10s", fps = ">=50",
          video_codec = ["hevc", "h264"], audio_codec = "aac",
          has_audio = false, container = "mp4",
          sharpness = "<40",                              # blur score, 0..100
          has_tag = ["GPSLatitude"], missing_tag = ["DateTimeOriginal"] }
```
`has_tag`/`missing_tag` take exiftool tag names (group optional).
`sharpness` is computed in Rust on a downscaled decode (variance of the
Laplacian, normalised to 0..100), cached like metadata; photos only (for
video: not supported in this version).

**duplicate** — byte-identical content (size, then a hash):
```toml
duplicate = { of = ["sources", "outputs"] }
```
- `sources`: another file in the scan has the same content; the copy
  considered *original* (not a duplicate) is the first by: source order in
  `[[sources]]`, then oldest capture date, then path. So exactly one copy of a
  group never matches.
- `outputs`: the same content already exists in an output folder of any
  `folders` target (path-template prefix before the first `{`; scanned
  recursively, hashed lazily, cached).

**series** — near-identical shots taken seconds apart (burst, re-takes):
files of one source sorted by capture time; consecutive files closer than
`gap` (default `"5s"`) whose thumbnails are visually similar (perceptual
hash, when `similar = true`, default) form a series. `keep = "best"` makes
the condition hold for every file of a series except the best one by `by`
(`sharpness` default; also `size`, `resolution`); single files (no series)
never match. Typical use: a `kind = "trash"` target for the blurry re-takes;
the best shot walks on to the targets below.
```toml
series = { gap = "5s", similar = true, keep = "best", by = "sharpness" }
```
Decided (user): files matched through `series` never go straight to the
target. They go to review, where the GUI shows the whole series side by side
with the suggested best shot marked; the user confirms or picks another best
shot, and only then do the others go to the matching target (e.g. trash). The
confirmation is a manual decision (override) per series, so later runs don't
ask again.

**path** — glob(s) on the path relative to the source root
(`path = "WhatsApp/**"`, lists = OR, `!` excludes), unlike `match`, which sees
only the file name.

**ai** — a category description. See below.

## Routing

1. Scan all sources once → frozen input list.
2. Pre-compute what the configured conditions need (lazily — only if some
   filter uses them): metadata (one batched exiftool read, cached by
   path+size+mtime in the cache dir), events, neighbours, duplicate groups.
   All of these are computed on the full frozen list, independent of routing.
3. Overrides (manual decisions) apply first, as today.
4. Walk targets in order; for each still-untaken file, a target takes it when
   any of its filters matches.
5. **AI, strict order with one request:** when a file reaches its first target
   whose matching filter has an `ai` condition (all other conditions of that
   filter hold), one request is sent with every AI category the file could
   still reach — i.e. AI filters of this and later targets whose non-AI
   conditions hold for the file. The model picks the best category or NONE.
   Then the walk continues in order: an AI condition holds only for the
   chosen category. (A non-AI target between two AI targets still gets the
   file first if it matches — order is respected.) Implementation: route all
   files up to their first AI need, send the AI batch (cache, examples,
   prefix/suffix per source as today), then resume each file's walk.
   Low confidence → review, as today.
6. `kind = "keep"` → decision Kept (file untouched, not unmatched).
   `kind = "trash"` → the file goes to the trash in the apply phase, after all
   other placements, with the same journal/undo/verify-skip rules as originals
   are trashed today (no outputs to verify, so it is trashed directly; undo
   restores it).

## Migration

`Config::load` accepts the old shape: `targets.before_ai`, `targets.ai`,
`targets.after_ai` are concatenated in that order into `[[targets]]`; an AI
target becomes a target with one filter `{ sources, match/exclude from
prefilter, ai = description }`. The GUI saves in the new shape (one-time
rewrite, keeping comments where toml_edit allows; a `.bak` copy first). The
CLI warns once that the file uses the old shape. Plans/overrides/reports refer
to targets by name, so they stay valid.

## GUI

- Targets screen: one list with drag handle and up/down; kind badge
  (folders / trash / keep); AI targets get an "AI" badge.
- Filter editor: `sources`, `match`/`exclude`, then "+ condition" menu
  (Date, Place, Event, Near, Device, Properties, Duplicate, Path, AI); each
  condition is a small form with its sub-options, each removable; a `not`
  toggle per condition; `expand to event` checkbox.
- Settings → Places: list of named points (name, lat/lon, radius); paste
  "lat, lon" or pick from a photo's GPS.
- Explain panel: shows per target which condition failed for a file
  ("near: no partner file within 2h/1 km").
- Review/Decide: grouping by event is available; a manual decision can be
  applied to a whole event.
- All new strings in 11 locales (UI + Rust catalogs).

## Constraints

- Linux/macOS/Windows; CI must stay green.
- Existing safety model, plan identity, pilot, verification, mtime handling,
  server plan + `gallery apply` unchanged.
- Server plans: conditions are evaluated where the plan is made (desktop);
  the plan stores decisions, so `gallery apply` on the server needs nothing
  new. Exception: `trash` targets move to trash on the server like originals.
- Tests must not touch the user's real dirs, media, Syncthing or the API.

## Decisions settled in review (designer's calls, marked for the user)

- **Metadata source** (answered): exiftool is already a required tool
  (`crates/gallery-core/src/exif.rs`, batched JSON reads); conditions reuse it.
  ffprobe/ffmpeg data (codecs, fps, duration) comes from the existing video
  probing. No new external dependency.
- **Plan format** (call): `Trash` and `Kept` become plan decision kinds; the
  plan schema version is bumped. An older `gallery apply` refuses a newer plan
  with a clear "update gallery on the server" message. Trash targets work on
  any source; a trashed file has no outputs, so it is trashed directly (it is
  journaled and restorable by undo like originals today).
- **Never lose the last copy** (call, safety): after routing, a plan check
  keeps one file of every duplicate group and every series out of the trash —
  if all copies/shots would be trashed, the best one is downgraded to review.
- **`expand` seeding** (call): the seed must be a file still untaken when the
  walk reaches this target (order is respected). `expand` and `ai` in the same
  filter is a config error in this version.
- **`duplicate` with `outputs`** (call): a file never counts as its own
  duplicate (same path, or an output dir that overlaps the source is skipped
  for that file); only files that exist at plan time count, not files placed
  in this run; `trash`/`keep` targets have no outputs to compare against.
- **`not`** (call): negates the whole condition table (an AND of its
  sub-options). A condition with missing data (no date, no GPS, no tag) is
  false; with `not = true` it is true. The explain panel shows the reason.
- **AI categories** (call): the category id is the target name. If several
  filters of one target have `ai`, the description sent is the one of the
  filter(s) whose other conditions hold, joined with "; ". Cache and learned
  examples stay keyed as today (prompt hash without examples; examples per
  source with the target name), so migration keeps them valid.
- **Per-file category sets** (call): requests are grouped by identical
  category set; the cache key includes the set (it already includes the
  prompt). Same cost as today when no non-AI conditions differ.

## Left to the builder

- Internal representation of conditions, metadata cache format, hashing
  algorithm (fast non-crypto for grouping + full compare before declaring
  duplicates is fine).
- Sharpness normalisation and perceptual-hash choice/threshold for `series`.
- Exact exiftool tag set read for props/device/geo/date.
- Event clustering details beyond the stated rules.
- GUI layout details within the existing design language.
- Splitting the work into reviewable commits/PRs.
