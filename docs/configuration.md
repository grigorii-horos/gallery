# Configuration reference

Gallery is configured by one TOML file. The GUI reads `~/.config/gallery/gallery.toml` by default
(use **Open config…** to pick another one); the CLI takes the path with `-c/--config`. The GUI's
Sources, Targets and Presets screens edit this same file and keep your comments and layout; you can
also edit it by hand and press **Reload**.

A complete, commented example lives in [`examples/gallery.toml`](../examples/gallery.toml).

Rules that hold everywhere:

- Unknown keys are errors (typos do not silently do nothing).
- `~` at the start of a path is your home folder.
- Relative paths in `ai_prefix_file` / `ai_suffix_file` are relative to the config file.
- `gallery scan` (and the GUI) validate the whole file first: unknown sources or presets, duplicate
  target names, empty AI descriptions, bad masks and unsafe name templates are reported with the
  target they belong to.

## Contents

- [How a file is routed](#how-a-file-is-routed)
- [`[[sources]]`](#sources)
- [`[places]`](#places)
- [Targets](#targets): [kinds](#target-kinds-folders-trash-keep), [filters](#filters), [outputs](#outputs)
- [Filter conditions](#filter-conditions): [`match` / `exclude`](#match-and-exclude), [`path`](#path), [`date`](#date), [`geo`](#geo), [`event` and `expand`](#event-and-expand), [`near`](#near), [`device`](#device), [`props`](#props), [`duplicate`](#duplicate), [`series`](#series), [`ai`](#ai-condition), [`not`](#negation-not)
- [Trash and keep targets](#trash-and-keep-targets)
- [Series review](#series-review)
- [Masks](#masks) (syntax, `@refs`, `!` exclusions, built-in masks)
- [Content presets](#content-presets-the-content-pipe) (built-ins, your own, bundles)
- [Name presets](#name-presets-the-rename-pipe)
- [`[ai]`](#ai) ([keywords and description](#ai-keywords-and-description))
- [Migration from the three stages](#migration-from-the-three-stages)
- [Where related files live](#related-files)

## How a file is routed

Targets are **one ordered list**, `[[targets]]`. You sort it by hand (the GUI has drag handles and
up/down buttons); the order alone decides which target takes a file.

1. The sources are scanned once. That list of files is frozen for the run: a file placed by this
   run is not seen again from its new place.
2. Your manual decisions (made in the GUI review/Check screens) apply first, per file.
3. Every other file walks the list from the top. The first target that has a filter matching the
   file takes it, and later targets never see it. **A file ends up in exactly one target.**
4. A file that no target takes is *unmatched*: Gallery leaves it alone and lists it (see
   `gallery unmatched` and the triage view in the GUI). A file that a filter would take but for an
   `exclude` mask is left alone on purpose and is not reported as unmatched.

Conditions that need more than the file itself (events, neighbours, duplicates, series) are
computed on the whole frozen file list, before routing, so they do not depend on the order of the
targets. The AI is just one more condition, `ai`; see [`ai`](#ai-condition). The order is strict for
it too: a file reaches the AI only when the walk reaches a filter with `ai` whose other conditions
hold for it.

## `[[sources]]`

Folders that are scanned.

```toml
[[sources]]
id = "camera"                       # required, unique; targets refer to it
path = "~/Sync/Camera"              # required
server_path = "/srv/photos/camera"  # optional: the same folder as the server sees it
recursive = false                   # default false: scan subfolders too
skip = [".stfolder", ".stversions", ".gallery-trash", ".gallery-tmp-*"]   # default shown
ai_prefix = "These are phone shots: family, trips, paperwork."
ai_suffix_file = "ai/camera-suffix.md"
```

| Key | Default | Meaning |
|---|---|---|
| `id` | – | Short name used by `sources = [...]` in targets. |
| `path` | – | Folder to scan. |
| `server_path` | none | Used by `gallery scan --server` (and the GUI's *export plan*): the plan is written with this path instead of `path`. Required for every source that has files in a server plan; otherwise building the plan fails and lists the missing paths. |
| `recursive` | `false` | Scan subfolders as well. |
| `skip` | `.stfolder`, `.stversions`, `.gallery-trash`, `.gallery-tmp-*` | Globs of entries to ignore. Setting it replaces the default list. |
| `ai_prefix` / `ai_prefix_file` | none | Text placed in the AI system prompt **before** the category list for this source's files. Inline text or a file, not both. |
| `ai_suffix` / `ai_suffix_file` | none | Text placed **after** the category list (before the answer format). |

Changing a prefix or suffix changes the prompt, so the AI is asked again for that source's files
(answers are cached per prompt). See [AI setup in the README](../README.md#ai-setup).

## `[places]`

Named points for the [`geo`](#geo) and [`event`](#event-and-expand) conditions. There is no reverse
geocoding (no country or city names): a place is a point and a radius that you define. The GUI edits
them under Settings → Places.

```toml
[places]
home  = { lat = 47.0245, lon = 28.8323, km = 2 }
dacha = { lat = 46.98,   lon = 28.70,   km = 1 }
```

| Key | Meaning |
|---|---|
| `lat`, `lon` | Decimal degrees. |
| `km` | Radius in kilometres. A file is "in" the place when its GPS point is at most `km` from the centre. |

A place is used as `"@home"` (the name after `@`). Using a place that is not defined is an error.

## Targets

A *target* is "a kind of file" plus what happens to it. Target names must be unique. The list order
is the routing order (see above).

```toml
[[targets]]
name = "Documents"                    # required, unique
kind = "folders"                      # "folders" (default) | "trash" | "keep"

[[targets.filters]]                   # a target takes a file when ANY filter matches
sources = ["camera"]                  # source ids this filter reads
match = "@photo"                      # file-name mask
ai = "Photos of documents: passports, contracts, forms, printed pages."

[[targets.outputs]]                   # only for kind = "folders"
dir = "~/Documents/Scans"
rename = ["capture-date"]
```

### Target kinds: folders, trash, keep

| `kind` | What happens to a file the target takes | `outputs` |
|---|---|---|
| `folders` (default) | The outputs are written and verified, then the source goes to the trash (or is moved, see [Outputs](#outputs)). | any number |
| `trash` | The source goes to the trash, nothing is written. See [Trash and keep targets](#trash-and-keep-targets). | none (an error if given) |
| `keep` | The file stays exactly where it is, and no target below sees it. | none (an error if given) |

### Filters

A target has any number of `filters` (it takes a file if **any** filter matches). A filter is
`sources` plus conditions; **every condition of a filter must hold (AND)**. All conditions are
optional: a filter with only `sources` takes every file of those sources.

| Filter key | Meaning |
|---|---|
| `sources` | List of source ids. A filter with no sources matches nothing. |
| `match` | [Mask](#masks) on the file **name**. Omitted: any file. |
| `exclude` | Mask; a name matching it is not taken by this filter. |
| `path` | Glob on the path inside the source. See [`path`](#path). |
| `date`, `geo`, `event`, `near`, `device`, `props`, `duplicate`, `series` | Conditions, one subsection each below. |
| `ai` | The AI condition: a category description. |
| `expand` | `"event"`: take the whole event of a matching file. See [`event` and `expand`](#event-and-expand). |

Each condition holds when **all** of its given sub-options hold. A condition with missing data (a
photo without a capture date, without GPS, without a tag) is **false**, never an error; the explain
panel says why ("no capture date", "no GPS").

### Outputs

Every target has `[[...outputs]]`. An output is one destination for the file:

```toml
[[targets.outputs]]
dir        = "~/Media/Timeline/{YYYY}/{MM}"
server_dir = "/srv/media/Timeline/{YYYY}/{MM}"   # optional; used for server plans
content    = ["meta-full", "compress-all"]
rename     = ["capture-date"]
```

| Key | Meaning |
|---|---|
| `dir` | Destination folder template. `{YYYY}`, `{MM}`, `{DD}` come from the file's capture date (`unknown` if it has none); `{source}` is the source id. |
| `server_dir` | The same folder as the server sees it; used when the plan is built for the server (`scan --server`, GUI *export plan*). Required for every output in a server plan (it takes the same `{YYYY}`-style tokens; a leading `~` stays literal and means the server's home). |
| `content` | The **content pipe**: a list of steps run in order on the file (see [presets](#content-presets-the-content-pipe)). Empty means a byte-for-byte copy. |
| `rename` | The **name pipe**: a list of name presets applied to the output's file name and folders. Empty means the name is kept. |

A step is either a bare preset name or a table that overrides the preset's mask:

```toml
content = ["meta-full", { mask = "*.png", preset = "avif-high" }]
```

Each step applies only to files whose *current* name matches its mask (the preset's own mask if you
write the bare name), so a pipe like `["meta-full", "compress-all"]` can do the right thing for
photos, PNGs and videos in one list.

Safety-relevant behaviour of outputs (details in [architecture](architecture.md#safety-guarantees)):

- Outputs are **never overwritten**. A name that is taken gets a free variant (`name_1.ext`, `name_2.ext`, …).
- The source goes to the trash only after **all** outputs of its target are written and verified.
- **In place:** if an output's folder is the source's own folder (or inside its source root) and the
  content pipe is empty, the source is not copied or trashed: it is *renamed or moved* there
  (a file that already has the right name is left out of the plan as "unchanged"). This is how the
  "Screenshots" example in `examples/gallery.toml` sorts files into `year/month` folders without duplicating them.
- An output with no `content` and no `rename` is a backup copy (byte for byte).

## Filter conditions

All conditions below go directly into a `[[targets.filters]]` table, next to `sources` and `match`.
Conditions that look at neighbouring files (`event`, `near`, `duplicate`, `series`) look at **every
file of the scan**, not only at files still unmatched when the walk reaches the target.

### `match` and `exclude`

The mask syntax is the same as everywhere else ([Masks](#masks)): `@name` refers to a named mask,
a list means OR, an item starting with `!` excludes.

```toml
match   = ["@photo", "@video"]
exclude = "*-edited*"
```

`match` and `exclude` see only the **file name**; use [`path`](#path) for folders.

### `path`

A glob (or a list, OR) on the path **relative to the source root**. A `!` item excludes.

```toml
path = "WhatsApp/**"                       # everything below the WhatsApp folder
path = ["Trips/**", "!Trips/**/raw/**"]    # trips, but not their raw folders
```

### `date`

Which date to look at and which constraints it has to meet.

```toml
date = { source = "capture",                     # capture (default) | file_mtime | name
         range = "2026-08-12..2026-08-18",       # open ends allowed: "2026-01-01.."
         older_than = "30d", newer_than = "1y",
         months = [6, 7, 8], weekdays = ["sat", "sun"],
         time = "22:00..05:00" }                 # a time of day; may wrap midnight
```

- `capture` is the same capture-date resolution as the name presets (EXIF, QuickTime, the file name,
  the file time as a last resort), in the plan's time zone. `name` is the date written in the file
  name; `file_mtime` is the modification time.
- A file without the chosen date never matches a `date` condition.
- `older_than` / `newer_than` are ages measured from now. Durations everywhere (`gap`, `time`, `min_duration`, ...) are a whole number and a unit: `s`, `m` (minutes), `h`, `d`, `w`, `y` (365 days).
- `time` is a time-of-day window and may wrap midnight (`22:00..05:00`).

### `geo`

```toml
geo = "@home"                                  # inside the place
geo = "!@home"                                 # outside the place (the file must have GPS)
geo = ["@home", "@dacha"]                      # inside any of them (OR)
geo = { lat = 46.5, lon = 30.7, km = 5 }       # an inline circle
geo = { has_gps = false }                      # files without GPS
```

Places come from [`[places]`](#places). A file without GPS does not match `geo` (not even `"!@home"`),
but it matches `has_gps = false`. The table form also takes `not = true`; for the string and list
forms write `"!@place"`.

### `event` and `expand`

Events group the files of some sources by time: files sorted by capture time, and a gap longer
than `gap` starts a new event. Optionally a jump in place (`km`) between two consecutive GPS-tagged
files splits an event too. `event` is a condition on the event a file belongs to:

```toml
event = { sources = ["camera", "partner"],    # which files form events (default: the filter's sources)
          gap = "4h", km = 30,             # split on a pause over 4 h or a jump over 30 km
          min_files = 10,                  # the event has at least 10 files
          min_duration = "1d", max_duration = "30d",
          away_from = "@home" }            # at least one file of the event is outside the place
```

Events are computed on the whole frozen file list, per set of event parameters. A file without a
capture date belongs to no event. Capture times are compared as plan-zone wall-clock times, the
same as in [`near`](#near).

`expand = "event"` is a filter-level flag. If the filter's **other** conditions hold for at least
one file of an event, the filter matches **every file of that event** that is in its `sources`
(still subject to `match` / `exclude`, and only files that no earlier target took). The event's
parameters come from the filter's `event` table if there is one, else the defaults (`gap = "4h"`,
the filter's sources).

```toml
# Everything shot on a trip: some file of the event is far from home, so all of it goes here.
[[targets]]
name = "Trips"
[[targets.filters]]
sources = ["camera"]
match = "@media"
event = { gap = "12h", away_from = "@home" }
expand = "event"
[[targets.outputs]]
dir = "~/Media/Trips/{YYYY}"
```

The seed must be a file that is still untaken when the walk reaches this target, so the order is
respected. `expand` and `ai` in one filter is a config error.

### `near`

The file has at least `min` neighbours among the files of the given sources, in the whole scan:

```toml
near = { sources = ["partner"], time = "2h", km = 1, min = 1 }
```

| Key | Meaning |
|---|---|
| `sources` | Required: whose files count as neighbours. |
| `time` | Largest time difference (default `"2h"`). |
| `km` | Largest distance. Checked only when **both** files have GPS; if either lacks it, the time alone decides. |
| `min` | Neighbours needed (default 1). |
| `require_gps` | `true`: a pair where either file has no GPS does not count. |

Capture times are compared as naive local wall-clock times in the plan's time zone, the same as
the name presets: a photo's time is the EXIF `DateTimeOriginal` as the phone wrote it; QuickTime
dates (stored in UTC) and Unix-time file names are converted to the plan's zone first.
`OffsetTimeOriginal` is not used, so two phones set to different zones differ by whole hours (the
difference between their clocks), not by the real moment. Explain shows a miss as `near: 0 files of partner within 2h / 1.0 km, at least 1 needed`.

**Photos taken together** (two phones, `camera` and `partner`): both people's photos go to a shared
folder, frames with neither person in them included, and the private ones stay with their owner.

```toml
[[targets]]
name = "Together"

[[targets.filters]]
sources = ["camera"]
match = "@media"
near = { sources = ["partner"], time = "2h", km = 1 }
expand = "event"
event = { sources = ["camera", "partner"], gap = "4h" }

[[targets.filters]]
sources = ["partner"]
match = "@media"
near = { sources = ["camera"], time = "2h", km = 1 }
expand = "event"
event = { sources = ["camera", "partner"], gap = "4h" }

[[targets.outputs]]
dir = "~/Sync/Shared/{YYYY}"
rename = ["capture-date"]
```

There is no special "together" concept: it is `near` for the seed and `expand` for the rest of the
event. Put the target above the ones that take each person's photos.

### `device`

EXIF / QuickTime make, model and software. Each is a glob or a list, case-insensitive, with the
usual `!` exclusion:

```toml
device = { make = "Google", model = "Pixel*", software = "!*Snapseed*" }
```

A missing tag means no match.

### `props`

Media and file properties. Numeric values take ranges: `"1MB..20MB"`, `"..1080"`, `">=60"`.

```toml
props = { size = "..500KB", width = ">=3000", height = "..1080",
          megapixels = ">=12", orientation = "portrait",   # portrait | landscape | square
          aspect = "16:9", duration = "..10s", fps = ">=50",
          video_codec = ["hevc", "h264"], audio_codec = "aac",
          has_audio = false, container = "mp4",
          sharpness = "<40",                              # blur score, 0..100
          has_tag = ["GPSLatitude"], missing_tag = ["DateTimeOriginal"] }
```

- Dimensions follow the EXIF orientation (a portrait shot stored sideways is still portrait).
- Codecs, `fps`, `duration` and `container` come from the video probe; the rest from exiftool.
- `has_tag` / `missing_tag` take exiftool tag names, with an optional group (`EXIF:Make`).
- `sharpness` is a blur score from 0 (blurry) to 100, computed in Rust on a downscaled decode
  (variance of the Laplacian) and cached. Photos only; a video has no score, so the condition is false.

### `duplicate`

Byte-identical content (compared by size, then a hash):

```toml
duplicate = { of = ["sources", "outputs"] }
```

- `sources`: another file in the scan has the same content. The copy treated as the *original* (so
  it is not a duplicate) is the first by source order in `[[sources]]`, then the oldest capture
  date, then the path. Exactly one copy of a group never matches.
- `outputs`: the same content already exists in an output folder of any `folders` target (the part
  of `dir` before the first `{`, scanned recursively, hashed lazily and cached). A file is never its
  own duplicate. Only files that exist at plan time count, not files this run is about to place.

A scan never trashes the last copy of a group, see [Trash and keep targets](#trash-and-keep-targets).

### `series`

Near-identical shots taken seconds apart (a burst, re-takes). Files of one source are sorted by
capture time; consecutive files closer than `gap` whose thumbnails look alike (a perceptual hash,
`similar = true`) form a series.

```toml
series = { gap = "5s", similar = true, keep = "best", by = "sharpness" }
```

| Key | Default | Meaning |
|---|---|---|
| `gap` | `"5s"` | Consecutive shots closer than this may belong to one series. |
| `similar` | `true` | The shots must also look alike. |
| `keep` | `"best"` | The condition holds for every shot of a series **except** the best one. |
| `by` | `"sharpness"` | How the best shot is chosen: `sharpness`, `size` or `resolution`. |

A single file (no series) never matches. All `series` conditions of a config must use the same
`gap`, `similar` and `by`. The typical use is a `trash` target for the blurry re-takes; the best
shot walks on to the targets below. Matches never go straight to the target: they are held for
[series review](#series-review).

<a id="ai-condition"></a>

### `ai`

A category description for the AI, like the old AI targets: the model sees a 512 px preview of a
photo, or three 400 px frames of a video, never the original and never the file name.

```toml
[[targets]]
name = "Receipts"
[[targets.filters]]
sources = ["camera"]
match = "@photo"                              # keeps non-photos from being sent at all
ai = "Photos of receipts, invoices and price tags."
[[targets.outputs]]
dir = "~/Documents/Receipts"
```

- The category the model answers with is the **target name**. If several filters of one target have
  `ai`, the description sent is the one of the filters whose other conditions hold, joined with `; `.
- **Strict order, one request.** When a file reaches the first target with a matching `ai` filter,
  one request is sent with every AI category the file could still reach (the AI filters of this and
  later targets whose other conditions hold for it). The model picks the best category or "none".
  The walk then goes on in order: an `ai` condition holds only for the chosen category, and a non-AI
  target between two AI targets still gets the file first if it matches.
- An unsure answer goes to the review list instead of being applied (unless you pass `--accept-low`).
  Cached answers and your learned corrections stay valid.
- `description` must not be empty. `ai` and `expand` in one filter is an error.

<a id="negation-not"></a>

### Negation: `not`

Every condition **table** accepts `not = true`, which negates the whole table (an AND of its
sub-options): `near = { sources = ["partner"], time = "2h", not = true }` means "no partner file within two
hours". A condition with missing data is false, so with `not = true` it is true. `geo` also accepts
`"!@home"`.

## Trash and keep targets

A `kind = "trash"` target moves the files it takes to the trash, with no outputs. It is an
ordinary target in the list: it takes a file when the walk reaches it and a filter matches.

- The move happens in the apply phase after all placements, with the same journal, verification
  skip and undo rules as an original that is trashed after its outputs are placed: a trashed file
  has no outputs to verify, so it is trashed directly, and **undo restores it**.
- `GALLERY_TRASH=folder` applies as everywhere else.
- **Never lose the last copy.** After routing, a plan check keeps one file of every group of
  identical files (see [`duplicate`](#duplicate), also with `of = ["outputs"]` only) and one shot of
  every [series](#series) out of the trash. If every copy or shot would be trashed (for example the
  original is taken by an earlier `trash` target, or both copies match trash targets), the original
  or best one is downgraded to review ("every other copy goes to the trash too: this one is kept for
  review") and nothing is lost.
- A plan with a trash or keep item has plan format version 2; see
  [architecture](architecture.md#plan-versions).

A `kind = "keep"` target takes the file and does nothing: the file stays where it is and is not
counted as unmatched. Use it to protect files from the targets below, for example photos shot at
home at night:

```toml
[[targets]]
name = "Night shots at home"
kind = "keep"
[[targets.filters]]
sources = ["camera"]
geo = "@home"
date = { time = "22:00..05:00" }
```

## Series review

Files matched through a [`series`](#series) condition never go straight to the target. They are
held in the review list, where the Decide screen shows the whole series side by side with the
suggested best shot marked. You confirm it or pick another best shot, and only then do the others go
to the matching target (for example the trash). The confirmation is a manual decision stored per
series, so later runs do not ask again. The best shot is never sent to the target; it walks on to the
targets below. In the CLI the plan shows these files in its review list with the series members.

A negated series condition (`not = true`) is not held: it matches the files that are *not* in the
way, so those go straight to the target.

## Masks

A mask is matched against a **file name**, case-insensitively.

| Form | Example | Meaning |
|---|---|---|
| glob | `*.{jpg,jpeg,heic}`, `IMG_*` | `*`, `?`, `{a,b}` alternatives. |
| regex | `^(IMG\|VID)_[0-9]{8}_` | A string starting with `^` is a regular expression. |
| named | `@photo` | Reference to a built-in or your own named mask. |
| exclusion | `!*.av1.mp4`, `!@raw` | An item starting with `!` vetoes a name. |
| list | `["@photo", "@video"]` | Any slot that takes a mask also takes a list, which means **OR**. |

A name matches a list when it matches at least one plain item and no `!` item. A list made only of
`!` items means "everything except":

```toml
match = ["*.mp4", "!*.av1.mp4"]   # mp4 files that are not already .av1.mp4
match = ["!@raw"]                 # everything except RAW files
```

Your own named masks go in `[masks]`; a name taken by a built-in mask is an error:

```toml
[masks]
phone-media = ["@photo", "@video"]
no-edits    = ["@photo", "!@edited"]
```

### Built-in masks

Exactly what `gallery presets` prints:

| Mask | Definition |
|---|---|
| `@audio` | `*.{mp3,m4a,aac,flac,wav,ogg,opus,wma,ape,alac}` |
| `@photo` | `*.{jpg,jpeg,heic,heif,webp,avif,tif,tiff,bmp,gif}` |
| `@photo-original` | `*.{jpg,jpeg,heic,heif}` |
| `@png` | `*.png` |
| `@raw` | `*.{cr2,cr3,nef,arw,dng,raf,orf,rw2}` |
| `@video` | `*.{mp4,m4v,mov,mkv,webm,avi,3gp}` |
| `@video-original` | `*.{mp4,m4v,mov,mkv}` and not `*.av1.mp4` |
| `@media` | `@photo` + `@video` + `@png` |
| `@screenshot` | `*screenshot*.{png,jpg,jpeg,webp}` or `Знімок екрана*` / `Снимок экрана*` (same extensions) |
| `@screen-recording` | `{screen-*,*screenrecord*,*screen_record*,*screencast*}.{mp4,webm,mkv,mov}` |
| `@tiktok` | `^[0-9a-f]{32}(_[0-9]+)?\.(mp4\|png\|webp\|jpg\|jpeg)$` |
| `@dated` | `^[0-9]{8}_[0-9]{6}` (already named `YYYYMMDD_HHMMSS…`) |
| `@whatsapp` | `^(IMG\|VID\|AUD\|PTT)-[0-9]{8}-WA[0-9]+` |
| `@epoch-name` | `^([0-9]{13}\|[0-9]{10})([-_][0-9]+)?\.` (epoch timestamp names) |
| `@camera-phone` | `^(IMG\|VID\|PXL\|MVIMG)_[0-9]{8}_[0-9]{6}` |
| `@edited` | `*{-edited,_edited}*` |
| `@copy` | `*{(1),(2),(3),_copy,-copy,- copy}*` |

The Presets → Masks tab of the GUI lists them too, and **Test on a name** in the Targets editor
shows which items of a mask match a given file name.

## Content presets (the content pipe)

Built-in presets need no definition; use them by name. A preset has a default mask (which files it
applies to). Photo presets default to `@photo-original`, video presets to `@video-original`, audio
presets to `@audio` (minus files already in the output format), `png-*` presets to `@png`, and
`meta-*` presets to `*`. `gallery presets` prints the live list.

### Built-in content presets

| Preset | What it does | Default mask |
|---|---|---|
| `meta-full` | Metadata fixes: capture date, Make/Model "GCam", XMP-GCamera, gain map XMP, OffsetTime, video Make/Model, video GPS | `*` |
| `meta-dates` | Metadata: capture date only | `*` |
| `avif-low` | AVIF, quality 55, speed 8 | `*.{jpg,jpeg,heic,heif}` |
| `avif-medium` | AVIF, quality 65, speed 6 | same |
| `avif-high` | AVIF, quality 72, speed 6 | same |
| `avif-max` | AVIF, quality 82, speed 4 | same |
| `avif-fast` | AVIF, quality 72, speed 10 | same |
| `png-avif` | PNG → AVIF, quality 80, speed 6 | `*.png` |
| `webp-avif` | WebP → AVIF, quality 72, speed 6 | `*.webp` |
| `webp-low` | WebP, quality 70, method 4 | `*.{jpg,jpeg,heic,heif}` |
| `webp-medium` | WebP, quality 80, method 5 | same |
| `webp-high` | WebP, quality 87, method 6 | same |
| `webp-max` | WebP, quality 93, method 6 | same |
| `png-optipng` | `optipng -o3` (lossless) | `*.png` |
| `av1-low` | AV1 (SVT-AV1), crf 36, preset 4 | `@video-original` |
| `av1-medium` | AV1, crf 32, preset 4 | same |
| `av1-high` | AV1, crf 30, preset 3 | same |
| `av1-max` | AV1, crf 27, preset 2 | same |
| `av1-fast` | AV1, crf 32, preset 8 | same |
| `av1-action-cam` | AV1, crf 30, preset 3, stabilization, denoise medium | same |
| `opus-64` / `opus-96` / `opus-128` / `opus-192` | Opus at that bitrate (kbps) | `@audio` and not `*.opus` |
| `mp3-128` / `mp3-192` / `mp3-256` / `mp3-320` | MP3 at that bitrate | `@audio` and not `*.mp3` |
| `aac-96` / `aac-128` / `aac-192` / `aac-256` | AAC at that bitrate | `@audio` and not `*.m4a` |
| `flac` | FLAC, lossless | `@audio` and not `*.flac` |

Behaviour worth knowing:

- Photo presets try the preset's quality, then, with `ladder` on (the default), lower quality levels
  (AVIF 82/72/65/55, WebP 93/87/80/70) until the result is **smaller than the input**. If nothing
  gets smaller, the original bytes are kept and a warning is recorded.
- Video presets output `.av1.mp4` (SVT-AV1 + Opus). Their default short side is capped at 1080,
  audio 160 kbps, GOP 240, and the ladder works like the photo one.
- Metadata and encoding steps record what they changed; the result is verified against the source
  (see [safety](architecture.md#safety-guarantees)).

### Bundles

A bundle is one name for an ordered list of presets, each on its own mask, so a single step can
treat every file type correctly:

| Bundle | Steps (in order) |
|---|---|
| `compress-all` | `avif-high` (photo originals), `png-avif` (PNG), `webp-avif` (WebP), `av1-high` (video originals) |
| `compress-photos` | `avif-high`, `png-avif`, `webp-avif` |
| `compress-fast` | `avif-fast`, `png-avif`, `webp-avif`, `av1-fast` |

Define your own:

```toml
[content_presets.my-pack]
kind = "bundle"
steps = ["avif-high", { mask = "*.png", preset = "png-avif" }]
```

A bundle has no mask of its own (giving it one is an error), and a step that uses a bundle takes no
mask. Bundles are expanded when the plan is built, so the plan and the GUI show the individual
steps (`compress-all › avif-high`).

### Your own presets

Put them under `[content_presets.<name>]` with a `kind`. A user preset must not reuse a built-in name.
Every kind accepts an optional `mask` (its default mask).

```toml
[content_presets.photos-small]
kind = "photo"
format = "avif"          # "avif" (default) or "webp"
quality = 60             # 0..100
speed = 6                # AVIF 0..10, WebP method 0..6
max_short_side = 2160    # optional downscale, 1..65535
ladder = true            # try lower quality until smaller
min_saving_pct = 10      # 0..99: result must save at least this much
mask = "@photo-original"

[content_presets.clips]
kind = "video"
crf = 32                 # 0..63
preset = 6               # SVT-AV1 preset 0..13
gop = 240                # 1..10000
max_short_side = 720     # 0 = no downscale (default 1080)
audio_kbps = 128         # 6..510 (Opus)
denoise = "low"          # optional: verylow | low | medium | high
stabilize = false
ladder = true

[content_presets.podcast]
kind = "audio"
codec = "opus"           # opus | mp3 | aac | flac
kbps = 64                # opus 6..510, mp3 8..320, aac 8..512; ignored for flac

[content_presets.png-max]
kind = "png"
level = 7                # optipng -o0..7

[content_presets.dates-and-offset]
kind = "metadata"
write_capture_date = true
fill_offset_time = true
# also available: fix_gcam_make_model, strip_gcamera_xmp,
# strip_gain_map_xmp_for_avif, video_make_model, video_restore_gps
```

Field defaults: a `photo` preset defaults to `avif-high`'s values (AVIF, quality 72, speed 6,
`ladder = true`, `min_saving_pct = 0`); a `video` preset to `av1-high`'s (crf 30, preset 3, GOP 240,
short side 1080, audio 160 kbps, `ladder = true`). The Presets screen in the GUI can **copy a
built-in preset as your own** and edits these fields with the valid ranges.

## Name presets (the rename pipe)

The `rename` list of an output holds name presets. Each step applies to files whose *current* name
matches its mask (default `*`) and may change the name and the folders.

### Built-in name presets

| Preset | Template | Notes |
|---|---|---|
| `capture-date` | `{date:YYYYMMDD_HHMMSS}.{ext}` | Capture date; names that already look like `YYYYMMDD_HHMMSS…` are kept; extension to lower case. |
| `capture-date-name` | `{date:YYYYMMDD_HHMMSS}_{stem}.{ext}` | Capture date, then the original name. |
| `capture-date-dirs` | `{date:YYYY}/{date:MM}/{date:YYYYMMDD_HHMMSS}.{ext}` | Folders `year/month` below the output directory. |
| `file-date` | `{date:YYYYMMDD_HHMMSS}.{ext}` | Uses the file's creation date instead of the capture date. |
| `ext-lower` | `{stem}.{ext}` | No date; only the extension goes to lower case. |

A template that needs a date leaves the name alone for a file that has none. The **capture date**
is chosen like this. For photos: EXIF `DateTimeOriginal` (a date and time in the file name wins when
it is within a minute of it), else date and time from the name, else `CreateDate`/`ModifyDate`,
else the day from the name. For videos: date and time from the name, then `DateTimeOriginal`,
`CreateDate`/`ModifyDate`, then a Unix-timestamp name, then the day from the name. `gallery explain`
shows what was decided for a file.

### Your own

```toml
[name_presets."TikTok"]
template = "tiktok_{date}_{stem}.{ext}"
date = "file_created"        # capture (default) | file_created | none
# mask = "@tiktok"           # optional default mask (default `*`)
# skip_if = "@dated"         # do nothing for names matching this
# normalize_ext = true       # lower-case the extension, jpeg → jpg
```

Template tokens:

| Token | Value |
|---|---|
| `{stem}` / `{ext}` | Current name without / with only the extension. |
| `{source}` | The source id. |
| `{date}` | `YYYYMMDD_HHMMSS` of the preset's `date` source. |
| `{date:FORMAT}` | Custom format: `YYYY` `MM` `DD` `HH` `mm` `SS` (`MM` right after `HH` means minutes). |

A template may contain `/`: everything before the last `/` becomes folders below the output
directory. A template must not contain `\`, empty or `.`/`..` components, or folders starting with
`.gallery-`. Add `skip_if` only to a template without `/`.

## `[ai]`

```toml
[ai]
model    = "claude-sonnet-5-5"   # default
effort   = "low"                 # default; passed to the API as the request's effort
jobs     = 8                     # default: parallel requests
examples = 20                    # default: how many of your latest corrections go into the prompt; 0 = off
# language = "en"               # default: unset, the interface language; name or code
```

| Key | Default | Meaning |
|---|---|---|
| `model` | `claude-sonnet-5-5` | Anthropic model used for classification. Used for cost estimates when its price is known. |
| `effort` | `low` | Request effort. Classification of a thumbnail rarely needs more. |
| `jobs` | `8` | Requests in flight at once. |
| `examples` | `20` | When you change what the AI decided (or settle a doubtful answer), the correction is stored as **text** (no images) and the latest `examples` for the same source are shown to the model in later requests. `0` turns learning off. List/clear them with `gallery examples list` / `clear` or in the GUI Settings. Examples affect only *new* requests: answers already cached stay valid. |
| `language` | unset | Language the model writes each answer's short reason, keywords and description in: a name or a code (`"en"`, `"ru"`, `"Ukrainian"`). **Unset, it follows the interface language** — the GUI's *Settings → Language*, or the CLI's `--lang` / `LANG` — and is English by default. An explicit value always wins. Category names and descriptions are sent exactly as you wrote them. |

Answers are cached per file (path, size, mtime), model and prompt in
`<cache dir>/gallery/ai-cache.jsonl`, so a file is asked about only once; examples are deliberately
not part of the cache key.

### AI keywords and description

The same request that picks a category also asks for 3–10 short lowercase keywords and a
one-sentence description of the frame, written in the `language` above. They never influence
routing; they are only written into the file's metadata:

- **Only for files the AI routed** (the decision is *via AI*, including an unsure answer accepted with
  `--accept-low`). Files routed by masks or by your manual decisions get nothing.
- **Only into processed outputs** — outputs whose content pipe has at least one step. A byte-for-byte
  copy (an empty content pipe) stays exactly the source.
- Keywords go to `XMP-dc:Subject` (and `IPTC:Keywords` in a JPEG), the description to
  `XMP-dc:Description`; videos in MP4/MOV get the same XMP tags. Keywords the file already has are
  kept (and not repeated); an existing description is never replaced. exiftool cannot write
  WebM/MKV: such an output is placed without them, with a warning.
- They are written before the output is verified against its source, so verification sees the
  final file; capture dates, GPS, dimensions and duration are untouched.

Answers cached before keywords were asked for are used as they are — without keywords, and without
a new (paid) request. A plan that carries keywords is a version 2 plan (an older `gallery apply`
refuses it; update the server binary).

## Migration from the three stages

Older configs have three fixed stages instead of one list:

```toml
[[targets.before_ai]]      # name masks
[[targets.ai]]             # name, sources, description, [targets.ai.prefilter], outputs
[[targets.after_ai]]       # name masks
```

They still load and mean the same as before: `Config::load` concatenates `before_ai`, `ai` and
`after_ai`, in that order, into one `[[targets]]` list. An AI target becomes a target with one filter
`{ sources, match / exclude from its prefilter, ai = description }`. Targets are referred to by name
in plans, overrides and reports, so those stay valid.

- `gallery check -c CONFIG` (and every command that loads the config) prints a one-line notice
  when the file uses the old shape. `gallery check` exits 0.
- The GUI saves in the new shape. It writes a `.bak` copy of the file first (`.bak.1`, `.bak.2`,
  … when that name is taken; later saves never replace it), then rewrites it once, keeping the
  comments where it can. Routing does not change.
- An empty `[targets]` table is not the old shape.
- `gallery scan --no-ai` now means "no `ai` condition holds for any file"; in the old shape, that
  is the same as the old "skip the AI stage".

To migrate by hand, rename the tables to `[[targets]]`/`[[targets.filters]]`/`[[targets.outputs]]`,
keep the order, and turn the AI target's `description` and `prefilter` into `ai` and `match` /
`exclude` of its filter.

## Related files

| What | Where |
|---|---|
| Config | `~/.config/gallery/gallery.toml` (GUI default) |
| API key file | `~/.config/gallery/api-key` (owner-only, e.g. mode 0600) |
| Other state | See [architecture → files on disk](architecture.md#files-on-disk) |
