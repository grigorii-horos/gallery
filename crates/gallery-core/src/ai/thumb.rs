//! Thumbnails for the classifier (port of the photo-sort `prep.sh`).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::media::MediaKind;
use crate::tools::Tools;
use crate::{Error, Result};

fn run(cmd: &mut Command, what: &str) -> Result<std::process::Output> {
    let out = cmd
        .stdin(Stdio::null())
        .output()
        .map_err(|e| Error::Tool(format!("{what}: {e}")))?;
    Ok(out)
}

fn run_ok(cmd: &mut Command, what: &str) -> Result<()> {
    let out = run(cmd, what)?;
    if out.status.success() {
        Ok(())
    } else {
        Err(Error::Tool(crate::t!(
            "thumbs.tool_failed",
            what = what,
            error = String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

/// Whole seconds of the video, `3` when ffprobe gives nothing numeric
/// (`N/A`, empty), never below 1.
fn duration_secs(tools: &Tools, src: &Path) -> u64 {
    let out = tools
        .command(&tools.ffprobe)
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "csv=p=0",
        ])
        .arg(src)
        .stdin(Stdio::null())
        .output();
    let text = out
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let whole = text.split('.').next().unwrap_or("");
    whole.parse::<u64>().unwrap_or(3).max(1)
}

fn frames_in(dir: &Path) -> Vec<PathBuf> {
    (1..=3)
        .map(|i| dir.join(format!("_frame_{i}.jpg")))
        .filter(|p| p.is_file())
        .collect()
}

/// Build a JPEG thumbnail of `src` in `out_dir` and return its path: 512px
/// for photos, a row of three 400px frames for videos.
pub fn make(tools: &Tools, src: &Path, kind: MediaKind, out_dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(out_dir)?;
    let stem = src.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
    if kind == MediaKind::Video {
        video(
            tools,
            src,
            &out_dir.join(format!("VIDEO_{stem}.jpg")),
            out_dir,
        )
    } else {
        let out = out_dir.join(format!("{stem}.jpg"));
        let mut input = src.as_os_str().to_owned();
        input.push("[0]");
        run_ok(
            tools
                .command(&tools.magick)
                .arg(input)
                .args(["-auto-orient", "-thumbnail", "512x512", "-quality", "80"])
                .arg(&out),
            "magick thumbnail",
        )?;
        Ok(out)
    }
}

fn video(tools: &Tools, src: &Path, out: &Path, work: &Path) -> Result<PathBuf> {
    let dur = duration_secs(tools, src);
    for p in frames_in(work) {
        let _ = std::fs::remove_file(p);
    }
    // Fast seek (`-ss` before `-i`) first; on some av1/vp9 files it yields
    // nothing, then retry with the slower exact seek (`-ss` after `-i`).
    for exact in [false, true] {
        for i in 1..=3u64 {
            let at = (dur * i / 4).to_string();
            let frame = work.join(format!("_frame_{i}.jpg"));
            let mut cmd = tools.command(&tools.ffmpeg);
            cmd.args(["-nostdin", "-v", "error"]);
            if exact {
                cmd.arg("-i").arg(src).args(["-ss", &at]);
            } else {
                cmd.args(["-ss", &at]).arg("-i").arg(src);
            }
            cmd.args([
                "-frames:v",
                "1",
                "-update",
                "1",
                "-vf",
                "scale=400:-1",
                "-y",
            ])
            .arg(&frame);
            // A failed frame is tolerated: only an empty result matters.
            let _ = run(&mut cmd, "ffmpeg frame");
        }
        if !frames_in(work).is_empty() {
            break;
        }
    }
    let frames = frames_in(work);
    if frames.is_empty() {
        return Err(Error::Tool(crate::t!(
            "thumbs.no_frames",
            path = src.display()
        )));
    }
    let res = run_ok(
        tools
            .command(&tools.magick)
            .arg("montage")
            .args(&frames)
            .args(["-tile", "3x1", "-geometry", "+2+2", "-background", "black"])
            .arg(out),
        "magick montage",
    );
    for p in frames {
        let _ = std::fs::remove_file(p);
    }
    res.map(|()| out.to_path_buf())
}
