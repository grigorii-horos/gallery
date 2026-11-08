mod support;

use gallery_core::config::{AudioPreset, ContentPreset};
use gallery_core::content::{StepCtx, StepResult, run_pipe};
use gallery_core::{exif, tools::Tools};
use std::path::Path;

fn encode(src: &Path, tmp: &Path, preset: AudioPreset) -> gallery_core::Result<StepResult> {
    let tools = Tools::discover().unwrap();
    let exif = exif::read(&tools, &[src.to_path_buf()])
        .unwrap()
        .remove(src)
        .unwrap();
    let ctx = StepCtx {
        tools: &tools,
        original: src,
        original_exif: &exif,
        capture: None,
        tmp_dir: tmp,
    };
    run_pipe(&ctx, &[&ContentPreset::Audio(preset)])
}

#[test]
fn audio_to_opus() {
    require_tool!("ffmpeg");
    let dir = tempfile::tempdir().unwrap();
    let src = support::fixture_wav(dir.path(), "a.wav");
    let before = std::fs::read(&src).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let r = encode(&src, tmp.path(), AudioPreset::opus(64)).unwrap();
    assert_eq!(r.ext, "opus");
    assert!(!r.fell_back);
    assert_ne!(r.path, src);
    assert!(r.path.starts_with(tmp.path()));
    let probe = support::ffprobe(&r.path);
    assert_eq!(probe["streams"][0]["codec_name"], "opus");
    let d: f64 = probe["format"]["duration"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((d - 2.0).abs() <= 1.0, "duration {d}");
    assert_eq!(std::fs::read(&src).unwrap(), before);
}

#[test]
fn audio_other_codecs() {
    require_tool!("ffmpeg");
    let dir = tempfile::tempdir().unwrap();
    let src = support::fixture_wav(dir.path(), "a.wav");
    let tmp = tempfile::tempdir().unwrap();
    for (preset, ext, codec) in [
        (AudioPreset::mp3(128), "mp3", "mp3"),
        (AudioPreset::aac(96), "m4a", "aac"),
        (AudioPreset::flac(), "flac", "flac"),
    ] {
        let r = encode(&src, tmp.path(), preset).unwrap();
        assert_eq!(r.ext, ext);
        let probe = support::ffprobe(&r.path);
        assert_eq!(probe["streams"][0]["codec_name"], codec);
    }
}

#[test]
fn audio_failure_leaves_no_temp() {
    require_tool!("ffmpeg");
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("bad.wav");
    std::fs::write(&src, b"not audio").unwrap();
    let tmp = tempfile::tempdir().unwrap();
    assert!(encode(&src, tmp.path(), AudioPreset::default()).is_err());
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
}
