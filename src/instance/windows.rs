//! Single instance on Windows: a named pipe per user (`\\.\pipe\miniutils-<user>`).
//!
//! The pipe goes away by itself when the process ends, so there is no "stale socket" to
//! clean up. Whoever creates it first (`FILE_FLAG_FIRST_PIPE_INSTANCE`) is the main
//! instance; the others open the pipe as a file, write the line and exit.

use std::fs::OpenOptions;
use std::io::{self, BufRead, BufReader, ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, ERROR_NO_DATA, ERROR_PIPE_CONNECTED, HANDLE};
use windows::Win32::Storage::FileSystem::{FILE_FLAG_FIRST_PIPE_INSTANCE, PIPE_ACCESS_INBOUND, ReadFile};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
    PIPE_WAIT,
};
use windows::core::HSTRING;

use super::{Command, Role, StatusFeed, clean, parse, stem};

/// `ERROR_PIPE_BUSY`: the main instance exists, but is serving another request right now.
const PIPE_BUSY: i32 = 231;

/// `\\.\pipe\miniutils-<user>` (or `miniutils-<name>-<user>` with `MINIUTILS_INSTANCE`).
/// The name includes the user because pipes are visible to every session on the machine.
pub fn socket_path() -> PathBuf {
    let user = std::env::var("USERNAME").unwrap_or_default();
    let instance = std::env::var("MINIUTILS_INSTANCE").ok();
    PathBuf::from(format!(r"\\.\pipe\{}-{}", stem(instance.as_deref()), clean(&user)))
}

/// Hands a command line to the instance listening at `path`. `false` = nobody listened.
pub fn send(path: &Path, message: &str) -> bool {
    matches!(deliver(path, Some(message)), Ok(true))
}

/// Hands a line to the main instance (`None`: opens the pipe and sends nothing, which the
/// main one ignores). `Ok(false)` = no instance is listening.
fn deliver(path: &Path, message: Option<&str>) -> io::Result<bool> {
    for _ in 0..20 {
        match OpenOptions::new().write(true).open(path) {
            Ok(mut pipe) => {
                if let Some(message) = message {
                    pipe.write_all(format!("{message}\n").as_bytes())?;
                }
                return Ok(true);
            }
            Err(e) if e.raw_os_error() == Some(PIPE_BUSY) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(ErrorKind::TimedOut, "the main instance did not answer"))
}

/// The (server) end of the pipe. Closed on drop.
struct Pipe(HANDLE);

// SAFETY: a pipe handle may be used from any thread; only one uses it at a time.
unsafe impl Send for Pipe {}

impl Drop for Pipe {
    fn drop(&mut self) {
        // SAFETY: the handle is ours and is not used after this.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

impl Read for &Pipe {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut read = 0u32;
        // SAFETY: `buf` and `read` live until the call returns (synchronous read).
        match unsafe { ReadFile(self.0, Some(buf), Some(&mut read), None) } {
            Ok(()) => Ok(read as usize),
            // the client closed its end: end of the line
            Err(_) => Ok(0),
        }
    }
}

/// Creates the pipe. With `first`, fails if another process already created it.
fn create(path: &Path, first: bool) -> io::Result<Pipe> {
    let name = HSTRING::from(path.as_os_str());
    let mut open = PIPE_ACCESS_INBOUND;
    if first {
        open |= FILE_FLAG_FIRST_PIPE_INSTANCE;
    }
    // only clients on this machine; one connection at a time (the request is a short line)
    let mode = PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS;
    // SAFETY: `name` lives until the call returns; the returned handle is owned by the `Pipe`.
    let handle = unsafe { CreateNamedPipeW(&name, open, mode, 1, 0, 512, 0, None) };
    if handle.is_invalid() { Err(io::Error::last_os_error()) } else { Ok(Pipe(handle)) }
}

/// Pipe of the main instance.
pub struct Server {
    /// Kept here until `serve` takes it to the serving thread.
    pipe: Mutex<Option<Pipe>>,
}

impl Server {
    /// Serves requests on a thread of its own; `on_command` runs on that thread. The pipe
    /// is one-way (the clients only write), so the questions `feed` answers on Linux
    /// (`status`, `subscribe`) get no answer here: they are read and ignored.
    pub fn serve<F>(&self, _feed: Arc<StatusFeed>, on_command: F) -> io::Result<()>
    where
        F: Fn(Command) + Send + 'static,
    {
        let pipe = self.pipe.lock().ok().and_then(|mut p| p.take());
        let pipe = pipe.ok_or_else(|| io::Error::other("the pipe is already being served"))?;
        std::thread::spawn(move || {
            let mut failures = 0;
            // gives up only if the pipe broke for good (several failures in a row)
            while failures < 10 {
                // SAFETY: the handle belongs to `pipe`, which lives for the whole loop.
                let connected = unsafe { ConnectNamedPipe(pipe.0, None) };
                // The client is quick (open, write, close) and may have arrived before this
                // call, or even left already: either way its line is in the pipe.
                let early = |e: &windows::core::Error| e.code() == ERROR_PIPE_CONNECTED.to_hresult() || e.code() == ERROR_NO_DATA.to_hresult();
                if connected.as_ref().is_err_and(|e| !early(e)) {
                    failures += 1;
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
                failures = 0;
                let mut line = String::new();
                // 256-byte limit: a command line is never longer
                if BufReader::new((&pipe).take(256)).read_line(&mut line).is_ok()
                    && let Some(cmd) = parse(&line)
                {
                    on_command(cmd);
                }
                // SAFETY: as above; releases this client so the next one can connect.
                unsafe {
                    let _ = DisconnectNamedPipe(pipe.0);
                }
            }
        });
        Ok(())
    }
}

/// Tries to be the main instance at `path`. If there already is one, hands it `message`
/// (`None`: sends nothing).
pub fn claim(path: &Path, message: Option<&str>) -> io::Result<Role> {
    // two attempts: the 2nd covers another instance starting at that very moment
    for _ in 0..2 {
        if deliver(path, message)? {
            return Ok(Role::Second);
        }
        match create(path, true) {
            Ok(pipe) => return Ok(Role::First(Server { pipe: Mutex::new(Some(pipe)) })),
            // another process created it first: go back and hand it the request
            Err(e) if e.kind() == ErrorKind::PermissionDenied => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(ErrorKind::AddrInUse, "pipe in use"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn second_instance_delivers_the_command_and_exits() {
        let path = PathBuf::from(format!(r"\\.\pipe\miniutils-test-{}", std::process::id()));
        let Role::First(server) = claim(&path, Some("show apps")).unwrap() else {
            panic!("the first instance should become the main one");
        };
        let (tx, rx) = mpsc::channel();
        server
            .serve(move |cmd| {
                let _ = tx.send(cmd);
            })
            .unwrap();
        for _ in 0..2 {
            assert!(matches!(claim(&path, Some("show apps")).unwrap(), Role::Second));
            let cmd = rx.recv_timeout(Duration::from_secs(2)).expect("command did not arrive");
            assert_eq!(cmd, Command::Show(Some("apps".into())));
        }
    }
}
