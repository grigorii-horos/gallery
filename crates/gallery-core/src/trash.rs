//! Moving sources to the trash: the system trash, or a `.gallery-trash`
//! folder in the source root when the system trash is unavailable.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

/// Where a source went; undo restores it from there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum TrashRef {
    /// In the system trash; `original` is the path it was trashed from.
    System { original: PathBuf },
    /// In the fallback folder, at `path`.
    Folder { path: PathBuf },
}

/// Which trash `apply` uses.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TrashMode {
    /// System trash, falling back to the folder on error.
    #[default]
    System,
    /// Always the `.gallery-trash` folder.
    Folder,
}

impl TrashMode {
    /// `GALLERY_TRASH=folder` selects the folder; anything else the system trash.
    pub fn from_env() -> TrashMode {
        match std::env::var("GALLERY_TRASH") {
            Ok(v) if v == "folder" => TrashMode::Folder,
            _ => TrashMode::System,
        }
    }
}

/// Move `path` to the system trash; on error, to the fallback folder.
pub fn move_to_trash(path: &Path, source_root: &Path, run_id: &str) -> Result<TrashRef> {
    match trash::delete(path) {
        Ok(()) if std::fs::symlink_metadata(path).is_err() => Ok(TrashRef::System {
            original: path.to_path_buf(),
        }),
        // Reported success but the file is still there, or a plain error:
        // the folder fallback only moves a file that is still in place.
        _ => move_to_folder(path, source_root, run_id),
    }
}

/// Move `path` to `<source_root>/.gallery-trash/<run_id>/<path relative to the
/// root>`. A path outside the root (or an unusable root) keeps only its file
/// name, under its own directory's `.gallery-trash`. Never overwrites.
pub fn move_to_folder(path: &Path, source_root: &Path, run_id: &str) -> Result<TrashRef> {
    let rel = path
        .strip_prefix(source_root)
        .ok()
        .filter(|r| source_root.is_absolute() && r.file_name().is_some());
    let (root, rel) = match rel {
        Some(r) => (source_root, r.to_path_buf()),
        None => {
            let parent = path.parent().unwrap_or(Path::new("."));
            let name = path
                .file_name()
                .ok_or_else(|| Error::Plan(format!("no file name: {}", path.display())))?;
            (parent, PathBuf::from(name))
        }
    };
    let target = root.join(".gallery-trash").join(run_id).join(rel);
    if std::fs::symlink_metadata(&target).is_ok() {
        return Err(Error::Plan(format!(
            "trash: already exists: {}",
            target.display()
        )));
    }
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // A rename, never a copy: the source either moves whole or stays.
    std::fs::rename(path, &target)?;
    Ok(TrashRef::Folder { path: target })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_keeps_relative_path_and_never_overwrites() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("src");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        let a = root.join("sub/a.jpg");
        std::fs::write(&a, b"a").unwrap();
        let r = move_to_folder(&a, &root, "20261104-120000").unwrap();
        let want = root.join(".gallery-trash/20261104-120000/sub/a.jpg");
        assert_eq!(r, TrashRef::Folder { path: want.clone() });
        assert_eq!(std::fs::read(&want).unwrap(), b"a");
        assert!(!a.exists());

        std::fs::write(&a, b"second").unwrap();
        assert!(move_to_folder(&a, &root, "20261104-120000").is_err());
        assert_eq!(std::fs::read(&a).unwrap(), b"second");
        assert_eq!(std::fs::read(&want).unwrap(), b"a");
    }

    #[test]
    fn folder_outside_root_uses_own_dir() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("x/a.jpg");
        std::fs::create_dir_all(a.parent().unwrap()).unwrap();
        std::fs::write(&a, b"a").unwrap();
        let r = move_to_folder(&a, Path::new(""), "r").unwrap();
        assert_eq!(
            r,
            TrashRef::Folder {
                path: d.path().join("x/.gallery-trash/r/a.jpg")
            }
        );
    }

    #[test]
    fn missing_file_is_an_error() {
        let d = tempfile::tempdir().unwrap();
        assert!(move_to_folder(&d.path().join("nope"), d.path(), "r").is_err());
    }
}
