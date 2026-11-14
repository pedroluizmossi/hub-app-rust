//! Start with the system on Windows: the "MiniUtils" value in
//! `HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Run` (per user, without
//! asking for administrator rights; shows in Task Manager › Startup).

use std::path::Path;

use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW};
use windows::core::{PCWSTR, w};

const RUN: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const NAME: PCWSTR = w!("MiniUtils");

/// The stored command line, if any.
pub fn current() -> Option<String> {
    let mut size = 0u32;
    // SAFETY: first it only asks for the size; then it reads into a buffer of that size.
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
    // SAFETY: `wide` is zero-terminated and lives until the call returns; the size is in bytes.
    let r = unsafe {
        RegSetKeyValueW(HKEY_CURRENT_USER, RUN, NAME, REG_SZ.0, Some(wide.as_ptr().cast()), (wide.len() * 2) as u32)
    };
    r.ok().map_err(|e| crate::tr!("Windows registry: {}", e.message()))
}

pub fn disable() -> Result<(), String> {
    // SAFETY: deletes only the "MiniUtils" value of the user's Run key.
    let r = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN, NAME) };
    if r == ERROR_SUCCESS || r == ERROR_FILE_NOT_FOUND {
        return Ok(());
    }
    r.ok().map_err(|e| crate::tr!("Windows registry: {}", e.message()))
}

/// `"C:\…\miniutils.exe" --hidden`.
pub fn entry_for(exe: &Path) -> String {
    format!("\"{}\" --hidden", exe.display())
}
