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
- [Targets](#targets): [before AI / after AI](#mask-targets-before_ai-and-after_ai), [AI targets](#ai-targets-targetsai), [outputs](#outputs)
- [Masks](#masks) (syntax, `@refs`, `!` exclusions, built-in masks)
- [Content presets](#content-presets-the-content-pipe) (built-ins, your own, bundles)
- [Name presets](#name-presets-the-rename-pipe)
- [`[ai]`](#ai)
- [Where related files live](#related-files)

## How a file is routed

Every file found in a source goes through three stages, in this order. **Each stage is
first-match-wins and a file ends up in exactly one target.**

1. **`targets.before_ai`**: name masks. The first target with a filter that takes the file wins.
   Use it for files you can recognise by name (screenshots, TikTok downloads, files already in
   their final place). These files are never sent to the AI.
2. **`targets.ai`**: one Claude request per file that is still unrouted and fits at least one AI
   target (its source is listed and the `prefilter` mask lets it through). The model picks one of the
   categories (your AI targets) or "none". An unsure answer goes to the review list instead of being
   applied (unless you pass `--accept-low`).
3. **`targets.after_ai`**: name masks again, for everything still unrouted, including files the AI
   called "none".

A file that no target takes is *unmatched*; Gallery leaves it alone and lists it (see
`gallery unmatched` and the triage view in the GUI). A file that a filter would take but for an
`exclude` mask is left alone on purpose and is not reported as unmatched.

Your manual decisions (made in the GUI review/Check screens) override all of this per file.

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

## Targets

A *target* is "a kind of file" plus where it goes. Target names must be unique across all stages.

### Mask targets (`before_ai` and `after_ai`)

```toml
[[targets.before_ai]]
name = "Screenshots"

[[targets.before_ai.filters]]
sources = ["camera", "partner"]            # source ids this filter reads
match   = ["@screenshot", "@screen-recording"]   # mask: which file names (list = OR); omit = any file
exclude = ["*-edited*"]                    # optional: names this filter must NOT take

[[targets.before_ai.outputs]]
dir = "~/Media/Screenshots/{YYYY}"
content = ["meta-full"]
rename  = ["capture-date"]
```

A target has any number of `filters` (it takes a file if **any** filter matches) and any number of
`outputs` (each output is produced for every file the target takes).

| Filter key | Meaning |
|---|---|
| `sources` | List of source ids. A filter with no sources matches nothing. |
| `match` | [Mask](#masks) on the file **name** (not the path). Omitted: any file. |
| `exclude` | Mask; a name matching it is not taken by this filter. |

### AI targets (`targets.ai`)

An AI target is a *category* the model can choose.

```toml
[[targets.ai]]
name = "Receipts"
sources = ["camera"]
description = "Photos of receipts, invoices and price tags."   # required, not empty

[targets.ai.prefilter]          # optional: only files whose name passes are sent to the AI at all
match = "@photo"
exclude = "@screenshot"

[[targets.ai.outputs]]
dir = "~/Documents/Receipts"
rename = ["capture-date"]
```

| Key | Meaning |
|---|---|
| `name` | The category name the model answers with. |
| `description` | Shown to the model next to the name; this is your main lever on accuracy. Required. |
| `sources` | Source ids whose files may be classified into this category. |
| `prefilter.match` / `prefilter.exclude` | Name masks deciding which files can be offered this category (e.g. only photos). A file that fits no AI target is not sent to the AI. |

The model sees a 512 px preview of a photo, or three 400 px frames of a video, never the original.
If it answers "none" or is unsure, see the routing rules above.

### Outputs

Every target has `[[...outputs]]`. An output is one destination for the file:

```toml
[[targets.after_ai.outputs]]
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
  "Screenshots (in place)" example sorts files into `year/month` folders without duplicating them.
- An output with no `content` and no `rename` is a backup copy (byte for byte).

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
| `language` | unset | Language the model writes each answer's short reason in: a name or a code (`"en"`, `"ru"`, `"Ukrainian"`). **Unset, it follows the interface language** — the GUI's *Settings → Language*, or the CLI's `--lang` / `LANG` — and is English by default. An explicit value always wins. Category names and descriptions are sent exactly as you wrote them. |

Answers are cached per file (path, size, mtime), model and prompt in
`<cache dir>/gallery/ai-cache.jsonl`, so a file is asked about only once; examples are deliberately
not part of the cache key.

## Related files

| What | Where |
|---|---|
| Config | `~/.config/gallery/gallery.toml` (GUI default) |
| API key file | `~/.config/gallery/api-key` (owner-only, e.g. mode 0600) |
| Other state | See [architecture → files on disk](architecture.md#files-on-disk) |
