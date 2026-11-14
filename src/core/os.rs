//! Small pieces that differ between systems and have no module of their own: ending a
//! process, checking whether it exists, creating a file only the owner can read.
//!
//! Project rule (see "Platforms" in AGENTS.md): system calls live here or in a
//! `linux.rs` / `windows.rs` module; the rest of the code uses only the common function.

use std::fs::OpenOptions;

/// Why a process could not be ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KillError {
    /// The process belongs to another user (or to the system).
    Denied,
    Other(String),
}

/// Asks process `pid` to end. On Linux it is a SIGTERM (the program can close calmly);
/// Windows has no equivalent for windowless programs, so it ends it at once.
#[cfg(unix)]
pub fn terminate(pid: u32) -> Result<(), KillError> {
    // SAFETY: `kill` only sends a signal to a process; the kernel refuses if it is not the user's.
    if unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) } == 0 {
        return Ok(());
    }
    let e = std::io::Error::last_os_error();
    Err(if e.raw_os_error() == Some(libc::EPERM) { KillError::Denied } else { KillError::Other(e.to_string()) })
}

#[cfg(windows)]
pub fn terminate(pid: u32) -> Result<(), KillError> {
    use windows::Win32::Foundation::{CloseHandle, E_ACCESSDENIED};
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};
    let denied = |e: windows::core::Error| if e.code() == E_ACCESSDENIED { KillError::Denied } else { KillError::Other(e.message()) };
    // SAFETY: the handle opened here is closed right below and does not leave the function.
    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE, false, pid).map_err(denied)?;
        let result = TerminateProcess(handle, 1).map_err(denied);
        let _ = CloseHandle(handle);
        result
    }
}

/// Process `pid` exists and has not exited yet. (On Linux, whoever needs this reads `/proc`.)
#[cfg(windows)]
pub fn alive(pid: u32) -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    /// Exit code of a process that is still running (`STILL_ACTIVE`).
    const STILL_ACTIVE: u32 = 259;
    // SAFETY: the handle opened here is closed right below and does not leave the function.
    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return false };
        let mut code = 0u32;
        let running = GetExitCodeProcess(handle, &mut code).is_ok() && code == STILL_ACTIVE;
        let _ = CloseHandle(handle);
        running
    }
}

/// New file only the owner can read and write (keys, credentials). On Windows it changes
/// nothing: the user folder is already private through the system permissions.
pub fn owner_only(options: &mut OpenOptions) -> &mut OpenOptions {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

/// XDG data folder (`~/.local/share`). Terminal tools that come from Linux (OpenCode) use
/// this path on Windows too, instead of `%APPDATA%`.
pub fn xdg_data_dir() -> Option<std::path::PathBuf> {
    if cfg!(windows) {
        std::env::var_os("XDG_DATA_HOME")
            .filter(|v| !v.is_empty())
            .map(std::path::PathBuf::from)
            .or_else(|| dirs::home_dir().map(|h| h.join(".local").join("share")))
    } else {
        dirs::data_dir()
    }
}

/// Folder of the log files (`core::logging`): `~/.local/state/miniutils` on Linux
/// (XDG_STATE_HOME, where logs and history go) and `%LOCALAPPDATA%\miniutils\logs` on
/// Windows (outside the roaming profile).
pub fn log_dir() -> Option<std::path::PathBuf> {
    if cfg!(windows) {
        dirs::data_local_dir().map(|d| d.join("miniutils").join("logs"))
    } else {
        dirs::state_dir().or_else(dirs::data_dir).map(|d| d.join("miniutils"))
    }
}
