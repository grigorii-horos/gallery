mod support;

use gallery_core::config::{ContentPreset, PhotoFormat, PhotoPreset};
use gallery_core::content::{StepCtx, StepResult, run_pipe};
use gallery_core::{exif, tools::Tools};
use std::path::Path;
use std::process::Command;

fn encode(
    tools: &Tools,
    src: &Path,
    tmp: &Path,
    preset: PhotoPreset,
) -> gallery_core::Result<StepResult> {
    let exif = exif::read(tools, &[src.to_path_buf()])
        .unwrap()
        .remove(src)
        .unwrap();
    let ctx = StepCtx {
        tools,
        original: src,
        original_exif: &exif,
        capture: None,
        tmp_dir: tmp,
    };
    run_pipe(&ctx, &[&ContentPreset::Photo(preset)])
}

fn size(p: &Path) -> u64 {
    std::fs::metadata(p).unwrap().len()
}

fn dims(p: &Path) -> (u32, u32) {
    let out = Command::new("magick")
        .args(["identify", "-format", "%w %h"])
        .arg(format!("{}[0]", p.display()))
        .output()
        .unwrap();
    assert!(out.status.success(), "identify {}", p.display());
    let s = String::from_utf8(out.stdout).unwrap();
    let mut it = s.split_whitespace().map(|v| v.parse().unwrap());
    (it.next().unwrap(), it.next().unwrap())
}

fn fast(mut p: PhotoPreset) -> PhotoPreset {
    p.speed = if p.format == PhotoFormat::Avif { 10 } else { 0 };
    p
}

#[test]
fn photo_jpeg_to_avif() {
    require_tool!("magick");
    require_tool!("exiftool");
    require_tool!("avifenc");
    let dir = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let src = support::fixture_jpeg(dir.path(), "a.jpg", Some("2026:04:12 10:33:14"), None);
    let tools = Tools::discover().unwrap();
    let r = encode(&tools, &src, tmp.path(), fast(PhotoPreset::avif_high())).unwrap();
    assert!(!r.fell_back, "{:?}", r.warnings);
    assert_eq!(r.ext, "avif");
    assert!(r.path.starts_with(tmp.path()));
    assert!(size(&r.path) < size(&src));
    assert_eq!(dims(&r.path), (640, 480));
    let rec = exif::read(&tools, std::slice::from_ref(&r.path))
        .unwrap()
        .remove(&r.path)
        .unwrap();
    assert_eq!(rec["ExifIFD:DateTimeOriginal"], "2026:04:12 10:33:14");
    assert!(src.exists());
}

#[test]
fn photo_jpeg_to_webp() {
    require_tool!("magick");
    require_tool!("exiftool");
    let dir = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let src = support::fixture_jpeg(
        dir.path(),
        "a.jpg",
        Some("2026:04:12 10:33:14"),
        Some((48.85, 2.35)),
    );
    let tools = Tools::discover().unwrap();
    let r = encode(&tools, &src, tmp.path(), fast(PhotoPreset::webp_high())).unwrap();
    assert_eq!(r.ext, "webp");
    assert!(!r.fell_back, "{:?}", r.warnings);
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    assert_eq!(dims(&r.path), (640, 480));
    let rec = exif::read(&tools, std::slice::from_ref(&r.path))
        .unwrap()
        .remove(&r.path)
        .unwrap();
    assert_eq!(rec["ExifIFD:DateTimeOriginal"], "2026:04:12 10:33:14");
    assert!(rec.get("GPS:GPSLatitude").is_some(), "{rec}");
}

#[test]
fn photo_heic_first_frame() {
    require_tool!("magick");
    require_tool!("exiftool");
    require_tool!("avifenc");
    let dir = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let src = dir.path().join("a.heic");
    let made = Command::new("magick")
        .args(["-size", "640x480", "gradient:red-blue", "-quality", "100"])
        .arg(&src)
        .output()
        .unwrap();
    if !made.status.success() {
        eprintln!("skip: magick cannot write HEIC");
        return;
    }
    let tools = Tools::discover().unwrap();
    let r = encode(&tools, &src, tmp.path(), fast(PhotoPreset::avif_high())).unwrap();
    assert_eq!(r.ext, "avif");
    assert_eq!(dims(&r.path), (640, 480));
}

#[test]
fn photo_undecodable_input_names_file() {
    require_tool!("magick");
    require_tool!("exiftool");
    require_tool!("avifenc");
    let dir = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let src = dir.path().join("broken.heic");
    std::fs::write(&src, b"not an image").unwrap();
    let tools = Tools::discover().unwrap();
    let e = encode(&tools, &src, tmp.path(), fast(PhotoPreset::avif_high())).unwrap_err();
    assert!(e.to_string().contains("broken.heic"), "{e}");
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
}

#[test]
fn photo_resize_short_side() {
    require_tool!("magick");
    require_tool!("exiftool");
    require_tool!("avifenc");
    let dir = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let src = dir.path().join("big.jpg");
    let made = Command::new("magick")
        .args(["-size", "4000x3000", "gradient:red-blue", "-quality", "95"])
        .arg(&src)
        .output()
        .unwrap();
    assert!(made.status.success());
    let tools = Tools::discover().unwrap();
    let mut p = fast(PhotoPreset::avif_high());
    p.max_short_side = Some(1080);
    let r = encode(&tools, &src, tmp.path(), p).unwrap();
    assert_eq!(dims(&r.path), (1440, 1080));
    // Only the result is left in tmp_dir (the intermediate PNG is gone).
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 1);
}

#[test]
fn photo_conversion_is_kept_even_when_larger() {
    require_tool!("magick");
    require_tool!("exiftool");
    require_tool!("avifenc");
    let dir = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let src = dir.path().join("tiny.jpg");
    let made = Command::new("magick")
        .args(["-size", "8x8", "gradient:red-blue", "-quality", "20"])
        .arg(&src)
        .output()
        .unwrap();
    assert!(made.status.success());
    let tools = Tools::discover().unwrap();
    let r = encode(&tools, &src, tmp.path(), fast(PhotoPreset::avif_high())).unwrap();
    // jpg to avif is a conversion: no size check, no ladder, no warning.
    assert!(!r.fell_back, "{:?}", r.warnings);
    assert_eq!(r.ext, "avif");
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    assert!(size(&r.path) >= size(&src), "the fixture must not shrink");
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 1);
}

/// Same-format recompression keeps the source when it cannot shrink it.
#[test]
fn photo_same_format_falls_back_to_original() {
    require_tool!("magick");
    require_tool!("exiftool");
    require_tool!("avifenc");
    let dir = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let png = dir.path().join("tiny.png");
    let src = dir.path().join("tiny.avif");
    let made = Command::new("magick")
        .args(["-size", "8x8", "xc:white"])
        .arg(&png)
        .output()
        .unwrap();
    assert!(made.status.success());
    let made = Command::new("avifenc")
        .args(["-q", "30", "-s", "10"])
        .arg(&png)
        .arg(&src)
        .output()
        .unwrap();
    assert!(made.status.success());
    let tools = Tools::discover().unwrap();
    let r = encode(&tools, &src, tmp.path(), fast(PhotoPreset::avif_high())).unwrap();
    assert!(r.fell_back);
    assert_eq!(r.ext, "avif");
    assert_ne!(r.path, src);
    assert!(r.path.starts_with(tmp.path()));
    assert_eq!(
        std::fs::read(&r.path).unwrap(),
        std::fs::read(&src).unwrap()
    );
    assert!(!r.warnings.is_empty());
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 1);
}

#[test]
fn photo_resized_jpeg_keeps_tags() {
    require_tool!("magick");
    require_tool!("exiftool");
    require_tool!("avifenc");
    let dir = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let src = support::fixture_jpeg(
        dir.path(),
        "a.jpg",
        Some("2026:04:12 10:33:14"),
        Some((48.85, 2.35)),
    );
    let tools = Tools::discover().unwrap();
    let mut p = fast(PhotoPreset::avif_high());
    p.max_short_side = Some(240);
    let r = encode(&tools, &src, tmp.path(), p).unwrap();
    assert!(!r.fell_back, "{:?}", r.warnings);
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    assert_eq!(dims(&r.path), (320, 240));
    let rec = exif::read(&tools, std::slice::from_ref(&r.path))
        .unwrap()
        .remove(&r.path)
        .unwrap();
    assert_eq!(rec["ExifIFD:DateTimeOriginal"], "2026:04:12 10:33:14");
    assert!(rec.get("GPS:GPSLatitude").is_some(), "{rec}");
}

#[test]
fn png_and_webp_convert_to_avif() {
    require_tool!("magick");
    require_tool!("avifenc");
    let tools = Tools::discover().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    for (ext, preset) in [
        ("png", PhotoPreset::png_avif()),
        ("webp", PhotoPreset::webp_avif()),
    ] {
        let src = dir.path().join(format!("shot.{ext}"));
        // A busy 640x480 image so AVIF is clearly smaller than the source.
        let ok = Command::new("magick")
            .args(["-size", "640x480", "plasma:", "-quality", "95"])
            .arg(&src)
            .status()
            .unwrap();
        assert!(ok.success());
        let r = encode(&tools, &src, tmp.path(), fast(preset)).unwrap();
        assert_eq!(r.ext, "avif", "{ext} converts to avif");
        assert!(!r.fell_back, "{ext}: {:?}", r.warnings);
        assert_eq!(dims(&r.path), (640, 480));
        assert!(size(&r.path) < size(&src));
    }
}
