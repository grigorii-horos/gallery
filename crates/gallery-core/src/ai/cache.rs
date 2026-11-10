//! Append-only jsonl cache of model answers.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::Result;
use crate::scan::FileEntry;

/// Version of the answer format. 2: `tags` and a `description` are required
/// of the model. A cached answer with a lower `meta` (0: written before) is
/// not used: the file is asked again once.
pub const META_VERSION: u32 = 2;

/// The model's structured answer as stored (category is the raw enum value).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Stored {
    pub category: String,
    pub reason: String,
    pub confidence: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Answer-format version; rows of older builds have none (0).
    #[serde(default)]
    pub meta: u32,
}

impl Stored {
    /// Whether the answer was made for the current format ([`META_VERSION`]).
    pub fn is_current(&self) -> bool {
        self.meta >= META_VERSION
    }
}

#[derive(Serialize, Deserialize)]
struct Row {
    key: String,
    result: Stored,
}

/// First 12 hex chars of sha256(system prompt).
pub fn prompt_hash(system: &str) -> String {
    let digest = Sha256::digest(system.as_bytes());
    digest.iter().take(6).map(|b| format!("{b:02x}")).collect()
}

/// `canonical path|size|mtime_ns|model|prompt hash`.
pub fn key(entry: &FileEntry, model: &str, prompt_hash: &str) -> String {
    let path = std::fs::canonicalize(&entry.path).unwrap_or_else(|_| entry.path.clone());
    format!(
        "{}|{}|{}|{model}|{prompt_hash}",
        path.display(),
        entry.size,
        entry.mtime_ns
    )
}

pub struct Cache {
    path: PathBuf,
    map: HashMap<String, Stored>,
}

impl Cache {
    /// Load the cache; a missing file is empty and broken lines are skipped.
    pub fn load(path: &Path) -> Cache {
        let mut map = HashMap::new();
        if let Ok(text) = std::fs::read_to_string(path) {
            for row in text
                .lines()
                .filter_map(|l| serde_json::from_str::<Row>(l).ok())
            {
                map.insert(row.key, row.result);
            }
        }
        Cache {
            path: path.to_path_buf(),
            map,
        }
    }

    pub fn get(&self, key: &str) -> Option<&Stored> {
        self.map.get(key)
    }

    /// Remember `result` and append it to the file right away.
    pub fn put(&mut self, key: String, result: Stored) -> Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let line = serde_json::to_string(&Row {
            key: key.clone(),
            result: result.clone(),
        })
        .expect("row serializes");
        let mut f = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&self.path)?;
        // A run that died in the middle of a line left it without its line
        // break: start on a fresh line so this answer is not glued to it.
        let mut row = String::new();
        if f.metadata()?.len() > 0 {
            let mut last = [0u8; 1];
            f.seek(SeekFrom::End(-1))?;
            f.read_exact(&mut last)?;
            if last[0] != b'\n' {
                row.push('\n');
            }
        }
        row.push_str(&line);
        row.push('\n');
        f.write_all(row.as_bytes())?;
        // Paid for: on disk before the next request, even if the power goes.
        f.sync_data()?;
        self.map.insert(key, result);
        Ok(())
    }
}

/// Move the answers in `old` (a store left at an earlier location) into
/// `new` and remove `old`. Answers already in `new` win over those from
/// `old`; nothing to do when `old` is missing.
pub fn adopt(old: &Path, new: &Path) -> std::io::Result<()> {
    if !old.exists() || old == new {
        return Ok(());
    }
    if let Some(dir) = new.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if !new.exists() && std::fs::rename(old, new).is_ok() {
        return Ok(());
    }
    // Another file system, or both exist: `load` lets later lines win, so
    // the old answers go first.
    let mut text = std::fs::read(old)?;
    if !text.is_empty() && !text.ends_with(b"\n") {
        text.push(b'\n');
    }
    if let Ok(cur) = std::fs::read(new) {
        text.extend_from_slice(&cur);
    }
    crate::paths::write_atomic(new, &text)?;
    std::fs::remove_file(old)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adopt_moves_old_answers_and_keeps_newer_ones() {
        let d = tempfile::tempdir().unwrap();
        let old = d.path().join("cache/ai-cache.jsonl");
        let new = d.path().join("data/ai-answers.jsonl");
        // Nothing there: nothing happens.
        adopt(&old, &new).unwrap();
        assert!(!new.exists());

        std::fs::create_dir_all(old.parent().unwrap()).unwrap();
        let mut c = Cache::load(&old);
        c.put("a".into(), stored("x")).unwrap();
        c.put("b".into(), stored("x")).unwrap();
        adopt(&old, &new).unwrap();
        assert!(!old.exists());
        assert_eq!(Cache::load(&new).get("a"), Some(&stored("x")));

        // Both exist: the old rows join, the new store's own answer wins.
        let mut c = Cache::load(&old);
        c.put("a".into(), stored("old")).unwrap();
        c.put("c".into(), stored("old")).unwrap();
        let mut n = Cache::load(&new);
        n.put("a".into(), stored("new")).unwrap();
        adopt(&old, &new).unwrap();
        assert!(!old.exists());
        let n = Cache::load(&new);
        assert_eq!(n.get("a"), Some(&stored("new")));
        assert_eq!(n.get("b"), Some(&stored("x")));
        assert_eq!(n.get("c"), Some(&stored("old")));
    }

    fn stored(c: &str) -> Stored {
        Stored {
            category: c.into(),
            reason: String::new(),
            confidence: "ok".into(),
            tags: Vec::new(),
            description: String::new(),
            meta: META_VERSION,
        }
    }

    #[test]
    fn an_answer_after_a_torn_line_is_kept() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("ai-cache.jsonl");
        let mut c = Cache::load(&path);
        c.put("a".into(), stored("A")).unwrap();
        // A run died while writing the next row.
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(br#"{"key":"b","res"#).unwrap();
        drop(f);
        let mut c = Cache::load(&path);
        c.put("c".into(), stored("C")).unwrap();
        let c = Cache::load(&path);
        assert_eq!(c.get("a").unwrap().category, "A");
        assert_eq!(c.get("c").unwrap().category, "C");
        assert!(c.get("b").is_none());
    }
}
