//! Single instance on Linux: a Unix socket in `$XDG_RUNTIME_DIR`.

use std::fs;
use std::io::{self, BufRead, BufReader, ErrorKind, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{Command, Role, parse, stem};

/// Hands a command line to the instance listening at `path`. `false` = nobody listened.
pub fn send(path: &Path, message: &str) -> bool {
    let Ok(mut stream) = UnixStream::connect(path) else { return false };
    let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
    stream.write_all(format!("{message}\n").as_bytes()).is_ok()
}

/// `$XDG_RUNTIME_DIR/miniutils.sock` (or `miniutils-<name>.sock` with `MINIUTILS_INSTANCE`).
pub fn socket_path() -> PathBuf {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR");
    let instance = std::env::var("MINIUTILS_INSTANCE").ok();
    // SAFETY: getuid has no side effects and cannot fail
    let uid = unsafe { libc::getuid() };
    socket_path_for(runtime.as_deref().map(Path::new), uid, instance.as_deref())
}

fn socket_path_for(runtime_dir: Option<&Path>, uid: u32, instance: Option<&str>) -> PathBuf {
    let stem = stem(instance);
    match runtime_dir.filter(|d| !d.as_os_str().is_empty()) {
        Some(dir) => dir.join(format!("{stem}.sock")),
        None => PathBuf::from(format!("/tmp/{stem}-{uid}.sock")),
    }
}

/// Socket of the main instance. Deletes the file on exit.
pub struct Server {
    path: PathBuf,
    listener: UnixListener,
}

impl Server {
    /// Serves requests on a thread of its own; `on_command` runs on that thread.
    pub fn serve<F>(&self, on_command: F) -> io::Result<()>
    where
        F: Fn(Command) + Send + 'static,
    {
        let listener = self.listener.try_clone()?;
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                handle(stream, &on_command);
            }
        });
        Ok(())
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn handle<F: Fn(Command)>(stream: UnixStream, on_command: &F) {
    // a client that sends nothing must not block the others
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut line = String::new();
    // 256-byte limit: a command line is never longer
    if BufReader::new((&stream).take(256)).read_line(&mut line).is_ok()
        && let Some(cmd) = parse(&line) {
            on_command(cmd);
        }
}

/// Tries to be the main instance at `path`. If there already is one, hands it `message`
/// (`None`: connects and sends nothing, which the main one ignores).
pub fn claim(path: &Path, message: Option<&str>) -> io::Result<Role> {
    // two attempts: the 2nd covers another instance starting at that very moment
    for _ in 0..2 {
        if let Ok(mut stream) = UnixStream::connect(path) {
            if let Some(message) = message {
                let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
                let _ = stream.write_all(format!("{message}\n").as_bytes());
            }
            return Ok(Role::Second);
        }
        // nobody answers: stale socket of a dead process (or a leftover file)
        let _ = fs::remove_file(path);
        match UnixListener::bind(path) {
            Ok(listener) => return Ok(Role::First(Server { path: path.to_path_buf(), listener })),
            Err(e) if e.kind() == ErrorKind::AddrInUse => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(ErrorKind::AddrInUse, "socket in use"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn test_path(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("miniutils-test-{}-{name}.sock", std::process::id()));
        let _ = fs::remove_file(&p);
        p
    }

    #[test]
    fn message_and_socket_path() {
        let run = Path::new("/run/user/1000");
        assert_eq!(socket_path_for(Some(run), 1000, None), run.join("miniutils.sock"));
        assert_eq!(socket_path_for(Some(run), 1000, Some("test1")), run.join("miniutils-test1.sock"));
        assert_eq!(socket_path_for(Some(Path::new("")), 1000, None), PathBuf::from("/tmp/miniutils-1000.sock"));
        assert_eq!(socket_path_for(None, 7, Some("a/b")), PathBuf::from("/tmp/miniutils-a_b-7.sock"));
    }

    #[test]
    fn stale_socket_of_a_dead_process_is_replaced() {
        // the file stays after the listener dies, and nobody answers anymore
        let path = test_path("stale");
        drop(UnixListener::bind(&path).unwrap());
        assert!(path.exists());
        assert!(matches!(claim(&path, Some("show")).unwrap(), Role::First(_)));
    }

    #[test]
    fn regular_file_in_place_of_the_socket_is_replaced() {
        let path = test_path("file");
        fs::write(&path, b"junk").unwrap();
        assert!(matches!(claim(&path, Some("show")).unwrap(), Role::First(_)));
    }

    #[test]
    fn second_instance_delivers_the_command_and_exits() {
        let path = test_path("second");
        let Role::First(server) = claim(&path, Some("show apps")).unwrap() else {
            panic!("the first instance should become the main one");
        };
        let (tx, rx) = mpsc::channel();
        server.serve(move |cmd| {
            let _ = tx.send(cmd);
        }).unwrap();
        assert!(matches!(claim(&path, Some("show apps")).unwrap(), Role::Second));
        let cmd = rx.recv_timeout(Duration::from_secs(2)).expect("command did not arrive");
        assert_eq!(cmd, Command::Show(Some("apps".into())));
        // a hidden start sends nothing: the main one gets no command
        assert!(matches!(claim(&path, None).unwrap(), Role::Second));
        assert!(send(&path, "quit"));
        let cmd = rx.recv_timeout(Duration::from_secs(2)).expect("command did not arrive");
        assert_eq!(cmd, Command::Quit, "the empty connection must not produce a command");
        assert!(!send(&test_path("nobody"), "quit"));
    }

    #[test]
    fn socket_goes_away_when_the_main_one_exits() {
        let path = test_path("exit");
        let Role::First(server) = claim(&path, Some("show")).unwrap() else {
            panic!("the first instance should become the main one");
        };
        assert!(path.exists());
        drop(server);
        assert!(!path.exists());
    }
}
