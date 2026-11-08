//! Thumbnails for the UI: the classifier's thumbnails (512 px photos, a row
//! of three frames for videos), cached under `<user cache dir>/gallery/thumbs/`
//! by source path, size and mtime. A changed file gets a new cache entry; an
//! unchanged one is never regenerated. Generation happens in a private temp
//! directory and the result is renamed into place, so concurrent calls for
//! the same file are safe and a reader never sees a partial thumbnail.

use std::path::{Path, PathBuf};

use crate::ai::thumb;
use crate::media::{MediaKind, kind_of};
use crate::scan::mtime_ns;
use crate::tools::Tools;
use crate::{Error, Result};

/// Prefix of the private work directories in the cache directory.
const TMP_PREFIX: &str = ".tmp-";
/// Work directories older than this are leftovers of a crashed process.
const TMP_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(3600);

/// Remove work directories a crashed process left behind (older than
/// [`TMP_MAX_AGE`]); a live call's directory is far younger. Best effort.
fn remove_stale_temp_dirs(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        if !e.file_name().to_string_lossy().starts_with(TMP_PREFIX) {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(e.path()) else {
            continue;
        };
        let old = meta
            .modified()
            .ok()
            .and_then(|m| m.elapsed().ok())
            .is_some_and(|age| age > TMP_MAX_AGE);
        if meta.is_dir() && old {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

/// `<user cache dir>/gallery/thumbs`.
pub fn default_dir() -> Option<PathBuf> {
    crate::paths::cache_dir().map(|d| d.join("thumbs"))
}

/// Cache file name for a file in this state: blake3 of path, size, mtime.
pub fn cache_name(path: &Path, size: u64, mtime_ns: i128) -> String {
    let mut h = blake3::Hasher::new();
    h.update(path.as_os_str().as_encoded_bytes());
    h.update(&[0]);
    h.update(&size.to_le_bytes());
    h.update(&mtime_ns.to_le_bytes());
    format!("{}.jpg", h.finalize().to_hex())
}

/// The thumbnail of `path` in the default cache directory.
pub fn thumb_for(tools: &Tools, path: &Path) -> Result<PathBuf> {
    let dir = default_dir().ok_or_else(|| Error::Tool(crate::t!("ai.no_cache_dir")))?;
    thumb_for_in(tools, path, &dir)
}

/// The thumbnail of `path` cached in `dir`, made when missing.
pub fn thumb_for_in(tools: &Tools, path: &Path, dir: &Path) -> Result<PathBuf> {
    let kind = kind_of(path);
    if matches!(kind, MediaKind::Audio | MediaKind::Other) {
        return Err(Error::Tool(crate::t!(
            "thumbs.no_type",
            path = path.display()
        )));
    }
    let meta = std::fs::metadata(path)?;
    if !meta.is_file() {
        return Err(Error::Tool(crate::t!(
            "thumbs.not_file",
            path = path.display()
        )));
    }
    let cached = dir.join(cache_name(path, meta.len(), mtime_ns(&meta)?));
    if cached.is_file() {
        return Ok(cached);
    }
    std::fs::create_dir_all(dir)?;
    remove_stale_temp_dirs(dir);
    // Private work dir on the same filesystem: removed on drop, whatever
    // happens; the finished thumbnail is renamed out of it atomically.
    let work = tempfile::Builder::new()
        .prefix(TMP_PREFIX)
        .tempdir_in(dir)?;
    let made = thumb::make(tools, path, kind, work.path())?;
    std::fs::rename(&made, &cached)?;
    Ok(cached)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bogus_tools() -> Tools {
        let p = PathBuf::from("/nonexistent/tool");
        Tools {
            ffmpeg: p.clone(),
            ffprobe: p.clone(),
            exiftool: p.clone(),
            avifenc: p.clone(),
            magick: p,
            optipng: None,
            tz: None,
        }
    }

    fn magick_tools() -> Option<Tools> {
        let magick = crate::tools::find("magick")?;
        Some(Tools {
            magick,
            ..bogus_tools()
        })
    }

    fn red_png(tools: &Tools, path: &Path, size: &str) {
        let ok = std::process::Command::new(&tools.magick)
            .args(["-size", size, "xc:red"])
            .arg(path)
            .status()
            .unwrap();
        assert!(ok.success());
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    fn key_of(path: &Path) -> String {
        let m = std::fs::metadata(path).unwrap();
        cache_name(path, m.len(), mtime_ns(&m).unwrap())
    }

    #[test]
    fn cache_name_depends_on_path_size_and_mtime() {
        let a = cache_name(Path::new("/s/a.jpg"), 1, 2);
        assert!(a.ends_with(".jpg"));
        assert_eq!(a, cache_name(Path::new("/s/a.jpg"), 1, 2));
        assert_ne!(a, cache_name(Path::new("/s/b.jpg"), 1, 2));
        assert_ne!(a, cache_name(Path::new("/s/a.jpg"), 3, 2));
        assert_ne!(a, cache_name(Path::new("/s/a.jpg"), 1, 3));
    }

    #[test]
    fn cached_thumbnail_is_returned_without_tools() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("a.jpg");
        std::fs::write(&src, b"not really a jpeg").unwrap();
        let cache = d.path().join("thumbs");
        std::fs::create_dir_all(&cache).unwrap();
        let cached = cache.join(key_of(&src));
        std::fs::write(&cached, b"thumb").unwrap();
        // The bogus tools would fail if anything were generated.
        assert_eq!(thumb_for_in(&bogus_tools(), &src, &cache).unwrap(), cached);
    }

    #[test]
    fn failed_generation_is_an_error_and_leaves_nothing() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("a.jpg");
        std::fs::write(&src, b"x").unwrap();
        let cache = d.path().join("thumbs");
        assert!(thumb_for_in(&bogus_tools(), &src, &cache).is_err());
        assert!(
            !cache.exists() || names(&cache).is_empty(),
            "{:?}",
            names(&cache)
        );
    }

    #[test]
    fn missing_source_and_non_media_are_errors() {
        let d = tempfile::tempdir().unwrap();
        let cache = d.path().join("thumbs");
        assert!(thumb_for_in(&bogus_tools(), &d.path().join("nope.jpg"), &cache).is_err());
        let txt = d.path().join("a.txt");
        std::fs::write(&txt, b"x").unwrap();
        assert!(thumb_for_in(&bogus_tools(), &txt, &cache).is_err());
    }

    #[test]
    fn generates_once_and_again_when_the_file_changes() {
        let Some(tools) = magick_tools() else {
            eprintln!("skip: magick not found");
            return;
        };
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("a.png");
        red_png(&tools, &src, "64x64");
        let cache = d.path().join("thumbs");
        let t = thumb_for_in(&tools, &src, &cache).unwrap();
        assert_eq!(t, cache.join(key_of(&src)));
        let bytes = std::fs::read(&t).unwrap();
        assert_eq!(&bytes[..2], &[0xFF, 0xD8], "a JPEG");
        assert_eq!(names(&cache), vec![key_of(&src)], "no temp leftovers");
        // Unchanged file: served from the cache even without tools.
        assert_eq!(thumb_for_in(&bogus_tools(), &src, &cache).unwrap(), t);
        // Changed file: a new thumbnail.
        red_png(&tools, &src, "80x80");
        let t2 = thumb_for_in(&tools, &src, &cache).unwrap();
        assert_ne!(t2, t);
        assert!(t2.is_file());
    }

    #[test]
    fn concurrent_calls_agree() {
        let Some(tools) = magick_tools() else {
            eprintln!("skip: magick not found");
            return;
        };
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("a.png");
        red_png(&tools, &src, "64x64");
        let cache = d.path().join("thumbs");
        let results: Vec<PathBuf> = std::thread::scope(|s| {
            let hs: Vec<_> = (0..8)
                .map(|_| s.spawn(|| thumb_for_in(&tools, &src, &cache).unwrap()))
                .collect();
            hs.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert!(results.windows(2).all(|w| w[0] == w[1]));
        let bytes = std::fs::read(&results[0]).unwrap();
        assert_eq!(&bytes[..2], &[0xFF, 0xD8]);
        assert_eq!(names(&cache), vec![key_of(&src)]);
    }

    #[test]
    fn video_thumbnail_is_a_frame_strip() {
        let (Some(magick), Some(ffmpeg), Some(ffprobe)) = (
            crate::tools::find("magick"),
            crate::tools::find("ffmpeg"),
            crate::tools::find("ffprobe"),
        ) else {
            eprintln!("skip: magick/ffmpeg/ffprobe not found");
            return;
        };
        let tools = Tools {
            magick,
            ffmpeg: ffmpeg.clone(),
            ffprobe,
            ..bogus_tools()
        };
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("v.mp4");
        let ok = std::process::Command::new(&ffmpeg)
            .args(["-nostdin", "-v", "error", "-f", "lavfi"])
            .args(["-i", "testsrc=d=2:s=64x48:r=10", "-pix_fmt", "yuv420p"])
            .arg(&src)
            .status()
            .unwrap();
        assert!(ok.success());
        let cache = d.path().join("thumbs");
        let t = thumb_for_in(&tools, &src, &cache).unwrap();
        assert_eq!(&std::fs::read(&t).unwrap()[..2], &[0xFF, 0xD8]);
        assert_eq!(names(&cache), vec![key_of(&src)]);
    }

    #[test]
    fn stale_temp_dirs_are_removed() {
        let d = tempfile::tempdir().unwrap();
        let cache = d.path().join("thumbs");
        let old = cache.join(".tmp-old");
        let fresh = cache.join(".tmp-fresh");
        std::fs::create_dir_all(old.join("inner")).unwrap();
        std::fs::write(old.join("_frame_1.jpg"), b"x").unwrap();
        std::fs::create_dir_all(&fresh).unwrap();
        std::fs::write(cache.join("keep.jpg"), b"x").unwrap();
        let two_hours_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 3600);
        std::fs::File::open(&old)
            .unwrap()
            .set_modified(two_hours_ago)
            .unwrap();
        let src = d.path().join("a.jpg");
        std::fs::write(&src, b"x").unwrap();
        // Generation fails (bogus tools); the cleanup happens regardless.
        let _ = thumb_for_in(&bogus_tools(), &src, &cache);
        assert_eq!(names(&cache), vec![".tmp-fresh", "keep.jpg"]);
    }

    #[test]
    fn default_dir_is_under_cache_dir() {
        if let Some(p) = default_dir() {
            assert!(p.ends_with("gallery/thumbs"));
        }
    }
}
