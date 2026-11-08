//! Where the Anthropic API key comes from: the environment
//! (`ANTHROPIC_API_KEY`, then `CLAUDE_API_KEY`), else the key file
//! `<user config dir>/gallery/api-key`, which must be private (mode 0600 on
//! unix). The key itself is never printed: [`ApiKey`]'s `Debug` hides it and
//! warnings name only the file.

use std::path::{Path, PathBuf};

use serde::Serialize;

/// Environment variables holding the key, in lookup order.
pub const ENV_VARS: [&str; 2] = ["ANTHROPIC_API_KEY", "CLAUDE_API_KEY"];

/// Where the key was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum KeySource {
    Env,
    File,
    None,
}

/// The result of a key lookup.
#[derive(Clone)]
pub struct ApiKey {
    key: Option<String>,
    pub source: KeySource,
    /// Why the key file was ignored (unsafe permissions, unreadable).
    pub warning: Option<String>,
}

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiKey")
            .field("key", &self.key.as_ref().map(|_| "<hidden>"))
            .field("source", &self.source)
            .field("warning", &self.warning)
            .finish()
    }
}

impl ApiKey {
    pub fn key(&self) -> Option<&str> {
        self.key.as_deref()
    }

    pub fn into_key(self) -> Option<String> {
        self.key
    }
}

/// `<user config dir>/gallery/api-key`.
pub fn default_key_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("gallery").join("api-key"))
}

/// Look the key up in `env` (a variable reader), then in `file`.
pub fn lookup_in(env: impl Fn(&str) -> Option<String>, file: Option<&Path>) -> ApiKey {
    let from_env = ENV_VARS
        .iter()
        .filter_map(|v| env(v))
        .map(|k| k.trim().to_string())
        .find(|k| !k.is_empty());
    if let Some(key) = from_env {
        return ApiKey {
            key: Some(key),
            source: KeySource::Env,
            warning: None,
        };
    }
    let (key, warning) = match file {
        Some(p) => read_key_file(p),
        None => (None, None),
    };
    ApiKey {
        source: if key.is_some() {
            KeySource::File
        } else {
            KeySource::None
        },
        key,
        warning,
    }
}

/// (key, warning) from the key file; a missing file is neither. A symlink
/// is refused; the file is opened once and its type, permissions and
/// identity are checked on the open handle, then read from it, so it cannot
/// be swapped between the check and the read.
fn read_key_file(path: &Path) -> (Option<String>, Option<String>) {
    use std::io::Read;
    let ignored = |why: String| Some(format!("key file {} is not used: {why}", path.display()));
    let link = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (None, None),
        Err(e) => return (None, ignored(e.to_string())),
    };
    if link.file_type().is_symlink() {
        return (None, ignored("it is a symlink".into()));
    }
    // Before opening: opening a FIFO would block.
    if !link.is_file() {
        return (None, ignored("it is not a file".into()));
    }
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => return (None, ignored(e.to_string())),
    };
    let meta = match file.metadata() {
        Ok(m) => m,
        Err(e) => return (None, ignored(e.to_string())),
    };
    if !meta.is_file() {
        return (None, ignored("it is not a file".into()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        // The path was replaced (e.g. by a symlink) after the first look.
        if (meta.dev(), meta.ino()) != (link.dev(), link.ino()) {
            return (None, ignored("file was replaced while reading".into()));
        }
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return (
                None,
                ignored(format!(
                    "permissions {mode:o} expose it to others, 600 required (chmod 600)"
                )),
            );
        }
    }
    let mut text = String::new();
    match file.read_to_string(&mut text) {
        Ok(_) => {
            let key = text.trim();
            ((!key.is_empty()).then(|| key.to_string()), None)
        }
        Err(e) => (None, ignored(e.to_string())),
    }
}

/// Look the key up in the process environment, then the default key file.
pub fn lookup() -> ApiKey {
    lookup_in(|k| std::env::var(k).ok(), default_key_path().as_deref())
}

/// Where [`lookup`] finds the key.
pub fn api_key_source() -> KeySource {
    lookup().source
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn only(name: &'static str, val: &'static str) -> impl Fn(&str) -> Option<String> {
        move |k: &str| (k == name).then(|| val.to_string())
    }

    fn key_file(dir: &Path, text: &str, mode: u32) -> PathBuf {
        let p = dir.join("api-key");
        std::fs::write(&p, text).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        #[cfg(not(unix))]
        let _ = mode;
        p
    }

    #[test]
    fn env_in_order_then_none() {
        let k = lookup_in(only("ANTHROPIC_API_KEY", "a"), None);
        assert_eq!((k.key(), k.source), (Some("a"), KeySource::Env));
        let k = lookup_in(only("CLAUDE_API_KEY", " c\n"), None);
        assert_eq!((k.key(), k.source), (Some("c"), KeySource::Env));
        let both = |n: &str| Some(if n == "ANTHROPIC_API_KEY" { "a" } else { "c" }.to_string());
        assert_eq!(lookup_in(both, None).key(), Some("a"));
        // Blank values do not count.
        let k = lookup_in(only("ANTHROPIC_API_KEY", "  "), None);
        assert_eq!((k.key(), k.source), (None, KeySource::None));
        assert!(k.warning.is_none());
    }

    #[test]
    fn private_file_is_used_and_trimmed() {
        let dir = tempfile::tempdir().unwrap();
        let p = key_file(dir.path(), "  sk-file\n", 0o600);
        let k = lookup_in(no_env, Some(&p));
        assert_eq!((k.key(), k.source), (Some("sk-file"), KeySource::File));
        assert!(k.warning.is_none());
    }

    #[test]
    fn env_wins_over_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = key_file(dir.path(), "sk-file", 0o600);
        let k = lookup_in(only("CLAUDE_API_KEY", "sk-env"), Some(&p));
        assert_eq!((k.key(), k.source), (Some("sk-env"), KeySource::Env));
    }

    #[cfg(unix)]
    #[test]
    fn file_readable_by_others_is_ignored_with_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        for mode in [0o644, 0o640, 0o604, 0o660] {
            let p = key_file(dir.path(), "sk-secret", mode);
            let k = lookup_in(no_env, Some(&p));
            assert_eq!((k.key(), k.source), (None, KeySource::None), "{mode:o}");
            let w = k.warning.expect("a warning");
            assert!(w.contains("api-key"), "{w}");
            assert!(!w.contains("sk-secret"), "{w}");
        }
        // Read-only for the owner is private too.
        let p = key_file(dir.path(), "sk-secret", 0o400);
        assert_eq!(lookup_in(no_env, Some(&p)).source, KeySource::File);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_key_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let real = key_file(dir.path(), "sk-secret", 0o600);
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let k = lookup_in(no_env, Some(&link));
        assert_eq!((k.key(), k.source), (None, KeySource::None));
        let w = k.warning.expect("a warning");
        assert!(!w.contains("sk-secret"), "{w}");
        // A dangling link is refused too, not taken for a missing file.
        std::fs::remove_file(&real).unwrap();
        assert!(lookup_in(no_env, Some(&link)).warning.is_some());
    }

    #[test]
    fn missing_or_empty_file_is_no_key() {
        let dir = tempfile::tempdir().unwrap();
        let k = lookup_in(no_env, Some(&dir.path().join("nope")));
        assert_eq!((k.source, k.warning.is_none()), (KeySource::None, true));
        let p = key_file(dir.path(), " \n", 0o600);
        let k = lookup_in(no_env, Some(&p));
        assert_eq!((k.key(), k.source), (None, KeySource::None));
        assert_eq!(lookup_in(no_env, None).source, KeySource::None);
    }

    #[test]
    fn unreadable_file_is_ignored_with_a_warning() {
        // A directory in place of the file.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("api-key");
        std::fs::create_dir(&p).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let k = lookup_in(no_env, Some(&p));
        assert_eq!(k.source, KeySource::None);
        assert!(k.warning.is_some());
    }

    #[test]
    fn debug_hides_the_key() {
        let k = lookup_in(only("ANTHROPIC_API_KEY", "sk-very-secret"), None);
        let s = format!("{k:?}");
        assert!(!s.contains("sk-very-secret"), "{s}");
        assert!(s.contains("hidden"), "{s}");
    }

    #[test]
    fn source_serializes_lowercase() {
        assert_eq!(serde_json::to_value(KeySource::File).unwrap(), "file");
        assert_eq!(serde_json::to_value(KeySource::Env).unwrap(), "env");
        assert_eq!(serde_json::to_value(KeySource::None).unwrap(), "none");
    }

    #[test]
    fn default_key_path_is_under_config_dir() {
        if let Some(p) = default_key_path() {
            assert!(p.ends_with("gallery/api-key"));
        }
    }
}
