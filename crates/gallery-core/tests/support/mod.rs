#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

/// Skip the current test (print a notice and return) when a tool is not on PATH.
#[macro_export]
macro_rules! require_tool {
    ($name:expr) => {
        if !$crate::support::tool_available($name) {
            eprintln!("skip: {} not found", $name);
            return;
        }
    };
}

pub fn tool_available(name: &str) -> bool {
    gallery_core::tools::find(name).is_some()
}

fn run(cmd: &mut Command) {
    let out = cmd.output().expect("spawn fixture tool");
    assert!(
        out.status.success(),
        "{:?} failed: {}",
        cmd,
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Small 640x480 JPEG, optionally with DateTimeOriginal ("YYYY:MM:DD HH:MM:SS") and GPS.
pub fn fixture_jpeg(dir: &Path, name: &str, dto: Option<&str>, gps: Option<(f64, f64)>) -> PathBuf {
    let path = dir.join(name);
    run(Command::new("magick")
        .args(["-size", "640x480", "gradient:red-blue"])
        .arg(&path));
    let mut args: Vec<String> = vec!["-overwrite_original".into(), "-q".into()];
    if let Some(d) = dto {
        args.push(format!("-DateTimeOriginal={d}"));
    }
    if let Some((lat, lon)) = gps {
        args.push(format!("-GPSLatitude={}", lat.abs()));
        args.push(format!(
            "-GPSLatitudeRef={}",
            if lat >= 0.0 { "N" } else { "S" }
        ));
        args.push(format!("-GPSLongitude={}", lon.abs()));
        args.push(format!(
            "-GPSLongitudeRef={}",
            if lon >= 0.0 { "E" } else { "W" }
        ));
    }
    if args.len() > 2 {
        run(Command::new("exiftool").args(&args).arg(&path));
    }
    path
}

/// Short h264+aac clip, optionally with a QuickTime CreateDate (stored raw, i.e. UTC; "YYYY:MM:DD HH:MM:SS").
pub fn fixture_mp4(dir: &Path, name: &str, secs: u32, create_date_utc: Option<&str>) -> PathBuf {
    let path = dir.join(name);
    run(Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i", "testsrc2=size=160x120:rate=10"])
        .args(["-f", "lavfi", "-i", "sine=frequency=440"])
        .args(["-t", &secs.to_string()])
        .args([
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
        ])
        .args(["-c:a", "aac", "-shortest"])
        .arg(&path));
    if let Some(d) = create_date_utc {
        run(Command::new("exiftool")
            .args([
                "-overwrite_original",
                "-q",
                &format!("-QuickTime:CreateDate={d}"),
            ])
            .arg(&path));
    }
    path
}

/// Run `exiftool -overwrite_original -q <args> <path>` on a fixture.
pub fn exiftool_set(path: &Path, args: &[&str]) {
    run(Command::new("exiftool")
        .args(["-overwrite_original", "-q"])
        .args(args)
        .arg(path));
}

/// Short h264 clip in a Matroska container.
pub fn fixture_mkv(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    run(Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args([
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=160x120:rate=10",
            "-t",
            "1",
        ])
        .args([
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(&path));
    path
}

/// 2-second clip of `width`x`height` encoded with `vcodec` (`libx264` or
/// `libsvtav1`), with an AAC tone when `audio`.
pub fn fixture_clip(
    dir: &Path,
    name: &str,
    (width, height): (u32, u32),
    vcodec: &str,
    audio: bool,
) -> PathBuf {
    let path = dir.join(name);
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i"])
        .arg(format!("testsrc2=size={width}x{height}:rate=25"));
    if audio {
        cmd.args(["-f", "lavfi", "-i", "sine=frequency=440", "-c:a", "aac"]);
    }
    cmd.args(["-t", "2", "-c:v", vcodec, "-pix_fmt", "yuv420p"]);
    match vcodec {
        "libsvtav1" => cmd.args(["-preset", "12"]),
        _ => cmd.args(["-preset", "ultrafast"]),
    };
    run(cmd.arg(&path));
    path
}

/// ffprobe streams and format of `path` as JSON.
pub fn ffprobe(path: &Path) -> serde_json::Value {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_streams",
            "-show_format",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .expect("spawn ffprobe");
    assert!(out.status.success(), "ffprobe {}", path.display());
    serde_json::from_slice(&out.stdout).unwrap()
}

/// 2-second 440 Hz sine wav.
pub fn fixture_wav(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    run(Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=2"])
        .arg(&path));
    path
}

/// Unoptimised 200x200 noisy-gradient PNG.
pub fn fixture_png(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    run(Command::new("magick")
        .args(["-size", "200x200", "plasma:fractal"])
        .arg(&path));
    path
}
