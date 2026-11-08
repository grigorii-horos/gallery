mod support;

use gallery_core::config::{ContentPreset, VideoPreset};
use gallery_core::content::{StepCtx, StepResult, run_pipe};
use gallery_core::{exif, tools::Tools};
use serde_json::Value;
use std::path::Path;
use std::process::Command;

fn encode(
    tools: &Tools,
    src: &Path,
    tmp: &Path,
    preset: VideoPreset,
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
    run_pipe(&ctx, &[&ContentPreset::Video(preset)])
}

/// The high preset with the fastest SVT-AV1 speed, so tests stay quick.
fn fast(max_short_side: Option<u32>) -> VideoPreset {
    VideoPreset {
        preset: 12,
        max_short_side,
        ..VideoPreset::high()
    }
}

fn stream<'a>(probe: &'a Value, kind: &str) -> Option<&'a Value> {
    probe["streams"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["codec_type"] == kind)
}

fn duration(probe: &Value) -> f64 {
    probe["format"]["duration"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

fn dims(probe: &Value) -> (u64, u64) {
    let v = stream(probe, "video").unwrap();
    (v["width"].as_u64().unwrap(), v["height"].as_u64().unwrap())
}

#[test]
fn video_to_av1() {
    require_tool!("ffmpeg");
    require_tool!("ffprobe");
    require_tool!("exiftool");
    let dir = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let src = support::fixture_clip(dir.path(), "v.mp4", (640, 360), "libx264", true);
    let tools = Tools::discover().unwrap();
    let r = encode(&tools, &src, tmp.path(), fast(Some(1080))).unwrap();
    assert!(!r.fell_back, "{:?}", r.warnings);
    assert_eq!(r.ext, "av1.mp4");
    assert!(r.path.starts_with(tmp.path()));
    assert!(r.path.to_string_lossy().ends_with(".av1.mp4"));
    let p = support::ffprobe(&r.path);
    assert_eq!(stream(&p, "video").unwrap()["codec_name"], "av1");
    let audio = stream(&p, "audio").unwrap();
    assert_eq!(audio["codec_name"], "opus");
    assert_eq!(audio["sample_rate"], "48000");
    assert_eq!(dims(&p), (640, 360));
    let src_dur = duration(&support::ffprobe(&src));
    assert!((duration(&p) - src_dur).abs() <= 1.0);
    assert!(src.exists());
}

#[test]
fn video_already_av1_passthrough() {
    require_tool!("ffmpeg");
    require_tool!("ffprobe");
    require_tool!("exiftool");
    let dir = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let src = support::fixture_clip(dir.path(), "v.mp4", (640, 360), "libsvtav1", false);
    let tools = Tools::discover().unwrap();
    let r = encode(&tools, &src, tmp.path(), fast(Some(1080))).unwrap();
    assert!(r.fell_back);
    assert_eq!(r.ext, "mp4");
    assert_ne!(r.path, src);
    assert!(r.path.starts_with(tmp.path()));
    assert_eq!(
        std::fs::read(&r.path).unwrap(),
        std::fs::read(&src).unwrap()
    );
}

#[test]
fn video_downscale_short_side() {
    require_tool!("ffmpeg");
    require_tool!("ffprobe");
    require_tool!("exiftool");
    let dir = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let src = support::fixture_clip(dir.path(), "v.mp4", (1920, 1080), "libx264", false);
    let tools = Tools::discover().unwrap();
    let r = encode(&tools, &src, tmp.path(), fast(Some(720))).unwrap();
    assert!(!r.fell_back, "{:?}", r.warnings);
    assert_eq!(dims(&support::ffprobe(&r.path)), (1280, 720));
}

#[test]
fn video_rotation_swaps() {
    require_tool!("ffmpeg");
    require_tool!("ffprobe");
    require_tool!("exiftool");
    let dir = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let src = support::fixture_clip(dir.path(), "v.mp4", (640, 360), "libx264", false);
    support::exiftool_set(&src, &["-Rotation=90"]);
    let tools = Tools::discover().unwrap();
    // Displayed upright the clip is 360x640: the short side is the width.
    let r = encode(&tools, &src, tmp.path(), fast(Some(180))).unwrap();
    assert!(!r.fell_back, "{:?}", r.warnings);
    assert_eq!(dims(&support::ffprobe(&r.path)), (180, 320));
}

#[test]
fn video_no_audio_uses_an() {
    require_tool!("ffmpeg");
    require_tool!("ffprobe");
    require_tool!("exiftool");
    let dir = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let src = support::fixture_clip(dir.path(), "v.mp4", (640, 360), "libx264", false);
    let tools = Tools::discover().unwrap();
    let r = encode(&tools, &src, tmp.path(), fast(Some(1080))).unwrap();
    assert!(!r.fell_back, "{:?}", r.warnings);
    let p = support::ffprobe(&r.path);
    assert_eq!(stream(&p, "video").unwrap()["codec_name"], "av1");
    assert!(stream(&p, "audio").is_none());
}

#[test]
fn video_keeps_gps() {
    require_tool!("ffmpeg");
    require_tool!("ffprobe");
    require_tool!("exiftool");
    let dir = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let src = support::fixture_clip(dir.path(), "v.mp4", (640, 360), "libx264", true);
    support::exiftool_set(
        &src,
        &[
            "-UserData:GPSCoordinates=52.520008, 13.404954, 34",
            "-Keys:AndroidMake=Xiaomi",
            "-Keys:AndroidModel=23078PND5G",
        ],
    );
    let tools = Tools::discover().unwrap();
    let r = encode(&tools, &src, tmp.path(), fast(Some(1080))).unwrap();
    assert!(!r.fell_back, "{:?}", r.warnings);
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    let mut recs = exif::read(&tools, &[src.clone(), r.path.clone()]).unwrap();
    let out = recs.remove(&r.path).unwrap();
    let orig = recs.remove(&src).unwrap();
    assert!(orig["UserData:GPSCoordinates"].is_string());
    assert_eq!(
        out["UserData:GPSCoordinates"],
        orig["UserData:GPSCoordinates"]
    );
    assert_eq!(out["Keys:Make"], "Xiaomi");
    assert_eq!(out["Keys:Model"], "23078PND5G");
}

#[test]
fn video_not_smaller_falls_back() {
    require_tool!("ffmpeg");
    require_tool!("ffprobe");
    require_tool!("exiftool");
    let dir = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    // A black frame and 16 kbit/s audio: Opus at 160 kbit/s alone outweighs it.
    let src = dir.path().join("v.mp4");
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i", "color=black:size=640x360:rate=25"])
        .args(["-f", "lavfi", "-i", "sine=frequency=440"])
        .args([
            "-t",
            "2",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-crf",
            "51",
        ])
        .args(["-pix_fmt", "yuv420p", "-c:a", "aac", "-b:a", "16k"])
        .arg(&src)
        .output()
        .unwrap();
    assert!(out.status.success());
    let tools = Tools::discover().unwrap();
    let r = encode(&tools, &src, tmp.path(), fast(Some(1080))).unwrap();
    assert!(r.fell_back);
    assert_eq!(r.ext, "mp4");
    assert!(!r.warnings.is_empty());
    assert_ne!(r.path, src);
    assert_eq!(
        std::fs::read(&r.path).unwrap(),
        std::fs::read(&src).unwrap()
    );
    // Only the fallback copy is left behind.
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 1);
}

#[test]
fn video_stabilize_and_denoise() {
    require_tool!("ffmpeg");
    require_tool!("ffprobe");
    require_tool!("exiftool");
    let filters = Command::new("ffmpeg")
        .args(["-hide_banner", "-filters"])
        .output()
        .unwrap();
    if !String::from_utf8_lossy(&filters.stdout).contains("vidstabdetect") {
        eprintln!("skip: ffmpeg without vid.stab");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let src = support::fixture_clip(dir.path(), "v.mp4", (640, 360), "libx264", true);
    let tools = Tools::discover().unwrap();
    let preset = VideoPreset {
        preset: 12,
        ..VideoPreset::action_cam()
    };
    let r = encode(&tools, &src, tmp.path(), preset).unwrap();
    assert!(!r.fell_back, "{:?}", r.warnings);
    assert_eq!(dims(&support::ffprobe(&r.path)), (640, 360));
    // The transforms file is gone; only the result is left.
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 1);
}

#[test]
fn video_hdr_goes_10_bit() {
    require_tool!("ffmpeg");
    require_tool!("ffprobe");
    require_tool!("exiftool");
    let dir = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let src = dir.path().join("hlg.mp4");
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i", "testsrc2=size=640x360:rate=25"])
        .args(["-t", "2", "-c:v", "libx264", "-preset", "ultrafast"])
        // Output colour options do not reach the encoder; setparams does.
        .args([
            "-vf",
            "setparams=color_primaries=bt2020:color_trc=arib-std-b67:colorspace=bt2020nc",
        ])
        .args(["-pix_fmt", "yuv420p10le"])
        .arg(&src)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let tools = Tools::discover().unwrap();
    let r = encode(&tools, &src, tmp.path(), fast(Some(1080))).unwrap();
    assert!(!r.fell_back, "{:?}", r.warnings);
    let p = support::ffprobe(&r.path);
    let v = stream(&p, "video").unwrap();
    assert_eq!(v["pix_fmt"], "yuv420p10le");
    assert_eq!(v["color_transfer"], "arib-std-b67");
    assert_eq!(v["color_primaries"], "bt2020");
}

#[test]
fn video_keeps_keys_gps_without_warning() {
    require_tool!("ffmpeg");
    require_tool!("ffprobe");
    require_tool!("exiftool");
    let dir = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let src = support::fixture_clip(dir.path(), "v.mp4", (640, 360), "libx264", true);
    // iPhone style: the location only in Keys:GPSCoordinates.
    support::exiftool_set(&src, &["-Keys:GPSCoordinates=52.520008, 13.404954, 34"]);
    let tools = Tools::discover().unwrap();
    let r = encode(&tools, &src, tmp.path(), fast(Some(1080))).unwrap();
    assert!(!r.fell_back, "{:?}", r.warnings);
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    let mut recs = exif::read(&tools, &[src.clone(), r.path.clone()]).unwrap();
    let out = recs.remove(&r.path).unwrap();
    let orig = recs.remove(&src).unwrap();
    assert!(orig["Keys:GPSCoordinates"].is_string());
    assert!(orig.get("UserData:GPSCoordinates").is_none());
    assert_eq!(out["Keys:GPSCoordinates"], orig["Keys:GPSCoordinates"]);
}

#[test]
fn video_restores_user_data_gps_from_keys_location() {
    require_tool!("ffmpeg");
    require_tool!("ffprobe");
    require_tool!("exiftool");
    let dir = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let clip = support::fixture_clip(dir.path(), "c.mp4", (640, 360), "libx264", true);
    // exiftool cannot write Keys:Location; ffmpeg writes it from "location".
    let src = dir.path().join("v.mp4");
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(&clip)
        .args([
            "-c",
            "copy",
            "-metadata",
            "location=+52.520008+013.404954+34.000/",
        ])
        .args(["-movflags", "use_metadata_tags"])
        .arg(&src)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let tools = Tools::discover().unwrap();
    let r = encode(&tools, &src, tmp.path(), fast(Some(1080))).unwrap();
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    let out = exif::read(&tools, std::slice::from_ref(&r.path))
        .unwrap()
        .remove(&r.path)
        .unwrap();
    assert_eq!(out["UserData:GPSCoordinates"], "52.520008 13.404954 34");
}
