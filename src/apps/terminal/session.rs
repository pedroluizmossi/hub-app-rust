//! A shell running in a pseudo-terminal: what it writes goes through the VT emulator
//! (`vt100`) on a thread of its own, and the window draws the emulator's screen.
//!
//! The shell is started by the PTY (`portable-pty`), not by `exec::command`: it has to be
//! attached to the terminal, and on Windows the ConPTY already opens no console window.

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};

/// What the emulator hears besides the screen: the window title, the bell and the queries
/// a program makes to the terminal (answered on the same thread that reads the shell).
#[derive(Default)]
pub struct Events {
    pub title: Option<String>,
    pub bell: bool,
    replies: Vec<u8>,
}

impl vt100::Callbacks for Events {
    fn audible_bell(&mut self, _: &mut vt100::Screen) {
        self.bell = true;
    }

    fn set_window_title(&mut self, _: &mut vt100::Screen, title: &[u8]) {
        self.title = Some(String::from_utf8_lossy(title).chars().take(200).collect());
    }

    fn unhandled_csi(
        &mut self,
        screen: &mut vt100::Screen,
        i1: Option<u8>,
        _i2: Option<u8>,
        params: &[&[u16]],
        c: char,
    ) {
        match (i1, c, params.first().and_then(|p| p.first())) {
            // device status: "ok"
            (None, 'n', Some(5)) => self.replies.extend_from_slice(b"\x1b[0n"),
            // where is the cursor (shells and editors ask to place their prompt)
            (None, 'n', Some(6)) => {
                let (row, col) = screen.cursor_position();
                self.replies.extend_from_slice(format!("\x1b[{};{}R", row + 1, col + 1).as_bytes());
            }
            // which terminal is this: a VT100 with advanced video
            (None, 'c', None | Some(0)) => self.replies.extend_from_slice(b"\x1b[?1;2c"),
            _ => {}
        }
    }
}

pub type Parser = vt100::Parser<Events>;

/// State shared with the thread that reads the shell.
struct Shared {
    parser: Mutex<Parser>,
    /// The shell exited (its exit code, when known, in `code`).
    exited: AtomicBool,
    code: Mutex<Option<u32>>,
    /// When the shell last wrote something.
    last_output: Mutex<Instant>,
}

/// How to start the shell: the program and its arguments, and the folder.
#[derive(Debug, Clone, PartialEq)]
pub struct Launch {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub dir: PathBuf,
    /// The `dir` setting named a folder that doesn't exist (as written), so the shell
    /// starts in the home folder instead.
    pub dir_missing: Option<String>,
}

impl Launch {
    /// From the app settings: `shell` (empty = the system's) and `dir` (empty or missing =
    /// the home folder; a leading `~` is the home folder). A shell with arguments is split at
    /// the spaces, unless the whole text is a file (a path with spaces on Windows).
    pub fn from_settings(shell: &str, dir: &str) -> anyhow::Result<Self> {
        let shell = shell.trim();
        let (program, args) = if shell.is_empty() {
            (crate::core::exec::default_shell(), Vec::new())
        } else if Path::new(shell).is_file() {
            (PathBuf::from(shell), Vec::new())
        } else {
            let mut words = shell.split_whitespace().map(str::to_string);
            let name = words.next().unwrap_or_default();
            let program = if Path::new(&name).is_file() {
                PathBuf::from(&name)
            } else {
                crate::core::exec::find(&name)
                    .ok_or_else(|| anyhow::anyhow!("{}", crate::tr!("shell not found: {}", name)))?
            };
            (program, words.collect())
        };
        let home = dirs::home_dir().unwrap_or_else(std::env::temp_dir);
        let written = dir.trim();
        let (dir, dir_missing) = match start_dir(written, &home) {
            Some(d) => (d, None),
            None if written.is_empty() => (home, None),
            None => {
                log::warn!("terminal: start folder {written:?} not found; using the home folder");
                (home, Some(written.to_string()))
            }
        };
        Ok(Launch { program, args, dir, dir_missing })
    }

    /// Name of the shell for the screen ("zsh", "pwsh").
    pub fn name(&self) -> String {
        self.program.file_stem().map_or_else(|| self.program.display().to_string(), |s| s.to_string_lossy().into())
    }
}

/// The folder `written` names, if it exists: absolute, or starting with `~` (the home folder).
fn start_dir(written: &str, home: &Path) -> Option<PathBuf> {
    let path = if written == "~" {
        home.to_path_buf()
    } else if let Some(rest) = written.strip_prefix("~/").or_else(|| written.strip_prefix("~\\")) {
        home.join(rest)
    } else {
        PathBuf::from(written)
    };
    (!written.is_empty() && path.is_absolute() && path.is_dir()).then_some(path)
}

/// A running shell (or one that has just exited, still showing its last screen).
pub struct Session {
    shared: Arc<Shared>,
    master: Box<dyn MasterPty + Send>,
    /// Bytes for the shell, written by a thread of their own: a program that doesn't read
    /// its input fills the terminal's buffer, and the write then waits (the UI must not).
    input: Sender<Vec<u8>>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    pub launch: Launch,
}

impl Session {
    /// Starts `launch` in a terminal of `rows` × `cols`, keeping `scrollback` lines of
    /// history. `wake` is called (on another thread) whenever the screen changes or the
    /// shell exits.
    pub fn start(
        launch: Launch,
        rows: u16,
        cols: u16,
        scrollback: usize,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> anyhow::Result<Self> {
        let (rows, cols) = (rows.max(2), cols.max(10));
        let pair = native_pty_system().openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })?;
        let mut argv: Vec<OsString> = vec![launch.program.clone().into()];
        argv.extend(launch.args.iter().map(OsString::from));
        let mut cmd = CommandBuilder::from_argv(argv);
        cmd.cwd(&launch.dir);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "MiniUtils");
        let mut child = pair.slave.spawn_command(cmd)?;
        // the shell holds its end; ours would keep the terminal open after it exits
        drop(pair.slave);
        let killer = child.clone_killer();
        let mut reader = pair.master.try_clone_reader()?;
        let mut writer = pair.master.take_writer()?;
        let parser = vt100::Parser::new_with_callbacks(
            rows.try_into()?,
            cols.try_into()?,
            scrollback,
            Events::default(),
        );
        let shared = Arc::new(Shared {
            parser: Mutex::new(parser),
            exited: AtomicBool::new(false),
            code: Mutex::new(None),
            last_output: Mutex::new(Instant::now()),
        });
        let wake = Arc::new(wake);

        // writes the keys, pastes and query answers to the shell, in order; ends when the
        // session and the reading thread are gone
        let (input, queued) = channel::<Vec<u8>>();
        std::thread::Builder::new().name("miniutils-term-write".into()).spawn(move || {
            for bytes in queued {
                if let Err(e) = writer.write_all(&bytes).and_then(|_| writer.flush()) {
                    log::debug!("terminal: write failed: {e}");
                }
            }
        })?;

        // reads what the shell writes and feeds the emulator
        let (sh, w, wk) = (shared.clone(), input.clone(), wake.clone());
        std::thread::Builder::new().name("miniutils-term-read".into()).spawn(move || {
            let mut buf = [0u8; 16 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let replies = {
                            let mut p = lock(&sh.parser);
                            p.process(&buf[..n]);
                            *lock(&sh.last_output) = Instant::now();
                            std::mem::take(&mut p.callbacks_mut().replies)
                        };
                        if !replies.is_empty() {
                            let _ = w.send(replies);
                        }
                        wk();
                    }
                }
            }
        })?;

        // waits for the shell to exit (on Windows the reading above only ends when the
        // terminal is closed, so the exit is noticed here)
        let (sh, wk) = (shared.clone(), wake);
        std::thread::Builder::new().name("miniutils-term-wait".into()).spawn(move || {
            let code = child.wait().ok().map(|s| s.exit_code());
            *lock(&sh.code) = code;
            sh.exited.store(true, Ordering::Release);
            wk();
        })?;

        Ok(Session { shared, master: pair.master, input, killer, launch })
    }

    /// The emulator (screen, cursor, modes). Hold it briefly: the reading thread waits.
    pub fn parser(&self) -> MutexGuard<'_, Parser> {
        lock(&self.shared.parser)
    }

    /// Sends bytes to the shell (keys, paste). Never waits: the writing thread delivers them.
    pub fn write(&self, bytes: &[u8]) {
        if self.exited().is_some() || bytes.is_empty() {
            return;
        }
        let _ = self.input.send(bytes.to_vec());
    }

    /// New size of the window, in cells.
    pub fn resize(&self, rows: u16, cols: u16) {
        let (rows, cols) = (rows.max(2), cols.max(10));
        let (Ok(r), Ok(c)) = (rows.try_into(), cols.try_into()) else { return };
        {
            let mut p = self.parser();
            if p.screen().size() == (r, c) {
                return;
            }
            p.set_size(r, c);
        }
        if let Err(e) = self.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 }) {
            log::debug!("terminal: resize failed: {e}");
        }
    }

    /// How long since the shell last wrote something.
    pub fn quiet_for(&self) -> Duration {
        lock(&self.shared.last_output).elapsed()
    }

    /// `Some(code)` once the shell has exited (`None` inside: the code is unknown).
    pub fn exited(&self) -> Option<Option<u32>> {
        self.shared.exited.load(Ordering::Acquire).then(|| *lock(&self.shared.code))
    }
}

impl Drop for Session {
    /// Closing the terminal ends the shell (and, with the master dropped, the reading thread).
    fn drop(&mut self) {
        if self.exited().is_none() {
            let _ = self.killer.kill();
        }
    }
}

/// A poisoned lock (a thread panicked holding it) still holds a usable screen.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_cursor_and_status_queries() {
        let mut p = vt100::Parser::new_with_callbacks(5.try_into().unwrap(), 20.try_into().unwrap(), 0, Events::default());
        p.process(b"ab\x1b[6n\x1b[5n\x1b[c\x1b]2;my title\x07\x07");
        let ev = p.callbacks_mut();
        assert_eq!(std::mem::take(&mut ev.replies), b"\x1b[1;3R\x1b[0n\x1b[?1;2c");
        assert_eq!(ev.title.as_deref(), Some("my title"));
        assert!(ev.bell);
    }

    #[test]
    fn launch_from_settings() {
        let l = Launch::from_settings("", "").unwrap();
        assert!(l.args.is_empty());
        assert_eq!(Some(l.dir), dirs::home_dir());
        assert_eq!(l.dir_missing, None);
        assert!(Launch::from_settings("miniutils-shell-that-does-not-exist", "").is_err());
        // "~" is the home folder
        let l = Launch::from_settings("", " ~ ").unwrap();
        assert_eq!((Some(l.dir), l.dir_missing), (dirs::home_dir(), None));
        // a folder that doesn't exist (or a relative one) starts at home, and says so
        let l = Launch::from_settings("", "~/miniutils-folder-that-does-not-exist").unwrap();
        assert_eq!(Some(l.dir), dirs::home_dir());
        assert_eq!(l.dir_missing.as_deref(), Some("~/miniutils-folder-that-does-not-exist"));
        assert!(Launch::from_settings("", "relative/dir").unwrap().dir_missing.is_some());
    }

    #[test]
    fn tilde_expands_to_home() {
        let home = std::env::temp_dir();
        let sub = home.join("miniutils-tilde-test");
        std::fs::create_dir_all(&sub).unwrap();
        assert_eq!(start_dir("~", &home), Some(home.clone()));
        assert_eq!(start_dir("~/miniutils-tilde-test", &home), Some(sub.clone()));
        #[cfg(windows)]
        assert_eq!(start_dir("~\\miniutils-tilde-test", &home), Some(sub.clone()));
        assert_eq!(start_dir("~/miniutils-tilde-missing", &home), None);
        assert_eq!(start_dir("", &home), None);
        let _ = std::fs::remove_dir(sub);
    }

    #[test]
    #[cfg(unix)]
    fn a_big_paste_does_not_wait_for_the_shell() {
        let launch = Launch::from_settings("sh", "/").unwrap();
        let launch = Launch { args: vec!["-c".into(), "sleep 2".into()], ..launch };
        let s = Session::start(launch, 4, 20, 0, || {}).unwrap();
        let paste = vec![b'x'; 1024 * 1024];
        let started = std::time::Instant::now();
        s.write(&paste);
        s.write(b"\r");
        assert!(started.elapsed() < std::time::Duration::from_millis(50), "write waited {:?}", started.elapsed());
    }

    #[test]
    #[cfg(unix)]
    fn runs_a_shell_and_sees_it_exit() {
        let (tx, rx) = std::sync::mpsc::channel();
        let launch = Launch::from_settings("sh -c", "/").unwrap();
        assert_eq!((launch.args.as_slice(), launch.dir.as_path()), (&["-c".to_string()][..], Path::new("/")));
        let launch = Launch { args: vec!["-c".into(), "printf hello".into()], ..launch };
        assert_eq!(launch.name(), "sh");
        let s = Session::start(launch, 4, 20, 100, move || {
            let _ = tx.send(());
        })
        .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while s.exited().is_none() && std::time::Instant::now() < deadline {
            let _ = rx.recv_timeout(std::time::Duration::from_millis(100));
        }
        assert_eq!(s.exited(), Some(Some(0)));
        // the output may arrive a moment after the exit
        while !s.parser().screen().contents().contains("hello") && std::time::Instant::now() < deadline {
            let _ = rx.recv_timeout(std::time::Duration::from_millis(100));
        }
        assert!(s.parser().screen().contents().contains("hello"));
    }
}
