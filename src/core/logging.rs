//! Log of what happens (warnings, errors, panics) to a file and to the terminal.
//!
//! The app lives in the tray, without a terminal, and on Windows the release build has no
//! console at all: without a file, an error vanishes without a trace. Every line goes to
//! both places:
//! - the file `miniutils.log` in `os::log_dir()` (`~/.local/state/miniutils/` on Linux,
//!   `%LOCALAPPDATA%\miniutils\logs\` on Windows); past `MAX_SIZE`, it becomes
//!   `miniutils.1.log` (the previous one is discarded) and a new one starts;
//! - standard error, for whoever runs it from a terminal (on Linux the journal gets it too).
//!
//! The code uses the macros of the `log` crate (`log::warn!`, `log::info!`…). MiniUtils
//! itself logs from `info` up (`debug` with `MINIUTILS_DEBUG=1` or `--plugin-dev`, which
//! thus shows the script's `debug()`); libraries, only warnings and errors. A panic in any
//! thread is logged too, with its location and backtrace.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use log::{Level, LevelFilter, Log, Metadata, Record};

/// Size from which the file is replaced by a new one.
const MAX_SIZE: u64 = 1 << 20;
const FILE_NAME: &str = "miniutils.log";
const OLD_NAME: &str = "miniutils.1.log";
/// After the file could not be opened (disk full, locked by an antivirus), how long until
/// the next try: the log does not stop for good, and a broken disk is not hit on every line.
const RETRY: Duration = Duration::from_secs(30);

struct Logger {
    /// Our level (`info` or `debug`).
    level: LevelFilter,
    file: Mutex<FileLog>,
}

/// The log file, reopened by itself when it was lost.
struct FileLog {
    /// Where the file goes (`None`: no log folder on this system).
    dir: Option<PathBuf>,
    sink: Option<Sink>,
    /// Last time opening the file failed.
    failed_at: Option<Instant>,
}

impl FileLog {
    fn new(dir: Option<PathBuf>) -> Self {
        let mut f = FileLog { dir, sink: None, failed_at: None };
        f.reopen(Instant::now());
        f
    }

    /// Opens the file if there is none and the last failure is old enough.
    fn reopen(&mut self, now: Instant) {
        if self.sink.is_some() || self.failed_at.is_some_and(|t| now.saturating_duration_since(t) < RETRY) {
            return;
        }
        let Some(dir) = &self.dir else { return };
        self.sink = Sink::open(dir);
        self.failed_at = if self.sink.is_some() { None } else { Some(now) };
    }

    fn write(&mut self, line: &str, now: Instant) {
        self.reopen(now);
        if let Some(sink) = self.sink.take() {
            self.sink = sink.write(line);
            if self.sink.is_none() {
                self.failed_at = Some(now);
            }
        }
    }
}

struct Sink {
    dir: PathBuf,
    file: File,
    size: u64,
}

impl Sink {
    fn open(dir: &Path) -> Option<Sink> {
        std::fs::create_dir_all(dir).ok()?;
        let path = dir.join(FILE_NAME);
        // the full file becomes the old one; if it can't be renamed (another program holds
        // it on Windows), keep appending and try again after another MAX_SIZE
        let stuck = std::fs::metadata(&path).is_ok_and(|m| m.len() >= MAX_SIZE)
            && std::fs::rename(&path, dir.join(OLD_NAME)).is_err();
        let file = OpenOptions::new().create(true).append(true).open(&path).ok()?;
        let size = if stuck { 0 } else { file.metadata().map(|m| m.len()).unwrap_or(0) };
        Some(Sink { dir: dir.to_path_buf(), file, size })
    }

    fn write(mut self, line: &str) -> Option<Sink> {
        let _ = self.file.write_all(line.as_bytes());
        self.size += line.len() as u64;
        if self.size < MAX_SIZE {
            return Some(self);
        }
        // over the limit: close it (Windows does not rename an open file) and start another
        let dir = self.dir;
        drop(self.file);
        Sink::open(&dir)
    }
}

impl Log for Logger {
    fn enabled(&self, meta: &Metadata) -> bool {
        let ours = meta.target().starts_with(env!("CARGO_CRATE_NAME"));
        meta.level() <= if ours { self.level } else { LevelFilter::Warn }
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format_line(
            &chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f").to_string(),
            record.level(),
            record.target(),
            &record.args().to_string(),
        );
        eprint!("{line}");
        self.file.lock().unwrap_or_else(|e| e.into_inner()).write(&line, Instant::now());
    }

    fn flush(&self) {
        if let Some(sink) = self.file.lock().unwrap_or_else(|e| e.into_inner()).sink.as_mut() {
            let _ = sink.file.flush();
        }
    }
}

/// "2026-11-11 14:03:22.120 WARN  core::config: message\n" (without the leading
/// "miniutils::" of the module, which is the same on every line).
fn format_line(time: &str, level: Level, target: &str, msg: &str) -> String {
    let prefix = concat!(env!("CARGO_CRATE_NAME"), "::");
    let target = target.strip_prefix(prefix).unwrap_or(target);
    format!("{time} {level:<5} {target}: {msg}\n")
}

/// Turns on logging and the panic hook. Call once, at the start of the app (terminal
/// commands, such as the store and `--help`, don't need it). `debug` also logs the details
/// (on its own with `MINIUTILS_DEBUG=1`). Returns where the file is.
pub fn init(debug: bool) -> Option<PathBuf> {
    let debug = debug || std::env::var_os("MINIUTILS_DEBUG").is_some();
    let level = if debug { LevelFilter::Debug } else { LevelFilter::Info };
    let file = FileLog::new(super::os::log_dir());
    let path = file.sink.as_ref().map(|s| s.dir.join(FILE_NAME));
    let logger = Box::leak(Box::new(Logger { level, file: Mutex::new(file) }));
    if log::set_logger(logger).is_ok() {
        log::set_max_level(level.max(LevelFilter::Warn));
    }
    std::panic::set_hook(Box::new(|info| {
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("unnamed");
        let trace = std::backtrace::Backtrace::force_capture();
        log::error!("panic in thread \"{name}\": {info}\n{trace}");
        log::logger().flush();
    }));
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_without_the_crate_name() {
        let l = format_line("2026-11-11 10:00:00.000", Level::Warn, "miniutils::core::config", "oops");
        assert_eq!(l, "2026-11-11 10:00:00.000 WARN  core::config: oops\n");
        let l = format_line("t", Level::Error, "winit::window", "x");
        assert_eq!(l, "t ERROR winit::window: x\n");
    }

    #[test]
    fn rotates_the_file_when_full() {
        let dir = std::env::temp_dir().join(format!("miniutils-log-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut sink = Sink::open(&dir);
        let line = "x".repeat(1000) + "\n";
        for _ in 0..(MAX_SIZE / 1000 + 5) {
            sink = sink.and_then(|s| s.write(&line));
        }
        assert!(dir.join(OLD_NAME).exists(), "the full file became the old one");
        let now = std::fs::metadata(dir.join(FILE_NAME)).unwrap().len();
        assert!(now < MAX_SIZE, "the current one started over");
        drop(sink);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn opens_the_file_again_after_a_failure() {
        let base = std::env::temp_dir().join(format!("miniutils-log-retry-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        // the log folder is taken by a file: opening fails
        let dir = base.join("logs");
        std::fs::write(&dir, "not a folder").unwrap();
        let mut log = FileLog::new(Some(dir.clone()));
        let t0 = Instant::now();
        assert!(log.sink.is_none());
        std::fs::remove_file(&dir).unwrap();
        // too soon: not tried again yet
        log.write("lost\n", t0);
        assert!(log.sink.is_none());
        // after RETRY it opens and writes
        log.write("back\n", t0 + RETRY);
        assert!(log.sink.is_some());
        assert_eq!(std::fs::read_to_string(dir.join(FILE_NAME)).unwrap(), "back\n");
        drop(log);
        std::fs::remove_dir_all(&base).unwrap();
    }
}
