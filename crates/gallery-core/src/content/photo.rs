//! Photo preset: a port of the encode path of the earlier `prepare-photos` shell script
//! (AVIF via avifenc, WebP via magick, quality ladder, optional downscale).

use super::{ContentStep, StepCtx, StepResult, copy_keep_mtime, tmp_output};
use crate::config::{PhotoFormat, PhotoPreset};
use crate::media::split_name;
use crate::{Error, Result, exif};
use std::path::Path;
use std::process::Command;

/// Quality levels of each format, best first (the starter presets).
const AVIF_LEVELS: [u8; 4] = [82, 72, 65, 55];
const WEBP_LEVELS: [u8; 4] = [93, 87, 80, 70];

impl ContentStep for PhotoPreset {
    fn run(&self, ctx: &StepCtx, input: &Path) -> Result<StepResult> {
        let name = input
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let in_ext = split_name(&name).1;
        let out = tmp_output(ctx.tmp_dir, input, self.output_ext())?;
        let mut png = None;
        let mut warnings = Vec::new();
        let res = self.encode_ladder(ctx, input, &in_ext, &out, &mut png, &mut warnings);
        if let Some(p) = png {
            let _ = std::fs::remove_file(p);
        }
        match res {
            Ok(true) => Ok(StepResult {
                path: out,
                ext: self.output_ext().to_string(),
                warnings,
                fell_back: false,
            }),
            Ok(false) => {
                // Nothing got smaller: hand over a copy of the input.
                let copy = tmp_output(ctx.tmp_dir, input, &in_ext);
                let _ = std::fs::remove_file(&out);
                let copy = copy?;
                if let Err(e) = copy_keep_mtime(input, &copy) {
                    let _ = std::fs::remove_file(&copy);
                    return Err(e);
                }
                Ok(StepResult {
                    path: copy,
                    ext: in_ext,
                    warnings: vec!["not smaller than the source".into()],
                    fell_back: true,
                })
            }
            Err(e) => {
                let _ = std::fs::remove_file(&out);
                Err(Error::Tool(format!(
                    "photo {}: {e}",
                    ctx.original.display()
                )))
            }
        }
    }
}

impl PhotoPreset {
    /// Qualities to try: the preset's own, then each lower format level.
    fn qualities(&self) -> Vec<u8> {
        let mut qs = vec![self.quality];
        if self.ladder {
            let levels = match self.format {
                PhotoFormat::Avif => AVIF_LEVELS,
                PhotoFormat::Webp => WEBP_LEVELS,
            };
            qs.extend(levels.into_iter().filter(|&l| l < self.quality));
        }
        qs
    }

    /// Encode into `out`, stepping down the ladder until the result is
    /// smaller than the input. Ok(false): even the last level did not help.
    fn encode_ladder(
        &self,
        ctx: &StepCtx,
        input: &Path,
        in_ext: &str,
        out: &Path,
        png: &mut Option<std::path::PathBuf>,
        warnings: &mut Vec<String>,
    ) -> Result<bool> {
        let in_size = std::fs::metadata(input)?.len();
        for q in self.qualities() {
            let needs_tags = self.encode_once(ctx, input, in_ext, out, q, png)?;
            let out_size = std::fs::metadata(out)?.len();
            if out_size == 0 {
                return Err(Error::Tool("encoder produced an empty file".into()));
            }
            let saved = in_size.saturating_sub(out_size);
            if out_size < in_size && saved * 100 >= in_size * u64::from(self.min_saving_pct) {
                self.verify(ctx, out)?;
                if needs_tags {
                    copy_tags(ctx, input, out, warnings);
                }
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn encode_once(
        &self,
        ctx: &StepCtx,
        input: &Path,
        in_ext: &str,
        out: &Path,
        q: u8,
        png: &mut Option<std::path::PathBuf>,
    ) -> Result<bool> {
        let tools = ctx.tools;
        let first_frame = format!("{}[0]", input.display());
        let resize = self
            .max_short_side
            .map(|n| format!("{n}x{n}^>"))
            .map(|g| vec!["-resize".to_string(), g])
            .unwrap_or_default();
        match self.format {
            PhotoFormat::Webp => {
                let mut cmd = tools.command(&tools.magick);
                cmd.arg("-quality").arg(q.to_string());
                cmd.arg("-define").arg(format!(
                    "webp:image-hint=photo,method={},thread-level=8",
                    self.speed
                ));
                cmd.arg(&first_frame)
                    .arg("-auto-orient")
                    .args(&resize)
                    .arg(out);
                run(cmd, "magick")?;
                Ok(true)
            }
            PhotoFormat::Avif => {
                let jpeg = matches!(in_ext.to_ascii_lowercase().as_str(), "jpg" | "jpeg");
                let direct = jpeg
                    && match self.max_short_side {
                        None => true,
                        Some(n) => short_side(ctx, &first_frame)? <= n,
                    };
                let mut cmd = tools.command(&tools.avifenc);
                cmd.args(["-q", &q.to_string(), "-s", &self.speed.to_string()]);
                if direct {
                    // avifenc reads JPEG itself, carrying EXIF and orientation.
                    cmd.args(["-j", "1"]).arg(input).arg(out);
                    run(cmd, "avifenc")?;
                    return Ok(false);
                }
                // RAW/HEIC (and JPEG to shrink): decode through magick once.
                if png.is_none() {
                    let p = tmp_output(ctx.tmp_dir, input, "png")?;
                    *png = Some(p.clone());
                    let mut dec = tools.command(&tools.magick);
                    dec.arg(&first_frame)
                        .arg("-auto-orient")
                        .args(&resize)
                        .arg(&p);
                    run(dec, "magick")?;
                }
                let png_path = png.as_ref().expect("set above");
                if !jpeg {
                    cmd.args(["-d", "10"]);
                }
                cmd.args(["-j", "1"]).arg(png_path).arg(out);
                run(cmd, "avifenc")?;
                Ok(true)
            }
        }
    }

    fn verify(&self, ctx: &StepCtx, out: &Path) -> Result<()> {
        let mut cmd = ctx.tools.command(&ctx.tools.magick);
        cmd.arg("identify").arg(out);
        run(cmd, "magick identify").map_err(|e| Error::Tool(format!("result unreadable: {e}")))
    }
}

fn short_side(ctx: &StepCtx, frame: &str) -> Result<u32> {
    let mut cmd = ctx.tools.command(&ctx.tools.magick);
    cmd.args(["identify", "-ping", "-format", "%w %h"])
        .arg(frame);
    cmd.env("MAGICK_THREAD_LIMIT", "1");
    let out = cmd.output()?;
    if !out.status.success() {
        return Err(Error::Tool(format!(
            "magick identify: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let dims: Vec<u32> = s
        .split_whitespace()
        .filter_map(|v| v.parse().ok())
        .collect();
    match dims[..] {
        [w, h, ..] => Ok(w.min(h)),
        _ => Err(Error::Tool(format!("magick identify: bad output {s:?}"))),
    }
}

/// Run a tool with a single ImageMagick thread; fail with its stderr.
fn run(mut cmd: Command, what: &str) -> Result<()> {
    cmd.env("MAGICK_THREAD_LIMIT", "1");
    let out = cmd.output()?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let msg = stderr
        .lines()
        .chain(stdout.lines())
        .find(|l| !l.trim().is_empty());
    Err(Error::Tool(format!(
        "{what}: {}",
        msg.unwrap_or("failed").trim()
    )))
}

/// Copy tags from the step input onto a re-encoded `out`. Pixels are already
/// rotated and scaled, so the orientation and size tags are left out.
/// Problems become warnings: a lost capture time must not go unnoticed.
fn copy_tags(ctx: &StepCtx, input: &Path, out: &Path, warnings: &mut Vec<String>) {
    let args = [
        "-TagsFromFile".to_string(),
        input.display().to_string(),
        "-all:all".into(),
        "--Orientation".into(),
        "--ExifImageWidth".into(),
        "--ExifImageHeight".into(),
    ];
    if let Err(e) = exif::write(ctx.tools, out, &args) {
        warnings.push(format!("metadata not copied: {e}"));
        return;
    }
    let dto = |p: &Path| {
        exif::read(ctx.tools, &[p.to_path_buf()])
            .ok()
            .and_then(|mut m| m.remove(p))
            .map(|v| {
                v.as_object()
                    .map(|o| o.keys().any(|k| k.ends_with(":DateTimeOriginal")))
                    .unwrap_or(false)
            })
    };
    if dto(input) == Some(true) && dto(out) != Some(true) {
        warnings.push("metadata not copied: DateTimeOriginal missing in the result".into());
    }
}
