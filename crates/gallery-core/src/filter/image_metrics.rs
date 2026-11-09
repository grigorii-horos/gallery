//! Pixel measures of a photo's thumbnail: a sharpness score (variance of
//! the Laplacian) and a perceptual difference hash (dHash). Both read the
//! 512 px JPEG thumbnail, never the original. Results are cached in
//! `<cache dir>/metrics.json` together with content hashes, keyed by
//! [`crate::thumbs::cache_name`] of the source file (path, size, mtime).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use image::GrayImage;
use image::imageops::FilterType;
use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// The largest side measured; bigger pictures are scaled down first, so a
/// score does not depend on whether the input is a thumbnail or not.
const MAX_SIDE: u32 = 512;
/// Laplacian variance mapped to a score of 100.
const VAR_AT_100: f64 = 2000.0;

fn gray(jpeg: &Path) -> Result<GrayImage> {
    let bad = |e: &dyn std::fmt::Display| {
        Error::Tool(crate::t!(
            "filter.image_unreadable",
            path = jpeg.display(),
            error = e
        ))
    };
    let img = image::ImageReader::open(jpeg)?
        .with_guessed_format()?
        .decode()
        .map_err(|e| bad(&e))?;
    let img = if img.width() > MAX_SIDE || img.height() > MAX_SIDE {
        img.thumbnail(MAX_SIDE, MAX_SIDE)
    } else {
        img
    };
    Ok(img.to_luma8())
}

fn sharpness_of(g: &GrayImage) -> f32 {
    let (w, h) = g.dimensions();
    if w < 3 || h < 3 {
        return 0.0;
    }
    let px = |x: u32, y: u32| f64::from(g.get_pixel(x, y)[0]);
    let (mut sum, mut sq, mut n) = (0.0, 0.0, 0.0);
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let l = px(x - 1, y) + px(x + 1, y) + px(x, y - 1) + px(x, y + 1) - 4.0 * px(x, y);
            sum += l;
            sq += l * l;
            n += 1.0;
        }
    }
    let mean = sum / n;
    let var = (sq / n - mean * mean).max(0.0);
    (100.0 * (1.0 + var).ln() / (1.0 + VAR_AT_100).ln()).min(100.0) as f32
}

fn dhash_of(g: &GrayImage) -> u64 {
    let small = image::imageops::resize(g, 9, 8, FilterType::Triangle);
    let mut bits = 0u64;
    for y in 0..8 {
        for x in 0..8 {
            bits <<= 1;
            if small.get_pixel(x, y)[0] < small.get_pixel(x + 1, y)[0] {
                bits |= 1;
            }
        }
    }
    bits
}

/// Sharpness of a JPEG, 0 (flat) to 100 (very sharp): the variance of the
/// 3×3 Laplacian of the grayscale picture, `100·ln(1+var)/ln(1+2000)`
/// capped at 100.
pub fn sharpness(jpeg: &Path) -> Result<f32> {
    Ok(sharpness_of(&gray(jpeg)?))
}

/// Difference hash of a JPEG: a 9×8 grayscale copy, one bit per pair of
/// horizontal neighbours. Similar pictures differ in few bits.
pub fn dhash(jpeg: &Path) -> Result<u64> {
    Ok(dhash_of(&gray(jpeg)?))
}

/// Both measures with one decode.
pub fn measure(jpeg: &Path) -> Result<(f32, u64)> {
    let g = gray(jpeg)?;
    Ok((sharpness_of(&g), dhash_of(&g)))
}

/// Bits in which two hashes differ.
pub fn hamming(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

/// What is known about one file in one state.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Metric {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sharpness: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dhash: Option<u64>,
    /// blake3 of the whole content, hex.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blake3: Option<String>,
    /// The file, so a save can drop the entries of files gone or changed;
    /// entries written before it was recorded get it when next looked up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
}

/// The `metrics.json` cache; safe to share between worker threads.
#[derive(Debug)]
pub struct MetricsCache {
    /// `None`: kept in memory only, never written.
    path: Option<PathBuf>,
    map: Mutex<HashMap<String, Metric>>,
    dirty: Mutex<bool>,
}

impl MetricsCache {
    /// Load `<dir>/metrics.json`; a missing or unreadable file is an empty
    /// cache.
    pub fn load(dir: &Path) -> MetricsCache {
        let path = dir.join("metrics.json");
        let map = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        MetricsCache {
            path: Some(path),
            map: Mutex::new(map),
            dirty: Mutex::new(false),
        }
    }

    /// An empty cache that is never written (no cache dir).
    pub fn in_memory() -> MetricsCache {
        MetricsCache {
            path: None,
            map: Mutex::new(HashMap::new()),
            dirty: Mutex::new(false),
        }
    }

    /// What is known about `path` with this size and mtime.
    pub fn get(&self, path: &Path, size: u64, mtime_ns: i128) -> Option<Metric> {
        let mut map = self.map.lock().unwrap();
        let m = map.get_mut(&crate::thumbs::cache_name(path, size, mtime_ns))?;
        if m.path.is_none() {
            m.path = Some(path.to_path_buf());
            *self.dirty.lock().unwrap() = true;
        }
        Some(m.clone())
    }

    /// Merge `f`'s changes into the entry of `path` with this size and mtime.
    pub fn update(&self, path: &Path, size: u64, mtime_ns: i128, f: impl FnOnce(&mut Metric)) {
        let mut map = self.map.lock().unwrap();
        let key = crate::thumbs::cache_name(path, size, mtime_ns);
        let m = map.entry(key).or_default();
        let before = m.clone();
        f(m);
        m.path = Some(path.to_path_buf());
        if *m != before {
            *self.dirty.lock().unwrap() = true;
        }
    }

    /// Write the file back when something changed, without the entries of
    /// files that are gone or no longer have the size and mtime they were
    /// measured with.
    pub fn save(&self) -> Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if !*self.dirty.lock().unwrap() {
            return Ok(());
        }
        let mut map = self.map.lock().unwrap();
        map.retain(|key, m| !m.path.as_deref().is_some_and(|p| stale(p, key)));
        let bytes = serde_json::to_vec(&*map).map_err(|e| Error::Io(std::io::Error::other(e)))?;
        drop(map);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        crate::paths::write_atomic(path, &bytes)?;
        *self.dirty.lock().unwrap() = false;
        Ok(())
    }
}

/// The entry `key` of file `p` is of no use any more: the file changed
/// (another size or mtime), or it is gone from a folder that is still
/// there. A file whose folder is missing too (an SD card or a share not
/// mounted now) keeps its entry for when it is back.
fn stale(p: &Path, key: &str) -> bool {
    match std::fs::metadata(p) {
        Ok(m) => crate::scan::mtime_ns(&m)
            .is_ok_and(|mtime_ns| crate::thumbs::cache_name(p, m.len(), mtime_ns) != key),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => p.parent().is_some_and(Path::is_dir),
        // Unreadable now (permissions, a share failing): keep it.
        Err(_) => false,
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use image::{GrayImage, Luma};
    use std::path::Path;

    /// A horizontal gradient with a fine checkerboard on top.
    pub fn sharp(size: u32) -> GrayImage {
        GrayImage::from_fn(size, size, |x, y| {
            let base = 40.0 + 170.0 * f64::from(x) / f64::from(size);
            let check = if (x / 4 + y / 4) % 2 == 0 {
                40.0
            } else {
                -40.0
            };
            Luma([(base + check).clamp(0.0, 255.0) as u8])
        })
    }

    /// The same picture, Gaussian-blurred.
    pub fn blurred(size: u32) -> GrayImage {
        image::imageops::blur(&sharp(size), 4.0)
    }

    /// A different picture: the gradient runs the other way.
    pub fn other(size: u32) -> GrayImage {
        GrayImage::from_fn(size, size, |x, _| {
            Luma([(210.0 - 170.0 * f64::from(x) / f64::from(size)) as u8])
        })
    }

    pub fn save(img: &GrayImage, path: &Path) {
        img.save(path).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

    #[test]
    fn sharp_scores_well_above_blurred() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a.jpg"), dir.path().join("b.jpg"));
        save(&sharp(512), &a);
        save(&blurred(512), &b);
        let (sa, sb) = (sharpness(&a).unwrap(), sharpness(&b).unwrap());
        assert!(sa > sb + 20.0, "sharp {sa}, blurred {sb}");
        assert!((0.0..=100.0).contains(&sa) && (0.0..=100.0).contains(&sb));
    }

    #[test]
    fn dhash_tells_similar_from_different() {
        let dir = tempfile::tempdir().unwrap();
        let p = |n: &str| dir.path().join(n);
        save(&sharp(512), &p("a.jpg"));
        save(&blurred(512), &p("b.jpg"));
        save(&other(512), &p("c.jpg"));
        let (a, b, c) = (
            dhash(&p("a.jpg")).unwrap(),
            dhash(&p("b.jpg")).unwrap(),
            dhash(&p("c.jpg")).unwrap(),
        );
        assert!(hamming(a, b) <= 10, "{}", hamming(a, b));
        assert!(hamming(a, c) > 10, "{}", hamming(a, c));
        assert_eq!(measure(&p("a.jpg")).unwrap().1, a);
    }

    #[test]
    fn unreadable_image_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.jpg");
        std::fs::write(&p, b"not a jpeg").unwrap();
        assert!(sharpness(&p).is_err());
    }

    #[test]
    fn cache_round_trips_and_writes_only_changes() {
        let dir = tempfile::tempdir().unwrap();
        let c = MetricsCache::load(dir.path());
        c.save().unwrap();
        assert!(!dir.path().join("metrics.json").exists());
        let f = dir.path().join("a.jpg");
        std::fs::write(&f, b"a").unwrap();
        let (size, mtime) = stamp(&f);
        c.update(&f, size, mtime, |m| m.dhash = Some(u64::MAX));
        c.save().unwrap();
        let again = MetricsCache::load(dir.path());
        assert_eq!(again.get(&f, size, mtime).unwrap().dhash, Some(u64::MAX));
    }

    fn stamp(p: &Path) -> (u64, i128) {
        let m = std::fs::metadata(p).unwrap();
        (m.len(), crate::scan::mtime_ns(&m).unwrap())
    }

    #[test]
    fn save_drops_entries_of_gone_or_changed_files() {
        let dir = tempfile::tempdir().unwrap();
        let mk = |n: &str| {
            let p = dir.path().join(n);
            std::fs::write(&p, n).unwrap();
            p
        };
        let (kept, gone, changed) = (mk("kept.jpg"), mk("gone.jpg"), mk("changed.jpg"));
        let c = MetricsCache::load(dir.path());
        for p in [&kept, &gone, &changed] {
            let (size, mtime) = stamp(p);
            c.update(p, size, mtime, |m| m.dhash = Some(1));
        }
        let old = stamp(&changed);
        std::fs::remove_file(&gone).unwrap();
        std::fs::write(&changed, "changed, longer").unwrap();
        c.save().unwrap();
        let again = MetricsCache::load(dir.path());
        assert_eq!(again.map.lock().unwrap().len(), 1);
        let (size, mtime) = stamp(&kept);
        assert!(again.get(&kept, size, mtime).is_some());
        assert!(again.get(&changed, old.0, old.1).is_none());
    }

    #[test]
    fn save_keeps_entries_of_files_on_a_folder_not_there_now() {
        let dir = tempfile::tempdir().unwrap();
        let card = dir.path().join("card");
        std::fs::create_dir(&card).unwrap();
        let p = card.join("a.jpg");
        std::fs::write(&p, "a").unwrap();
        let (size, mtime) = stamp(&p);
        let c = MetricsCache::load(dir.path());
        c.update(&p, size, mtime, |m| m.dhash = Some(1));
        // The card is taken out: its folder is gone with the file.
        std::fs::remove_dir_all(&card).unwrap();
        c.save().unwrap();
        let again = MetricsCache::load(dir.path());
        assert_eq!(again.get(&p, size, mtime).and_then(|m| m.dhash), Some(1));
    }
}
