//! `gps_of`: the coordinates a photo or video carries, for a place's row.

use gallery_core::exif;
use gallery_core::filter::facts::gps_of_record;
use gallery_core::tools::Tools;
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Serialize, PartialEq)]
pub struct LatLon {
    pub lat: f64,
    pub lon: f64,
}

/// GPS of the file at `path`; an error text when it has no fix.
pub fn gps_of(tools: &Tools, path: &Path) -> Result<LatLon, String> {
    let records = exif::read(tools, &[path.to_path_buf()]).map_err(|e| e.to_string())?;
    records
        .get(path)
        .and_then(gps_of_record)
        .map(|(lat, lon)| LatLon { lat, lon })
        .ok_or_else(|| gallery_core::t!("gui.place_no_gps"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn tools() -> Option<Tools> {
        gallery_core::tools::find("exiftool")?;
        Tools::discover().ok()
    }

    fn photo(dir: &Path, name: &str, gps: bool) -> std::path::PathBuf {
        let p = dir.join(name);
        // A minimal JPEG; exiftool adds tags to it.
        std::fs::write(&p, include_bytes!("../tests/tiny.jpg")).unwrap();
        if gps {
            let st = Command::new("exiftool")
                .args([
                    "-overwrite_original",
                    "-q",
                    "-GPSLatitude=47.0245",
                    "-GPSLatitudeRef=N",
                    "-GPSLongitude=28.8323",
                    "-GPSLongitudeRef=E",
                ])
                .arg(&p)
                .status()
                .unwrap();
            assert!(st.success());
        }
        p
    }

    #[test]
    fn reads_the_gps_of_a_photo() {
        let Some(tools) = tools() else { return };
        let dir = tempfile::tempdir().unwrap();
        let p = photo(dir.path(), "a.jpg", true);
        let got = gps_of(&tools, &p).unwrap();
        assert!((got.lat - 47.0245).abs() < 1e-4, "{got:?}");
        assert!((got.lon - 28.8323).abs() < 1e-4, "{got:?}");
    }

    #[test]
    fn a_photo_without_gps_is_an_error_text() {
        let Some(tools) = tools() else { return };
        let dir = tempfile::tempdir().unwrap();
        let p = photo(dir.path(), "b.jpg", false);
        assert_eq!(
            gps_of(&tools, &p),
            Err(gallery_core::t!("gui.place_no_gps"))
        );
    }
}
