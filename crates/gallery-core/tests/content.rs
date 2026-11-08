mod support;

use chrono::{Local, NaiveDate, NaiveDateTime, TimeZone, Utc};
use gallery_core::config::{ContentPreset, MetadataPreset};
use gallery_core::content::{StepCtx, run_pipe};
use gallery_core::{exif, tools::Tools};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

fn hash(p: &Path) -> blake3::Hash {
    blake3::hash(&std::fs::read(p).unwrap())
}

fn mtime(p: &Path) -> SystemTime {
    std::fs::metadata(p).unwrap().modified().unwrap()
}

fn read_one(tools: &Tools, p: &Path) -> Value {
    exif::read(tools, &[p.to_path_buf()])
        .unwrap()
        .remove(p)
        .unwrap()
}

fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> NaiveDateTime {
    NaiveDate::from_ymd_opt(y, mo, d)
        .unwrap()
        .and_hms_opt(h, mi, s)
        .unwrap()
}

/// Run a single full metadata step on `src` and return (result path, warnings).
fn run_meta(
    tools: &Tools,
    src: &Path,
    capture: Option<NaiveDateTime>,
    tmp: &Path,
) -> (PathBuf, Vec<String>) {
    let exif = read_one(tools, src);
    let ctx = StepCtx {
        tools,
        original: src,
        original_exif: &exif,
        capture,
        tmp_dir: tmp,
    };
    let preset = ContentPreset::Metadata(MetadataPreset::full());
    let r = run_pipe(&ctx, &[&preset]).unwrap();
    assert!(r.path.starts_with(tmp), "{:?}", r.path);
    (r.path, r.warnings)
}

#[test]
fn empty_pipe_is_byte_copy() {
    require_tool!("magick");
    require_tool!("exiftool");
    let dir = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let src = support::fixture_jpeg(dir.path(), "a.jpg", Some("2026:04:12 10:33:14"), None);
    let tools = Tools::discover().unwrap();
    let exif = read_one(&tools, &src);
    let ctx = StepCtx {
        tools: &tools,
        original: &src,
        original_exif: &exif,
        capture: None,
        tmp_dir: tmp.path(),
    };
    let r = run_pipe(&ctx, &[]).unwrap();
    assert_eq!(r.path, tmp.path().join("a.jpg"));
    assert_eq!(r.ext, "jpg");
    assert!(!r.fell_back);
    assert_eq!(hash(&r.path), hash(&src));
    assert!(src.exists());
}

#[test]
fn metadata_writes_capture_date() {
    require_tool!("magick");
    require_tool!("exiftool");
    require_tool!("ffmpeg");
    let dir = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let tools = Tools::discover().unwrap();
    let when = at(2026, 4, 12, 10, 33, 14);
    let want_mtime: SystemTime = Local.from_local_datetime(&when).unwrap().into();

    let jpg = support::fixture_jpeg(dir.path(), "a.jpg", None, None);
    let (out, _) = run_meta(&tools, &jpg, Some(when), tmp.path());
    let m = read_one(&tools, &out);
    assert_eq!(m["ExifIFD:DateTimeOriginal"], "2026:04:12 10:33:14");
    assert_eq!(m["ExifIFD:CreateDate"], "2026:04:12 10:33:14");
    assert_eq!(mtime(&out), want_mtime);

    // QuickTime stores UTC; read back under QuickTimeUTC it is local again.
    let mp4 = support::fixture_mp4(dir.path(), "v.mp4", 1, None);
    let (out, _) = run_meta(&tools, &mp4, Some(when), tmp.path());
    let m = read_one(&tools, &out);
    let created = m["QuickTime:CreateDate"].as_str().unwrap();
    assert!(created.starts_with("2026:04:12 10:33:14"), "{created}");
    assert_eq!(mtime(&out), want_mtime);
}

#[test]
fn metadata_fixes_gcam_make_model() {
    require_tool!("magick");
    require_tool!("exiftool");
    let dir = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let src = support::fixture_jpeg(dir.path(), "a.jpg", None, None);
    support::exiftool_set(
        &src,
        &[
            "-Make=M2012K11AG (alioth)",
            "-Model=Xiaomi",
            "-LensMake=M2012K11AG (alioth)",
            "-LensModel=Xiaomi",
        ],
    );
    let tools = Tools::discover().unwrap();
    let (out, _) = run_meta(&tools, &src, None, tmp.path());
    let m = read_one(&tools, &out);
    assert_eq!(m["IFD0:Make"], "Xiaomi");
    assert_eq!(m["IFD0:Model"], "M2012K11AG");
    assert!(m.get("ExifIFD:LensMake").is_none(), "{m}");
    assert!(m.get("ExifIFD:LensModel").is_none(), "{m}");
}

#[test]
fn metadata_offset_from_gps() {
    require_tool!("magick");
    require_tool!("exiftool");
    let dir = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let src = support::fixture_jpeg(
        dir.path(),
        "a.jpg",
        Some("2026:07:01 12:00:00"),
        Some((47.0105, 28.8638)),
    );
    support::exiftool_set(
        &src,
        &["-GPSDateStamp=2026:07:01", "-GPSTimeStamp=09:00:00"],
    );
    let tools = Tools::discover().unwrap();
    let (out, warnings) = run_meta(&tools, &src, None, tmp.path());
    assert!(warnings.is_empty(), "{warnings:?}");
    let m = read_one(&tools, &out);
    assert_eq!(m["ExifIFD:OffsetTimeOriginal"], "+03:00");
    assert_eq!(m["ExifIFD:OffsetTimeDigitized"], "+03:00");
    assert_eq!(m["ExifIFD:OffsetTime"], "+03:00");
    // The capture time itself is untouched.
    assert_eq!(m["ExifIFD:DateTimeOriginal"], "2026:07:01 12:00:00");
}

#[test]
fn metadata_clock_skew_warns_no_zone() {
    require_tool!("magick");
    require_tool!("exiftool");
    let dir = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let src = support::fixture_jpeg(
        dir.path(),
        "a.jpg",
        Some("2026:07:01 12:00:00"),
        Some((47.0105, 28.8638)),
    );
    support::exiftool_set(
        &src,
        &["-GPSDateStamp=2026:07:01", "-GPSTimeStamp=06:00:00"],
    );
    let tools = Tools::discover().unwrap();
    let (out, warnings) = run_meta(&tools, &src, None, tmp.path());
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].starts_with('⏰'), "{}", warnings[0]);
    assert!(warnings[0].contains("+3 h"), "{}", warnings[0]);
    let m = read_one(&tools, &out);
    assert!(m.get("ExifIFD:OffsetTimeOriginal").is_none(), "{m}");
    assert!(m.get("ExifIFD:OffsetTime").is_none(), "{m}");
}

#[test]
fn metadata_never_touches_source() {
    require_tool!("magick");
    require_tool!("exiftool");
    let dir = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let src = support::fixture_jpeg(
        dir.path(),
        "a.jpg",
        Some("2026:07:01 12:00:00"),
        Some((47.0105, 28.8638)),
    );
    support::exiftool_set(&src, &["-Make=M2012K11AG (alioth)", "-Model=Xiaomi"]);
    let before = (hash(&src), mtime(&src));
    let tools = Tools::discover().unwrap();
    let preset = ContentPreset::Metadata(MetadataPreset::full());
    let exif = read_one(&tools, &src);
    let ctx = StepCtx {
        tools: &tools,
        original: &src,
        original_exif: &exif,
        capture: Some(at(2020, 1, 1, 0, 0, 0)),
        tmp_dir: tmp.path(),
    };
    // Two steps: the second reads the first one's output, which is then removed.
    let r = run_pipe(&ctx, &[&preset, &preset]).unwrap();
    assert_eq!((hash(&src), mtime(&src)), before);
    let left: Vec<_> = std::fs::read_dir(tmp.path()).unwrap().collect();
    assert_eq!(left.len(), 1, "intermediate files left: {left:?}");
    assert_ne!(hash(&r.path), before.0);
}

#[test]
fn metadata_mkv_creation_time() {
    require_tool!("ffmpeg");
    require_tool!("ffprobe");
    require_tool!("exiftool");
    let dir = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let src = support::fixture_mkv(dir.path(), "v.mkv");
    let tools = Tools::discover().unwrap();
    let when = at(2026, 4, 12, 10, 33, 14);
    let (out, _) = run_meta(&tools, &src, Some(when), tmp.path());
    assert_eq!(out.extension().unwrap(), "mkv");
    let probe = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "format_tags=creation_time"])
        .args(["-of", "default=nw=1:nk=1"])
        .arg(&out)
        .output()
        .unwrap();
    let got = String::from_utf8_lossy(&probe.stdout).trim().to_string();
    let utc = Local
        .from_local_datetime(&when)
        .unwrap()
        .with_timezone(&Utc)
        .format("%Y-%m-%dT%H:%M:%S")
        .to_string();
    assert!(got.starts_with(&utc), "{got} vs {utc}");
    let want_mtime: SystemTime = Local.from_local_datetime(&when).unwrap().into();
    assert_eq!(mtime(&out), want_mtime);
}

#[test]
fn unimplemented_kind_errors() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("a.png");
    std::fs::write(&src, b"x").unwrap();
    let Ok(tools) = Tools::discover() else { return };
    let exif = Value::Null;
    let ctx = StepCtx {
        tools: &tools,
        original: &src,
        original_exif: &exif,
        capture: None,
        tmp_dir: dir.path(),
    };
    let preset = ContentPreset::Png(Default::default());
    assert!(run_pipe(&ctx, &[&preset]).is_err());
}

/// Full metadata step with a capture date on `src`; expects a pass-through.
fn assert_pass_through(src: &Path, warning: &str) {
    let tmp = tempfile::tempdir().unwrap();
    let tools = Tools::discover().unwrap();
    let when = at(2026, 4, 12, 10, 33, 14);
    let exif = Value::Null;
    let ctx = StepCtx {
        tools: &tools,
        original: src,
        original_exif: &exif,
        capture: Some(when),
        tmp_dir: tmp.path(),
    };
    let preset = ContentPreset::Metadata(MetadataPreset::full());
    let r = run_pipe(&ctx, &[&preset]).unwrap();
    assert_eq!(hash(&r.path), hash(src));
    assert_eq!(r.warnings.len(), 1, "{:?}", r.warnings);
    assert!(r.warnings[0].contains(warning), "{}", r.warnings[0]);
    let want_mtime: SystemTime = Local.from_local_datetime(&when).unwrap().into();
    assert_eq!(mtime(&r.path), want_mtime);
}

#[test]
fn metadata_unwritable_type_passes_through() {
    require_tool!("exiftool");
    require_tool!("ffmpeg");
    let dir = tempfile::tempdir().unwrap();
    let txt = dir.path().join("notes.txt");
    std::fs::write(&txt, b"plain text").unwrap();
    assert_pass_through(&txt, "exiftool does not write txt");
    let mp3 = dir.path().join("a.mp3");
    let out = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i", "sine", "-t", "1"])
        .arg(&mp3)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_pass_through(&mp3, "exiftool does not write mp3");
}

#[test]
fn metadata_mkv_remux_failure_passes_through() {
    require_tool!("ffmpeg");
    require_tool!("exiftool");
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("broken.mkv");
    std::fs::write(&src, b"definitely not matroska").unwrap();
    assert_pass_through(&src, "ffmpeg could not write the date");
}

#[test]
fn metadata_corrupt_writable_type_errors() {
    require_tool!("exiftool");
    let dir = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let src = dir.path().join("broken.jpg");
    std::fs::write(&src, b"not a jpeg at all").unwrap();
    let tools = Tools::discover().unwrap();
    let exif = Value::Null;
    let ctx = StepCtx {
        tools: &tools,
        original: &src,
        original_exif: &exif,
        capture: Some(at(2026, 4, 12, 10, 33, 14)),
        tmp_dir: tmp.path(),
    };
    let preset = ContentPreset::Metadata(MetadataPreset::full());
    let err = run_pipe(&ctx, &[&preset]).unwrap_err().to_string();
    assert!(err.contains("broken.jpg"), "{err}");
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
}
