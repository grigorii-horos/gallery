mod support;

use gallery_core::ai::thumb;
use gallery_core::media::MediaKind;
use gallery_core::tools::{Tools, find};
use std::path::Path;
use std::process::Command;

fn tools() -> Tools {
    let p = |n: &str| find(n).unwrap_or_else(|| n.into());
    Tools {
        ffmpeg: p("ffmpeg"),
        ffprobe: p("ffprobe"),
        exiftool: p("exiftool"),
        avifenc: p("avifenc"),
        magick: p("magick"),
        optipng: None,
        tz: None,
    }
}

fn size(p: &Path) -> String {
    let out = Command::new("magick")
        .args(["identify", "-format", "%wx%h"])
        .arg(p)
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn thumb_photo_512() {
    require_tool!("magick");
    let d = tempfile::tempdir().unwrap();
    let src = support::fixture_jpeg(d.path(), "p.jpg", None, None);
    let out = d.path().join("out");
    let t = thumb::make(&tools(), &src, MediaKind::Photo, &out).unwrap();
    assert!(t.starts_with(&out));
    assert_eq!(size(&t), "512x384");
}

#[test]
fn thumb_video_strip() {
    require_tool!("magick");
    require_tool!("ffmpeg");
    require_tool!("ffprobe");
    require_tool!("exiftool");
    let d = tempfile::tempdir().unwrap();
    let src = support::fixture_mp4(d.path(), "v.mp4", 4, None);
    let out = d.path().join("out");
    let t = thumb::make(&tools(), &src, MediaKind::Video, &out).unwrap();
    // three 400x300 frames with +2+2 geometry: 3*(400+4) x (300+4)
    assert_eq!(size(&t), "1212x304");
    assert!(!out.join("_frame_1.jpg").exists());
}
