//! Tray icon: the mascot in the hub's mood, with the menu (show, pin, media, quit).
//!
//! The same interface on both systems (`Tray::spawn`, `online`, `update`, `set_pinned`):
//! StatusNotifierItem over D-Bus on Linux (`linux.rs`), the notification area on Windows
//! (`windows.rs`).

#[cfg(target_os = "linux")]
#[path = "tray/linux.rs"]
mod imp;
#[cfg(windows)]
#[path = "tray/windows.rs"]
mod imp;

pub use imp::Tray;
