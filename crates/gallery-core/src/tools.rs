//! External tool discovery, capability probing and command construction.

use crate::{Error, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A command for an external tool: on unix in its own process group, so a
/// terminal Ctrl-C (sent to the foreground group) reaches only gallery, which
/// cancels between files and lets the tools in work finish; stdin is
/// `/dev/null`, since a background group must never read the terminal.
fn tool_command(program: &Path) -> Command {
    let mut cmd = Command::new(program);
    cmd.stdin(Stdio::null());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    cmd
}

/// Paths of the external tools gallery drives.
#[derive(Debug, Clone)]
pub struct Tools {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
    pub exiftool: PathBuf,
    pub avifenc: PathBuf,
    pub magick: PathBuf,
    pub optipng: Option<PathBuf>,
    /// IANA zone for local wall-clock times, passed as `TZ` to every child;
    /// None: this machine's zone.
    pub tz: Option<String>,
}

/// What the discovered tools can do, plus their version banners.
#[derive(Debug, Clone, Default)]
pub struct Capabilities {
    pub svtav1: bool,
    pub opus: bool,
    /// ffmpeg has the `vidstabdetect` filter (video stabilization).
    pub vidstab: bool,
    pub versions: BTreeMap<String, String>,
}

/// Search directories in priority order: `GALLERY_TOOLS_DIR`, then `PATH`.
fn search_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(d) = std::env::var_os("GALLERY_TOOLS_DIR") {
        dirs.push(PathBuf::from(d));
    }
    if let Some(p) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&p));
    }
    dirs
}

/// First directory (in order) holding `name` or `name` + the platform exe suffix.
fn find_in(name: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    let suffixed = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    dirs.iter()
        .flat_map(|d| [d.join(name), d.join(&suffixed)])
        .find(|p| p.is_file())
}

/// Find `name` in `GALLERY_TOOLS_DIR` first, then in `PATH`.
pub fn find(name: &str) -> Option<PathBuf> {
    find_in(name, &search_dirs())
}

impl Tools {
    pub fn discover() -> Result<Tools> {
        let need = |n: &str| find(n).ok_or_else(|| Error::Tool(format!("{n} not found")));
        Ok(Tools {
            ffmpeg: need("ffmpeg")?,
            ffprobe: need("ffprobe")?,
            exiftool: need("exiftool")?,
            avifenc: need("avifenc")?,
            magick: need("magick")?,
            optipng: find("optipng"),
            tz: None,
        })
    }

    pub fn capabilities(&self) -> Capabilities {
        let mut caps = Capabilities::default();
        if let Ok(out) = tool_command(&self.ffmpeg)
            .args(["-hide_banner", "-encoders"])
            .output()
        {
            let text = String::from_utf8_lossy(&out.stdout);
            caps.svtav1 = text.contains("libsvtav1");
            caps.opus = text.contains("libopus");
        }
        if let Ok(out) = tool_command(&self.ffmpeg)
            .args(["-hide_banner", "-filters"])
            .output()
        {
            caps.vidstab = String::from_utf8_lossy(&out.stdout).contains("vidstabdetect");
        }
        let mut probe = |name: &str, path: &Path, arg: &str| {
            if let Ok(out) = tool_command(path).arg(arg).output() {
                let text = String::from_utf8_lossy(&out.stdout);
                let line = text.lines().next().unwrap_or("").trim();
                if !line.is_empty() {
                    caps.versions.insert(name.to_string(), line.to_string());
                }
            }
        };
        probe("ffmpeg", &self.ffmpeg, "-version");
        probe("ffprobe", &self.ffprobe, "-version");
        probe("exiftool", &self.exiftool, "-ver");
        probe("avifenc", &self.avifenc, "--version");
        probe("magick", &self.magick, "-version");
        if let Some(o) = &self.optipng {
            probe("optipng", o, "-version");
        }
        caps
    }

    /// Command for `tool`, run at lowest CPU and IO priority on unix when
    /// `nice`/`ionice` are available, with `TZ` set to `self.tz` if any.
    pub fn command(&self, tool: &Path) -> Command {
        let mut cmd = self.bare_command(tool);
        if let Some(tz) = &self.tz {
            cmd.env("TZ", tz);
        }
        cmd
    }

    fn bare_command(&self, tool: &Path) -> Command {
        let mut prefix: Vec<(PathBuf, &[&str])> = Vec::new();
        if cfg!(unix) {
            if let Some(p) = find("ionice") {
                prefix.push((p, &["-c3"]));
            }
            if let Some(p) = find("nice") {
                prefix.push((p, &["-n", "19"]));
            }
        }
        let mut parts = prefix.into_iter();
        match parts.next() {
            None => tool_command(tool),
            Some((first, first_args)) => {
                let mut cmd = tool_command(&first);
                cmd.args(first_args);
                for (p, a) in parts {
                    cmd.arg(p).args(a);
                }
                cmd.arg(tool);
                cmd
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn fake_exe(dir: &Path, name: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[test]
    fn tools_dir_is_preferred_over_path() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let want = fake_exe(first.path(), "faketool");
        fake_exe(second.path(), "faketool");
        let dirs = [first.path().to_path_buf(), second.path().to_path_buf()];
        assert_eq!(find_in("faketool", &dirs), Some(want));
        assert_eq!(find_in("missing", &dirs), None);
    }

    /// Ctrl-C in a terminal goes to the foreground process group: tools run
    /// in their own group (so a cancelled run lets them finish) and never
    /// read the terminal.
    #[cfg(target_os = "linux")]
    #[test]
    fn tools_run_in_their_own_process_group_without_stdin() {
        let sleep = find("sleep").expect("sleep");
        let tools = Tools {
            ffmpeg: sleep.clone(),
            ffprobe: sleep.clone(),
            exiftool: sleep.clone(),
            avifenc: sleep.clone(),
            magick: sleep.clone(),
            optipng: None,
            tz: None,
        };
        let mut child = tools.command(&sleep).arg("5").spawn().unwrap();
        let pid = child.id() as libc::pid_t;
        // SAFETY: plain syscalls on our own child's pid and on ourselves.
        let (theirs, ours) = unsafe { (libc::getpgid(pid), libc::getpgid(0)) };
        let stdin = std::fs::read_link(format!("/proc/{pid}/fd/0"));
        let _ = child.kill();
        let _ = child.wait();
        assert_ne!(theirs, ours, "child shares our process group");
        assert_eq!(theirs, pid, "child leads its own group");
        assert_eq!(stdin.unwrap(), Path::new("/dev/null"));
    }
}
