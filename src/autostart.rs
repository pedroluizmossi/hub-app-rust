//! Start with the system (in the tray only, with `--hidden`).
//!
//! The system itself keeps whether it is on, not the config: the file in
//! `~/.config/autostart/` on Linux (`linux.rs`, the XDG standard that KDE, GNOME, XFCE,
//! Cinnamon and MATE follow) and the value in `HKCU\…\CurrentVersion\Run` on Windows
//! (`windows.rs`). So turning it off with the system's tools also shows as off here.

#[cfg(target_os = "linux")]
#[path = "autostart/linux.rs"]
mod imp;
#[cfg(windows)]
#[path = "autostart/windows.rs"]
mod imp;

use std::path::PathBuf;

/// Is it on?
pub fn is_enabled() -> bool {
    imp::current().is_some()
}

/// Turns it on or off.
pub fn set(on: bool) -> Result<(), String> {
    let r = if on { imp::enable(&program()?) } else { imp::disable() };
    match &r {
        Ok(()) => log::info!("start with the system: {}", if on { "on" } else { "off" }),
        Err(e) => log::warn!("start with the system: {e}"),
    }
    r
}

/// On, but pointing to another executable (the app moved or was replaced by a new version
/// elsewhere)? Rewrites it with the current one. Runs when the app opens. An entry the user
/// edited (an `env VAR=1` in front, other options) is left alone while it still runs this
/// executable.
pub fn refresh() {
    let Some(old) = imp::current() else { return };
    let Ok(exe) = program() else { return };
    if !runs(&imp::command_words(&old), &exe) {
        log::info!("start with the system: updated to {}", exe.display());
        if let Err(e) = imp::enable(&exe) {
            log::warn!("start with the system: {e}");
        }
    }
}

/// Does the command (split into words) run `exe`? Any word may be it, so a wrapper in front
/// (`env`, `nice`) still counts.
fn runs(words: &[String], exe: &std::path::Path) -> bool {
    words.iter().any(|w| same_path(w, &exe.to_string_lossy()))
}

/// Windows paths ignore case; Linux ones do not.
fn same_path(a: &str, b: &str) -> bool {
    if cfg!(windows) { a.to_lowercase() == b.to_lowercase() } else { a == b }
}

/// Words of a Windows command line (`"C:\…\miniutils.exe" --hidden`): a part in double quotes
/// is one word, with its spaces. Pure, so it is tested on Linux too.
#[cfg(any(windows, test))]
fn split_command_line(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let (mut quoted, mut any) = (false, false);
    for c in line.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any {
                    words.push(std::mem::take(&mut word));
                }
                any = false;
            }
            c => {
                word.push(c);
                any = true;
            }
        }
    }
    if any {
        words.push(word);
    }
    words
}

/// The executable to open. In an AppImage, `current_exe` is a temporary mount point: the
/// `.AppImage` file counts instead (variable `APPIMAGE`).
fn program() -> Result<PathBuf, String> {
    if let Some(image) = std::env::var_os("APPIMAGE").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(image));
    }
    std::env::current_exe().map_err(|e| crate::tr!("could not find the executable ({})", e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn words(list: &[&str]) -> Vec<String> {
        list.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn an_entry_that_still_runs_this_executable_is_kept() {
        let exe = Path::new("/home/x/bin/miniutils");
        assert!(runs(&words(&["/home/x/bin/miniutils", "--hidden"]), exe));
        assert!(runs(&words(&["env", "MINIUTILS_NATIVE_WAYLAND=1", "/home/x/bin/miniutils", "--hidden", "--tab", "X"]), exe));
        assert!(!runs(&words(&["/opt/old/miniutils", "--hidden"]), exe));
        assert!(!runs(&[], exe));
    }

    #[test]
    fn windows_command_line_words() {
        assert_eq!(split_command_line(r#""C:\Program Files\Mini Utils\miniutils.exe" --hidden"#), words(&[r"C:\Program Files\Mini Utils\miniutils.exe", "--hidden"]));
        assert_eq!(split_command_line(r"C:\bin\miniutils.exe  --hidden "), words(&[r"C:\bin\miniutils.exe", "--hidden"]));
        assert_eq!(split_command_line(r#""" x"#), words(&["", "x"]));
        assert!(split_command_line("  ").is_empty());
    }
}
