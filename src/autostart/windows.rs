//! Iniciar com o sistema no Windows: o valor "MiniUtils" em
//! `HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Run` (só do usuário, sem
//! pedir administrador; aparece em Gerenciador de Tarefas › Inicializar).

use std::path::Path;

use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW};
use windows::core::{PCWSTR, w};

const RUN: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const NAME: PCWSTR = w!("MiniUtils");

/// A linha de comando gravada, se há.
pub fn current() -> Option<String> {
    let mut size = 0u32;
    // SAFETY: primeiro só pergunta o tamanho; depois lê num buffer desse tamanho.
    unsafe {
        if RegGetValueW(HKEY_CURRENT_USER, RUN, NAME, RRF_RT_REG_SZ, None, None, Some(&raw mut size)) != ERROR_SUCCESS {
            return None;
        }
        let mut buf = vec![0u16; (size as usize).div_ceil(2)];
        let r = RegGetValueW(HKEY_CURRENT_USER, RUN, NAME, RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr().cast()), Some(&raw mut size));
        if r != ERROR_SUCCESS {
            return None;
        }
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..end]))
    }
}

pub fn enable(exe: &Path) -> Result<(), String> {
    let wide: Vec<u16> = entry_for(exe).encode_utf16().chain(Some(0)).collect();
    // SAFETY: `wide` termina em zero e vive até o fim da chamada; o tamanho é em bytes.
    let r = unsafe {
        RegSetKeyValueW(HKEY_CURRENT_USER, RUN, NAME, REG_SZ.0, Some(wide.as_ptr().cast()), (wide.len() * 2) as u32)
    };
    r.ok().map_err(|e| format!("registro do Windows: {}", e.message()))
}

pub fn disable() -> Result<(), String> {
    // SAFETY: apaga só o valor "MiniUtils" da chave Run do usuário.
    let r = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN, NAME) };
    if r == ERROR_SUCCESS || r == ERROR_FILE_NOT_FOUND {
        return Ok(());
    }
    r.ok().map_err(|e| format!("registro do Windows: {}", e.message()))
}

/// `"C:\…\miniutils.exe" --hidden`.
pub fn entry_for(exe: &Path) -> String {
    format!("\"{}\" --hidden", exe.display())
}
