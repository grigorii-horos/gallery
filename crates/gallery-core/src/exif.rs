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
    read_batches(tools, paths, &NoProgress, &Cancel::new(), &mut |_| {})
}

/// [`read`] answered from the persistent cache `<cache dir>/exif-cache.json`
/// where a file's path, size and mtime are unchanged; the rest is read in
/// batches, with a [`Stage::Exif`] event per batch, and stored. A cancel is
/// checked before each batch and ends the read with `Error::Cancelled`.
/// Without a cache dir nothing is persisted.
pub fn read_with(
    tools: &Tools,
    paths: &[PathBuf],
    progress: &dyn Progress,
    cancel: &Cancel,
) -> Result<HashMap<PathBuf, serde_json::Value>> {
    let file = crate::paths::cache_dir().map(|d| d.join("exif-cache.json"));
    read_cached(tools, paths, progress, cancel, file.as_deref())
}

#[derive(Serialize, Deserialize)]
struct Entry {
    size: u64,
    mtime_ns: u128,
    /// The raw exiftool record, before the QuickTime zone conversion.
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
        "1|{}|{stamp}|{}",
        tools.exiftool.display(),
        READ_ARGS.join(" ")
    )
}

/// Canonical path, size and mtime in nanoseconds: the cache key and check.
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

fn load_cache(file: Option<&Path>, tool: &str) -> CacheFile {
    let loaded: Option<CacheFile> = file
        .and_then(|f| std::fs::read(f).ok())
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
    cache_file: Option<&Path>,
) -> Result<HashMap<PathBuf, serde_json::Value>> {
    let Some(cache_file) = cache_file else {
        return read_batches(tools, paths, progress, cancel, &mut |_| {});
    };
    if !paths.is_empty() {
        cancel.check()?;
    }
    let tool = tool_identity(tools);
    let mut cache = load_cache(Some(cache_file), &tool);
    let mut result = HashMap::new();
    // Misses: path, cache key and the stat taken before the read, so a file
    // changed while exiftool runs is re-read next time.
    let mut misses: Vec<(PathBuf, Option<Stamp>)> = Vec::new();
    for p in paths {
        let key = std::fs::canonicalize(p).ok().zip(stat(p));
        let key = key.map(|(c, (s, m))| (c.to_string_lossy().into_owned(), s, m));
        let hit = key.as_ref().and_then(|(k, s, m)| {
            cache
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
    let hits = result.len();
    let mut dirty = false;
    let stats: HashMap<PathBuf, Stamp> = misses
        .iter()
        .filter_map(|(p, k)| k.clone().map(|k| (p.clone(), k)))
        .collect();
    let miss_paths: Vec<PathBuf> = misses.into_iter().map(|(p, _)| p).collect();
    let shifted = Shifted { progress, hits };
    let mut store = |raw: &serde_json::Value| {
        let Some(src) = raw.get("SourceFile").and_then(|s| s.as_str()) else {
            return;
        };
        if raw.get("Error").is_some() {
            return;
        }
        if let Some((k, size, mtime_ns)) = stats.get(Path::new(src)) {
            cache.entries.insert(
                k.clone(),
                Entry {
                    size: *size,
                    mtime_ns: *mtime_ns,
                    rec: raw.clone(),
                },
            );
            dirty = true;
        }
    };
    let fresh = read_batches(tools, &miss_paths, &shifted, cancel, &mut store);
    // Keep what was read even if a later batch failed or was cancelled.
    let fresh = match fresh {
        Ok(f) => f,
        Err(e) => {
            if dirty {
                save_cache(cache_file, &mut cache);
            }
            return Err(e);
        }
    };
    result.extend(fresh);
    if miss_paths.is_empty() && !paths.is_empty() {
        progress.step(ProgressEvent {
            stage: Stage::Exif,
            done: paths.len(),
            total: paths.len(),
            current: paths.last().cloned(),
        });
    }
    if dirty {
        save_cache(cache_file, &mut cache);
    }
    Ok(result)
}

/// Prune entries of vanished files, then write the cache atomically. A
/// failure to write only costs the next run a re-read.
fn save_cache(file: &Path, cache: &mut CacheFile) {
    cache.entries.retain(|k, _| Path::new(k).exists());
    let Ok(bytes) = serde_json::to_vec(cache) else {
        return;
    };
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = crate::paths::write_atomic(file, &bytes);
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
/// printed it, before the QuickTime zone conversion applied to the result.
fn read_batches(
    tools: &Tools,
    paths: &[PathBuf],
    progress: &dyn Progress,
    cancel: &Cancel,
    raw: &mut dyn FnMut(&serde_json::Value),
) -> Result<HashMap<PathBuf, serde_json::Value>> {
    let mut result = HashMap::new();
    let mut done = 0;
    for chunk in paths.chunks(BATCH) {
        cancel.check()?;
        let mut argfile = tempfile::NamedTempFile::new()?;
        for p in chunk {
            writeln!(argfile, "{}", p.display())?;
        }
        argfile.flush()?;
        let out = tools
            .command(&tools.exiftool)
            .args(READ_ARGS)
            .arg("-@")
            .arg(argfile.path())
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
            raw(&v);
            quicktime_to_local(tools, &mut v);
            if let Some(src) = v.get("SourceFile").and_then(|s| s.as_str()) {
                result.insert(PathBuf::from(src), v.clone());
            }
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
pub fn write(tools: &Tools, path: &Path, args: &[String]) -> Result<()> {
    let out = tools
        .command(&tools.exiftool)
        .args(["-overwrite_original", "-m", "-charset", "filename=utf8"])
        .args(args)
        .arg(path)
        .output()?;
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
                 while IFS= read -r f; do printf '%s{{\"SourceFile\":\"%s\",\"Tag\":\"%s\",\
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

    #[cfg(unix)]
    fn runs(count: &Path) -> usize {
        std::fs::read_to_string(count).map_or(0, |s| s.lines().count())
    }

    #[cfg(unix)]
    #[test]
    fn second_read_of_unchanged_files_runs_no_exiftool() {
        let d = tempfile::tempdir().unwrap();
        let (t, count) = fake_exiftool(d.path());
        let cache = d.path().join("cache").join("exif-cache.json");
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
        let cache = d.path().join("exif-cache.json");
        let a = d.path().join("a.txt");
        std::fs::write(&a, "one").unwrap();
        let p = [a.clone()];
        let r = read_cached(&t, &p, &NoProgress, &Cancel::new(), Some(&cache)).unwrap();
        assert_eq!(r[&a]["Tag"], "one");
        std::fs::write(&a, "two").unwrap();
        let f = std::fs::File::options().write(true).open(&a).unwrap();
        f.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(60))
            .unwrap();
        let r = read_cached(&t, &p, &NoProgress, &Cancel::new(), Some(&cache)).unwrap();
        assert_eq!(runs(&count), 2);
        assert_eq!(r[&a]["Tag"], "two");
    }

    #[cfg(unix)]
    #[test]
    fn corrupt_cache_is_ignored_and_rebuilt() {
        let d = tempfile::tempdir().unwrap();
        let (t, count) = fake_exiftool(d.path());
        let cache = d.path().join("exif-cache.json");
        std::fs::write(&cache, b"{ not json").unwrap();
        let a = d.path().join("a.txt");
        std::fs::write(&a, "one").unwrap();
        let p = [a.clone()];
        read_cached(&t, &p, &NoProgress, &Cancel::new(), Some(&cache)).unwrap();
        read_cached(&t, &p, &NoProgress, &Cancel::new(), Some(&cache)).unwrap();
        assert_eq!(runs(&count), 1);
        assert!(serde_json::from_slice::<CacheFile>(&std::fs::read(&cache).unwrap()).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn no_cache_file_reads_without_writing_and_saving_prunes_gone_files() {
        let d = tempfile::tempdir().unwrap();
        let (t, count) = fake_exiftool(d.path());
        let a = d.path().join("a.txt");
        let b = d.path().join("b.txt");
        std::fs::write(&a, "one").unwrap();
        std::fs::write(&b, "two").unwrap();
        let both = [a.clone(), b.clone()];
        read_cached(&t, &both, &NoProgress, &Cancel::new(), None).unwrap();
        read_cached(&t, &both, &NoProgress, &Cancel::new(), None).unwrap();
        assert_eq!(runs(&count), 2);

        let cache = d.path().join("exif-cache.json");
        read_cached(&t, &both, &NoProgress, &Cancel::new(), Some(&cache)).unwrap();
        let load = |c: &Path| serde_json::from_slice::<CacheFile>(&std::fs::read(c).unwrap());
        assert_eq!(load(&cache).unwrap().entries.len(), 2);
        std::fs::remove_file(&b).unwrap();
        let c = d.path().join("c.txt");
        std::fs::write(&c, "three").unwrap();
        read_cached(&t, &[c], &NoProgress, &Cancel::new(), Some(&cache)).unwrap();
        assert_eq!(load(&cache).unwrap().entries.len(), 2, "b pruned, c added");
    }
}
