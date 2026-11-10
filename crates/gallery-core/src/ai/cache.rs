//! Append-only jsonl cache of model answers.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::Result;
use crate::scan::FileEntry;

/// The model's structured answer as stored (category is the raw enum value).
/// `tags` and `description` are absent in answers cached before they were
/// asked for: such an answer is used as is, without them.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Stored {
    pub category: String,
    pub reason: String,
    pub confidence: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn stored(c: &str) -> Stored {
        Stored {
            category: c.into(),
            reason: String::new(),
            confidence: "ok".into(),
            tags: Vec::new(),
            description: String::new(),
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
