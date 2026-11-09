//! exiftool wrapper: batched JSON reads and single-file writes.

use crate::progress::{Cancel, NoProgress, Progress, ProgressEvent, Stage};
use crate::tools::Tools;
use crate::{Error, Result};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

const BATCH: usize = 500;

/// Read metadata for `paths`. Keys are `Group:Tag` (`-G1`), values numeric (`-n`).
pub fn read(tools: &Tools, paths: &[PathBuf]) -> Result<HashMap<PathBuf, serde_json::Value>> {
    read_with(tools, paths, &NoProgress, &Cancel::new())
}

/// [`read`] with a [`Stage::Exif`] event per batch of files read; a cancel
/// is checked before each batch and ends the read with `Error::Cancelled`.
pub fn read_with(
    tools: &Tools,
    paths: &[PathBuf],
    progress: &dyn Progress,
    cancel: &Cancel,
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
            .args(["-j", "-n", "-G1", "-api", "LargeFileSupport=1"])
            .args(["-charset", "filename=utf8", "-@"])
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
}
