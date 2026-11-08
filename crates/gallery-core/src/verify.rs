//! Before/after verification of a produced output: what a content pipe must
//! preserve (capture date, mtime, GPS, dimensions, duration) is compared
//! between the source and the staged output before anything is placed and
//! before the source goes to the trash. A difference is an [`Anomaly`].

use std::path::Path;
use std::time::SystemTime;

use serde_json::Value;

use crate::datetime::tag_dates;
use crate::media::{MediaKind, kind_of};
use crate::tools::Tools;
use crate::{Result, exif};

/// Allowed distance between the output's mtime and the expected one.
const MTIME_SLACK_SECS: f64 = 2.0;
/// Allowed difference of video and audio durations.
const DURATION_SLACK_SECS: f64 = 1.0;
/// Allowed relative difference of the aspect ratio of a resized image.
const ASPECT_SLACK: f64 = 0.01;

/// Tag names (any group) that hold a location.
const GPS_NAMES: [&str; 5] = [
    "GPSLatitude",
    "GPSLongitude",
    "GPSPosition",
    "GPSCoordinates",
    "Location",
];

/// What the pipe was supposed to deliver.
pub struct Expected<'a> {
    /// exiftool record of the source (`exif::read` format).
    pub source_exif: &'a Value,
    /// Wanted mtime of the output: the source's, or the capture date when a
    /// metadata step wrote it.
    pub mtime: SystemTime,
    /// The capture date a metadata step of the pipe wrote (it wins over the
    /// source's own tags); `None`: the output keeps the source's date.
    pub capture: Option<chrono::NaiveDateTime>,
    /// A preset of the pipe downsizes the image or video.
    pub resizes: bool,
    /// Extension of the output without the dot (`av1.mp4` is fine).
    pub ext: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnomalyKind {
    Empty,
    Unreadable,
    CaptureDate,
    Mtime,
    Gps,
    Dimensions,
    Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Anomaly {
    pub kind: AnomalyKind,
    /// Russian, for the report.
    pub message: String,
}

fn anomaly(kind: AnomalyKind, message: String) -> Anomaly {
    Anomaly { kind, message }
}

/// Whether the output format can carry the capture date and the location.
fn carries_meta(ext: &str) -> bool {
    let last = ext.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    matches!(
        last.as_str(),
        "jpg"
            | "jpeg"
            | "png"
            | "avif"
            | "webp"
            | "heic"
            | "heif"
            | "tif"
            | "tiff"
            | "mp4"
            | "m4v"
            | "mov"
    )
}

/// Whether the exiftool record holds a location (GPS tags of any group).
pub fn has_gps(rec: &Value) -> bool {
    rec.as_object().is_some_and(|o| {
        o.iter().any(|(k, v)| {
            let name = k.rsplit_once(':').map_or(k.as_str(), |(_, n)| n);
            GPS_NAMES.contains(&name) && !v.is_null()
        })
    })
}

/// The capture dates a file may carry: DateTimeOriginal first; videos also
/// CreateDate (QuickTime, local wall clock thanks to `-api QuickTimeUTC`).
fn dates(rec: &Value) -> Vec<chrono::NaiveDateTime> {
    let mut v = tag_dates(rec, "DateTimeOriginal");
    v.extend(tag_dates(rec, "CreateDate"));
    v
}

/// The capture date of an exiftool record: DateTimeOriginal, else CreateDate
/// (wall clock, offsets dropped).
pub fn capture_of(rec: &Value) -> Option<chrono::NaiveDateTime> {
    dates(rec).first().copied()
}

fn image_size(rec: &Value) -> Option<(u32, u32)> {
    let s = rec.get("Composite:ImageSize")?;
    let text = s
        .as_str()
        .map(str::to_string)
        .or_else(|| s.as_f64().map(|n| n.to_string()))?;
    let mut it = text.split(['x', ' ']).filter_map(|p| p.parse::<u32>().ok());
    Some((it.next()?, it.next()?))
}

#[derive(Debug, Default)]
struct Probe {
    duration: Option<f64>,
    size: Option<(u32, u32)>,
}

fn probe(tools: &Tools, path: &Path) -> Result<Probe> {
    let out = tools
        .command(&tools.ffprobe)
        .args([
            "-v",
            "error",
            "-show_streams",
            "-show_format",
            "-of",
            "json",
        ])
        .arg(path)
        .output()?;
    if !out.status.success() {
        return Err(crate::Error::Tool(crate::t!(
            "verify.ffprobe",
            error = String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let v: Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| crate::Error::Tool(crate::t!("verify.ffprobe_output", error = e)))?;
    let size = v["streams"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|s| s["codec_type"] == "video")
        .and_then(|s| {
            Some((
                u32::try_from(s["width"].as_u64()?).ok()?,
                u32::try_from(s["height"].as_u64()?).ok()?,
            ))
        });
    Ok(Probe {
        duration: v["format"]["duration"]
            .as_str()
            .and_then(|d| d.parse().ok()),
        size,
    })
}

/// Compare dimensions; rotation does not matter (sides are ordered), a
/// resize only has to keep the aspect ratio.
fn dims_differ(src: (u32, u32), out: (u32, u32), resizes: bool) -> bool {
    let order = |(w, h): (u32, u32)| (w.min(h), w.max(h));
    let (s, o) = (order(src), order(out));
    if !resizes {
        return s != o;
    }
    if s.0 == 0 || o.0 == 0 {
        return true;
    }
    let (a, b) = (
        f64::from(s.1) / f64::from(s.0),
        f64::from(o.1) / f64::from(o.0),
    );
    // A rounded side may be off by a pixel.
    let slack = ASPECT_SLACK.max(1.0 / f64::from(o.0));
    (a - b).abs() / a > slack
}

fn secs(t: SystemTime) -> f64 {
    match t.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => d.as_secs_f64(),
        Err(e) => -e.duration().as_secs_f64(),
    }
}

/// Compare the staged output with what the source says it must keep. An empty
/// list means no difference. Costs one exiftool read of the staged file (and
/// ffprobe for video and audio).
pub fn compare(tools: &Tools, source: &Path, staged: &Path, expected: &Expected) -> Vec<Anomaly> {
    let mut out = Vec::new();
    match std::fs::metadata(staged) {
        Ok(m) if m.len() > 0 => {}
        Ok(_) => return vec![anomaly(AnomalyKind::Empty, crate::t!("verify.empty"))],
        Err(e) => {
            return vec![anomaly(
                AnomalyKind::Unreadable,
                crate::t!("verify.unreadable", error = e),
            )];
        }
    }
    let rec = match exif::read(tools, &[staged.to_path_buf()]) {
        Ok(mut m) => m.remove(staged),
        Err(e) => {
            return vec![anomaly(
                AnomalyKind::Unreadable,
                crate::t!("verify.unreadable", error = e),
            )];
        }
    };
    let Some(rec) = rec.filter(|r| r.get("ExifTool:Error").is_none()) else {
        return vec![anomaly(
            AnomalyKind::Unreadable,
            crate::t!("verify.unreadable_exiftool"),
        )];
    };
    let src = expected.source_exif;
    let kind = kind_of(source);
    let carries = carries_meta(expected.ext);

    // Capture date: the first date of the source must be among the output's.
    if carries && matches!(kind, MediaKind::Photo | MediaKind::Png | MediaKind::Video) {
        let wanted = match kind {
            _ if expected.capture.is_some() => expected.capture,
            MediaKind::Video => tag_dates(src, "DateTimeOriginal")
                .into_iter()
                .chain(tag_dates(src, "CreateDate"))
                .next(),
            _ => tag_dates(src, "DateTimeOriginal").into_iter().next(),
        };
        if let Some(w) = wanted {
            let got = dates(&rec);
            if !got.contains(&w) {
                let got = got
                    .first()
                    .map_or_else(|| crate::t!("verify.none"), |d| d.to_string());
                out.push(anomaly(
                    AnomalyKind::CaptureDate,
                    crate::t!("verify.capture_date", source = w, result = got),
                ));
            }
        }
    }

    // mtime
    match std::fs::metadata(staged).and_then(|m| m.modified()) {
        Ok(m) if (secs(m) - secs(expected.mtime)).abs() <= MTIME_SLACK_SECS => {}
        Ok(m) => out.push(anomaly(
            AnomalyKind::Mtime,
            crate::t!(
                "verify.mtime",
                got = chrono::DateTime::<chrono::Local>::from(m).format("%Y-%m-%d %H:%M:%S"),
                expected = chrono::DateTime::<chrono::Local>::from(expected.mtime)
                    .format("%Y-%m-%d %H:%M:%S")
            ),
        )),
        Err(e) => out.push(anomaly(
            AnomalyKind::Unreadable,
            crate::t!("verify.mtime_unreadable", error = e),
        )),
    }

    // GPS
    if carries && has_gps(src) && !has_gps(&rec) {
        out.push(anomaly(AnomalyKind::Gps, crate::t!("verify.gps")));
    }

    // Dimensions and duration.
    match kind {
        MediaKind::Photo | MediaKind::Png => {
            if let Some(s) = image_size(src) {
                match image_size(&rec) {
                    Some(o) if dims_differ(s, o, expected.resizes) => out.push(anomaly(
                        AnomalyKind::Dimensions,
                        crate::t!("verify.size", w1 = s.0, h1 = s.1, w2 = o.0, h2 = o.1),
                    )),
                    Some(_) => {}
                    None => out.push(anomaly(
                        AnomalyKind::Unreadable,
                        crate::t!("verify.size_unreadable", w = s.0, h = s.1),
                    )),
                }
            }
        }
        MediaKind::Video | MediaKind::Audio => {
            // A source ffprobe cannot read has nothing to compare with.
            if let Ok(s) = probe(tools, source) {
                match probe(tools, staged) {
                    Err(e) => out.push(anomaly(
                        AnomalyKind::Unreadable,
                        crate::t!("verify.unreadable", error = e),
                    )),
                    Ok(o) => {
                        if let (Some(a), Some(b)) = (s.duration, o.duration)
                            && (a - b).abs() > DURATION_SLACK_SECS
                        {
                            out.push(anomaly(
                                AnomalyKind::Duration,
                                crate::t!(
                                    "verify.duration",
                                    from = crate::i18n::decimal(a, 1),
                                    to = crate::i18n::decimal(b, 1)
                                ),
                            ));
                        } else if s.duration.is_some() && o.duration.is_none() {
                            out.push(anomaly(
                                AnomalyKind::Duration,
                                crate::t!("verify.duration_unknown"),
                            ));
                        }
                        if kind == MediaKind::Video
                            && let (Some(a), Some(b)) = (s.size, o.size)
                            && dims_differ(a, b, expected.resizes)
                        {
                            out.push(anomaly(
                                AnomalyKind::Dimensions,
                                crate::t!("verify.size", w1 = a.0, h1 = a.1, w2 = b.0, h2 = b.1),
                            ));
                        }
                    }
                }
            }
        }
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;
    use std::process::Command;

    #[test]
    fn dims_rules() {
        assert!(!dims_differ((640, 480), (480, 640), false));
        assert!(dims_differ((640, 480), (320, 240), false));
        assert!(!dims_differ((640, 480), (320, 240), true));
        assert!(!dims_differ((4000, 3000), (1600, 1200), true));
        assert!(dims_differ((640, 480), (320, 320), true));
        // One pixel of rounding on a small side.
        assert!(!dims_differ((101, 75), (50, 37), true));
    }

    #[test]
    fn gps_names_any_group() {
        assert!(has_gps(&json!({"Keys:Location": "+1+2/"})));
        assert!(has_gps(&json!({"GPS:GPSLatitude": 1.0})));
        assert!(!has_gps(&json!({"GPS:GPSLatitude": null, "File:X": 1})));
        assert!(!has_gps(&json!({})));
    }

    #[test]
    fn carriers() {
        for e in ["avif", "WEBP", "av1.mp4", "jpg", "png"] {
            assert!(carries_meta(e), "{e}");
        }
        assert!(!carries_meta("mkv"));
        assert!(!carries_meta("opus"));
    }

    fn have(tool: &str) -> bool {
        crate::tools::find(tool).is_some()
    }

    fn tools() -> Tools {
        Tools::discover().unwrap()
    }

    fn make_jpeg(dir: &Path, name: &str, extra: &[&str]) -> PathBuf {
        let p = dir.join(name);
        assert!(
            Command::new("magick")
                .args(["-size", "64x48", "gradient:red-blue"])
                .arg(&p)
                .status()
                .unwrap()
                .success()
        );
        if !extra.is_empty() {
            assert!(
                Command::new("exiftool")
                    .args(["-overwrite_original", "-q"])
                    .args(extra)
                    .arg(&p)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        p
    }

    fn exif_of(t: &Tools, p: &Path) -> Value {
        exif::read(t, &[p.to_path_buf()])
            .unwrap()
            .remove(p)
            .unwrap()
    }

    fn mtime(p: &Path) -> SystemTime {
        std::fs::metadata(p).unwrap().modified().unwrap()
    }

    fn kinds(v: &[Anomaly]) -> Vec<AnomalyKind> {
        v.iter().map(|a| a.kind).collect()
    }

    #[test]
    fn identical_copy_is_clean_and_each_loss_is_named() {
        if !(have("magick") && have("exiftool") && have("ffprobe")) {
            eprintln!("skip: tools not found");
            return;
        }
        let t = tools();
        let d = tempfile::tempdir().unwrap();
        let src = make_jpeg(
            d.path(),
            "a.jpg",
            &[
                "-DateTimeOriginal=2026:04:12 10:33:14",
                "-GPSLatitude=10",
                "-GPSLatitudeRef=N",
                "-GPSLongitude=20",
                "-GPSLongitudeRef=E",
            ],
        );
        let rec = exif_of(&t, &src);
        let ex = |mtime, resizes| Expected {
            source_exif: &rec,
            mtime,
            capture: None,
            resizes,
            ext: "jpg",
        };
        let out = d.path().join("out.jpg");
        std::fs::copy(&src, &out).unwrap();
        assert_eq!(compare(&t, &src, &out, &ex(mtime(&out), false)), vec![]);

        // wrong mtime
        let wrong = mtime(&out) + std::time::Duration::from_secs(60);
        assert_eq!(
            kinds(&compare(&t, &src, &out, &ex(wrong, false))),
            [AnomalyKind::Mtime]
        );
        // within the slack
        let near = mtime(&out) + std::time::Duration::from_secs(1);
        assert!(compare(&t, &src, &out, &ex(near, false)).is_empty());

        // date lost (mtime unchanged by exiftool? restore it)
        let m = mtime(&out);
        assert!(
            Command::new("exiftool")
                .args(["-overwrite_original", "-q", "-DateTimeOriginal=", "-P"])
                .arg(&out)
                .status()
                .unwrap()
                .success()
        );
        let a = compare(&t, &src, &out, &ex(m, false));
        assert_eq!(kinds(&a), [AnomalyKind::CaptureDate], "{a:?}");

        // GPS lost
        assert!(
            Command::new("exiftool")
                .args(["-overwrite_original", "-q", "-gps:all=", "-P"])
                .arg(&out)
                .status()
                .unwrap()
                .success()
        );
        let a = compare(&t, &src, &out, &ex(m, false));
        assert!(kinds(&a).contains(&AnomalyKind::Gps), "{a:?}");

        // dimensions: a 32x24 copy is fine only when resizing
        let small = d.path().join("small.jpg");
        assert!(
            Command::new("magick")
                .arg(&src)
                .args(["-resize", "50%"])
                .arg(&small)
                .status()
                .unwrap()
                .success()
        );
        let both = |resizes| {
            kinds(&compare(&t, &src, &small, &ex(mtime(&small), resizes)))
                .into_iter()
                .filter(|k| *k == AnomalyKind::Dimensions)
                .count()
        };
        assert_eq!(both(false), 1);
        assert_eq!(both(true), 0);

        // empty and unreadable
        let empty = d.path().join("empty.jpg");
        std::fs::write(&empty, b"").unwrap();
        assert_eq!(
            kinds(&compare(&t, &src, &empty, &ex(m, false))),
            [AnomalyKind::Empty]
        );
        let junk = d.path().join("junk.jpg");
        std::fs::write(&junk, b"not an image at all").unwrap();
        assert!(kinds(&compare(&t, &src, &junk, &ex(m, false))).contains(&AnomalyKind::Unreadable));
    }

    #[test]
    fn video_duration_is_compared() {
        if !(have("ffmpeg") && have("ffprobe") && have("exiftool")) {
            eprintln!("skip: tools not found");
            return;
        }
        let t = tools();
        let d = tempfile::tempdir().unwrap();
        let mk = |name: &str, secs: &str| {
            let p = d.path().join(name);
            assert!(
                Command::new("ffmpeg")
                    .args(["-hide_banner", "-loglevel", "error", "-y"])
                    .args(["-f", "lavfi", "-i", "testsrc2=size=160x120:rate=10"])
                    .args(["-t", secs, "-c:v", "libx264", "-preset", "ultrafast"])
                    .args(["-pix_fmt", "yuv420p"])
                    .arg(&p)
                    .status()
                    .unwrap()
                    .success()
            );
            p
        };
        let (long, same, short) = (mk("l.mp4", "3"), mk("s.mp4", "3"), mk("c.mp4", "1"));
        let rec = exif_of(&t, &long);
        let ex = |p: &Path| Expected {
            source_exif: &rec,
            mtime: mtime(p),
            capture: None,
            resizes: false,
            ext: "mp4",
        };
        assert_eq!(compare(&t, &long, &same, &ex(&same)), vec![]);
        assert_eq!(
            kinds(&compare(&t, &long, &short, &ex(&short))),
            [AnomalyKind::Duration]
        );
    }
}
