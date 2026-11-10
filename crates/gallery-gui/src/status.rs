//! `app_status`: config health, external tools and the AI key source.

use gallery_core::ai::key::{self, KeySource};
use gallery_core::config::Config;
use gallery_core::tools::{self, Tools};
use serde::Serialize;
use std::path::Path;

/// What the status bar shows.
#[derive(Debug, Serialize)]
pub struct AppStatus {
    pub config_path: String,
    pub config_ok: bool,
    /// Load or validation error; `None` when `config_ok`.
    pub config_error: Option<String>,
    pub config_warnings: Vec<String>,
    pub tools: ToolsStatus,
    pub api_key_source: ApiKeySource,
    /// `[ai] backend` of the config (`api` when it cannot be read).
    pub ai_backend: String,
    /// Where the `claude` CLI is, for the `claude-code` backend.
    pub claude_path: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ToolsStatus {
    /// Every required tool was found.
    pub ok: bool,
    pub items: Vec<ToolInfo>,
    /// Probed only when every required tool is present.
    pub capabilities: Option<Capabilities>,
}

#[derive(Debug, Serialize)]
pub struct ToolInfo {
    pub name: String,
    pub required: bool,
    pub path: Option<String>,
    pub version: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Capabilities {
    /// ffmpeg has libsvtav1 (AV1 video).
    pub svtav1: bool,
    /// ffmpeg has libopus (Opus audio).
    pub opus: bool,
    /// ffmpeg has vidstab (stabilization).
    pub vidstab: bool,
}

/// Where the Anthropic API key comes from; the key itself is never sent to the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ApiKeySource {
    Env,
    File,
    None,
}

/// Tools gallery drives; `optipng` is only needed by PNG presets.
const TOOLS: [(&str, bool); 6] = [
    ("ffmpeg", true),
    ("ffprobe", true),
    ("exiftool", true),
    ("avifenc", true),
    ("magick", true),
    ("optipng", false),
];

pub fn app_status(config_path: &Path) -> AppStatus {
    let (config_ok, config_error, config_warnings) = config_health(config_path);
    let ai_backend = Config::load(config_path)
        .map(|c| c.ai.backend)
        .unwrap_or_else(|_| "api".into());
    AppStatus {
        ai_backend,
        claude_path: gallery_core::ai::claude_code::find().map(|p| p.display().to_string()),
        config_path: config_path.display().to_string(),
        config_ok,
        config_error,
        config_warnings,
        tools: tools_status(),
        api_key_source: api_key_source(
            |k| std::env::var(k).ok(),
            key::default_key_path().as_deref(),
        ),
    }
}

/// (ok, error, warnings) of loading and validating the config.
pub fn config_health(path: &Path) -> (bool, Option<String>, Vec<String>) {
    match Config::load(path).and_then(|c| c.validate().map(|()| c)) {
        Ok(c) => (true, None, c.warnings()),
        Err(e) => (false, Some(e.to_string()), Vec::new()),
    }
}

fn tools_status() -> ToolsStatus {
    let mut items: Vec<ToolInfo> = TOOLS
        .iter()
        .map(|&(name, required)| ToolInfo {
            name: name.into(),
            required,
            path: tools::find(name).map(|p| p.display().to_string()),
            version: None,
        })
        .collect();
    let ok = items.iter().all(|t| !t.required || t.path.is_some());
    let capabilities = if ok {
        Tools::discover().ok().map(|t| {
            let caps = t.capabilities();
            for item in &mut items {
                item.version = caps.versions.get(&item.name).cloned();
            }
            Capabilities {
                svtav1: caps.svtav1,
                opus: caps.opus,
                vidstab: caps.vidstab,
            }
        })
    } else {
        None
    };
    ToolsStatus {
        ok,
        items,
        capabilities,
    }
}

/// `ANTHROPIC_API_KEY`, then `CLAUDE_API_KEY`, then the private key file,
/// exactly as the core's client looks the key up.
pub fn api_key_source(
    env: impl Fn(&str) -> Option<String>,
    key_file: Option<&Path>,
) -> ApiKeySource {
    match key::lookup_in(env, key_file).source {
        KeySource::Env => ApiKeySource::Env,
        KeySource::File => ApiKeySource::File,
        KeySource::None => ApiKeySource::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn api_key_source_reads_env_names_then_the_key_file() {
        let only = |name: &'static str, val: &'static str| {
            move |k: &str| (k == name).then(|| val.to_string())
        };
        assert_eq!(api_key_source(|_| None, None), ApiKeySource::None);
        assert_eq!(
            api_key_source(only("ANTHROPIC_API_KEY", "x"), None),
            ApiKeySource::Env
        );
        assert_eq!(
            api_key_source(only("CLAUDE_API_KEY", "x"), None),
            ApiKeySource::Env
        );
        assert_eq!(
            api_key_source(only("CLAUDE_API_KEY", " "), None),
            ApiKeySource::None
        );
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("api-key");
        std::fs::write(&file, "sk-file\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert_eq!(api_key_source(|_| None, Some(&file)), ApiKeySource::File);
        assert_eq!(
            api_key_source(only("ANTHROPIC_API_KEY", "x"), Some(&file)),
            ApiKeySource::Env
        );
        assert_eq!(
            serde_json::to_value(ApiKeySource::Env).unwrap(),
            json!("env")
        );
        assert_eq!(
            serde_json::to_value(ApiKeySource::None).unwrap(),
            json!("none")
        );
        assert_eq!(
            serde_json::to_value(ApiKeySource::File).unwrap(),
            json!("file")
        );
    }

    #[test]
    fn status_payload_shape() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.toml");
        let s = app_status(&missing);
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["config_path"], json!(missing.display().to_string()));
        assert_eq!(v["config_ok"], json!(false));
        assert!(v["config_error"].is_string());
        assert_eq!(v["config_warnings"], json!([]));
        assert!(v["api_key_source"].is_string());
        let tools = v["tools"]["items"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(
            names,
            [
                "ffmpeg", "ffprobe", "exiftool", "avifenc", "magick", "optipng"
            ]
        );
        for t in tools {
            assert!(t["required"].is_boolean());
            assert!(t["path"].is_string() || t["path"].is_null());
            assert!(t["version"].is_string() || t["version"].is_null());
        }
        assert!(v["tools"]["ok"].is_boolean());
        let caps = &v["tools"]["capabilities"];
        assert!(caps.is_null() || (caps["svtav1"].is_boolean() && caps["vidstab"].is_boolean()));
    }

    #[test]
    fn config_health_ok_and_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good.toml");
        std::fs::write(&good, "[[sources]]\nid = \"a\"\npath = \"/tmp/a\"\n").unwrap();
        assert_eq!(config_health(&good), (true, None, vec![]));

        let bad = dir.path().join("bad.toml");
        std::fs::write(
            &bad,
            "[[targets.ai]]\nname = \"x\"\nsources = [\"nope\"]\ndescription = \"d\"\n",
        )
        .unwrap();
        let (ok, err, _) = config_health(&bad);
        assert!(!ok);
        assert!(err.unwrap().contains("nope"));
    }
}
