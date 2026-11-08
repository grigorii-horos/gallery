//! Audio preset: a port of the earlier `prepare-audio` shell script (opus/mp3/aac/flac, no
//! cover art, tags carried over).

use super::video::{duration_of, ffprobe_json, run};
use super::{ContentStep, StepCtx, StepResult, tmp_output};
use crate::config::{AudioCodec, AudioPreset};
use crate::{Error, Result};
use std::path::Path;

impl ContentStep for AudioPreset {
    fn run(&self, ctx: &StepCtx, input: &Path) -> Result<StepResult> {
        let out = tmp_output(ctx.tmp_dir, input, self.output_ext())?;
        match self.encode(ctx, input, &out) {
            Ok(()) => Ok(StepResult {
                path: out,
                ext: self.output_ext().to_string(),
                warnings: Vec::new(),
                fell_back: false,
            }),
            Err(e) => {
                let _ = std::fs::remove_file(&out);
                Err(Error::Tool(crate::t!(
                    "content.audio_failed",
                    path = ctx.original.display(),
                    error = e
                )))
            }
        }
    }
}

impl AudioPreset {
    fn encode(&self, ctx: &StepCtx, input: &Path, out: &Path) -> Result<()> {
        let bitrate = format!("{}k", self.kbps.unwrap_or(128));
        let mut cmd = ctx.tools.command(&ctx.tools.ffmpeg);
        cmd.args(["-y", "-v", "error", "-nostdin", "-i"]).arg(input);
        match self.codec {
            AudioCodec::Opus => cmd.args(["-c:a", "libopus", "-b:a", &bitrate]),
            AudioCodec::Mp3 => cmd.args(["-c:a", "libmp3lame", "-b:a", &bitrate]),
            AudioCodec::Aac => cmd.args(["-c:a", "aac", "-b:a", &bitrate]),
            AudioCodec::Flac => cmd.args(["-c:a", "flac", "-compression_level", "8"]),
        };
        cmd.args(["-vn", "-map_metadata", "0"]).arg(out);
        run(cmd, "ffmpeg")?;
        let (a, b) = (
            duration_of(&ffprobe_json(ctx, input)?),
            duration_of(&ffprobe_json(ctx, out)?),
        );
        match (a, b) {
            (Some(a), Some(b)) if (a - b).abs() <= 1.0 => Ok(()),
            (Some(a), Some(b)) => Err(Error::Tool(crate::t!(
                "content.duration_mismatch",
                from = crate::i18n::decimal(a, 1),
                to = crate::i18n::decimal(b, 1)
            ))),
            _ => Err(Error::Tool(crate::t!("content.no_duration"))),
        }
    }
}
