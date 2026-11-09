mod support;

use gallery_core::tools::Tools;

#[test]
fn discover_finds_tools() {
    for t in ["ffmpeg", "ffprobe", "exiftool", "avifenc", "magick"] {
        require_tool!(t);
    }
    let tools = Tools::discover().unwrap();
    assert!(tools.ffmpeg.is_absolute() || tools.ffmpeg.exists());
    assert!(tools.exiftool.exists());
}

#[test]
fn capabilities_detects_svtav1() {
    require_tool!("ffmpeg");
    require_tool!("ffprobe");
    require_tool!("exiftool");
    require_tool!("avifenc");
    require_tool!("magick");
    let caps = Tools::discover().unwrap().capabilities();
    assert!(caps.svtav1);
    assert!(caps.opus);
    // Homebrew's ffmpeg is built without libvidstab.
    assert!(caps.vidstab || cfg!(target_os = "macos"));
    assert!(caps.versions.contains_key("ffmpeg"));
}

#[test]
fn command_wraps_with_nice() {
    require_tool!("nice");
    require_tool!("ionice");
    let tools = Tools::discover();
    let Ok(tools) = tools else { return };
    let cmd = tools.command(&tools.exiftool);
    let dbg = format!("{cmd:?}");
    assert!(dbg.contains("nice"), "{dbg}");
}
