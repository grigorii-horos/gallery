//! "Why this file?": the core's explanation for the Plan screen.

use std::path::Path;

use gallery_core::config::Config;
use gallery_core::explain::{ExplainOptions, Explanation, explain};
use gallery_core::overrides::Overrides;
use gallery_core::tools::Tools;

/// Explain `file` under the config at `config`, with the decisions saved in
/// `overrides_file` (a missing file is no decisions). Read-only.
pub fn explain_in(
    config: &Path,
    overrides_file: &Path,
    file: &Path,
    tools: Option<&Tools>,
    examples: Option<&Path>,
) -> Result<Explanation, String> {
    let mut cfg = Config::load(config).map_err(|e| e.to_string())?;
    cfg.validate().map_err(|e| e.to_string())?;
    if let Some(f) = examples {
        cfg.attach_examples(&gallery_core::learn::Store::new(f));
    }
    let ov = Overrides::load(overrides_file).map_err(|e| e.to_string())?;
    let opts = ExplainOptions {
        tools,
        ai_cache: None,
    };
    explain(&cfg, file, &ov, &opts).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gallery_core::explain::{DecisionKind, Scope};

    #[test]
    fn explains_under_the_given_config_without_writing() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("src");
        std::fs::create_dir(&src).unwrap();
        std::fs::write(src.join("clip.mp4"), b"x").unwrap();
        let cfg = d.path().join("g.toml");
        std::fs::write(
            &cfg,
            format!(
                "[[sources]]\nid = \"s\"\npath = \"{r}\"\n\n[[targets.before_ai]]\nname = \"pics\"\n\
                 [[targets.before_ai.filters]]\nsources = [\"s\"]\nmatch = \"*.jpg\"\n\
                 [[targets.before_ai.outputs]]\ndir = \"{r}/out\"\n",
                r = src.display()
            ),
        )
        .unwrap();
        let ov = d.path().join("overrides.json");
        let e = explain_in(&cfg, &ov, &src.join("clip.mp4"), None, None).unwrap();
        assert!(matches!(e.scope, Scope::Scanned { .. }));
        assert_eq!(e.decision.unwrap().kind, DecisionKind::Unmatched);
        assert!(e.verdict.contains("is not in the match"), "{}", e.verdict);
        assert!(!ov.exists());
        assert!(explain_in(&d.path().join("none.toml"), &ov, &src, None, None).is_err());
    }
}
