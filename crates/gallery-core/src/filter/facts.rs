//! Per-file facts (date, place, device, media properties) read from the
//! exiftool records, and the per-file filter conditions that test them.

use std::fmt;

use chrono::{Datelike, Duration, Local, NaiveDateTime, Timelike, Utc, Weekday};
use chrono_tz::Tz;
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::ser::SerializeSeq;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use super::range::{DateRange, NumRange, TimeOfDay, Unit, parse_duration};
use crate::config::{Config, Filter, Place};
use crate::mask::MaskExpr;
use crate::media::{MediaKind, kind_of, split_name};
use crate::scan::FileEntry;
use crate::{Result, datetime};

// ---------------------------------------------------------------- facts

/// What the conditions know about one file. Everything missing in the
/// metadata is `None`; a condition on it then does not hold.
#[derive(Debug, Clone, PartialEq)]
pub struct Facts {
    /// Capture date as the name presets resolve it (plan zone).
    pub capture: Option<NaiveDateTime>,
    pub mtime: NaiveDateTime,
    /// Date and time written in the file name.
    pub name_date: Option<NaiveDateTime>,
    /// (latitude, longitude).
    pub gps: Option<(f64, f64)>,
    pub make: Option<String>,
    pub model: Option<String>,
    pub software: Option<String>,
    /// Pixels, after the EXIF orientation is applied.
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_s: Option<f64>,
    pub fps: Option<f64>,
    /// Normalised lowercase (`hevc`, `h264`, `av1`, ...).
    pub video_codec: Option<String>,
    /// Normalised lowercase (`aac`, ...).
    pub audio_codec: Option<String>,
    /// Only known for videos.
    pub has_audio: Option<bool>,
    /// Lowercase extension.
    pub container: String,
    pub size: u64,
}

/// Value of the tag `name` in any group (`IFD0:Make` is "Make"): the first
/// by key order that is not null.
fn tag<'a>(exif: &'a Value, name: &str) -> Option<&'a Value> {
    exif.as_object()?
        .iter()
        .filter(|(k, v)| !v.is_null() && k.rsplit_once(':').map_or(k.as_str(), |(_, n)| n) == name)
        .map(|(_, v)| v)
        .next()
}

fn tag_f64(exif: &Value, name: &str) -> Option<f64> {
    let v = tag(exif, name)?;
    v.as_f64().or_else(|| v.as_str()?.trim().parse().ok())
}

fn tag_str(exif: &Value, name: &str) -> Option<String> {
    let v = tag(exif, name)?;
    let s = match v {
        Value::String(s) => s.trim().to_string(),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    (!s.is_empty()).then_some(s)
}

fn first_f64(exif: &Value, names: &[&str]) -> Option<f64> {
    names.iter().find_map(|n| tag_f64(exif, n))
}

fn first_str(exif: &Value, names: &[&str]) -> Option<String> {
    names.iter().find_map(|n| tag_str(exif, n))
}

fn video_codec_name(raw: &str) -> String {
    let l = raw.trim().to_ascii_lowercase();
    match l.as_str() {
        "hvc1" | "hev1" | "hevc" | "h265" | "h.265" | "v_mpegh/iso/hevc" => "hevc".into(),
        "avc1" | "avc3" | "h264" | "h.264" | "v_mpeg4/iso/avc" => "h264".into(),
        "av01" | "av1" | "v_av1" => "av1".into(),
        "vp09" | "vp9" | "v_vp9" => "vp9".into(),
        _ => l,
    }
}

fn audio_codec_name(raw: &str) -> String {
    let l = raw.trim().to_ascii_lowercase();
    match l.as_str() {
        "mp4a" | "aac" | "a_aac" => "aac".into(),
        _ => l,
    }
}

fn local_naive(ns: i128, tz: Option<Tz>) -> NaiveDateTime {
    let secs = ns.div_euclid(1_000_000_000) as i64;
    let utc = chrono::DateTime::<Utc>::from_timestamp(secs, 0).unwrap_or_default();
    match tz {
        Some(tz) => utc.with_timezone(&tz).naive_local(),
        None => utc.with_timezone(&Local).naive_local(),
    }
}

/// Largest positive value of the tag `name` across all groups.
fn max_f64(exif: &Value, name: &str) -> Option<f64> {
    exif.as_object()?
        .iter()
        .filter(|(k, _)| k.rsplit_once(':').map_or(k.as_str(), |(_, n)| n) == name)
        .filter_map(|(_, v)| v.as_f64().or_else(|| v.as_str()?.trim().parse().ok()))
        .filter(|v| *v > 0.0)
        .reduce(f64::max)
}

/// Pixel size of the main image, before orientation. RAW and DNG files
/// carry a small preview in `IFD0` next to the full image in a `SubIFD`, so
/// the size exiftool works out (`File:` or `Composite:ImageSize`) wins, else
/// the largest size any group reports.
fn dimensions(exif: &Value) -> Option<(u32, u32)> {
    let pos = |v: Option<&Value>| {
        v.and_then(|v| v.as_f64().or_else(|| v.as_str()?.trim().parse().ok()))
            .filter(|v| *v > 0.0)
    };
    if let (Some(w), Some(h)) = (
        pos(exif.get("File:ImageWidth")),
        pos(exif.get("File:ImageHeight")),
    ) {
        return Some((w as u32, h as u32));
    }
    if let Some(size) = exif.get("Composite:ImageSize").and_then(|v| v.as_str()) {
        // `-n` writes "6000 4000"; without it, "6000x4000".
        let mut it = size
            .split(|c: char| c == 'x' || c.is_whitespace())
            .filter(|p| !p.is_empty())
            .map(|p| p.parse::<f64>().ok().filter(|v| *v > 0.0));
        if let (Some(Some(w)), Some(Some(h)), None) = (it.next(), it.next(), it.next()) {
            return Some((w as u32, h as u32));
        }
    }
    [
        ("ImageWidth", "ImageHeight"),
        ("ExifImageWidth", "ExifImageHeight"),
        ("SourceImageWidth", "SourceImageHeight"),
    ]
    .iter()
    .find_map(|(w, h)| Some((max_f64(exif, w)? as u32, max_f64(exif, h)? as u32)))
}

/// The GPS fix of an exiftool record. Some phones write exactly (0, 0) when
/// they have no fix: that is no fix.
pub fn gps_of_record(exif: &Value) -> Option<(f64, f64)> {
    match (tag_f64(exif, "GPSLatitude"), tag_f64(exif, "GPSLongitude")) {
        (Some(a), Some(o)) if a == 0.0 && o == 0.0 => None,
        (Some(a), Some(o)) if a.abs() <= 90.0 && o.abs() <= 180.0 => Some((a, o)),
        _ => None,
    }
}

/// Collect the facts of one file. `tz` is the plan's zone.
pub fn facts_of(entry: &FileEntry, exif: &Value, tz: Option<Tz>) -> Facts {
    let name = entry
        .path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let kind = kind_of(&entry.path);
    let (stem, _) = split_name(name);

    let gps = gps_of_record(exif);

    let (mut width, mut height) = match dimensions(exif) {
        Some((w, h)) => (Some(w), Some(h)),
        None => (None, None),
    };
    // EXIF orientation 5..8: the picture is shown rotated by a quarter turn.
    if tag_f64(exif, "Orientation").is_some_and(|o| (5.0..=8.0).contains(&o)) {
        std::mem::swap(&mut width, &mut height);
    }
    // A video's QuickTime rotation of 90 or 270 degrees: played a quarter
    // turn round, so a 1920x1080 clip shot upright is portrait.
    if kind == MediaKind::Video
        && tag_f64(exif, "Rotation").is_some_and(|r| matches!(r.rem_euclid(360.0), 90.0 | 270.0))
    {
        std::mem::swap(&mut width, &mut height);
    }

    let audio_codec =
        first_str(exif, &["AudioFormat", "AudioCodecID"]).map(|s| audio_codec_name(&s));
    let has_audio = (kind == MediaKind::Video).then(|| {
        audio_codec.is_some()
            || tag(exif, "AudioChannels").is_some()
            || tag(exif, "AudioSampleRate").is_some()
    });

    Facts {
        capture: datetime::capture_date(name, kind, exif, tz),
        mtime: local_naive(entry.mtime_ns, tz),
        name_date: datetime::from_name(&stem).map(|n| n.dt),
        gps,
        make: tag_str(exif, "Make"),
        model: tag_str(exif, "Model"),
        software: tag_str(exif, "Software"),
        width,
        height,
        duration_s: first_f64(exif, &["Duration", "MediaDuration"]).filter(|d| *d >= 0.0),
        fps: first_f64(exif, &["VideoFrameRate", "FrameRate"]),
        video_codec: first_str(exif, &["CompressorID", "VideoCodec", "CodecID"])
            .map(|s| video_codec_name(&s)),
        audio_codec,
        has_audio,
        container: entry
            .path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase(),
        size: entry.size,
    }
}

// ----------------------------------------------------- parsed TOML values

/// A value written as a string in TOML and parsed on load.
pub trait Parse: Sized {
    fn parse(s: &str) -> Result<Self>;
}

/// A parsed value that remembers how it was written, so a saved config
/// keeps the user's text.
#[derive(Debug, Clone)]
pub struct Spec<T> {
    raw: String,
    pub val: T,
}

impl<T: Parse> Spec<T> {
    pub fn parse(s: &str) -> Result<Self> {
        Ok(Spec {
            raw: s.trim().to_string(),
            val: T::parse(s)?,
        })
    }

    pub fn raw(&self) -> &str {
        &self.raw
    }
}

impl<T> Serialize for Spec<T> {
    fn serialize<S: Serializer>(&self, ser: S) -> std::result::Result<S::Ok, S::Error> {
        ser.serialize_str(&self.raw)
    }
}

impl<'de, T: Parse> Deserialize<'de> for Spec<T> {
    fn deserialize<D: Deserializer<'de>>(de: D) -> std::result::Result<Self, D::Error> {
        struct V<T>(std::marker::PhantomData<T>);
        impl<T: Parse> V<T> {
            fn from<E: de::Error>(s: &str) -> std::result::Result<Spec<T>, E> {
                Spec::parse(s).map_err(|e| E::custom(e.to_string()))
            }
        }
        impl<T: Parse> Visitor<'_> for V<T> {
            type Value = Spec<T>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str(&crate::t!("filter.expecting_text"))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<Spec<T>, E> {
                Self::from(v)
            }
            // `width = 4000` is as good as `width = "4000"`.
            fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<Spec<T>, E> {
                Self::from(&v.to_string())
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<Spec<T>, E> {
                Self::from(&v.to_string())
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> std::result::Result<Spec<T>, E> {
                Self::from(&v.to_string())
            }
        }
        de.deserialize_any(V(std::marker::PhantomData))
    }
}

macro_rules! num_range {
    ($(#[$m:meta])* $name:ident, $unit:expr) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy)]
        pub struct $name(pub NumRange);
        impl Parse for $name {
            fn parse(s: &str) -> Result<Self> {
                NumRange::parse(s, $unit).map($name)
            }
        }
    };
}
num_range!(
    /// A plain number range.
    PlainRange, Unit::Plain
);
num_range!(
    /// A byte-size range (`1MB..20MB`).
    ByteRange, Unit::Bytes
);
num_range!(
    /// A time range in seconds (`..10s`).
    SecRange, Unit::Seconds
);

impl Parse for DateRange {
    fn parse(s: &str) -> Result<Self> {
        DateRange::parse(s)
    }
}
impl Parse for TimeOfDay {
    fn parse(s: &str) -> Result<Self> {
        TimeOfDay::parse(s)
    }
}
impl Parse for Duration {
    fn parse(s: &str) -> Result<Self> {
        parse_duration(s)
    }
}

/// Width-to-height ratio written `16:9`.
#[derive(Debug, Clone, Copy)]
pub struct Aspect(f64);

impl Parse for Aspect {
    fn parse(s: &str) -> Result<Self> {
        let bad = || crate::Error::Config(crate::t!("filter.bad_aspect", value = s));
        let (a, b) = s.split_once(':').ok_or_else(bad)?;
        let a: f64 = a.trim().parse().map_err(|_| bad())?;
        let b: f64 = b.trim().parse().map_err(|_| bad())?;
        if !(a.is_finite() && b.is_finite() && a > 0.0 && b > 0.0) {
            return Err(bad());
        }
        Ok(Aspect(a / b))
    }
}

/// One string or a list of them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Strs(pub Vec<String>);

impl Serialize for Strs {
    fn serialize<S: Serializer>(&self, ser: S) -> std::result::Result<S::Ok, S::Error> {
        match self.0.as_slice() {
            [one] => ser.serialize_str(one),
            many => {
                let mut seq = ser.serialize_seq(Some(many.len()))?;
                for s in many {
                    seq.serialize_element(s)?;
                }
                seq.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for Strs {
    fn deserialize<D: Deserializer<'de>>(de: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Strs;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str(&crate::t!("filter.expecting_text_or_list"))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<Strs, E> {
                Ok(Strs(vec![v.to_string()]))
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<Strs, A::Error> {
                let mut v = Vec::new();
                while let Some(s) = seq.next_element::<String>()? {
                    v.push(s);
                }
                Ok(Strs(v))
            }
        }
        de.deserialize_any(V)
    }
}

mod weekdays {
    use super::*;

    pub fn serialize<S: Serializer>(v: &[Weekday], ser: S) -> std::result::Result<S::Ok, S::Error> {
        let mut seq = ser.serialize_seq(Some(v.len()))?;
        for d in v {
            seq.serialize_element(&d.to_string().to_ascii_lowercase())?;
        }
        seq.end()
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        de: D,
    ) -> std::result::Result<Vec<Weekday>, D::Error> {
        let names = Vec::<String>::deserialize(de)?;
        names
            .iter()
            .map(|n| {
                n.trim()
                    .parse::<Weekday>()
                    .map_err(|_| de::Error::custom(crate::t!("filter.bad_weekday", value = n)))
            })
            .collect()
    }
}

pub(crate) fn is_false(b: &bool) -> bool {
    !*b
}

// ------------------------------------------------------------ conditions

/// Which date a `date` condition looks at.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DateSource {
    /// Same resolution as the name presets (EXIF, QuickTime, name, mtime).
    #[default]
    Capture,
    FileMtime,
    /// The date written in the file name.
    Name,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DateCond {
    #[serde(default, skip_serializing_if = "is_capture")]
    pub source: DateSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<Spec<DateRange>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub older_than: Option<Spec<Duration>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub newer_than: Option<Spec<Duration>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub months: Vec<u32>,
    #[serde(default, with = "weekdays", skip_serializing_if = "Vec::is_empty")]
    pub weekdays: Vec<Weekday>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<Spec<TimeOfDay>>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub not: bool,
}

fn is_capture(s: &DateSource) -> bool {
    *s == DateSource::Capture
}

/// A named place, possibly negated (`"!@home"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaceRef {
    pub name: String,
    pub negated: bool,
}

impl PlaceRef {
    fn parse(s: &str) -> std::result::Result<PlaceRef, String> {
        let (negated, rest) = match s.trim().strip_prefix('!') {
            Some(r) => (true, r),
            None => (false, s.trim()),
        };
        match rest.strip_prefix('@') {
            Some(n) if !n.is_empty() => Ok(PlaceRef {
                name: n.to_string(),
                negated,
            }),
            _ => Err(crate::t!("filter.bad_place_ref", value = s)),
        }
    }

    fn as_string(&self) -> String {
        format!("{}@{}", if self.negated { "!" } else { "" }, self.name)
    }
}

/// `geo`: `"@place"`, `"!@place"`, a list of those (any one), or a table
/// `{ lat, lon, km }` and/or `{ has_gps }`, with `not`.
#[derive(Debug, Clone, Default)]
pub struct GeoCond {
    /// Any one holds (OR).
    pub places: Vec<PlaceRef>,
    pub circle: Option<Place>,
    pub has_gps: Option<bool>,
    pub not: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GeoTable {
    lat: Option<f64>,
    lon: Option<f64>,
    km: Option<f64>,
    has_gps: Option<bool>,
    #[serde(default)]
    not: bool,
}

impl Serialize for GeoCond {
    fn serialize<S: Serializer>(&self, ser: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        if self.circle.is_none() && self.has_gps.is_none() && !self.not {
            return match self.places.as_slice() {
                [one] => ser.serialize_str(&one.as_string()),
                many => {
                    let mut seq = ser.serialize_seq(Some(many.len()))?;
                    for p in many {
                        seq.serialize_element(&p.as_string())?;
                    }
                    seq.end()
                }
            };
        }
        let mut m = ser.serialize_map(None)?;
        if let Some(c) = &self.circle {
            m.serialize_entry("lat", &c.lat)?;
            m.serialize_entry("lon", &c.lon)?;
            m.serialize_entry("km", &c.km)?;
        }
        if let Some(g) = self.has_gps {
            m.serialize_entry("has_gps", &g)?;
        }
        if self.not {
            m.serialize_entry("not", &true)?;
        }
        m.end()
    }
}

impl<'de> Deserialize<'de> for GeoCond {
    fn deserialize<D: Deserializer<'de>>(de: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = GeoCond;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str(&crate::t!("filter.expecting_geo"))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<GeoCond, E> {
                Ok(GeoCond {
                    places: vec![PlaceRef::parse(v).map_err(E::custom)?],
                    ..GeoCond::default()
                })
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> std::result::Result<GeoCond, A::Error> {
                let mut places = Vec::new();
                while let Some(s) = seq.next_element::<String>()? {
                    places.push(PlaceRef::parse(&s).map_err(de::Error::custom)?);
                }
                Ok(GeoCond {
                    places,
                    ..GeoCond::default()
                })
            }
            fn visit_map<A: MapAccess<'de>>(
                self,
                map: A,
            ) -> std::result::Result<GeoCond, A::Error> {
                let t = GeoTable::deserialize(de::value::MapAccessDeserializer::new(map))?;
                let circle = match (t.lat, t.lon, t.km) {
                    (Some(lat), Some(lon), Some(km)) => Some(Place { lat, lon, km }),
                    (None, None, None) => None,
                    _ => return Err(de::Error::custom(crate::t!("filter.bad_circle"))),
                };
                // No file without GPS lies in a circle: the table holds nothing.
                if t.has_gps == Some(false) && circle.is_some() {
                    return Err(de::Error::custom(crate::t!("filter.no_gps_in_circle")));
                }
                Ok(GeoCond {
                    places: Vec::new(),
                    circle,
                    has_gps: t.has_gps,
                    not: t.not,
                })
            }
        }
        de.deserialize_any(V)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceCond {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub make: Option<MaskExpr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<MaskExpr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub software: Option<MaskExpr>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub not: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Orientation {
    Portrait,
    Landscape,
    Square,
}

impl Orientation {
    fn as_str(self) -> &'static str {
        match self {
            Orientation::Portrait => "portrait",
            Orientation::Landscape => "landscape",
            Orientation::Square => "square",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PropsCond {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<Spec<ByteRange>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<Spec<PlainRange>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<Spec<PlainRange>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub megapixels: Option<Spec<PlainRange>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<Spec<SecRange>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fps: Option<Spec<PlainRange>>,
    /// Blur score 0..100. Parsed here; the routing evaluates it, because the
    /// score is computed from the pixels, not read from the metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sharpness: Option<Spec<PlainRange>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orientation: Option<Orientation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aspect: Option<Spec<Aspect>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_codec: Option<Strs>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_codec: Option<Strs>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container: Option<Strs>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_audio: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub has_tag: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing_tag: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub not: bool,
}

// ------------------------------------------------------------- checking

/// Why a file does not meet a condition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Miss {
    pub condition: &'static str,
    /// Short translated reason.
    pub detail: String,
}

pub(crate) type Verdict = std::result::Result<(), String>;

/// Apply `not`: the inner verdict is inverted as a whole.
pub(crate) fn with_not(not: bool, inner: Verdict) -> Verdict {
    match (not, inner) {
        (false, v) => v,
        (true, Ok(())) => Err(crate::t!("filter.miss.negated")),
        (true, Err(_)) => Ok(()),
    }
}

pub(crate) fn miss(condition: &'static str, v: Verdict) -> std::result::Result<(), Miss> {
    v.map_err(|detail| Miss { condition, detail })
}

fn no_tag(tag: &str) -> String {
    crate::t!("filter.miss.no_tag", tag = tag)
}

fn range_verdict(prop: &str, raw: &str, r: &NumRange, v: Option<f64>) -> Verdict {
    let v = v.ok_or_else(|| no_tag(prop))?;
    if r.contains(v) {
        Ok(())
    } else {
        Err(crate::t!(
            "filter.miss.prop_range",
            prop = prop,
            value = crate::i18n::decimal(v, 2),
            wanted = raw
        ))
    }
}

pub(crate) fn haversine_km(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (la1, lo1) = (a.0.to_radians(), a.1.to_radians());
    let (la2, lo2) = (b.0.to_radians(), b.1.to_radians());
    let h = ((la2 - la1) / 2.0).sin().powi(2)
        + la1.cos() * la2.cos() * ((lo2 - lo1) / 2.0).sin().powi(2);
    2.0 * 6371.0088 * h.sqrt().min(1.0).asin()
}

/// Whether `pos` lies within `p`.
pub fn within(p: &Place, pos: (f64, f64)) -> bool {
    haversine_km((p.lat, p.lon), pos) <= p.km
}

fn date_verdict(c: &DateCond, f: &Facts, now: NaiveDateTime) -> Verdict {
    let dt = match c.source {
        DateSource::Capture => f
            .capture
            .ok_or_else(|| crate::t!("filter.miss.no_capture_date"))?,
        DateSource::FileMtime => f.mtime,
        DateSource::Name => f
            .name_date
            .ok_or_else(|| crate::t!("filter.miss.no_name_date"))?,
    };
    let day = dt.format("%Y-%m-%d").to_string();
    if let Some(r) = &c.range
        && !r.val.contains(dt.date())
    {
        return Err(crate::t!(
            "filter.miss.date_range",
            date = day,
            wanted = r.raw()
        ));
    }
    if let Some(d) = &c.older_than
        && dt >= now - d.val
    {
        return Err(crate::t!(
            "filter.miss.date_older",
            date = day,
            wanted = d.raw()
        ));
    }
    if let Some(d) = &c.newer_than
        && dt < now - d.val
    {
        return Err(crate::t!(
            "filter.miss.date_newer",
            date = day,
            wanted = d.raw()
        ));
    }
    if !c.months.is_empty() && !c.months.contains(&dt.month()) {
        return Err(crate::t!("filter.miss.month", month = dt.month()));
    }
    if !c.weekdays.is_empty() && !c.weekdays.contains(&dt.weekday()) {
        return Err(crate::t!(
            "filter.miss.weekday",
            weekday = dt.weekday().to_string().to_ascii_lowercase()
        ));
    }
    if let Some(t) = &c.time
        && !t.val.contains(dt.time())
    {
        return Err(crate::t!(
            "filter.miss.time",
            time = format!("{:02}:{:02}", dt.time().hour(), dt.time().minute()),
            wanted = t.raw()
        ));
    }
    Ok(())
}

fn geo_verdict(cfg: &Config, c: &GeoCond, f: &Facts) -> Verdict {
    if c.has_gps == Some(false) {
        return if f.gps.is_none() {
            Ok(())
        } else {
            Err(crate::t!("filter.miss.has_gps"))
        };
    }
    let pos = f.gps.ok_or_else(|| crate::t!("filter.miss.no_gps"))?;
    if let Some(circle) = &c.circle
        && !within(circle, pos)
    {
        return Err(crate::t!(
            "filter.miss.outside_place",
            place = format!("{}, {} ({} km)", circle.lat, circle.lon, circle.km)
        ));
    }
    if !c.places.is_empty() {
        // Any one reference holds; an unknown place holds nothing.
        let holds = |p: &PlaceRef| {
            cfg.places
                .get(&p.name)
                .is_some_and(|pl| within(pl, pos) != p.negated)
        };
        if !c.places.iter().any(holds) {
            let names: Vec<String> = c.places.iter().map(PlaceRef::as_string).collect();
            return Err(match c.places.as_slice() {
                [p] if p.negated => crate::t!("filter.miss.inside_place", place = names[0]),
                _ => crate::t!("filter.miss.outside_place", place = names.join(", ")),
            });
        }
    }
    Ok(())
}

fn device_verdict(cfg: &Config, c: &DeviceCond, f: &Facts) -> Verdict {
    for (tag, mask, value) in [
        ("Make", &c.make, &f.make),
        ("Model", &c.model, &f.model),
        ("Software", &c.software, &f.software),
    ] {
        let Some(mask) = mask else { continue };
        let value = value.as_deref().ok_or_else(|| no_tag(tag))?;
        if !mask.matches(value, cfg) {
            return Err(crate::t!(
                "filter.miss.tag_mismatch",
                tag = tag,
                value = value
            ));
        }
    }
    Ok(())
}

fn one_of(prop: &str, list: &Strs, value: Option<&str>) -> Verdict {
    let v = value.ok_or_else(|| no_tag(prop))?;
    if list.0.iter().any(|s| s.trim().eq_ignore_ascii_case(v)) {
        Ok(())
    } else {
        Err(crate::t!(
            "filter.miss.prop_value",
            prop = prop,
            value = v,
            wanted = list.0.join(", ")
        ))
    }
}

/// Whether the record has a non-null tag `wanted` (`Tag` or `Group:Tag`).
#[cfg(all(test, unix))]
pub(crate) fn has_tag_in_record(exif: &Value, wanted: &str) -> bool {
    has_tag(exif, wanted)
}

fn has_tag(exif: &Value, wanted: &str) -> bool {
    let (group, name) = wanted.rsplit_once(':').unwrap_or(("", wanted));
    let named = |k: &str| {
        let (g, n) = k.rsplit_once(':').unwrap_or(("", k));
        n.eq_ignore_ascii_case(name) && (group.is_empty() || g.eq_ignore_ascii_case(group))
    };
    exif.as_object().is_some_and(|o| {
        o.iter().any(|(k, v)| !v.is_null() && named(k))
            // A cached record keeps the other tags' names only, by group.
            || o.get(crate::exif::PRESENT)
                .and_then(Value::as_object)
                .is_some_and(|groups| {
                    groups.iter().any(|(g, names)| {
                        (group.is_empty() || g.eq_ignore_ascii_case(group))
                            && names.as_array().is_some_and(|l| {
                                l.iter()
                                    .filter_map(Value::as_str)
                                    .any(|n| n.eq_ignore_ascii_case(name))
                            })
                    })
                })
    })
}

/// How `props.sharpness` is treated by a check.
#[derive(Debug, Clone, Copy)]
enum Sharpness {
    /// Not looked at (the score is not known to the caller).
    Skip,
    /// The file's score; `None` when it has none (a video, no thumbnail).
    Score(Option<f32>),
}

fn props_verdict(c: &PropsCond, f: &Facts, exif: &Value, sharp: Sharpness) -> Verdict {
    if let (Some(s), Sharpness::Score(v)) = (&c.sharpness, sharp) {
        range_verdict("sharpness", s.raw(), &s.val.0, v.map(f64::from))?;
    }
    if let Some(s) = &c.size {
        range_verdict("size", s.raw(), &s.val.0, Some(f.size as f64))?;
    }
    if let Some(s) = &c.width {
        range_verdict("width", s.raw(), &s.val.0, f.width.map(f64::from))?;
    }
    if let Some(s) = &c.height {
        range_verdict("height", s.raw(), &s.val.0, f.height.map(f64::from))?;
    }
    if let Some(s) = &c.megapixels {
        let mp = f
            .width
            .zip(f.height)
            .map(|(w, h)| f64::from(w) * f64::from(h) / 1e6);
        range_verdict("megapixels", s.raw(), &s.val.0, mp)?;
    }
    if let Some(s) = &c.duration {
        range_verdict("duration", s.raw(), &s.val.0, f.duration_s)?;
    }
    if let Some(s) = &c.fps {
        range_verdict("fps", s.raw(), &s.val.0, f.fps)?;
    }
    // `sharpness` is checked first, when the caller knows the score.
    if let Some(want) = c.orientation {
        let (w, h) = f.width.zip(f.height).ok_or_else(|| no_tag("orientation"))?;
        let have = match w.cmp(&h) {
            std::cmp::Ordering::Greater => Orientation::Landscape,
            std::cmp::Ordering::Less => Orientation::Portrait,
            std::cmp::Ordering::Equal => Orientation::Square,
        };
        if have != want {
            return Err(crate::t!(
                "filter.miss.prop_value",
                prop = "orientation",
                value = have.as_str(),
                wanted = want.as_str()
            ));
        }
    }
    if let Some(a) = &c.aspect {
        let (w, h) = f.width.zip(f.height).ok_or_else(|| no_tag("aspect"))?;
        let r = f64::from(w) / f64::from(h);
        let near = |x: f64| (x - a.val.0).abs() / a.val.0 <= 0.01;
        if !(near(r) || near(1.0 / r)) {
            return Err(crate::t!(
                "filter.miss.prop_value",
                prop = "aspect",
                value = format!("{w}:{h}"),
                wanted = a.raw()
            ));
        }
    }
    if let Some(l) = &c.video_codec {
        one_of("video_codec", l, f.video_codec.as_deref())?;
    }
    if let Some(l) = &c.audio_codec {
        one_of("audio_codec", l, f.audio_codec.as_deref())?;
    }
    if let Some(l) = &c.container {
        one_of(
            "container",
            l,
            Some(f.container.as_str()).filter(|s| !s.is_empty()),
        )?;
    }
    if let Some(want) = c.has_audio {
        let have = f.has_audio.ok_or_else(|| no_tag("audio"))?;
        if have != want {
            return Err(crate::t!(
                "filter.miss.prop_value",
                prop = "has_audio",
                value = have,
                wanted = want
            ));
        }
    }
    for t in &c.has_tag {
        if !has_tag(exif, t) {
            return Err(no_tag(t));
        }
    }
    for t in &c.missing_tag {
        if has_tag(exif, t) {
            return Err(crate::t!("filter.miss.tag_present", tag = t));
        }
    }
    Ok(())
}

fn path_verdict(cfg: &Config, mask: &MaskExpr, entry: &FileEntry) -> Verdict {
    let rel = entry.rel.to_string_lossy().replace('\\', "/");
    if mask.matches(&rel, cfg) {
        Ok(())
    } else {
        Err(crate::t!("filter.miss.path", path = rel))
    }
}

/// Check the per-file conditions of `f` (`date`, `geo`, `device`, `props`,
/// `path`) against one file; the first that fails is the `Miss`. `sources`,
/// `match`, `exclude`, `ai` and `expand` are not looked at here.
pub fn check_file(
    cfg: &Config,
    f: &Filter,
    entry: &FileEntry,
    facts: &Facts,
    exif: &Value,
    now: NaiveDateTime,
) -> std::result::Result<(), Miss> {
    check_file_in(cfg, f, entry, facts, exif, now, Sharpness::Skip)
}

/// [`check_file`] with `props.sharpness` checked too, against the file's
/// score (`None`: it has none, so the sub-option does not hold). The score
/// is part of the `props` table, so `not = true` inverts it with the rest.
pub(crate) fn check_file_scored(
    cfg: &Config,
    f: &Filter,
    entry: &FileEntry,
    facts: &Facts,
    exif: &Value,
    now: NaiveDateTime,
    sharpness: Option<f32>,
) -> std::result::Result<(), Miss> {
    check_file_in(cfg, f, entry, facts, exif, now, Sharpness::Score(sharpness))
}

fn check_file_in(
    cfg: &Config,
    f: &Filter,
    entry: &FileEntry,
    facts: &Facts,
    exif: &Value,
    now: NaiveDateTime,
    sharp: Sharpness,
) -> std::result::Result<(), Miss> {
    if let Some(c) = &f.date {
        miss("date", with_not(c.not, date_verdict(c, facts, now)))?;
    }
    if let Some(c) = &f.geo {
        miss("geo", with_not(c.not, geo_verdict(cfg, c, facts)))?;
    }
    if let Some(c) = &f.device {
        miss("device", with_not(c.not, device_verdict(cfg, c, facts)))?;
    }
    if let Some(c) = &f.props {
        miss(
            "props",
            with_not(c.not, props_verdict(c, facts, exif, sharp)),
        )?;
    }
    if let Some(m) = &f.path {
        miss("path", path_verdict(cfg, m, entry))?;
    }
    Ok(())
}

impl Filter {
    /// Settings a parse cannot catch: places that exist, months 1..12.
    /// The error is a translated message for `target`.
    pub(crate) fn check_conditions(
        &self,
        cfg: &Config,
        target: &str,
    ) -> std::result::Result<(), String> {
        if let Some(g) = &self.geo
            && let Some(p) = g.places.iter().find(|p| !cfg.places.contains_key(&p.name))
        {
            return Err(crate::t!(
                "config.unknown_place",
                target = target,
                place = p.name
            ));
        }
        if let Some(d) = &self.date
            && let Some(m) = d.months.iter().find(|m| !(1..=12).contains(*m))
        {
            return Err(crate::t!("config.bad_month", target = target, month = m));
        }
        super::groups::check_conditions(self, cfg, target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    fn dt(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").unwrap()
    }

    fn cfg() -> Config {
        let text = r#"
[places]
home = { lat = 47.0245, lon = 28.8323, km = 2 }
"#;
        toml::from_str(text).unwrap()
    }

    fn filter(text: &str) -> Filter {
        toml::from_str(text).unwrap()
    }

    fn entry(rel: &str) -> FileEntry {
        FileEntry {
            source_id: "s".into(),
            path: PathBuf::from("/root").join(rel.replace('\\', "/")),
            rel: PathBuf::from(rel),
            size: 3_000_000,
            mtime_ns: 1_700_000_000_000_000_000,
        }
    }

    fn pixel() -> Value {
        json!({
            "IFD0:Make": "Google", "IFD0:Model": "Pixel 8",
            "ExifIFD:DateTimeOriginal": "2026:07:16 23:10:00",
            "GPS:GPSLatitude": 47.02, "GPS:GPSLongitude": 28.83,
            "File:ImageWidth": 4000, "File:ImageHeight": 3000,
        })
    }

    fn screenshot() -> Value {
        json!({ "File:ImageWidth": 1080, "File:ImageHeight": 2400 })
    }

    fn video() -> Value {
        json!({
            "QuickTime:Duration": 7.5,
            "Track1:ImageWidth": 1920, "Track1:ImageHeight": 1080,
            "Track1:CompressorID": "hvc1", "Track1:VideoFrameRate": 59.94,
            "Track2:AudioFormat": "mp4a",
        })
    }

    fn run(f: &str, rel: &str, exif: &Value) -> std::result::Result<(), Miss> {
        let e = entry(rel);
        let facts = facts_of(&e, exif, None);
        check_file(
            &cfg(),
            &filter(f),
            &e,
            &facts,
            exif,
            dt("2026-08-16 12:00:00"),
        )
    }

    fn hit(f: &str, rel: &str, exif: &Value) {
        if let Err(m) = run(f, rel, exif) {
            panic!("{f}: {m:?}");
        }
    }

    fn missed(f: &str, rel: &str, exif: &Value) -> Miss {
        run(f, rel, exif).expect_err(f)
    }

    #[test]
    fn pixel_device_geo_props() {
        let p = pixel();
        hit("device = { model = \"pixel*\" }", "a.jpg", &p);
        hit(
            "device = { make = \"GOOGLE\", model = [\"nope\", \"Pixel*\"] }",
            "a.jpg",
            &p,
        );
        assert_eq!(
            missed("device = { model = \"iphone*\" }", "a.jpg", &p).condition,
            "device"
        );
        assert_eq!(
            missed("device = { software = \"*\" }", "a.jpg", &p).condition,
            "device"
        );
        hit("geo = \"@home\"", "a.jpg", &p);
        hit("geo = [\"@nowhere\", \"@home\"]", "a.jpg", &p);
        hit("geo = { lat = 47.0, lon = 28.8, km = 10 }", "a.jpg", &p);
        assert_eq!(missed("geo = \"!@home\"", "a.jpg", &p).condition, "geo");
        assert_eq!(
            missed("geo = { lat = 0.0, lon = 0.0, km = 10 }", "a.jpg", &p).condition,
            "geo"
        );
        hit(
            "props = { megapixels = \">=12\", orientation = \"landscape\" }",
            "a.jpg",
            &p,
        );
        hit("props = { width = 4000, aspect = \"4:3\" }", "a.jpg", &p);
        hit("props = { aspect = \"3:4\" }", "a.jpg", &p);
        assert_eq!(
            missed("props = { aspect = \"16:9\" }", "a.jpg", &p).condition,
            "props"
        );
        assert_eq!(
            missed("props = { orientation = \"portrait\" }", "a.jpg", &p).condition,
            "props"
        );
        hit("props = { size = \"1MB..4MB\" }", "a.jpg", &p);
        hit(
            "props = { has_tag = [\"GPSLatitude\", \"GPS:GPSLongitude\"], missing_tag = [\"Flash\"] }",
            "a.jpg",
            &p,
        );
        assert_eq!(
            missed("props = { has_tag = [\"Flash\"] }", "a.jpg", &p).condition,
            "props"
        );
        assert_eq!(
            missed("props = { missing_tag = [\"GPSLatitude\"] }", "a.jpg", &p).condition,
            "props"
        );
    }

    #[test]
    fn raw_dimensions_are_the_main_image_not_the_preview() {
        // A DNG: IFD0 holds the 256 px preview, the SubIFD the real image.
        let dng = json!({
            "IFD0:ImageWidth": 256, "IFD0:ImageHeight": 171,
            "SubIFD:ImageWidth": 6000, "SubIFD:ImageHeight": 4000,
        });
        let f = facts_of(&entry("a.dng"), &dng, None);
        assert_eq!((f.width, f.height), (Some(6000), Some(4000)));
        hit(
            "props = { width = \">=6000\", megapixels = \">=20\" }",
            "a.dng",
            &dng,
        );

        // The size exiftool works out wins over any group.
        let mut c = dng.clone();
        c["Composite:ImageSize"] = json!("5000 3000");
        let f = facts_of(&entry("a.dng"), &c, None);
        assert_eq!((f.width, f.height), (Some(5000), Some(3000)));
        c["Composite:ImageSize"] = json!("5000x3000");
        assert_eq!(facts_of(&entry("a.dng"), &c, None).width, Some(5000));
        c["File:ImageWidth"] = json!(4000);
        c["File:ImageHeight"] = json!(2000);
        let f = facts_of(&entry("a.dng"), &c, None);
        assert_eq!((f.width, f.height), (Some(4000), Some(2000)));
    }

    #[test]
    fn gps_zero_zero_is_no_fix() {
        let mut p = pixel();
        p["GPS:GPSLatitude"] = json!(0.0);
        p["GPS:GPSLongitude"] = json!(0.0);
        assert_eq!(facts_of(&entry("a.jpg"), &p, None).gps, None);
        let m = missed("geo = { lat = 0.0, lon = 0.0, km = 10 }", "a.jpg", &p);
        assert_eq!(m.detail, crate::t!("filter.miss.no_gps"));
        hit("geo = { has_gps = false }", "a.jpg", &p);
        // Only exactly (0, 0): a real point on the equator still counts.
        p["GPS:GPSLongitude"] = json!(9.5);
        assert_eq!(facts_of(&entry("a.jpg"), &p, None).gps, Some((0.0, 9.5)));
    }

    #[test]
    fn rotated_photo_swaps_dimensions() {
        let mut p = pixel();
        p["IFD0:Orientation"] = json!(6);
        hit("props = { orientation = \"portrait\" }", "a.jpg", &p);
    }

    #[test]
    fn rotated_video_swaps_dimensions() {
        let mut v = video();
        v["Track1:Rotation"] = json!(90);
        hit("props = { orientation = \"portrait\" }", "v.mp4", &v);
        v["Track1:Rotation"] = json!(270);
        hit("props = { orientation = \"portrait\" }", "v.mp4", &v);
        v["Track1:Rotation"] = json!(180);
        hit("props = { orientation = \"landscape\" }", "v.mp4", &v);
    }

    #[test]
    fn no_gps_together_with_a_circle_is_a_load_error() {
        let e =
            toml::from_str::<Filter>("geo = { has_gps = false, lat = 1.0, lon = 2.0, km = 3.0 }")
                .unwrap_err()
                .to_string();
        assert!(e.contains(&crate::t!("filter.no_gps_in_circle")), "{e}");
        assert!(
            toml::from_str::<Filter>("geo = { has_gps = true, lat = 1.0, lon = 2.0, km = 3.0 }")
                .is_ok()
        );
    }

    #[test]
    fn facts_fall_back_to_other_tags() {
        let f = facts_of(
            &entry("v.mp4"),
            &json!({
                "ExifIFD:ExifImageWidth": 640, "ExifIFD:ExifImageHeight": 480,
                "Track1:MediaDuration": 12.5,
                "Matroska:CodecID": "V_MPEGH/ISO/HEVC",
            }),
            None,
        );
        assert_eq!((f.width, f.height), (Some(640), Some(480)));
        assert_eq!(f.duration_s, Some(12.5));
        assert_eq!(f.video_codec.as_deref(), Some("hevc"));
    }

    #[test]
    fn not_inverts_props_and_a_negated_path_mask() {
        let p = pixel();
        hit("props = { width = \"..1000\", not = true }", "a.jpg", &p);
        let m = missed("props = { width = \">=1000\", not = true }", "a.jpg", &p);
        assert_eq!(
            (m.condition, m.detail),
            ("props", crate::t!("filter.miss.negated"))
        );
        hit("path = \"!WhatsApp/**\"", "Camera/x.jpg", &p);
        assert_eq!(
            missed("path = \"!WhatsApp/**\"", "WhatsApp/x.jpg", &p).condition,
            "path"
        );
    }

    #[test]
    fn screenshot_has_no_gps() {
        let s = screenshot();
        let m = missed("geo = \"@home\"", "shot.png", &s);
        assert_eq!(m.condition, "geo");
        assert_eq!(m.detail, crate::t!("filter.miss.no_gps"));
        hit("geo = { has_gps = false }", "shot.png", &s);
        assert_eq!(
            missed("geo = { has_gps = false }", "a.jpg", &pixel()).condition,
            "geo"
        );
        // missing data: the condition is false, so `not` makes it hold
        hit("geo = { has_gps = true, not = true }", "shot.png", &s);
        hit(
            "geo = { lat = 47.0, lon = 28.8, km = 10, not = true }",
            "shot.png",
            &s,
        );
        hit(
            "device = { model = \"pixel*\", not = true }",
            "shot.png",
            &s,
        );
        hit(
            "date = { range = \"2026-01-01..\", not = true }",
            "shot.png",
            &s,
        );
        assert_eq!(
            missed("date = { range = \"2026-01-01..\" }", "shot.png", &s).detail,
            crate::t!("filter.miss.no_capture_date")
        );
        assert_eq!(
            missed(
                "device = { model = \"pixel*\", not = true }",
                "a.jpg",
                &pixel()
            )
            .detail,
            crate::t!("filter.miss.negated")
        );
    }

    #[test]
    fn date_conditions() {
        let p = pixel(); // captured 2026-07-16 (Thu) 23:10; now is 2026-08-16
        hit("date = { time = \"22:00..05:00\" }", "a.jpg", &p);
        assert_eq!(
            missed("date = { time = \"05:00..22:00\" }", "a.jpg", &p).condition,
            "date"
        );
        hit("date = { older_than = \"30d\" }", "a.jpg", &p);
        assert_eq!(
            missed("date = { older_than = \"31d\" }", "a.jpg", &p).condition,
            "date"
        );
        hit("date = { newer_than = \"1y\" }", "a.jpg", &p);
        assert_eq!(
            missed("date = { newer_than = \"7d\" }", "a.jpg", &p).condition,
            "date"
        );
        hit(
            "date = { range = \"2026-07-01..2026-07-31\", months = [6, 7], weekdays = [\"thu\"] }",
            "a.jpg",
            &p,
        );
        assert_eq!(
            missed("date = { weekdays = [\"sat\", \"sun\"] }", "a.jpg", &p).condition,
            "date"
        );
        assert_eq!(
            missed("date = { months = [8] }", "a.jpg", &p).condition,
            "date"
        );
        assert_eq!(
            missed("date = { range = \"2026-08-01..\" }", "a.jpg", &p).condition,
            "date"
        );
    }

    #[test]
    fn date_sources() {
        let s = screenshot();
        hit(
            "date = { source = \"name\", range = \"2026-07-16..2026-07-16\", time = \"21:00..22:00\" }",
            "20260716_211554.png",
            &s,
        );
        assert_eq!(
            missed("date = { source = \"name\" }", "shot.png", &s).detail,
            crate::t!("filter.miss.no_name_date")
        );
        // mtime 2023-11-14 22:13:20 UTC
        hit(
            "date = { source = \"file_mtime\", range = \"2023-11-13..2023-11-15\" }",
            "shot.png",
            &s,
        );
        assert_eq!(
            missed(
                "date = { source = \"file_mtime\", range = \"2026-01-01..\" }",
                "shot.png",
                &s
            )
            .condition,
            "date"
        );
    }

    #[test]
    fn video_props() {
        let v = video();
        hit(
            "props = { video_codec = [\"hevc\", \"h264\"], audio_codec = \"aac\", duration = \"..10s\", fps = \">=50\", has_audio = true, container = \"mp4\" }",
            "v.mp4",
            &v,
        );
        assert_eq!(
            missed("props = { has_audio = false }", "v.mp4", &v).condition,
            "props"
        );
        assert_eq!(
            missed("props = { video_codec = \"h264\" }", "v.mp4", &v).condition,
            "props"
        );
        assert_eq!(
            missed("props = { container = \"mov\" }", "v.mp4", &v).condition,
            "props"
        );
        // a photo has no audio information
        assert_eq!(
            missed("props = { has_audio = false }", "a.jpg", &pixel()).condition,
            "props"
        );
        let f = facts_of(
            &entry("v.mp4"),
            &json!({"Track1:CompressorID": "avc1", "Track2:AudioFormat": "x"}),
            None,
        );
        assert_eq!(f.video_codec.as_deref(), Some("h264"));
        let f = facts_of(
            &entry("v.mp4"),
            &json!({"Track1:CompressorID": "av01"}),
            None,
        );
        assert_eq!(
            (f.video_codec.as_deref(), f.has_audio),
            (Some("av1"), Some(false))
        );
    }

    #[test]
    fn sharpness_is_parsed_but_not_checked_here() {
        let f = filter("props = { sharpness = \"<40\" }");
        assert!(f.props.as_ref().unwrap().sharpness.is_some());
        hit("props = { sharpness = \"<40\" }", "a.jpg", &pixel());
        assert!(toml::from_str::<Filter>("props = { sharpness = \"lots\" }").is_err());
    }

    #[test]
    fn path_matches_relative_path() {
        let p = pixel();
        hit("path = \"WhatsApp/**\"", "WhatsApp/Media/x.jpg", &p);
        hit("path = \"WhatsApp/**\"", "WhatsApp\\Media\\x.jpg", &p);
        hit(
            "path = [\"Camera/**\", \"WhatsApp/**\"]",
            "WhatsApp/Media/x.jpg",
            &p,
        );
        assert_eq!(
            missed("path = \"WhatsApp/**\"", "Camera/x.jpg", &p).condition,
            "path"
        );
        assert_eq!(
            missed("path = \"!WhatsApp/**\"", "WhatsApp/x.jpg", &p).condition,
            "path"
        );
    }

    #[test]
    fn haversine_known_distance() {
        // Chisinau to Bucharest, about 365 km
        let d = haversine_km((47.0105, 28.8638), (44.4268, 26.1025));
        assert!((d - 365.0).abs() < 15.0, "{d}");
    }

    #[test]
    fn conditions_round_trip_through_toml() {
        let text = r#"
sources = ["a"]
geo = ["@home", "!@work"]
path = "WhatsApp/**"

[date]
source = "name"
range = "2026-08-12..2026-08-18"
older_than = "30d"
months = [6, 7]
weekdays = ["sat", "sun"]
time = "22:00..05:00"

[device]
model = "Pixel*"

[props]
size = "..500KB"
aspect = "16:9"
video_codec = ["hevc", "h264"]
container = "mp4"
sharpness = "<40"
has_tag = ["GPSLatitude"]
"#;
        let f = filter(text);
        let back = toml::to_string(&f).unwrap();
        let g: Filter = toml::from_str(&back).unwrap();
        assert_eq!(toml::to_string(&g).unwrap(), back);
        assert!(back.contains("\"!@work\"") && back.contains("..500KB") && back.contains("sat"));
        let circle = filter("geo = { lat = 1.0, lon = 2.0, km = 3.0, not = true }");
        let back = toml::to_string(&circle).unwrap();
        assert!(toml::from_str::<Filter>(&back).unwrap().geo.unwrap().not);
    }

    #[test]
    fn bad_condition_values_are_load_errors() {
        for bad in [
            "geo = \"home\"",
            "geo = { lat = 1.0 }",
            "geo = { km = 1.0, lat = 1.0, lon = 1.0, nope = 1 }",
            "date = { weekdays = [\"funday\"] }",
            "date = { older_than = \"soon\" }",
            "props = { aspect = \"wide\" }",
            "props = { size = \"big\" }",
            "device = { nope = \"x\" }",
        ] {
            assert!(toml::from_str::<Filter>(bad).is_err(), "{bad}");
        }
    }
}
