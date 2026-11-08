//! Lock files: an advisory `flock` on a persistent file. Holding the
//! `File` is holding the lock; the kernel releases it when the process dies
//! (Ctrl-C, crash, power loss), so there is no stale state to reclaim and no
//! reclaim race. The owner's pid is written into the file for messages only;
//! the file is never removed (unlinking would let two processes lock two
//! different inodes of the same path).

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};

use crate::{Error, Result};

/// Exclusive hold of a lock file; released when dropped, on every exit path.
#[derive(Debug)]
pub struct FileLock {
    path: PathBuf,
    _file: File,
}

impl FileLock {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Take the lock at `path`. `held` builds the error for a live holder
    /// from its pid text.
    pub fn acquire(path: &Path, held: impl Fn(&str, &Path) -> String) -> Result<FileLock> {
        let io = |e: std::io::Error| Error::Plan(format!("{}: {e}", path.display()));
        // No truncate: a refused attempt must leave the holder's pid readable.
        let mut f = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(io)?;
        // A thread of this very process may be between `fork` and `exec` of a
        // tool, its child holding a copy of a just-released lock for
        // microseconds; a short wait tells that apart from a real holder.
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(200);
        loop {
            match f.try_lock() {
                Ok(()) => break,
                Err(TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(TryLockError::WouldBlock) => {
                    let mut text = String::new();
                    let _ = f.read_to_string(&mut text);
                    return Err(Error::Plan(held(&text, path)));
                }
                Err(TryLockError::Error(e)) => return Err(io(e)),
            }
        }
        f.set_len(0).map_err(io)?;
        f.rewind().map_err(io)?;
        f.write_all(std::process::id().to_string().as_bytes())
            .map_err(io)?;
        Ok(FileLock {
            path: path.to_path_buf(),
            _file: f,
        })
    }
}

/// `<data dir>/apply.lock`: one apply/undo/retry at a time on the machine,
/// whichever program (GUI or CLI) starts it.
pub fn apply_lock_path() -> Option<PathBuf> {
    crate::paths::data_dir().map(|d| d.join("apply.lock"))
}

/// Take the global apply lock at `path` (creating its folder).
pub fn acquire_apply_at(path: &Path) -> Result<FileLock> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    FileLock::acquire(path, |pid, p| {
        format!(
            "Another Gallery run (GUI or CLI) is already applying or undoing a plan (pid {}). \
             Wait for it to finish. If there is no such process, delete {}",
            pid.trim(),
            p.display()
        )
    })
}

/// [`acquire_apply_at`] at the default place.
pub fn acquire_apply() -> Result<FileLock> {
    let p = apply_lock_path().ok_or_else(|| Error::Plan(crate::t!("lock.no_data_dir")))?;
    acquire_apply_at(&p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_holder_is_refused_until_the_first_drops() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("x/apply.lock");
        let a = acquire_apply_at(&p).unwrap();
        let e = acquire_apply_at(&p).unwrap_err().to_string();
        assert!(e.contains(&std::process::id().to_string()), "{e}");
        assert!(e.contains("Another Gallery run"), "{e}");
        drop(a);
        acquire_apply_at(&p).unwrap();
    }

    #[test]
    fn concurrent_acquire_from_two_threads_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("c.lock");
        let held = FileLock::acquire(&p, |_, _| "held".into()).unwrap();
        let p2 = p.clone();
        let r = std::thread::spawn(move || FileLock::acquire(&p2, |_, _| "held".into()).is_err())
            .join()
            .unwrap();
        assert!(r, "separate handle must be refused while held");
        drop(held);
        FileLock::acquire(&p, |_, _| "held".into()).unwrap();
    }

    #[test]
    fn leftover_file_with_dead_pid_is_just_taken() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("apply.lock");
        std::fs::write(&p, "4194999").unwrap();
        let l = acquire_apply_at(&p).unwrap();
        assert_eq!(
            std::fs::read_to_string(l.path()).unwrap(),
            std::process::id().to_string()
        );
    }

    #[test]
    fn garbage_content_does_not_block() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("apply.lock");
        std::fs::write(&p, "not a pid, longer than ours").unwrap();
        let l = acquire_apply_at(&p).unwrap();
        assert_eq!(
            std::fs::read_to_string(l.path()).unwrap(),
            std::process::id().to_string()
        );
    }

    #[cfg(unix)]
    #[test]
    fn lock_is_released_when_the_holder_process_is_killed() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("k.lock");
        // The child takes an flock on the file via the shell and sleeps.
        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg("exec 9>>\"$0\"; flock -n 9 || exit 3; echo ready; exec sleep 60")
            .arg(&p)
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut out = std::io::BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        std::io::BufRead::read_line(&mut out, &mut line).unwrap();
        assert_eq!(
            line.trim(),
            "ready",
            "child failed to lock (needs flock(1))"
        );
        assert!(FileLock::acquire(&p, |_, _| "held".into()).is_err());
        child.kill().unwrap();
        child.wait().unwrap();
        FileLock::acquire(&p, |_, _| "held".into()).expect("freed by the kernel on death");
    }
}
