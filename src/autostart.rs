//! Iniciar com o sistema (só na bandeja, com `--hidden`).
//!
//! Quem guarda se está ligado é o próprio sistema, não a config: o arquivo em
//! `~/.config/autostart/` no Linux (`linux.rs`, padrão XDG, que KDE, GNOME, XFCE, Cinnamon
//! e MATE seguem) e o valor em `HKCU\…\CurrentVersion\Run` no Windows (`windows.rs`).
//! Assim, desligar pelas ferramentas do sistema também aparece desligado aqui.

#[cfg(target_os = "linux")]
#[path = "autostart/linux.rs"]
mod imp;
#[cfg(windows)]
#[path = "autostart/windows.rs"]
mod imp;

use std::path::PathBuf;

/// Está ligado?
pub fn is_enabled() -> bool {
    imp::current().is_some()
}

/// Liga ou desliga.
pub fn set(on: bool) -> Result<(), String> {
    let r = if on { imp::enable(&program()?) } else { imp::disable() };
    match &r {
        Ok(()) => log::info!("iniciar com o sistema: {}", if on { "ligado" } else { "desligado" }),
        Err(e) => log::warn!("iniciar com o sistema: {e}"),
    }
    r
}

/// Ligado, mas apontando para outro executável (o app mudou de pasta ou foi trocado por
/// uma versão nova noutro lugar)? Regrava com o de agora. Roda ao abrir o app.
pub fn refresh() {
    let Some(old) = imp::current() else { return };
    let Ok(exe) = program() else { return };
    if old != imp::entry_for(&exe) {
        log::info!("iniciar com o sistema: atualizado para {}", exe.display());
        if let Err(e) = imp::enable(&exe) {
            log::warn!("iniciar com o sistema: {e}");
        }
    }
}

/// O executável a abrir. Num AppImage, o `current_exe` é um ponto de montagem temporário:
/// vale o arquivo `.AppImage` (variável `APPIMAGE`).
fn program() -> Result<PathBuf, String> {
    if let Some(image) = std::env::var_os("APPIMAGE").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(image));
    }
    std::env::current_exe().map_err(|e| format!("não achei o executável ({e})"))
}
