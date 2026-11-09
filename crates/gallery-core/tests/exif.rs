mod support;

use gallery_core::{exif, tools::Tools};

#[test]
fn exif_read_roundtrip() {
    require_tool!("magick");
    require_tool!("exiftool");
    let dir = tempfile::tempdir().unwrap();
    let p = support::fixture_jpeg(dir.path(), "a.jpg", Some("2026:04:12 10:33:14"), None);
    let q = support::fixture_jpeg(dir.path(), "b.jpg", None, None);
    let tools = Tools::discover().unwrap();
    let m = exif::read(&tools, &[p.clone(), q.clone()]).unwrap();
    assert_eq!(m[&p]["ExifIFD:DateTimeOriginal"], "2026:04:12 10:33:14");
    assert!(m[&q].get("ExifIFD:DateTimeOriginal").is_none());
}

#[test]
fn exif_write_then_read() {
    require_tool!("magick");
    require_tool!("exiftool");
    let dir = tempfile::tempdir().unwrap();
    let p = support::fixture_jpeg(dir.path(), "a.jpg", None, None);
    let tools = Tools::discover().unwrap();
    exif::write(&tools, &p, &["-OffsetTimeOriginal=+03:00".to_string()]).unwrap();
    let m = exif::read(&tools, std::slice::from_ref(&p)).unwrap();
    assert_eq!(m[&p]["ExifIFD:OffsetTimeOriginal"], "+03:00");
}

#[test]
fn exif_write_error_is_reported() {
    require_tool!("exiftool");
    let tools = Tools::discover().unwrap();
    let r = exif::write(
        &tools,
        std::path::Path::new("/nonexistent/x.jpg"),
        &["-Artist=x".to_string()],
    );
    assert!(r.is_err());
}

/// exiftool's own `-api QuickTimeUTC` follows the process `TZ`, which only
/// unix honours; gallery converts in Rust instead (next test).
#[cfg(unix)]
#[test]
fn quicktime_create_date_utc_converts_to_local_under_n() {
    require_tool!("ffmpeg");
    require_tool!("exiftool");
    let dir = tempfile::tempdir().unwrap();
    let p = support::fixture_mp4(dir.path(), "v.mp4", 1, Some("2026:04:12 10:00:00"));
    let out = std::process::Command::new("exiftool")
        .env("TZ", "Europe/Chisinau")
        .args(["-j", "-n", "-G1", "-api", "QuickTimeUTC"])
        .arg(&p)
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    // Chisinau is UTC+3 in April (EEST): 10:00 UTC -> 13:00 local.
    // Note: the value carries the local offset suffix even under -n.
    assert_eq!(
        v[0]["QuickTime:CreateDate"], "2026:04:12 13:00:00+03:00",
        "{}",
        v[0]
    );
}

/// The plan's zone decides the local QuickTime dates on every OS, whatever
/// zone the process (or Windows) is in.
#[test]
fn quicktime_dates_read_in_the_tools_zone() {
    require_tool!("ffmpeg");
    require_tool!("exiftool");
    let dir = tempfile::tempdir().unwrap();
    let p = support::fixture_mp4(dir.path(), "v.mp4", 1, Some("2026:04:12 10:00:00"));
    let at = |zone: &str| {
        let tools = Tools {
            tz: Some(zone.into()),
            ..Tools::discover().unwrap()
        };
        exif::read(&tools, std::slice::from_ref(&p)).unwrap()[&p]["QuickTime:CreateDate"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(at("Europe/Chisinau"), "2026:04:12 13:00:00+03:00");
    assert_eq!(at("Asia/Tokyo"), "2026:04:12 19:00:00+09:00");
    assert_eq!(at("America/New_York"), "2026:04:12 06:00:00-04:00");
    assert_eq!(at("UTC"), "2026:04:12 10:00:00+00:00");
}

#[test]
fn exif_write_nothing_to_change_is_ok() {
    require_tool!("magick");
    require_tool!("exiftool");
    let dir = tempfile::tempdir().unwrap();
    let p = support::fixture_jpeg(dir.path(), "a.jpg", None, None);
    let tools = Tools::discover().unwrap();
    exif::write(&tools, &p, &["-XMP-GCamera:all=".to_string()]).unwrap();
}

#[test]
fn exif_read_reports_batches_and_stops_on_cancel() {
    use gallery_core::progress::{Cancel, NoProgress, ProgressEvent, Stage};
    use std::sync::Mutex;
    // Cancelled before the first batch: no tool runs at all.
    let fake = Tools {
        ffmpeg: "/nonexistent".into(),
        ffprobe: "/nonexistent".into(),
        exiftool: "/nonexistent".into(),
        avifenc: "/nonexistent".into(),
        magick: "/nonexistent".into(),
        optipng: None,
        tz: None,
    };
    let cancel = Cancel::new();
    cancel.cancel();
    let r = exif::read_with(&fake, &["/x.jpg".into()], &NoProgress, &cancel);
    assert!(matches!(r, Err(gallery_core::Error::Cancelled)), "{r:?}");

    require_tool!("magick");
    require_tool!("exiftool");
    let dir = tempfile::tempdir().unwrap();
    let p = support::fixture_jpeg(dir.path(), "a.jpg", None, None);
    let q = support::fixture_jpeg(dir.path(), "b.jpg", None, None);
    let tools = Tools::discover().unwrap();
    let evs = Mutex::new(Vec::new());
    let rec = |e: ProgressEvent| evs.lock().unwrap().push(e);
    let m = exif::read_with(&tools, &[p, q], &rec, &Cancel::new()).unwrap();
    assert_eq!(m.len(), 2);
    let evs = evs.into_inner().unwrap();
    assert_eq!(evs.len(), 1, "{evs:?}");
    assert_eq!(
        (evs[0].stage, evs[0].done, evs[0].total),
        (Stage::Exif, 2, 2)
    );
}

#[test]
fn exif_write_carries_non_ascii_values_and_paths() {
    require_tool!("magick");
    require_tool!("exiftool");
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("фото 写真");
    std::fs::create_dir_all(&sub).unwrap();
    let p = support::fixture_jpeg(&sub, "кот.jpg", None, None);
    let tools = Tools::discover().unwrap();
    // Through the UTF-8 argument file: Cyrillic and CJK survive on every
    // platform (on Windows the command line would turn them into `?`).
    exif::write(
        &tools,
        &p,
        &[
            "-XMP-dc:Subject+=кот".to_string(),
            "-XMP-dc:Subject+=猫".to_string(),
            "-XMP-dc:Description=Рыжий кот, 猫.".to_string(),
        ],
    )
    .unwrap();
    let m = exif::read(&tools, std::slice::from_ref(&p)).unwrap();
    assert_eq!(m[&p]["XMP-dc:Subject"], serde_json::json!(["кот", "猫"]));
    assert_eq!(m[&p]["XMP-dc:Description"], "Рыжий кот, 猫.");
}
