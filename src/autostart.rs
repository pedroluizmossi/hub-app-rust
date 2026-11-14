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
/// elsewhere)? Rewrites it with the current one. Runs when the app opens.
pub fn refresh() {
    let Some(old) = imp::current() else { return };
    let Ok(exe) = program() else { return };
    if old != imp::entry_for(&exe) {
        log::info!("start with the system: updated to {}", exe.display());
        if let Err(e) = imp::enable(&exe) {
            log::warn!("start with the system: {e}");
        }
    }
}

/// The executable to open. In an AppImage, `current_exe` is a temporary mount point: the
/// `.AppImage` file counts instead (variable `APPIMAGE`).
fn program() -> Result<PathBuf, String> {
    if let Some(image) = std::env::var_os("APPIMAGE").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(image));
    }
    std::env::current_exe().map_err(|e| crate::tr!("could not find the executable ({})", e))
}
