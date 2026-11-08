use std::path::Path;

use serde::{Deserialize, Serialize};

/// Coarse media classification of a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    Photo,
    Raw,
    Video,
    Audio,
    Png,
    Other,
}

/// Classify a file by its extension.
pub fn kind_of(path: &Path) -> MediaKind {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" | "heic" | "heif" | "webp" | "avif" | "tif" | "tiff" | "bmp" | "gif" => {
            MediaKind::Photo
        }
        "cr2" | "cr3" | "nef" | "arw" | "dng" | "raf" | "orf" | "rw2" => MediaKind::Raw,
        "mp4" | "m4v" | "mov" | "mkv" | "webm" | "avi" | "3gp" => MediaKind::Video,
        "mp3" | "m4a" | "aac" | "flac" | "wav" | "ogg" | "opus" | "wma" | "ape" | "alac" => {
            MediaKind::Audio
        }
        "png" => MediaKind::Png,
        _ => MediaKind::Other,
    }
}

/// Split a file name into (stem, extension without dot, case preserved).
/// `av1.mp4` counts as one compound extension.
pub fn split_name(file_name: &str) -> (String, String) {
    const COMPOUND: &str = ".av1.mp4";
    if file_name.len() > COMPOUND.len()
        && file_name.is_char_boundary(file_name.len() - COMPOUND.len())
        && file_name[file_name.len() - COMPOUND.len()..].eq_ignore_ascii_case(COMPOUND)
    {
        let cut = file_name.len() - COMPOUND.len();
        return (
            file_name[..cut].to_string(),
            file_name[cut + 1..].to_string(),
        );
    }
    match file_name.rfind('.') {
        Some(i) if i > 0 => (file_name[..i].to_string(), file_name[i + 1..].to_string()),
        _ => (file_name.to_string(), String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_name_compound() {
        assert_eq!(split_name("a.av1.mp4"), ("a".into(), "av1.mp4".into()));
        assert_eq!(split_name("a.b.jpg"), ("a.b".into(), "jpg".into()));
        assert_eq!(split_name("README"), ("README".into(), "".into()));
        assert_eq!(split_name(".hidden"), (".hidden".into(), "".into()));
        assert_eq!(split_name("A.AV1.MP4"), ("A".into(), "AV1.MP4".into()));
    }

    #[test]
    fn kind_of_by_extension() {
        let k = |s: &str| kind_of(Path::new(s));
        assert_eq!(k("a.JPG"), MediaKind::Photo);
        assert_eq!(k("a.dng"), MediaKind::Raw);
        assert_eq!(k("a.av1.mp4"), MediaKind::Video);
        assert_eq!(k("a.opus"), MediaKind::Audio);
        assert_eq!(k("a.png"), MediaKind::Png);
        assert_eq!(k("a.txt"), MediaKind::Other);
        assert_eq!(k("README"), MediaKind::Other);
    }
}
