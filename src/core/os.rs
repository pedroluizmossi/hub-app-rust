//! Pedaços pequenos que mudam de um sistema para o outro e não têm módulo próprio:
//! encerrar um processo, saber se ele existe, criar arquivo que só o dono lê.
//!
//! Regra do projeto (ver "Plataformas" no AGENTS.md): chamada de sistema fica aqui ou num
//! módulo `linux.rs` / `windows.rs`; o resto do código usa só a função comum.

use std::fs::OpenOptions;

/// Por que não deu para encerrar um processo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KillError {
    /// O processo é de outro usuário (ou do sistema).
    Denied,
    Other(String),
}

/// Pede para o processo `pid` encerrar. No Linux é um SIGTERM (o programa pode fechar com
/// calma); o Windows não tem equivalente para programas sem janela, então encerra na hora.
#[cfg(unix)]
pub fn terminate(pid: u32) -> Result<(), KillError> {
    // SAFETY: `kill` só envia um sinal a um processo; o kernel recusa se não for do usuário.
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
    // SAFETY: o identificador aberto aqui é fechado logo abaixo e não sai da função.
    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE, false, pid).map_err(denied)?;
        let result = TerminateProcess(handle, 1).map_err(denied);
        let _ = CloseHandle(handle);
        result
    }
}

/// O processo `pid` existe e ainda não saiu. (No Linux quem precisa disso lê o `/proc`.)
#[cfg(windows)]
pub fn alive(pid: u32) -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    /// Código de saída de quem ainda está rodando (`STILL_ACTIVE`).
    const STILL_ACTIVE: u32 = 259;
    // SAFETY: o identificador aberto aqui é fechado logo abaixo e não sai da função.
    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return false };
        let mut code = 0u32;
        let running = GetExitCodeProcess(handle, &mut code).is_ok() && code == STILL_ACTIVE;
        let _ = CloseHandle(handle);
        running
    }
}

/// Arquivo novo que só o dono lê e escreve (chaves, credenciais). No Windows não muda
/// nada: a pasta do usuário já é só dele pelas permissões do sistema.
pub fn owner_only(options: &mut OpenOptions) -> &mut OpenOptions {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

/// Pasta de dados no padrão XDG (`~/.local/share`). Ferramentas de terminal vindas do
/// Linux (OpenCode) usam esse caminho também no Windows, em vez do `%APPDATA%`.
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
