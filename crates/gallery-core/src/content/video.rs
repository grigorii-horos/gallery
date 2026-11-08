//! Video preset: a port of the encode path of the earlier `prepare-videos` shell script
//! (SVT-AV1 + Opus, short-side downscale, colour tags, HDR in 10 bit,
//! optional denoise/stabilization, quality ladder, GPS/camera restore).

use super::{ContentStep, StepCtx, StepResult, copy_keep_mtime, tmp_output};
use crate::config::{Denoise, VideoPreset};
use crate::media::split_name;
use crate::{Error, Result, exif};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Quality levels (crf, SVT-AV1 preset), best first: max, high, medium, low.
/// p6 is no good for low: the encoder compresses worse there, and crf 35/p6
/// came out the same size as medium.
const LEVELS: [(u8, u8); 4] = [(27, 2), (30, 3), (32, 4), (36, 4)];

/// Tags the gallery apps read for the location; ffmpeg keeps only a rounded copy.
const GPS_TAGS: [&str; 3] = [
    "UserData:GPSCoordinates",
    "Keys:GPSCoordinates",
    "Keys:Location",
];

impl ContentStep for VideoPreset {
    fn run(&self, ctx: &StepCtx, input: &Path) -> Result<StepResult> {
        let name = input
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let in_ext = split_name(&name).1;
        let probe = probe(ctx, input).map_err(|e| {
            Error::Tool(crate::t!(
                "content.video_failed",
                path = ctx.original.display(),
                error = e
            ))
        })?;
        let Some(video) = &probe.video else {
            return fall_back(ctx, input, in_ext, &crate::t!("content.no_video_stream"));
        };
        // Re-encoding AV1 into AV1 only loses quality (prepare-media renames
        // by date and drops ".av1", so the name alone does not tell).
        if video.codec == "av1" {
            return fall_back(ctx, input, in_ext, &crate::t!("content.already_av1"));
        }
        let out = tmp_output(ctx.tmp_dir, input, self.output_ext())?;
        let mut trf = None;
        let res = self.encode_ladder(ctx, input, &out, &probe, video, &mut trf);
        if let Some(t) = trf {
            let _ = std::fs::remove_file(t);
        }
        match res {
            Ok(true) => {
                let mut warnings = Vec::new();
                restore_tags(ctx, input, &out, &mut warnings);
                Ok(StepResult {
                    path: out,
                    ext: self.output_ext().to_string(),
                    warnings,
                    fell_back: false,
                })
            }
            Ok(false) => {
                let _ = std::fs::remove_file(&out);
                fall_back(ctx, input, in_ext, &crate::t!("content.not_smaller_low"))
            }
            Err(e) => {
                let _ = std::fs::remove_file(&out);
                Err(Error::Tool(crate::t!(
                    "content.video_failed",
                    path = ctx.original.display(),
                    error = e
                )))
            }
        }
    }
}

impl VideoPreset {
    /// (crf, preset) pairs to try: the preset's own, then each lower level.
    /// A ladder step is never slower than the chosen preset: the script keeps
    /// an explicit or fast (-F) preset on every step.
    fn attempts(&self) -> Vec<(u8, u8)> {
        let mut steps = vec![(self.crf, self.preset)];
        if self.ladder {
            steps.extend(
                LEVELS
                    .into_iter()
                    .filter(|&(crf, _)| crf > self.crf)
                    .map(|(crf, preset)| (crf, preset.max(self.preset))),
            );
        }
        steps
    }

    /// Encode into `out`, stepping down the ladder until the result is
    /// smaller than the input. Ok(false): even the last level did not help.
    fn encode_ladder(
        &self,
        ctx: &StepCtx,
        input: &Path,
        out: &Path,
        probe: &Probe,
        video: &VideoStream,
        trf: &mut Option<PathBuf>,
    ) -> Result<bool> {
        // ffmpeg runs from tmp_dir (see below), so every path must be absolute.
        let input = std::path::absolute(input)?;
        let out = std::path::absolute(out)?;
        let tmp_dir = std::path::absolute(ctx.tmp_dir)?;
        let mut filters = Vec::new();
        if let Some(d) = self.denoise {
            filters.push(format!("hqdn3d={}", denoise_values(d)));
        }
        if self.stabilize {
            let path = tmp_output(&tmp_dir, Path::new("vidstab"), "trf")?;
            *trf = Some(path.clone());
            // The transforms file is named by bare file name relative to
            // tmp_dir: a full path would need filtergraph escaping.
            let file = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let mut cmd = ctx.tools.command(&ctx.tools.ffmpeg);
            cmd.current_dir(&tmp_dir)
                .args(["-y", "-v", "error", "-nostdin", "-i"])
                .arg(&input)
                .arg("-vf")
                .arg(format!(
                    "vidstabdetect=shakiness=5:accuracy=15:result={file}"
                ))
                .args(["-f", "null", "-"]);
            run(cmd, "ffmpeg vidstabdetect")?;
            filters.push(format!(
                "vidstabtransform=input={file}:smoothing=10:zoom=2:optzoom=1:crop=keep"
            ));
        }
        let colors = Colors::from(video);
        filters.push(self.scale_filter(video));
        // Output options -color_primaries/-color_trc do not reach the AV1
        // encoder (checked: they gave bt709,smpte170m,bt470bg instead of bt709
        // everywhere). Only the setparams filter really sets the tags.
        filters.push(format!(
            "setparams=color_primaries={}:color_trc={}:colorspace={}:range=tv",
            colors.primaries, colors.transfer, colors.space
        ));
        let vf = filters.join(",");
        // HDR (HLG/PQ) in 8 bit bands on gradients: those clips go in 10 bit.
        let pix_out = if matches!(colors.transfer, "arib-std-b67" | "smpte2084") {
            "yuv420p10le"
        } else {
            "yuv420p"
        };
        let in_size = std::fs::metadata(&input)?.len();
        for (crf, preset) in self.attempts() {
            let mut cmd = ctx.tools.command(&ctx.tools.ffmpeg);
            cmd.current_dir(&tmp_dir)
                .args(["-y", "-v", "error", "-nostdin", "-i"])
                .arg(&input)
                .arg("-vf")
                .arg(&vf)
                .args(["-c:v", "libsvtav1", "-crf", &crf.to_string()])
                .args(["-preset", &preset.to_string()])
                .args(["-g", &self.gop.to_string()])
                .args(["-svtav1-params", "tune=0", "-pix_fmt", pix_out])
                .args(["-colorspace", colors.space])
                .args(["-color_primaries", colors.primaries])
                .args(["-color_trc", colors.transfer, "-color_range", "tv"]);
            if probe.has_audio {
                cmd.args(["-c:a", "libopus", "-b:a"])
                    .arg(format!("{}k", self.audio_kbps))
                    .args(["-ar", "48000"]);
            } else {
                cmd.arg("-an");
            }
            cmd.args(["-map_metadata", "0"])
                .args(["-movflags", "use_metadata_tags+faststart"])
                .args(["-max_muxing_queue_size", "1024"])
                .arg(&out);
            run(cmd, "ffmpeg")?;
            let out_size = std::fs::metadata(&out)?.len();
            if out_size == 0 {
                return Err(Error::Tool(crate::t!("content.empty_encoder")));
            }
            if out_size < in_size {
                check_duration(ctx, probe.duration, &out)?;
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Lanczos scale to tv range, downscaled by the SHORT side so the frame
    /// orientation does not matter (a vertical 1080x1920 stays untouched).
    fn scale_filter(&self, v: &VideoStream) -> String {
        let (mut w, mut h) = (v.width, v.height);
        // ffmpeg autorotates, so the encoder gets the frame already turned.
        if v.rotation.rem_euclid(180) == 90 {
            (w, h) = (h, w);
        }
        if let Some(max) = self.max_short_side.filter(|&m| m > 0) {
            let short = w.min(h);
            if short > max {
                w = w * max / short;
                h = h * max / short;
            }
        }
        // AV1 4:2:0 needs even dimensions.
        w += w % 2;
        h += h % 2;
        let mut f = format!("scale=w={w}:h={h}:flags=lanczos:out_range=tv");
        // Full-range (yuvj*) sources must be mapped to tv range explicitly,
        // otherwise swscale loses the range and the colours shift.
        if v.pix_fmt.starts_with("yuvj") || v.range == "pc" {
            f.push_str(":in_range=full");
        }
        f
    }
}

fn denoise_values(d: Denoise) -> &'static str {
    match d {
        Denoise::VeryLow => "0.8:0.8:4:4",
        Denoise::Low => "1.0:1.0:6:6",
        Denoise::Medium => "1.5:1.5:6:6",
        Denoise::High => "2.0:2.0:8:8",
    }
}

struct VideoStream {
    codec: String,
    width: u32,
    height: u32,
    rotation: i64,
    pix_fmt: String,
    range: String,
    primaries: String,
    transfer: String,
    space: String,
}

struct Probe {
    video: Option<VideoStream>,
    has_audio: bool,
    duration: Option<f64>,
}

/// Colour tags for the output: unknown and junk values become bt709.
struct Colors {
    primaries: &'static str,
    transfer: &'static str,
    space: &'static str,
}

impl From<&VideoStream> for Colors {
    fn from(v: &VideoStream) -> Self {
        let pick = |val: &str, allowed: &[&'static str]| {
            allowed
                .iter()
                .copied()
                .find(|a| *a == val)
                .unwrap_or("bt709")
        };
        Colors {
            primaries: pick(&v.primaries, &["bt709", "bt470bg", "smpte170m", "bt2020"]),
            transfer: pick(
                &v.transfer,
                &[
                    "bt709",
                    "smpte170m",
                    "bt2020-10",
                    "arib-std-b67",
                    "smpte2084",
                ],
            ),
            space: pick(&v.space, &["bt709", "bt470bg", "smpte170m", "bt2020nc"]),
        }
    }
}

pub(super) fn ffprobe_json(ctx: &StepCtx, path: &Path) -> Result<Value> {
    let out = ctx
        .tools
        .command(&ctx.tools.ffprobe)
        .args([
            "-v",
            "error",
            "-show_streams",
            "-show_format",
            "-of",
            "json",
        ])
        .arg(path)
        .output()?;
    if !out.status.success() {
        return Err(Error::Tool(format!(
            "ffprobe: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    serde_json::from_slice(&out.stdout)
        .map_err(|e| Error::Tool(crate::t!("verify.ffprobe_output", error = e)))
}

pub(super) fn duration_of(v: &Value) -> Option<f64> {
    v["format"]["duration"].as_str()?.parse().ok()
}

fn probe(ctx: &StepCtx, path: &Path) -> Result<Probe> {
    let v = ffprobe_json(ctx, path)?;
    let streams = v["streams"].as_array().cloned().unwrap_or_default();
    let of_type = |t: &str| streams.iter().find(|s| s["codec_type"] == t);
    let text = |s: &Value, k: &str| s[k].as_str().unwrap_or("").to_string();
    let video = of_type("video").and_then(|s| {
        let rotation = s["side_data_list"]
            .as_array()
            .into_iter()
            .flatten()
            .find_map(|d| d["rotation"].as_i64())
            .unwrap_or(0);
        Some(VideoStream {
            codec: text(s, "codec_name"),
            width: u32::try_from(s["width"].as_u64()?).ok()?,
            height: u32::try_from(s["height"].as_u64()?).ok()?,
            rotation,
            pix_fmt: text(s, "pix_fmt"),
            range: text(s, "color_range"),
            primaries: text(s, "color_primaries"),
            transfer: text(s, "color_transfer"),
            space: text(s, "color_space"),
        })
    });
    Ok(Probe {
        video,
        has_audio: of_type("audio").is_some(),
        duration: duration_of(&v),
    })
}

/// The result must be readable by ffprobe and last as long as the input (±1 s).
fn check_duration(ctx: &StepCtx, input: Option<f64>, out: &Path) -> Result<()> {
    let got = duration_of(&ffprobe_json(ctx, out)?);
    match (input, got) {
        (Some(a), Some(b)) if (a - b).abs() <= 1.0 => Ok(()),
        (a, b) => Err(Error::Tool(crate::t!(
            "content.duration_vs",
            a = a.map_or("?".into(), |d| d.to_string()),
            b = b.map_or("?".into(), |d| d.to_string())
        ))),
    }
}

/// Hand over a copy of the input (never the input itself).
fn fall_back(ctx: &StepCtx, input: &Path, ext: String, why: &str) -> Result<StepResult> {
    let copy = tmp_output(ctx.tmp_dir, input, &ext)?;
    if let Err(e) = copy_keep_mtime(input, &copy) {
        let _ = std::fs::remove_file(&copy);
        return Err(e);
    }
    Ok(StepResult {
        path: copy,
        ext,
        warnings: vec![why.into()],
        fell_back: true,
    })
}

/// ffmpeg moves GPS into Keys:Location rounded to 4 digits (~10 m) and drops
/// the original UserData:GPSCoordinates (©xyz, the one galleries read);
/// Keys:GPSCoordinates is kept as is.
/// Android writes the camera into non-standard Keys:AndroidMake/AndroidModel;
/// they are duplicated into the common Keys:Make/Model, otherwise a gallery
/// does not know what the clip was shot with. Problems become warnings.
fn restore_tags(ctx: &StepCtx, input: &Path, out: &Path, warnings: &mut Vec<String>) {
    let read = |p: &Path| {
        exif::read(ctx.tools, &[p.to_path_buf()])
            .ok()
            .and_then(|mut m| m.remove(p))
    };
    let Some(rec) = read(input) else {
        warnings.push(crate::t!("content.meta_unreadable_input"));
        return;
    };
    let has = |r: &Value, k: &str| r.get(k).is_some_and(|v| !v.is_null());
    let had_gps = GPS_TAGS.iter().any(|t| has(&rec, t));
    if !had_gps && !has(&rec, "Keys:AndroidMake") && !has(&rec, "Keys:AndroidModel") {
        return;
    }
    // UserData comes from the input's own UserData tag or, when there is
    // none, from Keys:Location (exiftool converts ISO 6709 itself).
    // Keys:GPSCoordinates (iPhone) survives ffmpeg, it is copied to be sure.
    let user_data = if has(&rec, "UserData:GPSCoordinates") {
        "-UserData:GPSCoordinates"
    } else {
        "-UserData:GPSCoordinates<Keys:Location"
    };
    let args = [
        "-TagsFromFile".to_string(),
        input.display().to_string(),
        user_data.into(),
        "-Keys:GPSCoordinates".into(),
        "-Keys:Make<Keys:AndroidMake".into(),
        "-Keys:Model<Keys:AndroidModel".into(),
    ];
    if let Err(e) = exif::write(ctx.tools, out, &args) {
        warnings.push(crate::t!("content.meta_not_copied", error = e));
        return;
    }
    // Each location tag of the input must be present in the output.
    let mut expected = Vec::new();
    if has(&rec, "UserData:GPSCoordinates") || has(&rec, "Keys:Location") {
        expected.push("UserData:GPSCoordinates");
    }
    if has(&rec, "Keys:GPSCoordinates") {
        expected.push("Keys:GPSCoordinates");
    }
    if expected.is_empty() {
        return;
    }
    let result = read(out);
    for tag in expected {
        if !result.as_ref().is_some_and(|r| has(r, tag)) {
            warnings.push(crate::t!("content.gps_not_copied", tag = tag));
        }
    }
}

/// Run a tool; fail with the last lines of its stderr (SVT-AV1 prints its
/// banner there, the actual error comes last).
pub(super) fn run(mut cmd: Command, what: &str) -> Result<()> {
    let out = cmd.output()?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let lines: Vec<&str> = stderr.lines().filter(|l| !l.trim().is_empty()).collect();
    let tail = lines[lines.len().saturating_sub(5)..].join("; ");
    Err(Error::Tool(format!(
        "{what}: {}",
        if tail.is_empty() {
            crate::t!("content.failed")
        } else {
            tail
        }
    )))
}
