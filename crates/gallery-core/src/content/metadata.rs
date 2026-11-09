//! Metadata preset: a port of the earlier `prepare-photos-meta` script (camera, XMP
//! and time-zone fixes) plus the date writing of the earlier `prepare-media` shell script.
//!
//! The fixes are planned from the ORIGINAL's exiftool record (like the
//! script's `--output` mode, where a converted file gets the source's fixes)
//! and applied to a copy of the step input. The capture time itself is never
//! changed by the fixes; only the zone (OffsetTime*) is filled in.

use super::{ContentStep, StepCtx, StepResult, copy_keep_mtime, tmp_output};
use crate::config::MetadataPreset;
use crate::datetime::parse_tag;
use crate::media::split_name;
use crate::route::AiMeta;
use crate::tools::Tools;
use crate::{Error, Result, exif};
use chrono::offset::LocalResult;
use chrono::{Duration, FixedOffset, Local, NaiveDateTime, Offset, TimeZone, Utc};
use chrono_tz::Tz;
use regex::Regex;
use serde_json::Value;
use std::fs::File;
use std::path::Path;
use std::sync::OnceLock;
use std::time::SystemTime;
use tzf_rs::DefaultFinder;

/// Containers whose camera/GPS tags live in QuickTime Keys/UserData.
const QUICKTIME_EXT: [&str; 3] = ["mp4", "mov", "m4v"];
/// Containers exiftool cannot write: dates go in via an ffmpeg remux.
const MATROSKA_EXT: [&str; 2] = ["mkv", "webm"];

impl ContentStep for MetadataPreset {
    fn run(&self, ctx: &StepCtx, input: &Path) -> Result<StepResult> {
        let name = input
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let ext = split_name(&name).1;
        let container = ext.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
        let out = tmp_output(ctx.tmp_dir, input, &ext)?;
        let mut warnings = Vec::new();
        if let Err(e) = self.apply(ctx, input, &out, &container, &mut warnings) {
            let _ = std::fs::remove_file(&out);
            return Err(Error::Tool(crate::t!(
                "content.metadata_for",
                path = ctx.original.display(),
                error = e
            )));
        }
        Ok(StepResult {
            path: out,
            ext,
            warnings,
            fell_back: false,
        })
    }
}

impl MetadataPreset {
    fn apply(
        &self,
        ctx: &StepCtx,
        input: &Path,
        out: &Path,
        container: &str,
        warnings: &mut Vec<String>,
    ) -> Result<()> {
        let date = ctx.capture.filter(|_| self.write_capture_date);
        if MATROSKA_EXT.contains(&container) {
            let remuxed = match date {
                Some(d) => remux_creation_time(ctx, input, out, d),
                None => Ok(false),
            };
            match remuxed {
                Ok(true) => {}
                Ok(false) => copy_keep_mtime(input, out)?,
                // Like prepare-media: a failed remux is a warning, the file goes as is.
                Err(e) => {
                    warnings.push(crate::t!("content.ffmpeg_date", error = e));
                    copy_keep_mtime(input, out)?;
                }
            }
        } else {
            copy_keep_mtime(input, out)?;
            let mut args = Vec::new();
            if let Some(d) = date {
                args.extend(date_args(ctx.tools, container, d)?);
            }
            let rec = ctx.original_exif;
            if QUICKTIME_EXT.contains(&container) {
                args.extend(self.plan_video(rec));
            } else {
                args.extend(self.plan_photo(rec, container == "avif", warnings));
            }
            if !args.is_empty() {
                match exif::write(ctx.tools, out, &args) {
                    Ok(()) => {}
                    // exiftool leaves the file untouched when it has no writer for it.
                    Err(Error::Unwritable(_)) => warnings.push(crate::t!(
                        "content.exiftool_no_write",
                        container = container
                    )),
                    Err(e) => return Err(e),
                }
            }
        }
        if let Some(d) = date {
            set_mtime(ctx.tools, out, d)?;
        }
        Ok(())
    }

    fn plan_video(&self, rec: &Value) -> Vec<String> {
        let mut args = Vec::new();
        if self.video_make_model {
            // Android writes the camera only to Keys:AndroidMake/AndroidModel.
            if text(rec, "Keys:AndroidMake").is_some() && text(rec, "Keys:Make").is_none() {
                args.push("-Keys:Make<Keys:AndroidMake".into());
            }
            if text(rec, "Keys:AndroidModel").is_some() && text(rec, "Keys:Model").is_none() {
                args.push("-Keys:Model<Keys:AndroidModel".into());
            }
        }
        if self.video_restore_gps && text(rec, "UserData:GPSCoordinates").is_none() {
            // ffmpeg writes the position as Keys:Location (ISO 6709, 4 decimals ~10 m)
            if let Some(src) = ["Keys:GPSCoordinates", "Keys:Location"]
                .into_iter()
                .find(|k| text(rec, k).is_some())
            {
                args.push(format!("-UserData:GPSCoordinates<{src}"));
            }
        }
        args
    }

    fn plan_photo(&self, rec: &Value, is_avif: bool, warnings: &mut Vec<String>) -> Vec<String> {
        let mut args = Vec::new();

        if self.fix_gcam_make_model {
            // Google Camera ports swap them: "M2012K11AG (alioth)" / "Xiaomi".
            let make = text(rec, "IFD0:Make");
            let model = text(rec, "IFD0:Model");
            if let (Some(make), Some(model)) = (&make, &model)
                && let Some(c) = re(&SWAPPED_MAKE, r"^(.+?) \(([^()]+)\)$").captures(make)
                && re(&VENDOR_MODEL, r"^[A-Za-z][A-Za-z .-]*$").is_match(model)
            {
                args.push(format!("-IFD0:Make={model}"));
                args.push(format!("-IFD0:Model={}", &c[1]));
                // The lens tags are equally bogus copies of the swapped Make.
                if text(rec, "ExifIFD:LensMake").as_ref() == Some(make) {
                    args.push("-ExifIFD:LensMake=".into());
                    args.push("-ExifIFD:LensModel=".into());
                }
            }
        }

        // XMP-GCamera: ~30 KB of HDR+ debug blobs. XMP-GContainer/XMP-hdrgm
        // describe a gain map stored the JPEG way; AVIF keeps it natively.
        let mut groups = Vec::new();
        if self.strip_gcamera_xmp {
            groups.push("XMP-GCamera");
        }
        if self.strip_gain_map_xmp_for_avif && is_avif {
            groups.extend(["XMP-GContainer", "XMP-hdrgm"]);
        }
        let keys = rec.as_object();
        for g in groups {
            let prefix = format!("{g}:");
            if keys.is_some_and(|o| o.keys().any(|k| k.starts_with(&prefix))) {
                args.push(format!("-{g}:all="));
            }
        }

        if self.fill_offset_time {
            args.extend(plan_offset(rec, warnings));
        }
        args
    }
}

/// OffsetTime* from the camera's own Google:CreateDate, else from the GPS
/// position. No GPS and no camera hint = left empty: guessing would be worse.
fn plan_offset(rec: &Value, warnings: &mut Vec<String>) -> Vec<String> {
    let have = text(rec, "ExifIFD:OffsetTimeOriginal");
    let google = text(rec, "Google:CreateDate").and_then(|s| {
        re(&TRAILING_OFFSET, r"([+-]\d{2}:\d{2})$")
            .captures(&s)
            .map(|c| c[1].to_string())
    });
    let camera = have.clone().or(google);
    let place = offset_from_gps(rec);
    // The place decides what the clock should have shown; the camera's own
    // offset only when the place is unknown.
    let expected = place.clone().or(camera.clone());
    if let Some(gap) = clock_gap(rec, expected.as_deref()) {
        // A zone stamped onto a wrong clock would assert a wrong moment.
        let hours = gap as f64 / 60.0;
        warnings.push(format!(
            "⏰ phone clock is off from the place's time zone by {hours:+} h \
             (shot {}, GPS {} {} UTC, place zone {}){}",
            text(rec, "ExifIFD:DateTimeOriginal").unwrap_or_default(),
            text(rec, "GPS:GPSDateStamp").unwrap_or_default(),
            gps_time(rec).unwrap_or_default(),
            expected.unwrap_or_default(),
            if have.is_some() {
                ""
            } else {
                " — zone not recorded"
            },
        ));
        return Vec::new();
    }
    if have.is_some() {
        return Vec::new();
    }
    match camera.or(place) {
        Some(off) => vec![
            format!("-ExifIFD:OffsetTimeOriginal={off}"),
            format!("-ExifIFD:OffsetTimeDigitized={off}"),
            format!("-ExifIFD:OffsetTime={off}"),
        ],
        None => Vec::new(),
    }
}

/// Minutes by which the phone clock disagrees with the zone, or None if
/// fine/unknown.
fn clock_gap(rec: &Value, expected: Option<&str>) -> Option<i64> {
    let local = dto(rec)?;
    let utc = gps_utc(rec)?;
    let exp = parse_offset(expected?)?;
    let gap = ((local - utc).num_seconds() as f64 / 60.0).round() as i64 - exp;
    // whole hours (±3 min of fix lag) and at least 45 min: a clock/zone error;
    // anything else is a GPS fix taken earlier than the shot
    let whole = ((gap as f64 / 60.0).round() as i64) * 60;
    (gap.abs() >= 45 && (gap - whole).abs() <= 3).then_some(gap)
}

/// Zone offset of the GPS position at the shot's local time.
fn offset_from_gps(rec: &Value) -> Option<String> {
    let lat = rec.get("Composite:GPSLatitude")?.as_f64()?;
    let lon = rec.get("Composite:GPSLongitude")?.as_f64()?;
    let local = dto(rec)?;
    if lat == 0.0 && lon == 0.0 {
        return None; // "no fix" written as zeros
    }
    let name = finder().get_tz_name(lon, lat);
    let tz: Tz = name.parse().ok()?;
    let minutes = |o: <Tz as TimeZone>::Offset| i64::from(o.fix().local_minus_utc()) / 60;
    // Ambiguous: the hour that repeats when clocks go back (earlier side
    // first, like Python's fold=0). None: the skipped hour, so the offsets
    // just before and after the jump.
    let mut candidates: Vec<i64> = match tz.offset_from_local_datetime(&local) {
        LocalResult::Single(o) => vec![minutes(o)],
        LocalResult::Ambiguous(a, b) => vec![minutes(a), minutes(b)],
        LocalResult::None => [local - Duration::hours(3), local + Duration::hours(3)]
            .iter()
            .filter_map(|t| tz.offset_from_local_datetime(t).earliest())
            .map(minutes)
            .collect(),
    };
    candidates.dedup();
    let first = *candidates.first()?;
    // The GPS UTC time picks the side.
    let best = match gps_utc(rec) {
        Some(utc) if candidates.len() > 1 => candidates
            .into_iter()
            .min_by_key(|off| {
                ((local - Duration::minutes(*off)) - utc)
                    .num_seconds()
                    .abs()
            })
            .unwrap_or(first),
        _ => first,
    };
    Some(fmt_offset(best))
}

fn finder() -> &'static DefaultFinder {
    static FINDER: OnceLock<DefaultFinder> = OnceLock::new();
    FINDER.get_or_init(DefaultFinder::new)
}

fn fmt_offset(minutes: i64) -> String {
    let sign = if minutes >= 0 { '+' } else { '-' };
    let m = minutes.abs();
    format!("{sign}{:02}:{:02}", m / 60, m % 60)
}

fn parse_offset(s: &str) -> Option<i64> {
    let c = re(&OFFSET, r"([+-])(\d{2}):(\d{2})$").captures(s)?;
    let minutes = c[2].parse::<i64>().ok()? * 60 + c[3].parse::<i64>().ok()?;
    Some(if &c[1] == "-" { -minutes } else { minutes })
}

fn dto(rec: &Value) -> Option<NaiveDateTime> {
    parse_tag(&text(rec, "ExifIFD:DateTimeOriginal")?)
}

/// GPS time stamp cut to whole seconds ("HH:MM:SS").
fn gps_time(rec: &Value) -> Option<String> {
    let t = text(rec, "GPS:GPSTimeStamp")?;
    Some(t.chars().take(8).collect())
}

fn gps_utc(rec: &Value) -> Option<NaiveDateTime> {
    let date = text(rec, "GPS:GPSDateStamp")?;
    parse_tag(&format!("{date} {}", gps_time(rec)?))
}

/// A tag as text; absent, null, empty and zero values count as missing.
fn text(rec: &Value, key: &str) -> Option<String> {
    match rec.get(key)? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) if n.as_f64() != Some(0.0) => Some(n.to_string()),
        _ => None,
    }
}

static SWAPPED_MAKE: OnceLock<Regex> = OnceLock::new();
static VENDOR_MODEL: OnceLock<Regex> = OnceLock::new();
static TRAILING_OFFSET: OnceLock<Regex> = OnceLock::new();
static OFFSET: OnceLock<Regex> = OnceLock::new();

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("static regex"))
}

/// prepare-media's date tags for `container`. `d` is local time in the
/// tools' zone. Nothing is left to exiftool's idea of the local zone (the
/// process `TZ`, which Windows does not understand): QuickTime stores UTC, so
/// those tags get the UTC time computed here; EXIF dates are plain local
/// wall-clock times; the file time gets an explicit offset.
pub(crate) fn date_args(tools: &Tools, container: &str, d: NaiveDateTime) -> Result<Vec<String>> {
    let fmt = "%Y:%m:%d %H:%M:%S";
    let local = local_time(tools, d)?;
    let (tags, val): (&[&str], String) = if QUICKTIME_EXT.contains(&container) {
        (
            &[
                "QuickTime:CreateDate",
                "QuickTime:ModifyDate",
                "QuickTime:TrackCreateDate",
                "QuickTime:TrackModifyDate",
                "QuickTime:MediaCreateDate",
                "QuickTime:MediaModifyDate",
            ],
            local.with_timezone(&Utc).format(fmt).to_string(),
        )
    } else {
        (&["AllDates"], d.format(fmt).to_string())
    };
    let mut args: Vec<String> = tags.iter().map(|t| format!("-{t}={val}")).collect();
    args.push(format!(
        "-FileModifyDate={}",
        local.format("%Y:%m:%d %H:%M:%S%:z")
    ));
    Ok(args)
}

/// exiftool cannot write Matroska: remux with ffmpeg, creation time in UTC.
/// Ok(true) once `out` holds the remuxed file.
fn remux_creation_time(ctx: &StepCtx, input: &Path, out: &Path, d: NaiveDateTime) -> Result<bool> {
    let utc = local_time(ctx.tools, d)?
        .with_timezone(&Utc)
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    let res = ctx
        .tools
        .command(&ctx.tools.ffmpeg)
        .args(["-nostdin", "-loglevel", "error", "-y", "-i"])
        .arg(input)
        .args(["-map", "0", "-codec", "copy"])
        .args(["-metadata", &format!("creation_time={utc}")])
        .args(["-metadata", &format!("date={utc}")])
        .arg(out)
        .output()?;
    if !res.status.success() {
        return Err(Error::Tool(format!(
            "ffmpeg: {}",
            String::from_utf8_lossy(&res.stderr).trim()
        )));
    }
    Ok(true)
}

/// Add the AI's keywords to `path` (`XMP-dc:Subject`, and `IPTC:Keywords`
/// in a JPEG) and its description (`XMP-dc:Description`) when the file has
/// none: keywords already there and an existing description stay.
/// `Error::Unwritable` when exiftool has no writer for the format (WebM, MKV).
pub(crate) fn write_ai_meta(tools: &Tools, path: &Path, meta: &AiMeta) -> Result<()> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let jpeg = ext == "jpg" || ext == "jpeg";
    let mut args = Vec::new();
    if jpeg && !meta.tags.is_empty() {
        // IPTC strings are Latin-1 unless the record says UTF-8.
        args.push("-IPTC:CodedCharacterSet=UTF8".to_string());
    }
    for t in &meta.tags {
        // Removing first keeps a keyword the file already has from repeating.
        args.push(format!("-XMP-dc:Subject-={t}"));
        args.push(format!("-XMP-dc:Subject+={t}"));
        if jpeg {
            args.push(format!("-IPTC:Keywords-={t}"));
            args.push(format!("-IPTC:Keywords+={t}"));
        }
    }
    if !meta.description.is_empty() {
        // Deleted only when empty, then set: written only where there is none.
        args.push("-XMP-dc:Description-=".to_string());
        args.push(format!("-XMP-dc:Description={}", meta.description));
    }
    if args.is_empty() {
        return Ok(());
    }
    exif::write(tools, path, &args)
}

/// `d` as a wall-clock time in the tools' zone (this machine's without one).
pub(crate) fn local_time(tools: &Tools, d: NaiveDateTime) -> Result<chrono::DateTime<FixedOffset>> {
    let t = match tools.tz.as_deref().map(str::parse::<Tz>) {
        Some(Ok(tz)) => tz
            .from_local_datetime(&d)
            .earliest()
            .map(|t| t.fixed_offset()),
        Some(Err(e)) => return Err(Error::Tool(crate::t!("content.time_zone", error = e))),
        None => Local
            .from_local_datetime(&d)
            .earliest()
            .map(|t| t.fixed_offset()),
    };
    t.ok_or_else(|| Error::Tool(crate::t!("content.no_local_time", time = d)))
}

fn set_mtime(tools: &Tools, path: &Path, d: NaiveDateTime) -> Result<()> {
    let t: SystemTime = local_time(tools, d)?.into();
    File::options().write(true).open(path)?.set_modified(t)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    fn rec(dto: &str, lat: f64, lon: f64, gps: Option<(&str, &str)>) -> Value {
        let mut r = json!({
            "ExifIFD:DateTimeOriginal": dto,
            "Composite:GPSLatitude": lat,
            "Composite:GPSLongitude": lon,
        });
        if let Some((d, t)) = gps {
            r["GPS:GPSDateStamp"] = json!(d);
            r["GPS:GPSTimeStamp"] = json!(t);
        }
        r
    }

    #[test]
    fn repeated_hour_side_follows_gps_utc() {
        // Chisinau falls back 03:00 -> 02:00 on 2026-10-25: 02:30 happens twice.
        let (lat, lon) = (47.0105, 28.8638);
        let dto = "2026:10:25 02:30:00";
        let summer = rec(dto, lat, lon, Some(("2026:10:24", "23:30:00")));
        let winter = rec(dto, lat, lon, Some(("2026:10:25", "00:30:00")));
        assert_eq!(offset_from_gps(&summer).as_deref(), Some("+03:00"));
        assert_eq!(offset_from_gps(&winter).as_deref(), Some("+02:00"));
        assert_eq!(
            offset_from_gps(&rec(dto, lat, lon, None)).as_deref(),
            Some("+03:00")
        );
        // Outside the repeated hour the zone rules alone decide.
        let jan = rec("2026:01:10 12:00:00", lat, lon, None);
        assert_eq!(offset_from_gps(&jan).as_deref(), Some("+02:00"));
    }

    #[test]
    fn zero_coordinates_are_no_fix() {
        assert_eq!(
            offset_from_gps(&rec("2026:07:01 12:00:00", 0.0, 0.0, None)),
            None
        );
    }

    #[test]
    fn stale_fix_is_not_a_clock_error() {
        let (lat, lon) = (47.0105, 28.8638);
        // 50 minutes off: a fix taken earlier than the shot.
        let r = rec(
            "2026:07:01 12:00:00",
            lat,
            lon,
            Some(("2026:07:01", "08:10:00")),
        );
        assert_eq!(clock_gap(&r, Some("+03:00")), None);
        // Whole hour within 3 minutes of fix lag: the clock is wrong.
        let r = rec(
            "2026:07:01 12:00:00",
            lat,
            lon,
            Some(("2026:07:01", "10:02:00")),
        );
        assert_eq!(clock_gap(&r, Some("+03:00")), Some(-62));
    }

    #[test]
    fn camera_offset_wins_over_place() {
        let mut r = rec("2026:07:01 12:00:00", 47.0105, 28.8638, None);
        r["Google:CreateDate"] = json!("2026:07:01 12:00:00+02:00");
        let mut w = Vec::new();
        let args = plan_offset(&r, &mut w);
        assert_eq!(args[0], "-ExifIFD:OffsetTimeOriginal=+02:00");
        assert!(w.is_empty());
    }

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

    fn dt(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").unwrap()
    }

    /// The date tags depend only on the plan's zone, never on the zone the
    /// process runs in (Windows exiftool cannot read an IANA `TZ`).
    #[test]
    fn date_args_use_explicit_values_for_the_plan_zone() {
        let summer = dt("2026-07-01 12:00:00");
        let winter = dt("2026-01-10 12:00:00");
        let args = |tz, c, d| date_args(&tools_in(Some(tz)), c, d).unwrap();

        // QuickTime stores UTC: 12:00 in Chisinau (EEST, +03:00) is 09:00.
        let qt = args("Europe/Chisinau", "mp4", summer);
        assert_eq!(qt[0], "-QuickTime:CreateDate=2026:07:01 09:00:00");
        assert_eq!(qt[5], "-QuickTime:MediaModifyDate=2026:07:01 09:00:00");
        assert_eq!(qt[6], "-FileModifyDate=2026:07:01 12:00:00+03:00");
        assert_eq!(qt.len(), 7);
        // The same wall clock in winter (EET, +02:00) and in other zones.
        assert_eq!(
            args("Europe/Chisinau", "mov", winter)[0],
            "-QuickTime:CreateDate=2026:01:10 10:00:00"
        );
        assert_eq!(
            args("Asia/Tokyo", "mp4", summer)[0],
            "-QuickTime:CreateDate=2026:07:01 03:00:00"
        );
        assert_eq!(
            args("America/New_York", "mp4", summer)[0],
            "-QuickTime:CreateDate=2026:07:01 16:00:00"
        );
        assert_eq!(
            args("America/New_York", "mp4", summer)[6],
            "-FileModifyDate=2026:07:01 12:00:00-04:00"
        );
        assert_eq!(
            args("UTC", "mp4", summer)[0],
            "-QuickTime:CreateDate=2026:07:01 12:00:00"
        );
        // EXIF dates are the wall clock itself, whatever the zone.
        for tz in ["Europe/Chisinau", "Asia/Tokyo", "UTC"] {
            assert_eq!(args(tz, "jpg", summer)[0], "-AllDates=2026:07:01 12:00:00");
        }
        assert_eq!(
            args("Asia/Tokyo", "jpg", summer)[1],
            "-FileModifyDate=2026:07:01 12:00:00+09:00"
        );
    }

    #[test]
    fn date_args_without_zone_use_the_machine_zone() {
        let d = dt("2026-07-01 12:00:00");
        let a = date_args(&tools_in(None), "mp4", d).unwrap();
        let off = local_time(&tools_in(None), d)
            .unwrap()
            .format("%:z")
            .to_string();
        assert_eq!(a[6], format!("-FileModifyDate=2026:07:01 12:00:00{off}"));
    }

    #[test]
    fn date_args_reject_a_bad_zone_and_a_skipped_hour() {
        assert!(
            date_args(
                &tools_in(Some("Not/AZone")),
                "mp4",
                dt("2026-07-01 12:00:00")
            )
            .is_err()
        );
        // 02:30 does not exist in Chisinau on 2026-03-29 (clocks jump 02:00 -> 03:00).
        let t = tools_in(Some("Europe/Chisinau"));
        assert!(date_args(&t, "mp4", dt("2026-03-29 02:30:00")).is_err());
    }

    /// End to end: the tags exiftool stores are identical whatever `TZ` the
    /// process has, because nothing is left to exiftool's local zone.
    #[test]
    fn written_quicktime_dates_do_not_depend_on_process_tz() {
        let (Some(ffmpeg), Some(exiftool)) =
            (crate::tools::find("ffmpeg"), crate::tools::find("exiftool"))
        else {
            eprintln!("skip: ffmpeg/exiftool not found");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let mut tools = tools_in(Some("Asia/Tokyo"));
        tools.exiftool = exiftool.clone();
        let src = dir.path().join("v.mp4");
        let ok = std::process::Command::new(ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "color=c=red:s=64x64:d=1",
            ])
            .arg(&src)
            .status()
            .is_ok_and(|s| s.success());
        assert!(ok, "fixture");
        let args = date_args(&tools, "mp4", dt("2026-07-01 12:00:00")).unwrap();
        let mut seen = Vec::new();
        for tz in ["UTC", "America/Los_Angeles", "Asia/Tokyo"] {
            let out = dir.path().join(format!("{}.mp4", tz.replace('/', "_")));
            std::fs::copy(&src, &out).unwrap();
            // The process zone is set on the exiftool child only where the
            // OS knows IANA names; the result must not change.
            let mut cmd = std::process::Command::new(&exiftool);
            cmd.env("TZ", tz)
                .args(["-overwrite_original", "-m", "-q"])
                .args(&args)
                .arg(&out);
            assert!(cmd.status().unwrap().success());
            let raw = std::process::Command::new(&exiftool)
                .args(["-s3", "-QuickTime:CreateDate", "-QuickTime:MediaModifyDate"])
                .arg(&out)
                .output()
                .unwrap();
            seen.push(
                String::from_utf8_lossy(&raw.stdout)
                    .replace("\r", "")
                    .trim()
                    .to_string(),
            );
        }
        // 12:00 JST is 03:00 UTC.
        assert_eq!(seen[0], "2026:07:01 03:00:00\n2026:07:01 03:00:00");
        assert!(seen.iter().all(|s| *s == seen[0]), "{seen:?}");
    }

    #[test]
    fn offsets_format_and_parse() {
        assert_eq!(fmt_offset(-330), "-05:30");
        assert_eq!(parse_offset("2026:07:01 12:00:00-05:30"), Some(-330));
    }
}
