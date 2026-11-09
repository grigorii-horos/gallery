# Command-line reference

The CLI binary is called `gallery`. It shares its core with the desktop app, so a plan written by the
CLI can be applied in the GUI and the other way round, and both use the same history, manual
decisions, AI cache and locks.

```
gallery <command> [options]
```

`-h` prints the short help, `-H` / `--help-full` the detailed one (this binary has no `--help` or
`--version`; `-h` and `-H` work after any command too).

## Overview

| Command | What it does | Changes anything? |
|---|---|---|
| [`check`](#check) | Show required tools, versions and capabilities | no |
| [`presets`](#presets) | Print the built-in presets, bundles and masks | no |
| [`scan`](#scan) | Scan sources, route files (calling the AI for uncached ones) and write a plan | only the plan file (and the AI cache) |
| [`apply`](#apply) | Execute a plan | yes: writes outputs, trashes sources |
| [`undo`](#undo) | Roll a run back from its report | yes |
| [`explain`](#explain) | Why a file gets the treatment it gets | no |
| [`unmatched`](#unmatched) | Group the unmatched files and say why nothing takes them | no |
| [`pending`](#pending) | Count files waiting to be sorted (for scripts and cron) | only a small cache index |
| [`sync`](#sync) | Show or resume the Syncthing folders Gallery paused | Syncthing pause state |
| [`examples`](#examples) | List or clear the AI's learned corrections | `clear` deletes them |
| [`install-desktop`](#install-desktop) | Write the app-menu entry for `gallery-gui` | writes two files |

## Language

`gallery --lang <code> <command>` sets the language of `--help` and of the messages and output
(`en`, `ru`, `uk`, `de`, `fr`, `es`, `pt-BR`, `it`, `pl`, `ro`, `zh-CN`). Without it the language comes
from `LC_ALL`, `LC_MESSAGES`, `LANG` (first one set; `C`/`POSIX` count as unset), else English.
`uk_UA` is `uk`, `pt_PT` is `pt-BR`, `zh_TW` is `zh-CN`; an unsupported language is English.
Machine-readable output is not translated: `--json`, and the first line of `pending` (`pending: N`).
When `[ai] language` is unset, the AI writes its reasons in this language too. Errors from clap
itself (an unknown flag, a missing argument) stay in English.

## Environment

| Variable | Effect |
|---|---|
| `ANTHROPIC_API_KEY`, `CLAUDE_API_KEY` | AI key (first one that is set). Otherwise `~/.config/gallery/api-key` is read if only its owner can access it (mode `0600`); a file with wider permissions is ignored with a warning. |
| `GALLERY_TRASH=folder` | Move sources to `<source>/.gallery-trash/<run>/` instead of the system trash. Handy on a server without a desktop trash. The GUI's pilot before/after view can only read trashed originals from this folder (the system trash is not searched). |
| `LC_ALL`, `LC_MESSAGES`, `LANG` | Language of the output when `--lang` is not given (see [Language](#language)). |
| `GALLERY_TOOLS_DIR` | Look for `ffmpeg`, `ffprobe`, `exiftool`, `avifenc`, `magick`, `optipng` here first, then in `PATH`. |

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | Any error; `check`: a required tool or capability is missing |
| 2 | `apply`: at least one file failed, or the run stopped because of discrepancies (see [safety](architecture.md#safety-guarantees)); `sync resume`: a folder could not be resumed |
| 130 | Stopped by Ctrl-C (started files are finished, new ones are not started; a second Ctrl-C during `scan` reading ends it at once) |

---

## `check`

```
gallery check [-c <CONFIG>]
```

With `-c/--config` the config file is loaded and validated first: an unknown source or preset, a
duplicate target name, a bad condition and so on is an error (exit 1) that names the target. A file
that still uses the old three-stage targets (`targets.before_ai` / `ai` / `after_ai`) prints a
one-line migration notice and exits 0 (see [Migration](configuration.md#migration-from-the-three-stages));
otherwise it prints `config <path>: ok (N sources, M targets)`. Every other command that loads the
config prints the same notice on stderr.

Then it lists the external tools (`ffmpeg`, `ffprobe`, `exiftool`, `avifenc`, `magick`, `optipng`), their
versions and the ffmpeg capabilities. Exit 1 if a required tool or `libsvtav1` is missing
(`optipng` is only needed for PNG presets; `libopus` and `vidstab` only by the presets that use them).

```
$ gallery check
ffmpeg: /usr/bin/ffmpeg
...
ffmpeg libsvtav1 (AV1 video): yes
ffmpeg libopus (Opus audio): yes
ffmpeg vidstab (stabilization): yes
```

## `presets`

```
gallery presets
```

Prints the built-in name presets, content presets, bundles and named masks, with their key
parameters. The tables in [configuration.md](configuration.md) come from this output.

## `scan`

```
gallery scan -c <CONFIG> -o <PLAN> [--server] [--accept-low] [--no-ai] [--overrides <FILE>]
gallery scan -c <CONFIG> --estimate
```

Scans the sources, reads EXIF, routes every file through the ordered targets, and writes a plan. It
prints a summary: files per target, the review list, unmatched files, warnings and the AI cost.
Nothing but the plan file (and the AI answer cache) is written; sources are never touched.

| Flag | Meaning |
|---|---|
| `-c, --config <FILE>` | Config file (TOML). Required. |
| `-o, --output <FILE>` | Plan file to write. Required unless `--estimate`. |
| `--estimate` | Only estimate the AI cost (how many files would be sent, how many are cached). No calls, no plan. Conflicts with `--no-ai` and `-o`. |
| `--server` | Build the plan with server paths (`server_path` of sources, `server_dir` of outputs). See [Server workflow](../README.md#server-workflow). |
| `--accept-low` | Accept unsure AI answers instead of sending them to the review list. |
| `--no-ai` | Skip the AI: no `ai` condition holds, so files go on to the later targets. |
| `--overrides <FILE>` | Manual decisions file. Default: `~/.local/share/gallery/overrides.json` if it exists. |

When AI is on and some filter has an `ai` condition, the key and model are checked **before** the
scan starts, so a missing key fails at once (exit 1). Unlike the GUI (which only calls the AI when
you press *Ask AI*), `gallery scan` asks the AI for every file without a cached answer; run
`--estimate` first to see how many that is.

```
$ gallery scan -c ~/.config/gallery/gallery.toml --estimate
AI: ≈ $0.00 (0 files, 12 cached) (rough estimate)

$ gallery scan -c ~/.config/gallery/gallery.toml -o plan.json
Plan: plan.json (25 files)
  Documents: 1
  Receipts: 2
  ...
For review: 3
  /home/me/Sync/Camera/IMG_20260417_101010.jpg -> Receipts (Looks like a receipt, but …)
Matched no target: 4
```

## `apply`

```
gallery apply <PLAN> [--dry-run] [-j N] [--pilot N] [--allow-anomalies] [--pause-sync]
```

Executes a plan: for each file, runs the content pipes into staged temporary files, **verifies** each
result against its source, places the outputs without overwriting anything, and only then moves the
source to the trash. A report is written next to the plan as `<plan>.report.json`. Files that changed
or vanished since the scan are skipped, not processed.

| Flag | Meaning |
|---|---|
| `--dry-run` | Only check (tool capabilities, missing and changed sources); write nothing. Prints a size/time estimate. |
| `-j, --jobs <N>` | Files processed in parallel (default 4). |
| `--pilot <N>` | Apply only a trial: up to `N` files of each target, preferring different extensions. The pilot gets its own plan (`<name>-pilot.plan.json`) and the rest is written to `<name>-rest.plan.json`; look at the results, then `apply` the rest plan, or `undo` the pilot. |
| `--allow-anomalies` | Place results that differ from their source in date, mtime, GPS, size or duration. Off by default: such a file is not placed and its source is left alone. |
| `--pause-sync` | Pause the local Syncthing folders the plan touches (found via Syncthing's own `config.xml`) and **do not resume them**; check the result and run `gallery sync resume`. Without a reachable Syncthing the run goes on with a warning. |

Only one `apply`/`undo` runs on a machine at a time (the GUI included); a second one fails
at once with a message naming the holder. A dry run takes no lock.

```
$ gallery apply plan.json --pilot 5
pilot: 14 files in plan-pilot.plan.json, rest (21) in plan-rest.plan.json
processed: 14, renamed: 2, errors: 0, skipped: 0
Report: plan-pilot.plan.report.json
$ gallery apply plan-rest.plan.json -j 8
```

Exit code 2 if any file failed or the run stopped on discrepancies.

## `undo`

```
gallery undo <REPORT> [--file <PATH>]... [--target <NAME>]...
```

Restores the sources from the trash to their places and deletes the outputs the run created. It
overwrites nothing: an occupied original place keeps its occupant (the source then stays in the
trash and is listed as a problem), and an output that was modified after the run is kept. A report can
be undone once. Pass the report file by its actual name (`<plan>.report.json`).

| Option | Meaning |
|---|---|
| `--file <PATH>` | Undo only this source file of the run (repeatable). The path must be an item of the report, else the command fails and nothing moves. |
| `--target <NAME>` | Undo only the files the plan routed to this target (repeatable; `trash` targets restore their trashed sources, `keep` targets have nothing to undo). The plan is the one the report names, or the `<plan>.json` next to the report. |

With `--file`/`--target`, only those files are undone, with the same guards as a full undo; the other files and the outputs they
need are not touched. The undone files are recorded in the report, so a later full `undo` skips
them; the report counts as undone once every file is. The GUI History screen offers the same per
file and per target.

## `explain`

```
gallery explain -c <CONFIG> [--overrides <FILE>] <PATH>...
```

For each file: the source it belongs to, every target in list order with the filters that took the
file or did not and why (match, exclude, `!`), the **first condition that missed** for each target
that did not take it, the manual decision or cached AI answer, the content and name steps that would
fire, the final destination, and a one-line verdict with a hint. It never calls the AI and changes
nothing.

When the config uses conditions on groups (`event`, `expand`, `near`, `duplicate`, `series`,
`props.sharpness`), explain scans all the sources as a real scan does, so those conditions see the
same files, and it applies the last-copy guard to the plan it shows: a file a scan holds for review
is never explained as "goes to the trash". A miss reads like `near: 0 files of olga within 2h / 1.0 km,
at least 1 needed`, `duplicate: no identical copy` or `date: no capture date`.

```
$ gallery explain -c gallery.toml ~/Sync/Camera/IMG_20260417_101010.jpg
/home/me/Sync/Camera/IMG_20260417_101010.jpg
├─ Source: camera, the scan sees it
├─ Targets, in order
│  ├─ Duplicates — no
│  │  ├─ duplicate: no identical copy
│  ├─ Screenshots and screen recordings — no
│  │  ├─ filter 1: match [@screenshot, @screen-recording] did not take
...
├─ AI (from cache): Receipts (unsure) — Looks like a receipt, but …
├─ Decision: for review
└─ The AI is unsure: suggests Receipts (...). The file waits for a decision in the review screen
```

## `unmatched`

```
gallery unmatched -c <CONFIG> [--plan <PLAN>] [--overrides <FILE>]
```

Groups the unmatched files by source, folder and extension and prints, per group, the file count,
size and reason (the same reasoning as `explain`). Without `--plan`, a plan is built on the fly
without AI but with the metadata and groups a scan reads (dates, places, events, duplicates, ...), so
the result matches what a scan without an AI answer would leave unmatched. In the GUI the same view offers
*Add to target…*, *Always ignore in the whole source* and *Create target…*.

## `pending`

```
gallery pending -c <CONFIG> [--overrides <FILE>] [--json]
```

Quietly counts how many files a scan would put into targets and how many still wait for an AI
answer. The AI is not called. A small index in the cache directory remembers each file's size,
mtime and EXIF, so repeated calls only read new and changed files; when nothing changed it answers
without routing at all. Meant for scripts and cron.

```
$ gallery pending -c gallery.toml
pending: 28
ready to apply: 28
awaiting AI: 0
  Documents: 2
  Receipts: 4
  ...
$ gallery pending -c gallery.toml --json
{"to_process":28,"need_ai":0,"by_target":{"Documents":2,...},"exif_reads":0,"fast_path":false}
```

## `sync`

```
gallery sync status
gallery sync resume
```

Works only with a local Syncthing (address and API key come from its `config.xml`). `status` shows
which folders Gallery paused (and since when, for which plan) and whether Syncthing answers;
`resume` resumes **only** the folders Gallery itself paused and clears the pause record. Folders
that were already paused before the run are never touched. Exit 2 if some folder could not be
resumed.

## `examples`

```
gallery examples list [--source <ID>]
gallery examples clear [--source <ID>]
```

When you change what the AI decided (or settle an unsure answer), the correction is stored as text
(no images) and shown to the AI as an example in later requests for the same source; the number of
examples per request is `[ai] examples` in the config (default 20, `0` = off). `list` shows what the
AI sees, `clear` deletes all corrections or those of one source. Examples only affect new requests:
cached answers stay valid.

## `install-desktop`

```
gallery install-desktop [--exe <PATH>]
```

Writes `~/.local/share/applications/gallery.desktop` and an icon under `~/.local/share/icons`, so
`gallery-gui` appears in the application menu. `Exec` points to the `gallery-gui` binary next to
this program, or to `--exe`. It never runs by itself.

## Typical sessions

Sort once, by hand, on your desktop:

```sh
gallery check
gallery scan -c gallery.toml --estimate        # how much will the AI cost?
gallery scan -c gallery.toml -o plan.json      # review the summary
gallery apply plan.json --dry-run              # nothing written
gallery apply plan.json --pilot 5              # a few files per target first
gallery apply plan-rest.plan.json -j 8
gallery undo plan-pilot.plan.report.json       # if the pilot looked wrong
```

Plan on the desktop, apply on a server (see the [README](../README.md#server-workflow)):

```sh
gallery scan --server -c gallery.toml -o plan.json   # desktop, AI runs here
# copy plan.json to the server, then on the server:
GALLERY_TRASH=folder gallery apply plan.json
# plan.report.json appears next to the plan; undo from anywhere that can reach the files:
gallery undo plan.report.json
```

A plan that trashes or keeps files (a `trash` or `keep` target) is plan version 2. A server running
an older `gallery` refuses it with `unsupported plan version 2` and touches nothing: update
`gallery` on the server. Plans without `trash`/`keep` targets stay version 1.

A nightly "is there something new?" check:

```sh
test "$(gallery pending -c gallery.toml | head -1)" != "pending: 0" && notify-send "Photos to sort"
```
