//! exiftool wrapper: batched JSON reads and single-file writes.

use crate::progress::{Cancel, NoProgress, Progress, ProgressEvent, Stage};
use crate::tools::Tools;
use crate::{Error, Result};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

const BATCH: usize = 500;

/// Read metadata for `paths`. Keys are `Group:Tag` (`-G1`), values numeric (`-n`).
pub fn read(tools: &Tools, paths: &[PathBuf]) -> Result<HashMap<PathBuf, serde_json::Value>> {
    read_with(tools, paths, &NoProgress, &Cancel::new())
}

/// [`read`] with a [`Stage::Exif`] event per batch of files read; a cancel
/// is checked before each batch and ends the read with `Error::Cancelled`.
pub fn read_with(
    tools: &Tools,
    paths: &[PathBuf],
    progress: &dyn Progress,
    cancel: &Cancel,
) -> Result<HashMap<PathBuf, serde_json::Value>> {
    let mut result = HashMap::new();
    let mut done = 0;
    for chunk in paths.chunks(BATCH) {
        cancel.check()?;
        let mut argfile = tempfile::NamedTempFile::new()?;
        for p in chunk {
            writeln!(argfile, "{}", p.display())?;
        }
        argfile.flush()?;
        let out = tools
            .command(&tools.exiftool)
            .args([
                "-j",
                "-n",
                "-G1",
                "-api",
                "QuickTimeUTC",
                "-api",
                "LargeFileSupport=1",
            ])
            .args(["-charset", "filename=utf8", "-@"])
            .arg(argfile.path())
            .output()?;
        // exiftool exits 1 when some file is unreadable but still prints the rest.
        let parsed: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).map_err(|e| {
            Error::Tool(crate::t!(
                "exif.output",
                error = e,
                stderr = String::from_utf8_lossy(&out.stderr)
            ))
        })?;
        for v in parsed {
            if let Some(src) = v.get("SourceFile").and_then(|s| s.as_str()) {
                result.insert(PathBuf::from(src), v.clone());
            }
        }
        done += chunk.len();
        progress.step(ProgressEvent {
            stage: Stage::Exif,
            done,
            total: paths.len(),
            current: chunk.last().cloned(),
        });
    }
    Ok(result)
}

/// Write tags to `path` in place (no `_original` backup, minor errors ignored).
pub fn write(tools: &Tools, path: &Path, args: &[String]) -> Result<()> {
    let out = tools
        .command(&tools.exiftool)
        .args(["-overwrite_original", "-m", "-charset", "filename=utf8"])
        .args(args)
        .arg(path)
        .output()?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    // exiftool reports an unwritable file on stderr/stdout and may still exit 0.
    // "0 image files updated, 1 image files unchanged" means nothing to change.
    if !out.status.success()
        || (stdout.contains("0 image files updated") && !stdout.contains("image files unchanged"))
    {
        let stderr = String::from_utf8_lossy(&out.stderr);
        // The format has no writer: not a broken file, just nothing to do.
        if stderr.contains("is not yet supported") || stderr.contains("Can't currently write") {
            return Err(Error::Unwritable(format!(
                "{}: {}",
                path.display(),
                stderr.trim()
            )));
        }
        return Err(Error::Tool(crate::t!(
            "exif.write",
            path = path.display(),
            stderr = String::from_utf8_lossy(&out.stderr).trim(),
            stdout = stdout.trim()
        )));
    }
    Ok(())
}
