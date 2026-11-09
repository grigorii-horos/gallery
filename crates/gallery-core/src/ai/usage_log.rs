//! Append-only jsonl log of what each AI scan used: the estimate before a
//! scan takes the average request cost from it.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::Usage;
use crate::Result;

/// One scan's usage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageRow {
    pub model: String,
    pub requests: u64,
    pub input: u64,
    pub output: u64,
    pub cache_write: u64,
    pub cache_read: u64,
}

impl UsageRow {
    pub fn new(model: &str, u: Usage) -> Self {
        UsageRow {
            model: model.to_string(),
            requests: u.requests,
            input: u.input,
            output: u.output,
            cache_write: u.cache_write,
            cache_read: u.cache_read,
        }
    }

    pub fn usage(&self) -> Usage {
        Usage {
            requests: self.requests,
            input: self.input,
            output: self.output,
            cache_write: self.cache_write,
            cache_read: self.cache_read,
        }
    }
}

/// `<user cache dir>/gallery/ai-usage.jsonl`.
pub fn default_path() -> Option<PathBuf> {
    crate::paths::cache_dir().map(|d| d.join("ai-usage.jsonl"))
}

/// Append a row; a scan that made no request leaves no row.
pub fn append(path: &Path, row: &UsageRow) -> Result<()> {
    if row.requests == 0 {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let line = serde_json::to_string(row).expect("row serializes");
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(f, "{line}")?;
    Ok(())
}

/// All rows; a missing file is empty and broken lines are skipped.
pub fn load(path: &Path) -> Vec<UsageRow> {
    std::fs::read_to_string(path)
        .map(|t| {
            t.lines()
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_then_load_skips_empty_scans_and_broken_lines() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("x/usage.jsonl");
        assert!(load(&p).is_empty());
        let u = Usage {
            requests: 2,
            input: 10,
            output: 4,
            cache_write: 1,
            cache_read: 6,
        };
        append(&p, &UsageRow::new("m", u)).unwrap();
        append(&p, &UsageRow::new("m", Usage::default())).unwrap();
        std::fs::write(
            &p,
            format!("{}garbage\n", std::fs::read_to_string(&p).unwrap()),
        )
        .unwrap();
        let rows = load(&p);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].usage(), u);
    }
}
