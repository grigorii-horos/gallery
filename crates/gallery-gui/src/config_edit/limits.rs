//! Allowed values of content preset fields, as the encoders take them:
//! avifenc `-q 0..100 -s 0..10`, ImageMagick WebP quality 0..100 and
//! `webp:method` 0..6, SVT-AV1 `-crf 0..63 -preset 0..13`, libopus 6..510
//! kbit/s, LAME 8..320, optipng `-o0..7`. The editor checks a draft with
//! them and the forms take their bounds from here.

use serde::Serialize;

use super::model::ContentPresetDraft;

/// Bounds of one numeric field of a content kind; `variant` narrows it to
/// a photo `format` or an audio `codec`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Limit {
    pub kind: &'static str,
    pub variant: Option<&'static str>,
    pub key: &'static str,
    pub min: i64,
    pub max: i64,
}

const fn l(
    kind: &'static str,
    variant: Option<&'static str>,
    key: &'static str,
    min: i64,
    max: i64,
) -> Limit {
    Limit {
        kind,
        variant,
        key,
        min,
        max,
    }
}

const LIMITS: &[Limit] = &[
    l("photo", None, "quality", 0, 100),
    l("photo", Some("avif"), "speed", 0, 10),
    l("photo", Some("webp"), "speed", 0, 6),
    l("photo", None, "max_short_side", 1, 65535),
    l("photo", None, "min_saving_pct", 0, 99),
    l("video", None, "crf", 0, 63),
    l("video", None, "preset", 0, 13),
    l("video", None, "gop", 1, 10000),
    l("video", None, "audio_kbps", 6, 510),
    // 0: no downscale.
    l("video", None, "max_short_side", 0, 65535),
    l("audio", Some("opus"), "kbps", 6, 510),
    l("audio", Some("mp3"), "kbps", 8, 320),
    l("audio", Some("aac"), "kbps", 8, 512),
    l("png", None, "level", 0, 7),
];

/// Every bound.
pub fn all() -> &'static [Limit] {
    LIMITS
}

/// The values a string field may take.
pub fn choices(kind: &str, key: &str) -> Option<&'static [&'static str]> {
    match (kind, key) {
        ("photo", "format") => Some(&["avif", "webp"]),
        ("video", "denoise") => Some(&["verylow", "low", "medium", "high"]),
        ("audio", "codec") => Some(&["opus", "mp3", "aac", "flac"]),
        _ => None,
    }
}

pub const KINDS: &[&str] = &["metadata", "photo", "video", "audio", "png", "bundle"];

/// The bound of `key` for preset `p` (its kind and variant).
pub fn limit_for(p: &ContentPresetDraft, key: &str) -> Option<&'static Limit> {
    let variant = match p.kind.as_str() {
        // The core's defaults when the key is absent.
        "photo" => p.format.as_deref().unwrap_or("avif"),
        "audio" => p.codec.as_deref().unwrap_or("opus"),
        _ => "",
    };
    LIMITS
        .iter()
        .filter(|x| x.kind == p.kind && x.key == key)
        .find(|x| x.variant.is_none_or(|v| v == variant))
}
