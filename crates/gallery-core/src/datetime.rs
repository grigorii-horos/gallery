//! Capture date resolution: file name, EXIF/QuickTime tags, file system times.

use std::path::Path;
use std::sync::OnceLock;

use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use regex::Regex;
use serde_json::Value;

use crate::media::{MediaKind, split_name};

/// A date found in a file name. `day_only` means no time part was present
/// (the time is then midnight and must not be trusted as a capture time).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NameDate {
    pub dt: NaiveDateTime,
    pub day_only: bool,
}

fn name_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // Port of the prepare-media regex. Rust has no look-around, so the
        // leading "not preceded by a digit" and the trailing "not followed by
        // a digit" (HH-MM branch) are checked by hand in `from_name`.
        Regex::new(
            r"(19|20)([0-9]{2})[-_.]?(0[1-9]|1[0-2])[-_.]?(0[1-9]|[12][0-9]|3[01])(?:(?:[-_.T ]| at )?(?:([01][0-9]|2[0-3])[-_.:]?([0-5][0-9])[-_.:]?([0-5][0-9])|([01][0-9]|2[0-3])[-_.:]([0-5][0-9])))?",
        )
        .expect("static regex")
    })
}

/// Date from a file name (`YYYYMMDD[_-. T]HHMMSS` and variations, including
/// Firefox "Screenshot 2026-09-13 at 18-26-07" and "2024-04-17 18-53" without
/// seconds). Years outside 1970..=2099 and impossible dates give `None`.
/// A date without time is midnight with `day_only` set.
pub fn from_name(name: &str) -> Option<NameDate> {
    let re = name_regex();
    let bytes = name.as_bytes();
    let mut from = 0;
    let caps = loop {
        let caps = re.captures_at(name, from)?;
        let m = caps.get(0).expect("group 0");
        if m.start() > 0 && bytes[m.start() - 1].is_ascii_digit() {
            from = m.start() + 1;
            continue;
        }
        break caps;
    };
    let num = |i: usize| {
        caps.get(i)
            .map(|g| g.as_str().parse::<u32>().expect("digits"))
    };
    let year = (num(1)? * 100 + num(2)?) as i32;
    if !(1970..=2099).contains(&year) {
        return None;
    }
    let date = NaiveDate::from_ymd_opt(year, num(3)?, num(4)?)?;
    if is_clock_reset(date) {
        return None;
    }
    let end = caps.get(0).expect("group 0").end();
    let (h, mi, s) = if let (Some(h), Some(mi), Some(s)) = (num(5), num(6), num(7)) {
        (h, mi, s)
    } else if let (Some(h), Some(mi)) = (num(8), num(9)) {
        // HH-MM only counts when no further digit follows
        if bytes.get(end).is_some_and(u8::is_ascii_digit) {
            return Some(NameDate {
                dt: date.and_hms_opt(0, 0, 0)?,
                day_only: true,
            });
        }
        (h, mi, 0)
    } else {
        return Some(NameDate {
            dt: date.and_hms_opt(0, 0, 0)?,
            day_only: true,
        });
    };
    Some(NameDate {
        dt: date.and_hms_opt(h, mi, s)?,
        day_only: false,
    })
}

/// 1970-01-01 and 1980-01-01 are what cameras write after a clock reset
/// (Unix and DOS epochs): a date on either day is no date at all.
fn is_clock_reset(d: NaiveDate) -> bool {
    use chrono::Datelike;
    d.ordinal() == 1 && matches!(d.year(), 1970 | 1980)
}

fn epoch_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^([0-9]{13}|[0-9]{10})(?:[-_][0-9]+)?$").expect("static regex"))
}

/// Date from a stem that is a Unix time: exactly 13 digits (milliseconds) or
/// 10 (seconds), optionally followed by `_N`/`-N`. Only instants from
/// 2000-01-01 to a day from now count; the result is the wall clock in `tz`
/// (this machine's zone without one).
pub fn from_epoch_name(stem: &str, tz: Option<Tz>) -> Option<NaiveDateTime> {
    let digits = epoch_regex().captures(stem)?.get(1)?.as_str();
    let n: i64 = digits.parse().ok()?;
    // Whole seconds, like every other capture date.
    let secs = if digits.len() == 13 { n / 1000 } else { n };
    let at = DateTime::<Utc>::from_timestamp(secs, 0)?;
    let earliest = Utc.with_ymd_and_hms(2000, 1, 1, 0, 0, 0).single()?;
    if at < earliest || at > Utc::now() + chrono::Duration::days(1) {
        return None;
    }
    Some(match tz {
        Some(tz) => at.with_timezone(&tz).naive_local(),
        None => at.with_timezone(&Local).naive_local(),
    })
}

fn tag_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"^(\d{4}):(\d{2}):(\d{2})[ T](\d{2}):(\d{2}):(\d{2})(?:\.\d+)?(?:Z|[+-]\d{2}:?\d{2})?$",
        )
        .expect("static regex")
    })
}

/// Parse an exiftool date ("2026:04:12 10:33:14", optionally with fraction and
/// offset) into wall-clock time. Zero dates (ffmpeg writes "0000:00:00 ...")
/// years outside 1970..=2099 and camera clock resets (1970-01-01, 1980-01-01)
/// are treated as absent.
pub(crate) fn parse_tag(s: &str) -> Option<NaiveDateTime> {
    let c = tag_regex().captures(s.trim())?;
    let n = |i: usize| c[i].parse::<u32>().ok();
    let year = n(1)? as i32;
    if !(1970..=2099).contains(&year) {
        return None;
    }
    let date = NaiveDate::from_ymd_opt(year, n(2)?, n(3)?)?;
    if is_clock_reset(date) {
        return None;
    }
    date.and_hms_opt(n(4)?, n(5)?, n(6)?)
}

/// Every valid date among tags named `tag` in any group, offsets dropped.
pub(crate) fn tag_dates(exif: &Value, tag: &str) -> Vec<NaiveDateTime> {
    let Some(obj) = exif.as_object() else {
        return Vec::new();
    };
    obj.iter()
        .filter(|(k, _)| k.rsplit_once(':').map_or(k.as_str(), |(_, n)| n) == tag)
        .filter_map(|(_, v)| parse_tag(v.as_str()?))
        .collect()
}

/// First valid date among tags named `tag` in any group (`ExifIFD:DateTimeOriginal`
/// matches "DateTimeOriginal"). Groups are tried in a fixed preference order.
fn tag_date(exif: &Value, tag: &str) -> Option<NaiveDateTime> {
    const GROUPS: [&str; 5] = ["ExifIFD", "IFD0", "QuickTime", "Keys", "XMP-exif"];
    let obj = exif.as_object()?;
    let mut found: Vec<(usize, &str, NaiveDateTime)> = obj
        .iter()
        .filter_map(|(k, v)| {
            let (group, name) = k.rsplit_once(':').unwrap_or(("", k));
            if name != tag {
                return None;
            }
            let rank = GROUPS
                .iter()
                .position(|g| *g == group)
                .unwrap_or(GROUPS.len());
            Some((rank, k.as_str(), parse_tag(v.as_str()?)?))
        })
        .collect();
    found.sort_by_key(|(rank, key, _)| (*rank, *key));
    found.first().map(|f| f.2)
}

/// Capture date, ported from prepare-media.
/// Photo: EXIF DateTimeOriginal (the name wins when it differs by under a
/// minute) -> date+time from the name -> CreateDate/ModifyDate -> day from name.
/// Video: date+time from the name -> DateTimeOriginal -> CreateDate/ModifyDate
/// (QuickTime is already local time thanks to `-api QuickTimeUTC`) -> day.
/// A name that is a Unix time (see `from_epoch_name`, read in `tz`, the
/// planning machine's zone) comes after every embedded date, before a
/// day-only name date.
pub fn capture_date(
    file_name: &str,
    kind: MediaKind,
    exif: &Value,
    tz: Option<Tz>,
) -> Option<NaiveDateTime> {
    let (stem, _) = split_name(file_name);
    let name = from_name(&stem);
    let full = name.filter(|n| !n.day_only).map(|n| n.dt);
    let dto = tag_date(exif, "DateTimeOriginal");

    if let (Some(dto), true) = (dto, kind != MediaKind::Video) {
        // Name and EXIF differ by a second or two (file named at shutter
        // press): keep the name's time. A real mismatch (UTC in the name) -> EXIF.
        return match full {
            Some(n) if (n - dto).num_seconds().abs() < 60 => Some(n),
            _ => Some(dto),
        };
    }
    // A Unix-time name is often a receive time (messengers) or just an id:
    // every embedded date beats it.
    full.or(dto)
        .or_else(|| tag_date(exif, "CreateDate"))
        .or_else(|| tag_date(exif, "ModifyDate"))
        .or_else(|| from_epoch_name(&stem, tz))
        .or(name.map(|n| n.dt))
}

/// File birth time, falling back to mtime, in local time.
pub fn file_created(path: &Path) -> Option<NaiveDateTime> {
    let md = std::fs::metadata(path).ok()?;
    let t = md.created().or_else(|_| md.modified()).ok()?;
    Some(DateTime::<Local>::from(t).naive_local())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn dt(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").unwrap()
    }

    #[test]
    fn from_name_cases() {
        let full = |s: &str| from_name(s).map(|n| (n.dt, n.day_only));
        assert_eq!(
            full("IMG_20260101_120000.jpg"),
            Some((dt("2026-01-01 12:00:00"), false))
        );
        assert_eq!(
            full("Screenshot 2026-09-13 at 18-26-07.png"),
            Some((dt("2026-09-13 18:26:07"), false))
        );
        assert_eq!(
            full("2024-04-17 18-53.jpg"),
            Some((dt("2024-04-17 18:53:00"), false))
        );
        assert_eq!(
            full("VID-20260719-WA0000.mp4"),
            Some((dt("2026-07-19 00:00:00"), true))
        );
        assert_eq!(full("IMG_1234.jpg"), None);
        assert_eq!(full("19691231_000000"), None);
        // glued to a longer digit run: not a date
        assert_eq!(full("1120260101.jpg"), None);
        // invalid calendar day
        assert_eq!(full("20260230_101010"), None);
        // HH-MM followed by a digit is not a time
        assert_eq!(
            full("2024-04-17 18-531"),
            Some((dt("2024-04-17 00:00:00"), true))
        );
    }

    #[test]
    fn capture_photo_prefers_name_within_60s() {
        let ex = json!({"ExifIFD:DateTimeOriginal": "2026:04:12 10:33:14"});
        assert_eq!(
            capture_date("IMG_20260412_103350.jpg", MediaKind::Photo, &ex, None),
            Some(dt("2026-04-12 10:33:50"))
        );
    }

    #[test]
    fn capture_photo_dto_beyond_60s() {
        let ex = json!({"ExifIFD:DateTimeOriginal": "2026:04:12 10:33:14"});
        assert_eq!(
            capture_date("IMG_20260412_073314.jpg", MediaKind::Photo, &ex, None),
            Some(dt("2026-04-12 10:33:14"))
        );
    }

    #[test]
    fn capture_video_name_first() {
        let ex = json!({
            "QuickTime:CreateDate": "2026:04:12 13:00:00+03:00",
            "IFD0:ModifyDate": "0000:00:00 00:00:00",
        });
        assert_eq!(
            capture_date("VID_20260412_101010.mp4", MediaKind::Video, &ex, None),
            Some(dt("2026-04-12 10:10:10"))
        );
        assert_eq!(
            capture_date("clip.mp4", MediaKind::Video, &ex, None),
            Some(dt("2026-04-12 13:00:00"))
        );
    }

    #[test]
    fn capture_fallbacks() {
        let ex = json!({"IFD0:ModifyDate": "2026:04:12 10:33:14.25Z", "QuickTime:CreateDate": "0000:00:00 00:00:00"});
        assert_eq!(
            capture_date("x.jpg", MediaKind::Photo, &ex, None),
            Some(dt("2026-04-12 10:33:14"))
        );
        let none = json!({"QuickTime:CreateDate": "1904:01:01 00:00:00"});
        assert_eq!(capture_date("x.jpg", MediaKind::Photo, &none, None), None);
        assert_eq!(
            capture_date("VID-20260719-WA0000.mp4", MediaKind::Video, &none, None),
            Some(dt("2026-07-19 00:00:00"))
        );
    }

    #[test]
    fn epoch_names_are_local_times_in_the_given_zone() {
        let tz: chrono_tz::Tz = "Europe/Berlin".parse().unwrap();
        // The same conversion, spelled out: the instant, then Berlin's wall clock.
        let want = |secs: i64| {
            DateTime::from_timestamp(secs, 0)
                .unwrap()
                .with_timezone(&tz)
                .naive_local()
        };
        let at = |n: &str| capture_date(n, MediaKind::Photo, &Value::Null, Some(tz));
        let got = at("1481122477728.jpg").unwrap();
        assert_eq!(got, want(1_481_122_477));
        assert_eq!(got, dt("2016-12-07 15:54:37"));
        assert_eq!(at("1481122477.jpg"), Some(want(1_481_122_477)));
        assert_eq!(at("1481122477728_1.jpg"), Some(want(1_481_122_477)));
        assert_eq!(at("1481122477728-12.jpg"), Some(want(1_481_122_477)));
        // Other zone, other wall clock.
        let ny: chrono_tz::Tz = "America/New_York".parse().unwrap();
        assert_eq!(
            capture_date(
                "1481122477728.jpg",
                MediaKind::Photo,
                &Value::Null,
                Some(ny)
            ),
            Some(dt("2016-12-07 09:54:37"))
        );
        // Not exactly 10 or 13 digits, other suffixes, out of range: no date.
        for n in [
            "148112247772.jpg",
            "14811224777280.jpg",
            "IMG_1481122477728.jpg",
            "1481122477728_a.jpg",
            "1481122477728_.jpg",
            "0946684799.jpg",
            "9999999999.jpg",
            "9999999999999.jpg",
        ] {
            assert_eq!(at(n), None, "{n}");
        }
        // The date regex goes first.
        assert_eq!(at("20161207_101010.jpg"), Some(dt("2016-12-07 10:10:10")));
    }

    #[test]
    fn epoch_names_rank_below_embedded_dates() {
        let tz: chrono_tz::Tz = "Europe/Berlin".parse().unwrap();
        // Messenger receive time in the name; the clip knows when it was shot.
        let ex = json!({"QuickTime:CreateDate": "2016:12:01 10:00:00"});
        assert_eq!(
            capture_date("1481122477728.mp4", MediaKind::Video, &ex, Some(tz)),
            Some(dt("2016-12-01 10:00:00"))
        );
        // Photo without DTO: CreateDate, then ModifyDate, beat the name.
        assert_eq!(
            capture_date("1481122477728.jpg", MediaKind::Photo, &ex, Some(tz)),
            Some(dt("2016-12-01 10:00:00"))
        );
        let ex = json!({"IFD0:ModifyDate": "2016:12:02 10:00:00"});
        assert_eq!(
            capture_date("1481122477728.jpg", MediaKind::Photo, &ex, Some(tz)),
            Some(dt("2016-12-02 10:00:00"))
        );
        // An arbitrary 10-digit id never overrides DTO, even within a minute.
        let ex = json!({"ExifIFD:DateTimeOriginal": "2009:02:14 00:31:00"});
        assert_eq!(
            capture_date("1234567890.jpg", MediaKind::Photo, &ex, Some(tz)),
            Some(dt("2009-02-14 00:31:00"))
        );
        // No metadata: the epoch name, before a day-only regex name.
        assert_eq!(
            capture_date(
                "1481122477728.jpg",
                MediaKind::Photo,
                &Value::Null,
                Some(tz)
            ),
            Some(dt("2016-12-07 15:54:37"))
        );
    }

    #[test]
    fn camera_clock_reset_days_are_absent() {
        let ex = json!({"ExifIFD:DateTimeOriginal": "1980:01:01 00:00:08"});
        assert_eq!(
            capture_date("19800101_000008_16.jpg", MediaKind::Photo, &ex, None),
            None
        );
        assert_eq!(parse_tag("1970:01:01 23:59:59"), None);
        assert_eq!(parse_tag("1980:01:01 12:00:00"), None);
        assert_eq!(from_name("19700101_000000"), None);
        assert_eq!(from_name("IMG_19800101.jpg"), None);
        // Neighbour days are real dates.
        assert_eq!(
            parse_tag("1980:01:02 00:00:00"),
            Some(dt("1980-01-02 00:00:00"))
        );
        assert!(from_name("19791231_235959").is_some());
        // A reset EXIF date gives way to other sources.
        let ex = json!({
            "ExifIFD:DateTimeOriginal": "1980:01:01 00:00:08",
            "IFD0:ModifyDate": "2026:04:12 10:33:14",
        });
        assert_eq!(
            capture_date("x.jpg", MediaKind::Photo, &ex, None),
            Some(dt("2026-04-12 10:33:14"))
        );
    }

    #[test]
    fn file_created_exists() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a");
        std::fs::write(&p, b"x").unwrap();
        assert!(file_created(&p).is_some());
        assert!(file_created(&d.path().join("missing")).is_none());
    }
}
