//! PNG preset: lossless `optipng -oN -preserve` on a copy of the input.

use super::video::run;
use super::{ContentStep, StepCtx, StepResult, copy_keep_mtime, tmp_output};
use crate::config::PngPreset;
use crate::{Error, Result};
use std::path::Path;

impl ContentStep for PngPreset {
    fn run(&self, ctx: &StepCtx, input: &Path) -> Result<StepResult> {
        let optipng = ctx
            .tools
            .optipng
            .as_ref()
            .ok_or_else(|| Error::Tool(crate::t!("content.optipng_missing")))?;
        let out = tmp_output(ctx.tmp_dir, input, self.output_ext())?;
        // The result may be no smaller; that is fine, the copy is still valid.
        let res = copy_keep_mtime(input, &out).and_then(|()| {
            let mut cmd = ctx.tools.command(optipng);
            cmd.arg(format!("-o{}", self.level))
                .arg("-preserve")
                .arg("-quiet")
                .arg(&out);
            run(cmd, "optipng")
        });
        match res {
            Ok(()) => Ok(StepResult {
                path: out,
                ext: self.output_ext().to_string(),
                warnings: Vec::new(),
                fell_back: false,
            }),
            Err(e) => {
                let _ = std::fs::remove_file(&out);
                Err(Error::Tool(crate::t!(
                    "content.png_failed",
                    path = ctx.original.display(),
                    error = e
                )))
            }
        }
    }
}
