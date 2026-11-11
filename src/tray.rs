//! Ícone na bandeja: a mascote no humor do hub, com o menu (mostrar, fixar, mídia, sair).
//!
//! A mesma interface nos dois sistemas (`Tray::spawn`, `online`, `update`, `set_pinned`):
//! StatusNotifierItem pelo D-Bus no Linux (`linux.rs`), área de notificação no Windows
//! (`windows.rs`).

#[cfg(target_os = "linux")]
#[path = "tray/linux.rs"]
mod imp;
#[cfg(windows)]
#[path = "tray/windows.rs"]
mod imp;

pub use imp::Tray;
