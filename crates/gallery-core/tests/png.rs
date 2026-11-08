mod support;

use gallery_core::config::{ContentPreset, PngPreset};
use gallery_core::content::{StepCtx, StepResult, run_pipe};
use gallery_core::{exif, tools::Tools};
use std::path::Path;

fn optimize(tools: &Tools, src: &Path, tmp: &Path) -> gallery_core::Result<StepResult> {
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
    run_pipe(&ctx, &[&ContentPreset::Png(PngPreset::default())])
}

#[test]
fn png_optimized_not_larger() {
    require_tool!("optipng");
    require_tool!("magick");
    let tools = Tools::discover().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let src = support::fixture_png(dir.path(), "a.png");
    let before = std::fs::read(&src).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let r = optimize(&tools, &src, tmp.path()).unwrap();
    assert_eq!(r.ext, "png");
    assert!(!r.fell_back);
    assert_ne!(r.path, src);
    assert!(std::fs::metadata(&r.path).unwrap().len() <= before.len() as u64);
    assert_eq!(std::fs::read(&src).unwrap(), before);
}

#[test]
fn png_without_optipng_names_the_tool() {
    let mut tools = Tools::discover().unwrap();
    tools.optipng = None;
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("a.png");
    std::fs::write(&src, b"x").unwrap();
    let tmp = tempfile::tempdir().unwrap();
    // exif::read on a bogus file may fail; build the ctx with an empty record.
    let exif = serde_json::Value::Null;
    let ctx = StepCtx {
        tools: &tools,
        original: &src,
        original_exif: &exif,
        capture: None,
        tmp_dir: tmp.path(),
    };
    let err = run_pipe(&ctx, &[&ContentPreset::Png(PngPreset::default())]).unwrap_err();
    assert!(err.to_string().contains("optipng"), "{err}");
}
