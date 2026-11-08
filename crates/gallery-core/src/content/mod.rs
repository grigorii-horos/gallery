//! Content pipe: a chain of content presets turning the original into the
//! file that is finally placed. Every step writes a new file in `tmp_dir`;
//! the original is only ever read.

mod audio;
pub(crate) mod metadata;
mod photo;
mod png;
mod video;

use crate::config::ContentPreset;
use crate::media::split_name;
use crate::tools::Tools;
use crate::{Error, Result};
use chrono::NaiveDateTime;
use serde_json::Value;
use std::fs::File;
use std::path::{Path, PathBuf};

/// What a content step knows about the file it works on.
pub struct StepCtx<'a> {
    pub tools: &'a Tools,
    /// The source file; never modified.
    pub original: &'a Path,
    /// exiftool record of the original (`exif::read` format).
    pub original_exif: &'a Value,
    /// Capture date (local wall-clock time), if known.
    pub capture: Option<NaiveDateTime>,
    /// Directory for step outputs.
    pub tmp_dir: &'a Path,
}

/// Output of a step (or of the whole pipe).
#[derive(Debug, Clone)]
pub struct StepResult {
    pub path: PathBuf,
    /// Extension of `path` without the dot (`av1.mp4` is one extension).
    pub ext: String,
    /// Human-readable warnings for the report.
    pub warnings: Vec<String>,
    /// The step could not do its main job and kept a fallback result.
    pub fell_back: bool,
}

/// One content transformation: reads `input`, writes a new file in `ctx.tmp_dir`.
pub trait ContentStep {
    fn run(&self, ctx: &StepCtx, input: &Path) -> Result<StepResult>;
}

impl ContentStep for ContentPreset {
    fn run(&self, ctx: &StepCtx, input: &Path) -> Result<StepResult> {
        match self {
            ContentPreset::Metadata(p) => p.run(ctx, input),
            ContentPreset::Photo(p) => p.run(ctx, input),
            ContentPreset::Video(p) => p.run(ctx, input),
            ContentPreset::Audio(p) => p.run(ctx, input),
            ContentPreset::Png(p) => p.run(ctx, input),
            ContentPreset::Bundle(_) => Err(Error::Plan(
                "a bundle is expanded at plan time, it cannot run as a step".into(),
            )),
        }
    }
}

/// Run `steps` in order. The first step reads the original, each next one the
/// previous result; intermediate files are removed once the next step is done.
/// With no steps the original is copied into `tmp_dir` under its own name.
pub fn run_pipe(ctx: &StepCtx, steps: &[&ContentPreset]) -> Result<StepResult> {
    if steps.is_empty() {
        let name = ctx
            .original
            .file_name()
            .ok_or_else(|| Error::Plan(format!("no file name: {}", ctx.original.display())))?;
        let path = ctx.tmp_dir.join(name);
        if path.exists() {
            // Never copy over an existing file (it could be the original itself).
            return Err(Error::Plan(format!("already exists: {}", path.display())));
        }
        copy_keep_mtime(ctx.original, &path)?;
        let ext = split_name(&name.to_string_lossy()).1;
        return Ok(StepResult {
            path,
            ext,
            warnings: Vec::new(),
            fell_back: false,
        });
    }
    let mut current: Option<StepResult> = None;
    for step in steps {
        let input = current.as_ref().map_or(ctx.original, |r| r.path.as_path());
        let out = step.run(ctx, input);
        if let Some(prev) = &current {
            // Intermediate: not needed any more, whatever the step's outcome.
            let _ = std::fs::remove_file(&prev.path);
        }
        let mut out = out?;
        if let Some(prev) = current.take() {
            let mut warnings = prev.warnings;
            warnings.append(&mut out.warnings);
            out.warnings = warnings;
            out.fell_back |= prev.fell_back;
        }
        current = Some(out);
    }
    Ok(current.expect("steps is not empty"))
}

/// Fresh, uniquely named file in `dir` ending in `.ext`, named after `like`.
pub(crate) fn tmp_output(dir: &Path, like: &Path, ext: &str) -> Result<PathBuf> {
    let name = like
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    let stem = split_name(&name).0;
    let (_, path) = tempfile::Builder::new()
        .prefix(&format!("{stem}."))
        .suffix(&format!(".{ext}"))
        .tempfile_in(dir)?
        .keep()
        .map_err(|e| Error::Io(e.error))?;
    Ok(path)
}

/// Copy `from` to `to`, carrying over the modification time.
pub(crate) fn copy_keep_mtime(from: &Path, to: &Path) -> Result<()> {
    std::fs::copy(from, to)?;
    let modified = std::fs::metadata(from)?.modified()?;
    File::options()
        .write(true)
        .open(to)?
        .set_modified(modified)?;
    Ok(())
}
