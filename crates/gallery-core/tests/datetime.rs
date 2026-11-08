mod support;

use gallery_core::{datetime, exif, media::MediaKind, tools::Tools};

fn dt(s: &str) -> chrono::NaiveDateTime {
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S").unwrap()
}

#[test]
fn capture_date_from_real_files() {
    require_tool!("magick");
    require_tool!("ffmpeg");
    require_tool!("exiftool");
    let dir = tempfile::tempdir().unwrap();
    let photo = support::fixture_jpeg(dir.path(), "p.jpg", Some("2026:04:12 10:33:14"), None);
    let video = support::fixture_mp4(dir.path(), "v.mp4", 1, Some("2026:04:12 10:00:00"));
    let tools = Tools::discover().unwrap();
    let m = exif::read(&tools, &[photo.clone(), video.clone()]).unwrap();
    assert_eq!(
        datetime::capture_date("p.jpg", MediaKind::Photo, &m[&photo], None),
        Some(dt("2026-04-12 10:33:14"))
    );
    // ffmpeg leaves "0000:00:00" in other tags; CreateDate is local time
    // (here the test host's zone), so only check it resolves to a valid date.
    let got = datetime::capture_date("v.mp4", MediaKind::Video, &m[&video], None).unwrap();
    assert_eq!(got.date().to_string(), "2026-04-12");
}
