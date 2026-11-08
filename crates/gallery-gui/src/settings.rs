//! The tray and notification settings the Settings screen edits. Plain
//! data with defaults and bounds; the file lives next to the plans.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Shortest and longest background check interval, minutes.
pub const MIN_INTERVAL_MIN: u32 = 5;
pub const MAX_INTERVAL_MIN: u32 = 24 * 60;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TraySettings {
    /// Minutes between background checks.
    pub check_interval_min: u32,
    /// Notify when this many new files came since the last notice.
    pub notify_threshold: u32,
    /// The window's close button hides it to the tray instead of quitting.
    pub hide_to_tray: bool,
    /// Interface language: `"system"` (the OS's, else English) or one of the
    /// supported codes (`gallery_core::i18n::LANGUAGES`).
    pub language: String,
}

/// The setting a language choice is stored as: `"system"` or a supported code.
pub fn language_setting(value: &str) -> String {
    let v = value.trim();
    gallery_core::i18n::LANGUAGES
        .iter()
        .find(|l| l.code == v)
        .map_or_else(|| "system".to_string(), |l| l.code.to_string())
}

impl Default for TraySettings {
    fn default() -> Self {
        TraySettings {
            check_interval_min: 30,
            notify_threshold: 1,
            hide_to_tray: true,
            language: "system".into(),
        }
    }
}

impl TraySettings {
    /// The same settings brought into bounds (what is saved and used).
    pub fn sanitized(mut self) -> Self {
        self.check_interval_min = self
            .check_interval_min
            .clamp(MIN_INTERVAL_MIN, MAX_INTERVAL_MIN);
        self.notify_threshold = self.notify_threshold.max(1);
        self.language = language_setting(&self.language);
        self
    }
}

/// `settings.json` in the GUI data dir (`$GALLERY_DATA_DIR/` in debug builds
/// when set, else `<user data dir>/gallery/`).
pub fn data_dir() -> Option<PathBuf> {
    match crate::data_dir_override() {
        Some(dir) => Some(dir),
        None => dirs::data_dir().map(|d| d.join("gallery")),
    }
}

/// A missing or broken file gives the defaults.
pub fn load(path: &Path) -> TraySettings {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice::<TraySettings>(&b).ok())
        .unwrap_or_default()
        .sanitized()
}

pub fn save(path: &Path, s: &TraySettings) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let text = serde_json::to_vec_pretty(&s.clone().sanitized()).map_err(|e| e.to_string())?;
    std::fs::write(path, text).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_thirty_minutes_one_file_and_hide() {
        let d = TraySettings::default();
        assert_eq!(
            (d.check_interval_min, d.notify_threshold, d.hide_to_tray),
            (30, 1, true)
        );
    }

    #[test]
    fn missing_fields_take_defaults_and_roundtrip() {
        let s: TraySettings = serde_json::from_str(r#"{"hide_to_tray": false}"#).unwrap();
        assert_eq!(
            s,
            TraySettings {
                hide_to_tray: false,
                ..Default::default()
            }
        );
        let back: TraySettings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn language_is_system_or_a_supported_code() {
        assert_eq!(TraySettings::default().language, "system");
        for code in [
            "en", "ru", "uk", "de", "fr", "es", "pt-BR", "it", "pl", "zh-CN",
        ] {
            assert_eq!(language_setting(code), code);
        }
        for junk in ["", "system", " ru-RU ", "klingon", "pt_BR"] {
            assert_eq!(language_setting(junk), "system", "{junk}");
        }
        // Older files have no language.
        let s: TraySettings = serde_json::from_str(r#"{"check_interval_min": 60}"#).unwrap();
        assert_eq!(s.language, "system");
    }

    #[test]
    fn bounds_are_enforced() {
        let s = TraySettings {
            check_interval_min: 0,
            notify_threshold: 0,
            hide_to_tray: true,
            language: "klingon".into(),
        }
        .sanitized();
        assert_eq!(
            (s.check_interval_min, s.notify_threshold),
            (MIN_INTERVAL_MIN, 1)
        );
        // An unknown language is the system's.
        assert_eq!(s.language, "system");
        let s = TraySettings {
            check_interval_min: u32::MAX,
            ..Default::default()
        }
        .sanitized();
        assert_eq!(s.check_interval_min, MAX_INTERVAL_MIN);
    }

    #[test]
    fn file_roundtrip_and_broken_file_gives_defaults() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("sub/settings.json");
        assert_eq!(load(&p), TraySettings::default());
        let s = TraySettings {
            check_interval_min: 90,
            notify_threshold: 10,
            hide_to_tray: false,
            language: "pt-BR".into(),
        };
        save(&p, &s).unwrap();
        assert_eq!(load(&p), s);
        std::fs::write(&p, "{broken").unwrap();
        assert_eq!(load(&p), TraySettings::default());
    }
}
