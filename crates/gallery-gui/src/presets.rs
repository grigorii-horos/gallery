//! Preset and mask listings: the built-ins (as `gallery presets` prints them)
//! and a config's own.

use gallery_core::config::{ContentPreset, NamePreset};
use gallery_core::mask::{MaskExpr, MaskScope};
use gallery_core::presets::{
    builtin_content_presets, builtin_masks, builtin_name_presets, describe_content, describe_name,
};
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Serialize)]
pub struct BuiltinPresets {
    pub name: Vec<PresetInfo>,
    pub content: Vec<PresetInfo>,
    pub masks: Vec<MaskInfo>,
}

/// One name or content preset.
#[derive(Debug, Clone, Serialize)]
pub struct PresetInfo {
    pub name: String,
    /// Content kind (`photo`, `video`, `audio`, `png`, `metadata`); `name` for name presets.
    pub kind: String,
    /// Human description (Russian), as in `gallery presets`.
    pub description: String,
    /// The preset's mask as written (its kind default if it has none).
    pub mask: String,
    /// The mask with every `@name` expanded.
    pub mask_expanded: String,
    /// All fields of the preset, as in a config file.
    pub params: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct MaskInfo {
    pub name: String,
    pub source: String,
    pub expanded: String,
}

pub fn builtin_presets() -> BuiltinPresets {
    let scope = gallery_core::config::Config::default();
    BuiltinPresets {
        name: name_infos(&builtin_name_presets(), &scope),
        content: content_infos(&builtin_content_presets(), &scope),
        masks: mask_infos(builtin_masks(), &scope),
    }
}

pub fn name_infos(
    presets: &BTreeMap<String, NamePreset>,
    scope: &impl MaskScope,
) -> Vec<PresetInfo> {
    presets
        .iter()
        .map(|(name, p)| {
            let mask = p.mask_or_default();
            PresetInfo {
                name: name.clone(),
                kind: "name".into(),
                description: describe_name(p),
                mask: mask.source(),
                mask_expanded: mask.expand(scope),
                params: serde_json::to_value(p).unwrap_or_default(),
            }
        })
        .collect()
}

pub fn content_infos(
    presets: &BTreeMap<String, ContentPreset>,
    scope: &impl MaskScope,
) -> Vec<PresetInfo> {
    presets
        .iter()
        .map(|(name, p)| {
            let mask = p.mask_or_default();
            let params = serde_json::to_value(p).unwrap_or_default();
            let kind = params["kind"].as_str().unwrap_or("").to_string();
            PresetInfo {
                name: name.clone(),
                kind,
                description: describe_content(p),
                mask: mask.source(),
                mask_expanded: mask.expand(scope),
                params,
            }
        })
        .collect()
}

pub fn mask_infos(masks: &BTreeMap<String, MaskExpr>, scope: &impl MaskScope) -> Vec<MaskInfo> {
    masks
        .iter()
        .map(|(name, e)| MaskInfo {
            name: name.clone(),
            source: e.source(),
            expanded: e.expand(scope),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_payload_shape() {
        let v = serde_json::to_value(builtin_presets()).unwrap();
        let name = v["name"].as_array().unwrap();
        let content = v["content"].as_array().unwrap();
        let masks = v["masks"].as_array().unwrap();
        assert!(!name.is_empty() && !content.is_empty() && !masks.is_empty());
        for p in name.iter().chain(content) {
            for key in ["name", "kind", "description", "mask", "mask_expanded"] {
                assert!(p[key].is_string(), "{key} in {p}");
            }
            assert!(p["params"].is_object());
        }
        assert!(name.iter().all(|p| p["kind"] == "name"));
        let kinds: Vec<&str> = content
            .iter()
            .map(|p| p["kind"].as_str().unwrap())
            .collect();
        assert!(kinds.contains(&"photo") && kinds.contains(&"video"));
        for m in masks {
            assert!(m["name"].is_string() && m["source"].is_string() && m["expanded"].is_string());
        }
    }

    #[test]
    fn masks_expand_references() {
        let b = builtin_presets();
        let photo = b.masks.iter().find(|m| m.name == "photo").unwrap();
        assert!(!photo.expanded.contains('@'), "{}", photo.expanded);
        let avif = b.content.iter().find(|p| p.name == "avif-high").unwrap();
        assert_eq!(avif.mask, "@photo-original");
        assert!(!avif.mask_expanded.contains('@'));
    }
}
