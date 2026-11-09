//! External tool discovery, capability probing and command construction.

use crate::{Error, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A command for an external tool: on unix in its own process group (Windows:
/// a new process group, below), so a
/// terminal Ctrl-C (sent to the foreground group) reaches only gallery, which
/// cancels between files and lets the tools in work finish; stdin is
/// `/dev/null`, since a background group must never read the terminal.
fn tool_command(program: &Path) -> Command {
    let mut cmd = Command::new(program);
    cmd.stdin(Stdio::null());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    // Windows: no console window flashing up from the GUI, a separate group
    // (Ctrl-C reaches only gallery) and idle priority (the `nice` stand-in).
    #[cfg(windows)]
    {
        const IDLE_PRIORITY_CLASS: u32 = 0x0000_0040;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        std::os::windows::process::CommandExt::creation_flags(
            &mut cmd,
            IDLE_PRIORITY_CLASS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW,
        );
    }
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
    /// IANA zone for local wall-clock times; None: this machine's zone.
    /// Every conversion is done here (chrono-tz) and handed to the tools as
    /// explicit offsets or UTC, so no OS-specific `TZ` handling is needed;
    /// on unix it is also exported as `TZ` to the children.
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

/// Search directories in priority order: `GALLERY_TOOLS_DIR`, a `tools`
/// folder next to the app executable (tools bundled with the app), then
/// `PATH`.
fn search_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(d) = std::env::var_os("GALLERY_TOOLS_DIR") {
        dirs.push(PathBuf::from(d));
    }
    if let Some(d) = bundled_dir() {
        dirs.push(d);
    }
    if let Some(p) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&p));
    }
    dirs
}

/// `<folder of the running executable>/tools`.
fn bundled_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join("tools"))
}

/// How to install `tool` on this OS (a command line to run), for `gallery
/// check` when it is missing.
pub fn install_hint(tool: &str) -> Option<String> {
    let cmd = if cfg!(windows) {
        match tool {
            "ffmpeg" | "ffprobe" => "winget install Gyan.FFmpeg",
            "exiftool" => "winget install OliverBetz.ExifTool",
            "magick" => "winget install ImageMagick.ImageMagick",
            "avifenc" => "scoop install libavif",
            "optipng" => "scoop install optipng",
            _ => return None,
        }
    } else if cfg!(target_os = "macos") {
        match tool {
            "ffmpeg" | "ffprobe" => "brew install ffmpeg",
            "exiftool" => "brew install exiftool",
            "magick" => "brew install imagemagick",
            "avifenc" => "brew install libavif",
            "optipng" => "brew install optipng",
            _ => return None,
        }
    } else {
        match tool {
            "ffmpeg" | "ffprobe" => "sudo apt install ffmpeg",
            "exiftool" => "sudo apt install libimage-exiftool-perl",
            "magick" => "sudo apt install imagemagick",
            "avifenc" => "sudo apt install libavif-bin",
            "optipng" => "sudo apt install optipng",
            _ => return None,
        }
    };
    Some(cmd.to_string())
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
        let need =
            |n: &str| find(n).ok_or_else(|| Error::Tool(crate::t!("tools.not_found", name = n)));
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

    /// Command for `tool`, run at lowest CPU and IO priority (nice/ionice on
    /// unix when available, the idle priority class on Windows), with `TZ`
    /// set to `self.tz` on unix. Windows does not understand IANA `TZ`
    /// names, so nothing may depend on it: dates cross as explicit values.
    pub fn command(&self, tool: &Path) -> Command {
        #[cfg_attr(not(unix), allow(unused_mut))]
        let mut cmd = self.bare_command(tool);
        #[cfg(unix)]
        if let Some(tz) = &self.tz {
            cmd.env("TZ", tz);
        }
        cmd
    }

    /// A UTC time as a wall-clock time with offset in the tools' zone (this
    /// machine's without one).
    pub fn utc_to_local(
        &self,
        utc: chrono::NaiveDateTime,
    ) -> Option<chrono::DateTime<chrono::FixedOffset>> {
        use chrono::TimeZone;
        match self.tz.as_deref().map(str::parse::<chrono_tz::Tz>) {
            Some(Ok(tz)) => Some(tz.from_utc_datetime(&utc).fixed_offset()),
            Some(Err(_)) => None,
            None => Some(chrono::Local.from_utc_datetime(&utc).fixed_offset()),
        }
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

#[cfg(test)]
mod hint_tests {
    use super::*;

    #[test]
    fn every_tool_has_an_install_hint() {
        for t in [
            "ffmpeg", "ffprobe", "exiftool", "avifenc", "magick", "optipng",
        ] {
            assert!(install_hint(t).is_some(), "{t}");
        }
        assert_eq!(install_hint("nope"), None);
    }

    #[test]
    fn bundled_tools_dir_sits_next_to_the_executable() {
        let d = bundled_dir().unwrap();
        assert_eq!(d.file_name().unwrap(), "tools");
        assert_eq!(d.parent(), std::env::current_exe().unwrap().parent());
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
