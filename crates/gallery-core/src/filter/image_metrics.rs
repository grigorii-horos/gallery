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

    pub fn get(&self, key: &str) -> Option<Metric> {
        self.map.lock().unwrap().get(key).cloned()
    }

    /// Merge `f`'s changes into the entry `key`.
    pub fn update(&self, key: &str, f: impl FnOnce(&mut Metric)) {
        let mut map = self.map.lock().unwrap();
        let m = map.entry(key.to_string()).or_default();
        let before = m.clone();
        f(m);
        if *m != before {
            *self.dirty.lock().unwrap() = true;
        }
    }

    /// Write the file back when something changed.
    pub fn save(&self) -> Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if !*self.dirty.lock().unwrap() {
            return Ok(());
        }
        let bytes = serde_json::to_vec(&*self.map.lock().unwrap())
            .map_err(|e| Error::Io(std::io::Error::other(e)))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        crate::paths::write_atomic(path, &bytes)?;
        *self.dirty.lock().unwrap() = false;
        Ok(())
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
        c.update("k", |m| m.dhash = Some(u64::MAX));
        c.save().unwrap();
        let again = MetricsCache::load(dir.path());
        assert_eq!(again.get("k").unwrap().dhash, Some(u64::MAX));
    }
}
