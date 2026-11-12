//! Instância única no Windows: um named pipe por usuário (`\\.\pipe\miniutils-<usuário>`).
//!
//! O pipe some sozinho quando o processo termina, então não há "socket velho" para limpar.
//! Quem cria primeiro (`FILE_FLAG_FIRST_PIPE_INSTANCE`) é a instância principal; as outras
//! abrem o pipe como um arquivo, escrevem a linha e saem.

use std::fs::OpenOptions;
use std::io::{self, BufRead, BufReader, ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, ERROR_NO_DATA, ERROR_PIPE_CONNECTED, HANDLE};
use windows::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_INBOUND, ReadFile};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
    PIPE_WAIT,
};
use windows::core::HSTRING;

use super::{Command, Role, clean, parse, stem};

/// `ERROR_PIPE_BUSY`: a principal existe, mas está atendendo outro pedido agora.
const PIPE_BUSY: i32 = 231;

/// `\\.\pipe\miniutils-<usuário>` (ou `miniutils-<nome>-<usuário>` com `MINIUTILS_INSTANCE`).
/// O nome leva o usuário porque os pipes são vistos por todas as sessões da máquina.
pub fn socket_path() -> PathBuf {
    let user = std::env::var("USERNAME").unwrap_or_default();
    let instance = std::env::var("MINIUTILS_INSTANCE").ok();
    PathBuf::from(format!(r"\\.\pipe\{}-{}", stem(instance.as_deref()), clean(&user)))
}

/// Avisa a instância rodando (se houver) que os plugins mudaram. `false` = nenhuma ouviu.
pub fn notify_plugins_changed() -> bool {
    matches!(send(&socket_path(), "plugins"), Ok(true))
}

/// Entrega uma linha à instância principal. `Ok(false)` = não há instância escutando.
fn send(path: &Path, message: &str) -> io::Result<bool> {
    for _ in 0..20 {
        match OpenOptions::new().write(true).open(path) {
            Ok(mut pipe) => {
                pipe.write_all(format!("{message}\n").as_bytes())?;
                return Ok(true);
            }
            Err(e) if e.raw_os_error() == Some(PIPE_BUSY) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(ErrorKind::TimedOut, "a instância principal não atendeu"))
}

/// Uma ponta (do servidor) do pipe. Fecha ao sair.
struct Pipe(HANDLE);

// SAFETY: um identificador de pipe pode ser usado de qualquer thread; só uma o usa por vez.
unsafe impl Send for Pipe {}

impl Drop for Pipe {
    fn drop(&mut self) {
        // SAFETY: o identificador é nosso e não é usado depois daqui.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

impl Read for &Pipe {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut read = 0u32;
        // SAFETY: `buf` e `read` vivem até a chamada voltar (leitura síncrona).
        match unsafe { ReadFile(self.0, Some(buf), Some(&mut read), None) } {
            Ok(()) => Ok(read as usize),
            // o cliente fechou a ponta dele: fim da linha
            Err(_) => Ok(0),
        }
    }
}

/// Cria o pipe. Com `first`, falha se outro processo já o criou.
fn create(path: &Path, first: bool) -> io::Result<Pipe> {
    let name = HSTRING::from(path.as_os_str());
    let mut open = PIPE_ACCESS_INBOUND;
    if first {
        open |= FILE_FLAG_FIRST_PIPE_INSTANCE;
    }
    // só clientes desta máquina; uma conexão por vez (o pedido é uma linha curta)
    let mode = PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS;
    // SAFETY: `name` vive até a chamada voltar; o identificador devolvido passa a ser do `Pipe`.
    let handle = unsafe { CreateNamedPipeW(&name, open, mode, 1, 0, 512, 0, None) };
    if handle.is_invalid() { Err(io::Error::last_os_error()) } else { Ok(Pipe(handle)) }
}

/// Pipe da instância principal.
pub struct Server {
    /// Fica aqui até o `serve` levar para a thread que atende.
    pipe: Mutex<Option<Pipe>>,
}

impl Server {
    /// Atende os pedidos numa thread própria; `on_command` roda nessa thread.
    pub fn serve<F>(&self, on_command: F) -> io::Result<()>
    where
        F: Fn(Command) + Send + 'static,
    {
        let pipe = self.pipe.lock().ok().and_then(|mut p| p.take());
        let pipe = pipe.ok_or_else(|| io::Error::new(ErrorKind::Other, "o pipe já está sendo atendido"))?;
        std::thread::spawn(move || {
            let mut failures = 0;
            // desiste só se o pipe quebrou de vez (várias falhas seguidas)
            while failures < 10 {
                // SAFETY: o identificador é do `pipe`, que vive durante todo o laço.
                let connected = unsafe { ConnectNamedPipe(pipe.0, None) };
                // O cliente é rápido (abre, escreve, fecha) e pode ter chegado antes desta
                // chamada, ou até já ter saído: nos dois casos a linha dele está no pipe.
                let early = |e: &windows::core::Error| e.code() == ERROR_PIPE_CONNECTED.to_hresult() || e.code() == ERROR_NO_DATA.to_hresult();
                if connected.as_ref().is_err_and(|e| !early(e)) {
                    failures += 1;
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
                failures = 0;
                let mut line = String::new();
                // limite de 256 bytes: uma linha de comando nunca passa disso
                if BufReader::new((&pipe).take(256)).read_line(&mut line).is_ok() {
                    if let Some(cmd) = parse(&line) {
                        on_command(cmd);
                    }
                }
                // SAFETY: como acima; solta este cliente para o próximo poder entrar.
                unsafe {
                    let _ = DisconnectNamedPipe(pipe.0);
                }
            }
        });
        Ok(())
    }
}

/// Tenta ser a instância principal em `path`. Se já houver uma, entrega `message` a ela.
pub fn claim(path: &Path, message: &str) -> io::Result<Role> {
    // duas tentativas: a 2ª cobre o caso de outra instância ter subido bem na hora
    for _ in 0..2 {
        if send(path, message)? {
            return Ok(Role::Second);
        }
        match create(path, true) {
            Ok(pipe) => return Ok(Role::First(Server { pipe: Mutex::new(Some(pipe)) })),
            // outro processo criou primeiro: volta e entrega o pedido a ele
            Err(e) if e.kind() == ErrorKind::PermissionDenied => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(ErrorKind::AddrInUse, "pipe ocupado"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn segunda_instancia_entrega_o_comando_e_sai() {
        let path = PathBuf::from(format!(r"\\.\pipe\miniutils-test-{}", std::process::id()));
        let Role::First(server) = claim(&path, "show apps").unwrap() else {
            panic!("a primeira instância devia virar a principal");
        };
        let (tx, rx) = mpsc::channel();
        server
            .serve(move |cmd| {
                let _ = tx.send(cmd);
            })
            .unwrap();
        for _ in 0..2 {
            assert!(matches!(claim(&path, "show apps").unwrap(), Role::Second));
            let cmd = rx.recv_timeout(Duration::from_secs(2)).expect("comando não chegou");
            assert_eq!(cmd, Command::Show(Some("apps".into())));
        }
    }
}
