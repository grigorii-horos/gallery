//! exiftool wrapper: batched JSON reads and single-file writes.

use crate::progress::{Cancel, NoProgress, Progress, ProgressEvent, Stage};
use crate::tools::Tools;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

const BATCH: usize = 500;

/// The exiftool arguments of a read; part of the cache identity.
const READ_ARGS: [&str; 7] = [
    "-j",
    "-n",
    "-G1",
    "-api",
    "LargeFileSupport=1",
    "-charset",
    "filename=utf8",
];

/// Read metadata for `paths`, always fresh (no cache). Keys are `Group:Tag`
/// (`-G1`), values numeric (`-n`). Use this to verify files just written or
/// staged: they are never worth caching and must never be answered stale.
pub fn read(tools: &Tools, paths: &[PathBuf]) -> Result<HashMap<PathBuf, serde_json::Value>> {
    read_batches(tools, paths, &NoProgress, &Cancel::new(), &mut |_, _| {})
}

/// [`read`] answered from the persistent cache `<cache dir>/exif/<xx>.json`
/// (256 shards by key hash; only the shards a request touches are read or
/// rewritten) where a file's path, size and mtime are unchanged; the rest is read in
/// batches, with a [`Stage::Exif`] event per batch, and stored. A cancel is
/// checked before each batch and ends the read with `Error::Cancelled`.
/// Without a cache dir nothing is persisted.
pub fn read_with(
    tools: &Tools,
    paths: &[PathBuf],
    progress: &dyn Progress,
    cancel: &Cancel,
) -> Result<HashMap<PathBuf, serde_json::Value>> {
    let dir = crate::paths::cache_dir().map(|d| d.join("exif"));
    read_cached(tools, paths, progress, cancel, dir.as_deref())
}

/// Tags (any group: `IFD0:Make` is "Make") of a read record that readers of
/// [`read_with`] look at; the cache keeps these and drops the rest of the
/// raw `-G1` record (3-20 KB each, mostly maker notes and binary stubs).
///
/// WHERE TO ADD A TAG: a reader of the records `read_with` returns (the
/// lookups in `filter/facts.rs`, `datetime.rs`, and what `facts_of` reads
/// for route, plan, explain, pending and the effect preview) that starts to
/// look at a tag not listed here must add its name below. The list is part
/// of the cache identity, so adding a name discards the old shards. A user
/// `has_tag`/`missing_tag` name cannot be listed: those are answered from
/// [`PRESENT`], the names of every other non-null tag of the file by group.
const KEPT_TAGS: &[&str] = &[
    // filter/facts.rs: device, GPS, size, orientation, media codecs.
    "GPSLatitude",
    "GPSLongitude",
    "Make",
    "Model",
    "Software",
    "ImageSize",
    "ImageWidth",
    "ImageHeight",
    "ExifImageWidth",
    "ExifImageHeight",
    "SourceImageWidth",
    "SourceImageHeight",
    "Orientation",
    "Rotation",
    "Duration",
    "MediaDuration",
    "VideoFrameRate",
    "FrameRate",
    "CompressorID",
    "VideoCodec",
    "CodecID",
    "AudioFormat",
    "AudioCodecID",
    "AudioChannels",
    "AudioSampleRate",
    // datetime.rs: capture date.
    "DateTimeOriginal",
    "CreateDate",
    "ModifyDate",
];

/// Key of the cached record that lists the other non-null tags as
/// `{"Group": ["Tag", ...]}` (a group written once), so `props.has_tag` /
/// `missing_tag` of any name, with or without a group, still hold.
pub(crate) const PRESENT: &str = "_Present";

/// `rec` cut down to `SourceFile`, the [`KEPT_TAGS`] and [`PRESENT`].
fn minimized(rec: &serde_json::Value) -> serde_json::Value {
    let Some(obj) = rec.as_object() else {
        return rec.clone();
    };
    let mut out = serde_json::Map::new();
    let mut present: std::collections::BTreeMap<&str, Vec<&str>> = Default::default();
    for (k, v) in obj {
        let name = k.rsplit_once(':').map_or(k.as_str(), |(_, n)| n);
        if k == "SourceFile" || KEPT_TAGS.contains(&name) {
            out.insert(k.clone(), v.clone());
        } else if !v.is_null()
            && let Some((g, n)) = k.rsplit_once(':')
        {
            present.entry(g).or_default().push(n);
        }
    }
    if !present.is_empty() {
        out.insert(PRESENT.into(), serde_json::json!(present));
    }
    serde_json::Value::Object(out)
}

#[derive(Serialize, Deserialize)]
struct Entry {
    size: u64,
    mtime_ns: u128,
    /// The exiftool record cut to what readers use (see [`KEPT_TAGS`]), before
    /// the QuickTime zone conversion.
    rec: serde_json::Value,
}

#[derive(Serialize, Deserialize, Default)]
struct CacheFile {
    /// exiftool binary and read arguments the entries were read with.
    tool: String,
    entries: HashMap<String, Entry>,
}

/// Identity of the exiftool and arguments: its path, size and mtime stand
/// for its version, so no process has to run to know it.
fn tool_identity(tools: &Tools) -> String {
    let stamp = stat(&tools.exiftool)
        .map(|(s, m)| format!("{s}:{m}"))
        .unwrap_or_default();
    format!(
        "3|{}|{stamp}|{}|{}",
        tools.exiftool.display(),
        READ_ARGS.join(" "),
        KEPT_TAGS.join(",")
    )
}

/// Canonical path, size and mtime in nanoseconds: the cache key and check.
/// Limitation: a file rewritten to the same size within one mtime tick (coarse
/// filesystems: FAT 2 s, some network shares 1 s) is not noticed.
type Stamp = (String, u64, u128);

fn stat(p: &Path) -> Option<(u64, u128)> {
    let m = std::fs::metadata(p).ok()?;
    let ns = m
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some((m.len(), ns))
}

/// Shard of a cache key: first byte of its blake3 hash, as `xx`.
fn shard_of(key: &str) -> String {
    format!("{:02x}", blake3::hash(key.as_bytes()).as_bytes()[0])
}

fn load_shard(dir: &Path, shard: &str, tool: &str) -> CacheFile {
    let loaded: Option<CacheFile> = std::fs::read(dir.join(format!("{shard}.json")))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok());
    match loaded {
        Some(c) if c.tool == tool => c,
        _ => CacheFile {
            tool: tool.to_string(),
            entries: HashMap::new(),
        },
    }
}

fn read_cached(
    tools: &Tools,
    paths: &[PathBuf],
    progress: &dyn Progress,
    cancel: &Cancel,
    cache_dir: Option<&Path>,
) -> Result<HashMap<PathBuf, serde_json::Value>> {
    let Some(cache_dir) = cache_dir else {
        let mut all = read_batches(tools, paths, progress, cancel, &mut |_, _| {})?;
        all.values_mut().for_each(|r| *r = minimized(r));
        return Ok(all);
    };
    if !paths.is_empty() {
        cancel.check()?;
    }
    let tool = tool_identity(tools);
    let mut shards: HashMap<String, CacheFile> = HashMap::new();
    let mut result = HashMap::new();
    // Misses: path and the stamp taken before the read, so a file changed
    // while exiftool runs is re-read next time.
    let mut misses: Vec<(PathBuf, Option<Stamp>)> = Vec::new();
    for p in paths {
        let key = std::fs::canonicalize(p).ok().zip(stat(p));
        let key = key.map(|(c, (s, m))| (c.to_string_lossy().into_owned(), s, m));
        let hit = key.as_ref().and_then(|(k, s, m)| {
            shards
                .entry(shard_of(k))
                .or_insert_with_key(|sh| load_shard(cache_dir, sh, &tool))
                .entries
                .get(k)
                .filter(|e| e.size == *s && e.mtime_ns == *m)
        });
        match hit {
            Some(e) => {
                let mut rec = e.rec.clone();
                rec["SourceFile"] = p.to_string_lossy().into_owned().into();
                quicktime_to_local(tools, &mut rec);
                result.insert(p.clone(), rec);
            }
            None => misses.push((p.clone(), key)),
        }
    }
    drop(shards);
    let hits = result.len();
    let stamps: HashMap<PathBuf, Stamp> = misses
        .iter()
        .filter_map(|(p, k)| k.clone().map(|k| (p.clone(), k)))
        .collect();
    let miss_paths: Vec<PathBuf> = misses.into_iter().map(|(p, _)| p).collect();
    let shifted = Shifted { progress, hits };
    // New entries by shard; merged onto the shard as it is on disk by then.
    let mut fresh_entries: HashMap<String, Vec<(String, Entry)>> = HashMap::new();
    let mut store = |path: &Path, raw: &serde_json::Value| {
        // `-G1` names the key `ExifTool:Error`; older output has it bare.
        if raw.get("Error").is_some() || raw.get("ExifTool:Error").is_some() {
            return;
        }
        if let Some((k, size, mtime_ns)) = stamps.get(path) {
            fresh_entries.entry(shard_of(k)).or_default().push((
                k.clone(),
                Entry {
                    size: *size,
                    mtime_ns: *mtime_ns,
                    rec: minimized(raw),
                },
            ));
        }
    };
    let fresh = read_batches(tools, &miss_paths, &shifted, cancel, &mut store);
    // Keep what was read even if a later batch failed or was cancelled.
    save_shards(cache_dir, &tool, fresh_entries);
    // The same shape as a hit, so a reader never works on a first run only.
    result.extend(fresh?.into_iter().map(|(p, r)| (p, minimized(&r))));
    if miss_paths.is_empty() && !paths.is_empty() {
        progress.step(ProgressEvent {
            stage: Stage::Exif,
            done: paths.len(),
            total: paths.len(),
            current: paths.last().cloned(),
        });
    }
    Ok(result)
}

/// For each shard with new entries: re-read it from disk (another process
/// may have written it while exiftool ran), add the entries, drop entries of
/// files gone from a folder that is still there, write it atomically. A failure to write only costs the
/// next run a re-read.
fn save_shards(dir: &Path, tool: &str, fresh: HashMap<String, Vec<(String, Entry)>>) {
    if fresh.is_empty() {
        return;
    }
    let _ = std::fs::create_dir_all(dir);
    for (shard, entries) in fresh {
        let mut cache = load_shard(dir, &shard, tool);
        cache.entries.extend(entries);
        // A missing file goes only when its folder is still there: an SD
        // card or a share not mounted now keeps its entries.
        cache.entries.retain(|k, _| {
            let p = Path::new(k);
            p.exists() || !p.parent().is_some_and(Path::is_dir)
        });
        if let Ok(bytes) = serde_json::to_vec(&cache) {
            let _ = crate::paths::write_atomic(&dir.join(format!("{shard}.json")), &bytes);
        }
    }
}

/// Reports a miss-only read against the whole request: cached files count
/// as done from the start.
struct Shifted<'a> {
    progress: &'a dyn Progress,
    hits: usize,
}

impl Progress for Shifted<'_> {
    fn step(&self, mut e: ProgressEvent) {
        e.done += self.hits;
        e.total += self.hits;
        self.progress.step(e);
    }
}

/// Run exiftool over `paths` in batches. `raw` sees each record as exiftool
/// printed it, with the requested path it answers, before the QuickTime zone
/// conversion applied to the result. Result keys are the requested paths.
fn read_batches(
    tools: &Tools,
    paths: &[PathBuf],
    progress: &dyn Progress,
    cancel: &Cancel,
    raw: &mut dyn FnMut(&Path, &serde_json::Value),
) -> Result<HashMap<PathBuf, serde_json::Value>> {
    let mut result = HashMap::new();
    let mut done = 0;
    for chunk in paths.chunks(BATCH) {
        cancel.check()?;
        let mut argfile = tempfile::NamedTempFile::new()?;
        let mut on_argv = Vec::new();
        for p in chunk {
            match argfile_line(p) {
                Some(line) => writeln!(argfile, "{line}")?,
                None => on_argv.push(p),
            }
        }
        argfile.flush()?;
        let requested = Requested::new(chunk);
        let out = tools
            .command(&tools.exiftool)
            .args(READ_ARGS)
            .arg("-@")
            .arg(argfile.path())
            .args(on_argv)
            .output()?;
        // exiftool exits 1 when some file is unreadable but still prints the rest.
        let parsed: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).map_err(|e| {
            Error::Tool(crate::t!(
                "exif.output",
                error = e,
                stderr = String::from_utf8_lossy(&out.stderr)
            ))
        })?;
        for mut v in parsed {
            let Some(src) = v.get("SourceFile").and_then(|s| s.as_str()) else {
                continue;
            };
            let path = requested
                .get(src)
                .cloned()
                .unwrap_or_else(|| PathBuf::from(src));
            raw(&path, &v);
            quicktime_to_local(tools, &mut v);
            result.insert(path, v);
        }
        done += chunk.len();
        progress.step(ProgressEvent {
            stage: Stage::Exif,
            done,
            total: paths.len(),
            current: chunk.last().cloned(),
        });
    }
    Ok(result)
}

/// `p` as one line of an exiftool argument file, or None when a line cannot
/// hold it: not UTF-8, a line break inside, a leading `#` (a comment) or
/// edge spaces (trimmed). Such a path goes on the command line instead, as
/// raw bytes, so it can never turn into extra options.
fn argfile_line(p: &Path) -> Option<&str> {
    p.to_str()
        .filter(|s| !s.contains(['\r', '\n']) && !s.starts_with('#') && s.trim() == *s)
}

/// Requested paths of one batch by how exiftool may echo them in
/// `SourceFile`: spelled the same, or (where `\` is a separator too, on
/// Windows) equal up to the path separator: exiftool there may echo
/// `C:/a/b.jpg` for `C:\a\b.jpg`. Elsewhere `a\b` and `a/b` are two files.
struct Requested<'a> {
    exact: HashMap<&'a std::ffi::OsStr, &'a PathBuf>,
    unified: HashMap<String, &'a PathBuf>,
}

impl<'a> Requested<'a> {
    fn new(chunk: &'a [PathBuf]) -> Self {
        Self::with_separators(chunk, cfg!(windows))
    }

    /// `unify`: match up to the path separator.
    fn with_separators(chunk: &'a [PathBuf], unify: bool) -> Self {
        let unified = if unify {
            chunk
                .iter()
                .filter_map(|p| Some((unify_separators(p.to_str()?), p)))
                .collect()
        } else {
            HashMap::new()
        };
        Requested {
            exact: chunk.iter().map(|p| (p.as_os_str(), p)).collect(),
            unified,
        }
    }

    fn get(&self, src: &str) -> Option<&'a PathBuf> {
        let exact = self.exact.get(std::ffi::OsStr::new(src));
        exact
            .or_else(|| self.unified.get(&unify_separators(src)))
            .copied()
    }
}

fn unify_separators(s: &str) -> String {
    s.replace('\\', "/")
}

/// QuickTime date tags exiftool stores as UTC: the movie, track and media
/// header times.
const QUICKTIME_DATES: [&str; 6] = [
    "CreateDate",
    "ModifyDate",
    "TrackCreateDate",
    "TrackModifyDate",
    "MediaCreateDate",
    "MediaModifyDate",
];

/// Turn the raw (UTC) QuickTime date tags of one exiftool record into local
/// wall-clock times with an explicit offset ("2026:04:12 13:00:00+03:00") in
/// the tools' zone. This is what `-api QuickTimeUTC` does, but exiftool does
/// it with the process `TZ`, which Windows does not understand; here the
/// zone's rules come from chrono-tz, so the result is the same on every OS.
/// Values that are not plain dates (zero dates) stay as they are.
pub(crate) fn quicktime_to_local(tools: &Tools, rec: &mut serde_json::Value) {
    let Some(obj) = rec.as_object_mut() else {
        return;
    };
    for (key, val) in obj.iter_mut() {
        let Some((group, name)) = key.rsplit_once(':') else {
            continue;
        };
        if !QUICKTIME_DATES.contains(&name) || !(group == "QuickTime" || group.starts_with("Track"))
        {
            continue;
        }
        let Some(utc) = val
            .as_str()
            .and_then(|s| chrono::NaiveDateTime::parse_from_str(s, "%Y:%m:%d %H:%M:%S").ok())
        else {
            continue;
        };
        if let Some(local) = tools.utc_to_local(utc) {
            *val = serde_json::Value::String(local.format("%Y:%m:%d %H:%M:%S%:z").to_string());
        }
    }
}

/// Write tags to `path` in place (no `_original` backup, minor errors ignored).
/// The arguments and the path go through a UTF-8 argument file (`-@`): on
/// Windows exiftool reads its command line in the ANSI code page, which
/// would turn Cyrillic or CJK values into `?`.
pub fn write(tools: &Tools, path: &Path, args: &[String]) -> Result<()> {
    write_impl(tools, None, None, path, args)
}

/// [`write`] with an exiftool config file (`-config`), e.g. one that
/// defines a custom XMP namespace.
pub fn write_with_config(tools: &Tools, config: &Path, path: &Path, args: &[String]) -> Result<()> {
    write_impl(tools, Some(config), None, path, args)
}

/// [`write`] with `-TagsFromFile from` ahead of `args`: tags are copied from
/// `from` as `args` select them.
pub fn write_from(tools: &Tools, from: &Path, path: &Path, args: &[String]) -> Result<()> {
    write_impl(tools, None, Some(from), path, args)
}

fn write_impl(
    tools: &Tools,
    config: Option<&Path>,
    from: Option<&Path>,
    path: &Path,
    args: &[String],
) -> Result<()> {
    let mut argfile = tempfile::NamedTempFile::new()?;
    let mut cmd = tools.command(&tools.exiftool);
    // `-config` only works as the very first argument.
    if let Some(config) = config {
        cmd.arg("-config").arg(config);
    }
    cmd.args(["-overwrite_original", "-m", "-charset", "filename=utf8"]);
    // A path the file cannot hold as one line stays on the command line, as
    // raw bytes; `-TagsFromFile` then goes there too, ahead of the file.
    if let Some(from) = from {
        match argfile_line(from) {
            Some(line) => writeln!(argfile, "-TagsFromFile\n{line}")?,
            None => {
                cmd.arg("-TagsFromFile").arg(from);
            }
        }
    }
    let utf8_path = argfile_line(path);
    // One argument per line: a line break inside one would split it.
    for a in args.iter().map(String::as_str) {
        writeln!(argfile, "{}", a.replace(['\r', '\n'], " "))?;
    }
    if let Some(line) = utf8_path {
        writeln!(argfile, "{line}")?;
    }
    argfile.flush()?;
    cmd.arg("-@").arg(argfile.path());
    if utf8_path.is_none() {
        cmd.arg(path);
    }
    let out = cmd.output()?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    // exiftool reports an unwritable file on stderr/stdout and may still exit 0.
    // "0 image files updated, 1 image files unchanged" means nothing to change.
    if !out.status.success()
        || (stdout.contains("0 image files updated") && !stdout.contains("image files unchanged"))
    {
        let stderr = String::from_utf8_lossy(&out.stderr);
        // The format has no writer: not a broken file, just nothing to do.
        if stderr.contains("is not yet supported") || stderr.contains("Can't currently write") {
            return Err(Error::Unwritable(format!(
                "{}: {}",
                path.display(),
                stderr.trim()
            )));
        }
        return Err(Error::Tool(crate::t!(
            "exif.write",
            path = path.display(),
            stderr = String::from_utf8_lossy(&out.stderr).trim(),
            stdout = stdout.trim()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::scan::FileEntry;
    use serde_json::json;
    use std::path::PathBuf;

    fn tools_in(tz: Option<&str>) -> Tools {
        let p = PathBuf::from("unused");
        Tools {
            ffmpeg: p.clone(),
            ffprobe: p.clone(),
            exiftool: p.clone(),
            avifenc: p.clone(),
            magick: p,
            optipng: None,
            tz: tz.map(String::from),
        }
    }

    fn record() -> serde_json::Value {
        json!({
            "SourceFile": "v.mp4",
            "QuickTime:CreateDate": "2026:04:12 10:00:00",
            "QuickTime:ModifyDate": "2026:04:12 10:00:00",
            "Track1:TrackCreateDate": "2026:04:12 10:00:00",
            "Track2:MediaModifyDate": "2026:01:10 10:00:00",
            "QuickTime:MediaCreateDate": "0000:00:00 00:00:00",
            "Keys:CreationDate": "2026:04:12 10:00:00+05:00",
            "ExifIFD:CreateDate": "2026:04:12 10:00:00",
            "ExifIFD:DateTimeOriginal": "2026:04:12 10:00:00",
        })
    }

    #[test]
    fn quicktime_utc_dates_become_zone_local_with_offset() {
        let mut r = record();
        quicktime_to_local(&tools_in(Some("Europe/Chisinau")), &mut r);
        assert_eq!(r["QuickTime:CreateDate"], "2026:04:12 13:00:00+03:00");
        assert_eq!(r["QuickTime:ModifyDate"], "2026:04:12 13:00:00+03:00");
        assert_eq!(r["Track1:TrackCreateDate"], "2026:04:12 13:00:00+03:00");
        // Winter time: +02:00.
        assert_eq!(r["Track2:MediaModifyDate"], "2026:01:10 12:00:00+02:00");
        // Zero dates, other groups and non-QuickTime tags are left alone.
        assert_eq!(r["QuickTime:MediaCreateDate"], "0000:00:00 00:00:00");
        assert_eq!(r["Keys:CreationDate"], "2026:04:12 10:00:00+05:00");
        assert_eq!(r["ExifIFD:CreateDate"], "2026:04:12 10:00:00");
        assert_eq!(r["ExifIFD:DateTimeOriginal"], "2026:04:12 10:00:00");
        assert_eq!(r["SourceFile"], "v.mp4");

        let mut r = record();
        quicktime_to_local(&tools_in(Some("Asia/Tokyo")), &mut r);
        assert_eq!(r["QuickTime:CreateDate"], "2026:04:12 19:00:00+09:00");
        let mut r = record();
        quicktime_to_local(&tools_in(Some("UTC")), &mut r);
        assert_eq!(r["QuickTime:CreateDate"], "2026:04:12 10:00:00+00:00");
    }

    #[test]
    fn unparsable_zone_leaves_dates_raw() {
        let mut r = record();
        quicktime_to_local(&tools_in(Some("Not/AZone")), &mut r);
        assert_eq!(r["QuickTime:CreateDate"], "2026:04:12 10:00:00");
    }

    /// A fake exiftool: one counter line per run, a record per listed file
    /// carrying the file's content as `Tag`.
    #[cfg(unix)]
    fn fake_exiftool(dir: &Path) -> (Tools, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let count = dir.join("runs");
        let script = dir.join("exiftool");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\necho x >> '{}'\nfor last; do :; done\nprintf '['\nsep=''\n\
                 while IFS= read -r f; do printf '%s{{\"SourceFile\":\"%s\",\"Model\":\"%s\",\
                 \"QuickTime:CreateDate\":\"2026:04:12 10:00:00\"}}' \"$sep\" \"$f\" \"$(cat \"$f\")\"; sep=,; done < \"$last\"\nprintf ']'\n",
                count.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut t = tools_in(Some("Asia/Tokyo"));
        t.exiftool = script;
        (t, count)
    }

    #[test]
    fn source_file_with_forward_slashes_finds_the_backslash_request() {
        let chunk = [PathBuf::from(r"C:\Photos\a.jpg"), PathBuf::from("b.jpg")];
        let r = Requested::with_separators(&chunk, true);
        assert_eq!(r.get("C:/Photos/a.jpg"), Some(&chunk[0]));
        assert_eq!(r.get(r"C:\Photos\a.jpg"), Some(&chunk[0]));
        assert_eq!(r.get("b.jpg"), Some(&chunk[1]));
        assert_eq!(r.get("C:/Photos/c.jpg"), None);
        // Where `\` is part of a name, only the exact spelling matches.
        let r = Requested::with_separators(&chunk, false);
        assert_eq!(r.get("C:/Photos/a.jpg"), None);
        assert_eq!(r.get(r"C:\Photos\a.jpg"), Some(&chunk[0]));
    }

    /// A fake exiftool that saves its argument file as `argfile` and its
    /// arguments, NUL-separated, as `argv`, prints `[]` and succeeds.
    #[cfg(unix)]
    fn recording_exiftool(dir: &Path) -> Tools {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join("exiftool");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nprev=\nfor a; do [ \"$prev\" = -@ ] && cp \"$a\" '{0}/argfile'; prev=$a; done\n\
                 printf '%s\\0' \"$@\" > '{0}/argv'\nprintf '[]'\necho ' 1 image files updated' >&2\n",
                dir.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut t = tools_in(None);
        t.exiftool = script;
        t
    }

    #[cfg(unix)]
    fn recorded(dir: &Path) -> (String, Vec<String>) {
        let argfile = std::fs::read_to_string(dir.join("argfile")).unwrap();
        let argv = std::fs::read_to_string(dir.join("argv")).unwrap();
        let argv = argv.split_terminator('\0').map(String::from).collect();
        (argfile, argv)
    }

    #[cfg(unix)]
    #[test]
    fn a_file_name_with_a_line_break_is_never_an_argfile_line() {
        let d = tempfile::tempdir().unwrap();
        let t = recording_exiftool(d.path());
        let plain = d.path().join("a.jpg");
        let evil = d.path().join("b\n-delete_original!\n.jpg");
        read(&t, &[plain.clone(), evil.clone()]).unwrap();
        let (argfile, argv) = recorded(d.path());
        assert_eq!(argfile, format!("{}\n", plain.display()));
        assert_eq!(argv.last().map(String::as_str), evil.to_str());
    }

    #[cfg(unix)]
    #[test]
    fn real_exiftool_reads_a_file_with_a_line_break_in_its_name() {
        let Some(exiftool) = crate::tools::find("exiftool") else {
            return;
        };
        let d = tempfile::tempdir().unwrap();
        let mut t = tools_in(None);
        t.exiftool = exiftool;
        let plain = d.path().join("a.txt");
        let evil = d.path().join("b\n-ver\n.txt");
        std::fs::write(&plain, "a").unwrap();
        std::fs::write(&evil, "b").unwrap();
        let got = read(&t, &[plain.clone(), evil.clone()]).unwrap();
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got.contains_key(&plain) && got.contains_key(&evil));
    }

    #[cfg(unix)]
    #[test]
    fn tags_from_a_file_with_a_line_break_go_on_the_command_line() {
        let d = tempfile::tempdir().unwrap();
        let t = recording_exiftool(d.path());
        let out = d.path().join("out.jpg");
        let args = ["-all:all".to_string()];
        let plain = d.path().join("in.jpg");
        write_from(&t, &plain, &out, &args).unwrap();
        let (argfile, _) = recorded(d.path());
        assert_eq!(
            argfile,
            format!(
                "-TagsFromFile\n{}\n-all:all\n{}\n",
                plain.display(),
                out.display()
            )
        );
        let evil = d.path().join("in\n-delete_original!.jpg");
        write_from(&t, &evil, &out, &args).unwrap();
        let (argfile, argv) = recorded(d.path());
        assert_eq!(argfile, format!("-all:all\n{}\n", out.display()));
        let at = argv.iter().position(|a| a == "-TagsFromFile").unwrap();
        assert_eq!(argv[at + 1], evil.to_str().unwrap());
        assert_eq!(
            argv[at + 2],
            "-@",
            "tags-from-file comes before the arguments"
        );
    }

    #[cfg(unix)]
    fn runs(count: &Path) -> usize {
        std::fs::read_to_string(count).map_or(0, |s| s.lines().count())
    }

    #[cfg(unix)]
    #[test]
    fn second_read_of_unchanged_files_runs_no_exiftool() {
        let d = tempfile::tempdir().unwrap();
        let (t, count) = fake_exiftool(d.path());
        let cache = d.path().join("cache").join("exif");
        let a = d.path().join("a.txt");
        std::fs::write(&a, "one").unwrap();
        let p = [a.clone()];
        let first = read_cached(&t, &p, &NoProgress, &Cancel::new(), Some(&cache)).unwrap();
        assert_eq!(runs(&count), 1);
        let second = read_cached(&t, &p, &NoProgress, &Cancel::new(), Some(&cache)).unwrap();
        assert_eq!(runs(&count), 1, "answered from the cache");
        assert_eq!(first, second);
        // Zone conversion is applied after the cache, with the current zone.
        assert_eq!(
            second[&a]["QuickTime:CreateDate"],
            "2026:04:12 19:00:00+09:00"
        );
        let mut other = t.clone();
        other.tz = Some("UTC".into());
        let third = read_cached(&other, &p, &NoProgress, &Cancel::new(), Some(&cache)).unwrap();
        assert_eq!(runs(&count), 1);
        assert_eq!(
            third[&a]["QuickTime:CreateDate"],
            "2026:04:12 10:00:00+00:00"
        );
    }

    #[cfg(unix)]
    #[test]
    fn rewritten_file_with_same_size_is_read_again() {
        let d = tempfile::tempdir().unwrap();
        let (t, count) = fake_exiftool(d.path());
        let cache = d.path().join("exif");
        let a = d.path().join("a.txt");
        std::fs::write(&a, "one").unwrap();
        let p = [a.clone()];
        let r = read_cached(&t, &p, &NoProgress, &Cancel::new(), Some(&cache)).unwrap();
        assert_eq!(r[&a]["Model"], "one");
        std::fs::write(&a, "two").unwrap();
        let f = std::fs::File::options().write(true).open(&a).unwrap();
        f.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(60))
            .unwrap();
        let r = read_cached(&t, &p, &NoProgress, &Cancel::new(), Some(&cache)).unwrap();
        assert_eq!(runs(&count), 2);
        assert_eq!(r[&a]["Model"], "two");
    }

    /// Entry keys over all shard files of a cache dir.
    #[cfg(unix)]
    fn cached_keys(dir: &Path) -> Vec<String> {
        let mut keys = Vec::new();
        for f in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let c: CacheFile = serde_json::from_slice(&std::fs::read(f.path()).unwrap()).unwrap();
            keys.extend(c.entries.into_keys());
        }
        keys.sort();
        keys
    }

    #[cfg(unix)]
    fn key_of(p: &Path) -> String {
        std::fs::canonicalize(p)
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    #[cfg(unix)]
    #[test]
    fn corrupt_cache_is_ignored_and_rebuilt() {
        let d = tempfile::tempdir().unwrap();
        let (t, count) = fake_exiftool(d.path());
        let cache = d.path().join("exif");
        std::fs::create_dir(&cache).unwrap();
        for i in 0..=255u8 {
            std::fs::write(cache.join(format!("{i:02x}.json")), b"{ not json").unwrap();
        }
        let a = d.path().join("a.txt");
        std::fs::write(&a, "one").unwrap();
        let p = [a.clone()];
        read_cached(&t, &p, &NoProgress, &Cancel::new(), Some(&cache)).unwrap();
        read_cached(&t, &p, &NoProgress, &Cancel::new(), Some(&cache)).unwrap();
        assert_eq!(runs(&count), 1);
        assert!(
            std::fs::read(cache.join(format!("{}.json", shard_of(&key_of(&a)))))
                .is_ok_and(|b| serde_json::from_slice::<CacheFile>(&b).is_ok())
        );
    }

    /// A fake exiftool that prints `records.json` beside it.
    #[cfg(unix)]
    fn canned_exiftool(dir: &Path, records: &serde_json::Value) -> Tools {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(dir.join("records.json"), records.to_string()).unwrap();
        let script = dir.join("exiftool");
        std::fs::write(
            &script,
            format!("#!/bin/sh\ncat '{}/records.json'\n", dir.display()),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut t = tools_in(Some("Asia/Tokyo"));
        t.exiftool = script;
        t
    }

    /// Realistic `-G1 -n` records: a phone clip (QuickTime dates, GPS,
    /// tracks, rotation, bulky maker data) and a camera photo.
    #[cfg(unix)]
    fn realistic(video: &Path, photo: &Path) -> serde_json::Value {
        json!([
            {
                "SourceFile": video.to_string_lossy(),
                "System:FileName": "v.mp4",
                "File:ImageWidth": 1920,
                "File:ImageHeight": 1080,
                "QuickTime:CreateDate": "2026:04:12 10:00:00",
                "QuickTime:ModifyDate": "2026:04:12 10:00:05",
                "QuickTime:Duration": 12.5,
                "QuickTime:Rotation": 90,
                "QuickTime:CompressorID": "hvc1",
                "QuickTime:AudioFormat": "mp4a",
                "QuickTime:AudioChannels": 2,
                "Track1:TrackCreateDate": "2026:04:12 10:00:00",
                "Track1:VideoFrameRate": 29.97,
                "Keys:Make": "Camera Co",
                "Keys:Model": "Phone 1",
                "Keys:GPSCoordinates": "48.85 2.35 35",
                "GPS:GPSLatitude": 48.8566,
                "GPS:GPSLongitude": 2.3522,
                "UserData:Thumbnail": "(Binary data 9000 bytes)",
                "Null:Empty": null,
            },
            {
                "SourceFile": photo.to_string_lossy(),
                "File:ImageWidth": 6000,
                "File:ImageHeight": 4000,
                "IFD0:Make": "Camera Co",
                "IFD0:Model": "Body 7",
                "IFD0:Software": "fw 1.2",
                "IFD0:Orientation": 6,
                "IFD0:ModifyDate": "2026:05:01 09:00:00",
                "ExifIFD:DateTimeOriginal": "2026:05:01 08:59:58",
                "ExifIFD:CreateDate": "2026:05:01 08:59:58",
                "ExifIFD:ExposureTime": 0.004,
                "MakerNotes:SerialNumber": "0123456789",
                "MakerNotes:Blob": "x".repeat(4000),
                "Composite:ImageSize": "6000 4000",
            },
        ])
    }

    #[cfg(unix)]
    #[test]
    fn cached_records_keep_only_listed_tags_and_read_like_full_ones() {
        use crate::filter::facts::facts_of;
        use crate::media::kind_of;
        let d = tempfile::tempdir().unwrap();
        let video = d.path().join("v.mp4");
        let photo = d.path().join("p.jpg");
        std::fs::write(&video, "v").unwrap();
        std::fs::write(&photo, "p").unwrap();
        let t = canned_exiftool(d.path(), &realistic(&video, &photo));
        let cache = d.path().join("cache").join("exif");
        let paths = [video.clone(), photo.clone()];
        let first = read_cached(&t, &paths, &NoProgress, &Cancel::new(), Some(&cache)).unwrap();
        let second = read_cached(&t, &paths, &NoProgress, &Cancel::new(), Some(&cache)).unwrap();
        let full = read_batches(&t, &paths, &NoProgress, &Cancel::new(), &mut |_, _| {}).unwrap();
        assert_eq!(first, second, "a first read looks like a cached one");

        // Stored: SourceFile, listed tags, and the names list; nothing else.
        for f in std::fs::read_dir(&cache).unwrap().flatten() {
            let c: CacheFile = serde_json::from_slice(&std::fs::read(f.path()).unwrap()).unwrap();
            for e in c.entries.values() {
                for (k, v) in e.rec.as_object().unwrap() {
                    let name = k.rsplit_once(':').map_or(k.as_str(), |(_, n)| n);
                    assert!(
                        k == "SourceFile" || k == PRESENT || KEPT_TAGS.contains(&name),
                        "stray tag {k}"
                    );
                    assert!(!v.to_string().contains("xxxx"), "bulk kept in {k}");
                }
            }
        }
        let bulk = std::fs::read_dir(&cache)
            .unwrap()
            .flatten()
            .map(|f| f.metadata().unwrap().len())
            .sum::<u64>();
        assert!(bulk < 3000, "cache of two records is {bulk} bytes");

        for p in &paths {
            let e = FileEntry {
                source_id: "camera".into(),
                path: p.clone(),
                rel: p.file_name().unwrap().into(),
                size: 1,
                mtime_ns: 0,
            };
            let tz = Some(chrono_tz::Asia::Tokyo);
            for got in [&first[p], &second[p]] {
                assert_eq!(facts_of(&e, got, tz), facts_of(&e, &full[p], tz), "{p:?}");
                let name = e.rel.to_string_lossy();
                let cap = |x| crate::datetime::capture_date(&name, kind_of(p), x, tz);
                assert_eq!(cap(got), cap(&full[p]));
                assert_eq!(got["SourceFile"], full[p]["SourceFile"]);
            }
        }
        let f = facts_of(
            &FileEntry {
                source_id: "camera".into(),
                path: video.clone(),
                rel: "v.mp4".into(),
                size: 1,
                mtime_ns: 0,
            },
            &second[&video],
            Some(chrono_tz::Asia::Tokyo),
        );
        assert_eq!(f.gps, Some((48.8566, 2.3522)));
        assert_eq!(
            f.capture.map(|c| c.to_string()).as_deref(),
            Some("2026-04-12 19:00:00")
        );
        assert_eq!((f.width, f.height), (Some(1080), Some(1920)), "rotated");
    }

    #[cfg(unix)]
    #[test]
    fn has_tag_of_an_unlisted_tag_survives_the_cache() {
        use crate::filter::facts::has_tag_in_record;
        let d = tempfile::tempdir().unwrap();
        let video = d.path().join("v.mp4");
        let photo = d.path().join("p.jpg");
        std::fs::write(&video, "v").unwrap();
        std::fs::write(&photo, "p").unwrap();
        let t = canned_exiftool(d.path(), &realistic(&video, &photo));
        let cache = d.path().join("exif");
        let paths = [video, photo.clone()];
        read_cached(&t, &paths, &NoProgress, &Cancel::new(), Some(&cache)).unwrap();
        let hit = read_cached(&t, &paths, &NoProgress, &Cancel::new(), Some(&cache)).unwrap();
        let r = &hit[&photo];
        assert!(has_tag_in_record(r, "ExposureTime"));
        assert!(has_tag_in_record(r, "ExifIFD:exposuretime"));
        assert!(!has_tag_in_record(r, "IFD0:ExposureTime"));
        assert!(!has_tag_in_record(r, "Flash"));
    }

    #[cfg(unix)]
    #[test]
    fn a_record_with_an_exiftool_error_is_not_cached() {
        let d = tempfile::tempdir().unwrap();
        let bad = d.path().join("bad.jpg");
        let good = d.path().join("good.jpg");
        std::fs::write(&bad, "b").unwrap();
        std::fs::write(&good, "g").unwrap();
        let recs = json!([
            { "SourceFile": bad.to_string_lossy(), "ExifTool:Error": "File format error" },
            { "SourceFile": good.to_string_lossy(), "IFD0:Make": "Camera Co" },
        ]);
        let t = canned_exiftool(d.path(), &recs);
        let cache = d.path().join("exif");
        read_cached(
            &t,
            &[bad.clone(), good.clone()],
            &NoProgress,
            &Cancel::new(),
            Some(&cache),
        )
        .unwrap();
        assert_eq!(cached_keys(&cache), [key_of(&good)]);
    }

    #[cfg(unix)]
    #[test]
    fn old_full_record_shards_are_discarded() {
        let d = tempfile::tempdir().unwrap();
        let (t, count) = fake_exiftool(d.path());
        let cache = d.path().join("exif");
        std::fs::create_dir(&cache).unwrap();
        let a = d.path().join("a.txt");
        std::fs::write(&a, "one").unwrap();
        let (size, mtime_ns) = stat(&a).unwrap();
        let k = key_of(&a);
        let old = json!({
            "tool": tool_identity(&t).replacen("3|", "1|", 1),
            "entries": { k.clone(): { "size": size, "mtime_ns": mtime_ns,
                "rec": { "SourceFile": "x", "MakerNotes:Blob": "stale" } } },
        });
        std::fs::write(
            cache.join(format!("{}.json", shard_of(&k))),
            old.to_string(),
        )
        .unwrap();
        let r = read_cached(
            &t,
            std::slice::from_ref(&a),
            &NoProgress,
            &Cancel::new(),
            Some(&cache),
        )
        .unwrap();
        assert_eq!(runs(&count), 1, "old identity: read again");
        assert!(r[&a].get("MakerNotes:Blob").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn no_cache_dir_reads_without_writing() {
        let d = tempfile::tempdir().unwrap();
        let (t, count) = fake_exiftool(d.path());
        let a = d.path().join("a.txt");
        std::fs::write(&a, "one").unwrap();
        read_cached(
            &t,
            std::slice::from_ref(&a),
            &NoProgress,
            &Cancel::new(),
            None,
        )
        .unwrap();
        read_cached(&t, &[a], &NoProgress, &Cancel::new(), None).unwrap();
        assert_eq!(runs(&count), 2);
    }

    #[cfg(unix)]
    #[test]
    fn saving_prunes_gone_files_in_touched_shards_only() {
        let d = tempfile::tempdir().unwrap();
        let (t, _) = fake_exiftool(d.path());
        let cache = d.path().join("exif");
        let mk = |n: &str| {
            let p = d.path().join(n);
            std::fs::write(&p, n).unwrap();
            p
        };
        let b = mk("b.txt");
        // A file in b's shard, and one in another shard.
        let same = (0..)
            .map(|i| mk(&format!("s{i}.txt")))
            .find(|p| shard_of(&key_of(p)) == shard_of(&key_of(&b)));
        let same = same.unwrap();
        let other = (0..)
            .map(|i| mk(&format!("o{i}.txt")))
            .find(|p| shard_of(&key_of(p)) != shard_of(&key_of(&b)))
            .unwrap();
        let other2 = (0..)
            .map(|i| mk(&format!("q{i}.txt")))
            .find(|p| {
                shard_of(&key_of(p)) != shard_of(&key_of(&b))
                    && shard_of(&key_of(p)) != shard_of(&key_of(&other))
            })
            .unwrap();
        let all = [b.clone(), other.clone(), other2.clone()];
        read_cached(&t, &all, &NoProgress, &Cancel::new(), Some(&cache)).unwrap();
        assert_eq!(cached_keys(&cache).len(), 3);
        std::fs::remove_file(&b).unwrap();
        std::fs::remove_file(&other2).unwrap();
        read_cached(&t, &[same], &NoProgress, &Cancel::new(), Some(&cache)).unwrap();
        let keys = cached_keys(&cache);
        assert!(!keys.contains(&key_of_gone(&b)), "b pruned from its shard");
        assert_eq!(
            keys.len(),
            3,
            "other2 in an untouched shard stays: {keys:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn saving_keeps_entries_of_an_unmounted_folder() {
        let d = tempfile::tempdir().unwrap();
        let cache = d.path().join("exif");
        let entry = |k: &str| {
            (
                k.to_string(),
                Entry {
                    size: 1,
                    mtime_ns: 1,
                    rec: json!({}),
                },
            )
        };
        let away = d.path().join("sd-card/a.jpg");
        let here_gone = d.path().join("gone.jpg");
        let ka = away.to_string_lossy().into_owned();
        let kg = here_gone.to_string_lossy().into_owned();
        let shard = shard_of(&ka);
        let mut batch = HashMap::from([(shard.clone(), vec![entry(&ka)])]);
        batch.entry(shard_of(&kg)).or_default().push(entry(&kg));
        save_shards(&cache, "T", batch);
        let keys = cached_keys(&cache);
        assert!(keys.contains(&ka), "unmounted folder kept: {keys:?}");
        assert!(!keys.contains(&kg), "gone from a present folder: {keys:?}");
    }

    #[cfg(unix)]
    fn key_of_gone(p: &Path) -> String {
        std::fs::canonicalize(p.parent().unwrap())
            .unwrap()
            .join(p.file_name().unwrap())
            .to_string_lossy()
            .into_owned()
    }

    #[cfg(unix)]
    #[test]
    fn save_merges_with_what_another_process_wrote_meanwhile() {
        let d = tempfile::tempdir().unwrap();
        let cache = d.path().join("exif");
        let a = d.path().join("a.txt");
        std::fs::write(&a, "a").unwrap();
        let ka = key_of(&a);
        let entry = |k: &str| {
            (
                k.to_string(),
                Entry {
                    size: 1,
                    mtime_ns: 1,
                    rec: json!({}),
                },
            )
        };
        // Another process stored `a` first; we store `b` in the same shard.
        let shard = shard_of(&ka);
        save_shards(
            &cache,
            "T",
            HashMap::from([(shard.clone(), vec![entry(&ka)])]),
        );
        let b = (0..)
            .map(|i| d.path().join(format!("b{i}.txt")))
            .find(|p| {
                std::fs::write(p, "b").unwrap();
                shard_of(&key_of(p)) == shard
            })
            .unwrap();
        let kb = key_of(&b);
        save_shards(&cache, "T", HashMap::from([(shard, vec![entry(&kb)])]));
        let keys = cached_keys(&cache);
        assert!(keys.contains(&ka) && keys.contains(&kb), "{keys:?}");
    }
}
