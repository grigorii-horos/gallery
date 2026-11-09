//! XDG desktop files for the GUI: the app-menu entry (`gallery install-desktop`)
//! and the autostart entry (the GUI's "Launch at login"). Every function
//! takes its base directory, so tests write to a temp dir; nothing here runs
//! by itself.

use std::path::{Path, PathBuf};

use crate::Result;

/// File name of both entries.
pub const FILE_NAME: &str = "gallery.desktop";
/// Icon name the entry refers to (installed into the hicolor theme).
pub const ICON_NAME: &str = "gallery";
/// Argument of the autostart entry: start in the tray, window hidden.
pub const HIDDEN_ARG: &str = "--hidden";

/// Whether the app-menu and autostart entries exist on this OS: they are XDG
/// desktop files, Linux only. (macOS LaunchAgents and the Windows Run key are
/// left to the operating system's own settings.)
pub const SUPPORTED: bool = cfg!(target_os = "linux");

/// The error for asking for either entry where they are not supported.
pub fn linux_only() -> crate::Error {
    crate::Error::Config(crate::t!("desktop.linux_only"))
}

/// The `Exec` value for `exe`: quoted as the desktop entry spec asks
/// (spaces, quotes, `$`, backticks, backslashes, and `%` doubled).
fn exec_quote(exe: &Path) -> String {
    let s = exe.display().to_string();
    let needs_quote = s.chars().any(|c| " \t\n\"'\\><~|&;$*?#()`%".contains(c));
    if !needs_quote {
        return s;
    }
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' | '`' | '$' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            '%' => out.push_str("%%"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Text of an entry that starts `exe` with `args`.
pub fn entry_text(exe: &Path, args: &[&str], autostart: bool) -> String {
    let mut exec = exec_quote(exe);
    for a in args {
        exec.push(' ');
        exec.push_str(a);
    }
    let mut text = format!(
        "[Desktop Entry]\nType=Application\nName=Gallery\n\
         Comment=Media organizer: scan, plan, apply\n\
         Exec={exec}\nIcon={ICON_NAME}\nTerminal=false\nCategories=Graphics;Utility;\n\
         StartupWMClass=gallery-gui\n"
    );
    if autostart {
        text.push_str("X-GNOME-Autostart-enabled=true\n");
    }
    text
}

/// `<data_home>/applications/gallery.desktop`.
pub fn menu_entry_path(data_home: &Path) -> PathBuf {
    data_home.join("applications").join(FILE_NAME)
}

/// `<data_home>/icons/hicolor/128x128/apps/gallery.png`.
pub fn icon_path(data_home: &Path) -> PathBuf {
    data_home
        .join("icons/hicolor/128x128/apps")
        .join(format!("{ICON_NAME}.png"))
}

/// `<config_home>/autostart/gallery.desktop`.
pub fn autostart_path(config_home: &Path) -> PathBuf {
    config_home.join("autostart").join(FILE_NAME)
}

fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, bytes)?;
    Ok(())
}

/// Write the app-menu entry for `exe` and its icon under `data_home`;
/// the files written.
pub fn install_menu_entry(data_home: &Path, exe: &Path, icon_png: &[u8]) -> Result<Vec<PathBuf>> {
    let entry = menu_entry_path(data_home);
    let icon = icon_path(data_home);
    write(&icon, icon_png)?;
    write(&entry, entry_text(exe, &[], false).as_bytes())?;
    Ok(vec![entry, icon])
}

/// Turn start at login on (an entry for `exe --hidden`) or off (the entry
/// removed) under `config_home`.
pub fn set_autostart(config_home: &Path, exe: &Path, enabled: bool) -> Result<()> {
    let path = autostart_path(config_home);
    if enabled {
        write(&path, entry_text(exe, &[HIDDEN_ARG], true).as_bytes())
    } else {
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }
}

/// Whether the autostart entry exists under `config_home`.
pub fn autostart_enabled(config_home: &Path) -> bool {
    autostart_path(config_home).is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_entry_points_at_the_binary_and_installs_the_icon() {
        let d = tempfile::tempdir().unwrap();
        let exe = Path::new("/opt/gallery/gallery-gui");
        let files = install_menu_entry(d.path(), exe, b"PNG").unwrap();
        assert_eq!(files.len(), 2);
        let text = std::fs::read_to_string(menu_entry_path(d.path())).unwrap();
        assert!(text.starts_with("[Desktop Entry]\n"));
        assert!(text.contains("\nExec=/opt/gallery/gallery-gui\n"));
        assert!(text.contains("\nIcon=gallery\n"));
        assert!(text.contains("\nType=Application\n"));
        assert!(!text.contains("--hidden"));
        assert_eq!(std::fs::read(icon_path(d.path())).unwrap(), b"PNG");
    }

    #[test]
    fn exec_with_spaces_and_specials_is_quoted() {
        let t = entry_text(Path::new("/home/a b/$x/gui"), &[], false);
        assert!(t.contains("\nExec=\"/home/a b/\\$x/gui\"\n"), "{t}");
        let t = entry_text(Path::new("/p/100%/gui"), &[], false);
        assert!(t.contains("Exec=\"/p/100%%/gui\""), "{t}");
    }

    #[test]
    fn autostart_is_written_only_on_request_and_removed_on_off() {
        let d = tempfile::tempdir().unwrap();
        let exe = Path::new("/opt/gallery/gallery-gui");
        assert!(!autostart_enabled(d.path()));
        set_autostart(d.path(), exe, false).unwrap(); // nothing to remove: fine
        assert!(!autostart_path(d.path()).exists());
        set_autostart(d.path(), exe, true).unwrap();
        assert!(autostart_enabled(d.path()));
        let text = std::fs::read_to_string(autostart_path(d.path())).unwrap();
        assert!(text.contains("\nExec=/opt/gallery/gallery-gui --hidden\n"));
        assert!(text.contains("X-GNOME-Autostart-enabled=true"));
        set_autostart(d.path(), exe, false).unwrap();
        assert!(!autostart_enabled(d.path()));
    }
}
