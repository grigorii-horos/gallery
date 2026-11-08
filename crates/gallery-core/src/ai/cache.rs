//! Append-only jsonl cache of model answers.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::Result;
use crate::scan::FileEntry;

/// The model's structured answer as stored (category is the raw enum value).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Stored {
    pub category: String,
    pub reason: String,
    pub confidence: String,
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
            .append(true)
            .open(&self.path)?;
        writeln!(f, "{line}")?;
        f.flush()?;
        self.map.insert(key, result);
        Ok(())
    }
}
