//! External apps (`[app]` in the manifest): an executable of their own, with full access,
//! that the host starts when the app is turned on and ends when it is turned off. See the
//! manifest format at the top of `core/plugins.rs`; the screen, the mood and the chat are
//! the same as for plugins with a screen (`apps/plugin/`).
//!
//! ## Protocol (version 1)
//!
//! JSON, **one message per line** (UTF-8, ending in `\n`), always with the `type` field.
//! The app reads from standard input and writes to standard output; standard error goes
//! to the hub's log (`app <id>: …`), so that's where debugging output belongs.
//!
//! From the host to the app (standard input):
//! ```text
//! {"type":"start","api":1,"id":"io.x.app","os":"linux","data_dir":"…","data_version":1,"lang":"en","settings":{…}}
//!                                  always the first message; data_version is the version
//!                                  of the data in the folder (see "Data" below); lang is the
//!                                  interface language ("en" or "pt-BR")
//! {"type":"tick"}                  every [app] every seconds
//! {"type":"action","name":"…","arg":"…"}   the screen called `callback action(string, string)`
//! {"type":"event","topic":"sys","data":{…}}   a reading from a `uses` provider
//! {"type":"settings","settings":{…}}          the user changed one of the app's options
//! {"type":"shown"}                 the hub window opened (a good moment for fresh data)
//! {"type":"stop"}                  about to turn off: exit soon (after 2 s the host kills it)
//! ```
//! Standard input closing (EOF) also means "exit": that's what happens if the hub crashes.
//!
//! From the app to the host (standard output):
//! ```text
//! {"type":"set","prop":"total","value":42}    changes a property of the screen (text,
//!                                  number, bool, list or object, as in the script)
//! {"type":"image","prop":"chart","path":"/absolute/path.png"}   image from disk
//! {"type":"mood","mood":"happy"}   sleep, idle, happy, busy, alarm
//! {"type":"summary","text":"…"}    line in the tray tooltip
//! {"type":"notify","title":"…","body":"…"}
//! {"type":"fact","kind":"strain","subject":"CPU","value":93}   chat between mascots
//! {"type":"error","message":"…"}   error in the cell and the mascot in alarm ("" clears it)
//! {"type":"migrated","version":2}  finished migrating the data to version 2
//! ```
//! A line that isn't protocol JSON goes to the log and is ignored.
//!
//! The process runs in the app's folder, with the variables `MINIUTILS_APP_ID`,
//! `MINIUTILS_APP_DATA` (folder for the app's data, which survives updates; on removal the
//! user chooses whether to delete it) and `MINIUTILS_APP_API` (protocol version).
//!
//! ## Data
//!
//! The `data_dir` folder belongs to the app (`core/appdata.rs`; the hub only keeps
//! `.miniutils.json` in it, with the version). The `version` in the manifest's `[data]` says
//! which format the app writes. If `data_version` (from `start`) is lower than yours, the
//! data is from an old version: migrate it and answer `migrated` with your version. The hub
//! copies the folder before starting the app; if the app exits before `migrated`, the copy
//! is restored and the card shows the error. If the data is from a newer version than the
//! app's, the app isn't even started. If it exits on its own, the app goes into alarm with
//! the exit code; the card's refresh button starts it again.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::core::exec;
use crate::tr;

/// Protocol version this hub speaks.
pub const PROTOCOL: u32 = 1;
/// How long the app has to exit after `stop`.
const STOP_GRACE: Duration = Duration::from_secs(2);
/// Longest line accepted from the app.
const MAX_LINE: usize = 4 << 20;

/// From the host to the app.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToApp {
    Start { api: u32, id: String, os: &'static str, data_dir: String, data_version: u32, lang: &'static str, settings: serde_json::Value },
    Tick,
    Action { name: String, arg: String },
    Event { topic: String, data: serde_json::Value },
    Settings { settings: serde_json::Value },
    Shown,
    Stop,
}

impl ToApp {
    /// The line that goes to the app's input (with the `\n`).
    pub fn line(&self) -> String {
        let mut s = serde_json::to_string(self).unwrap_or_default();
        s.push('\n');
        s
    }
}

/// From the app to the host.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FromApp {
    Set {
        prop: String,
        value: serde_json::Value,
    },
    Image {
        prop: String,
        path: String,
    },
    Mood {
        mood: String,
    },
    Summary {
        text: String,
    },
    Notify {
        title: String,
        #[serde(default)]
        body: String,
    },
    Fact {
        kind: String,
        #[serde(default)]
        subject: String,
        #[serde(default)]
        value: Option<f64>,
    },
    Error {
        message: String,
    },
    Migrated {
        version: u32,
    },
}

/// A line of the app's output: the message, or why it isn't one.
pub fn parse_line(line: &str) -> Result<FromApp, String> {
    serde_json::from_str(line.trim()).map_err(|e| e.to_string())
}

/// What arrives from the process.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    Msg(FromApp),
    /// Exited without the host asking (the message comes ready for the screen).
    Exited(String),
}

/// A running external app.
pub struct Process {
    /// Lines for the thread that writes to the app's input (`None` = already stopped).
    tx: Option<Sender<String>>,
    child: Arc<Mutex<Option<Child>>>,
    /// The host asked it to stop: the process exiting is not a failure.
    stopping: Arc<AtomicBool>,
    /// Becomes `true` when the process is gone for good (exited or was killed).
    ended: Arc<AtomicBool>,
}

/// How to start the process.
pub struct Launch<'a> {
    pub id: &'a str,
    pub program: &'a Path,
    /// Folder it runs in (the app's).
    pub dir: &'a Path,
    pub data_dir: &'a Path,
}

impl Process {
    /// Starts the app. `on` receives each message (on a reader thread, not the UI one).
    pub fn spawn(l: &Launch, on: impl Fn(Incoming) + Send + 'static) -> Result<Process, String> {
        if !l.program.is_file() {
            return Err(tr!("the executable {} doesn't exist", l.program.display()));
        }
        let _ = std::fs::create_dir_all(l.data_dir);
        let mut cmd = exec::command_path(l.program);
        cmd.current_dir(l.dir)
            .env("MINIUTILS_APP_ID", l.id)
            .env("MINIUTILS_APP_DATA", l.data_dir)
            .env("MINIUTILS_APP_API", PROTOCOL.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // its own group: when it is killed, the children the app started go with it
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
        let mut child = cmd.spawn().map_err(|e| tr!("couldn't start {}: {}", l.program.display(), e))?;
        let (stdin, stdout, stderr) = (child.stdin.take(), child.stdout.take(), child.stderr.take());
        let child = Arc::new(Mutex::new(Some(child)));
        let stopping = Arc::new(AtomicBool::new(false));
        let ended = Arc::new(AtomicBool::new(false));
        let name = |what: &str| format!("app-{what}-{}", l.id);

        // writing: the host never waits on a full pipe on the UI thread
        let (tx, rx) = channel::<String>();
        if let Some(mut stdin) = stdin {
            let _ = std::thread::Builder::new().name(name("in")).spawn(move || {
                for line in rx {
                    if stdin.write_all(line.as_bytes()).and_then(|_| stdin.flush()).is_err() {
                        break;
                    }
                }
                // end of the channel: the input closes and the app gets EOF
            });
        }
        // standard error: to the log
        if let Some(stderr) = stderr {
            let id = l.id.to_string();
            let _ = std::thread::Builder::new().name(name("err")).spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    let line: String = line.chars().take(1000).collect();
                    log::info!("app {id}: {line}");
                }
            });
        }
        // standard output: the messages
        if let Some(stdout) = stdout {
            let (id, child, stopping, ended) = (l.id.to_string(), child.clone(), stopping.clone(), ended.clone());
            let _ = std::thread::Builder::new().name(name("out")).spawn(move || {
                let end = read_messages(stdout, &id, &on);
                if !stopping.load(Ordering::SeqCst) {
                    let status = finish(&child, STOP_GRACE);
                    ended.store(true, Ordering::SeqCst);
                    on(Incoming::Exited(match end {
                        Some(e) => e,
                        None => tr!("the app exited on its own ({})", status),
                    }));
                }
            });
        }
        Ok(Process { tx: Some(tx), child, stopping, ended })
    }

    /// The "gone for good" flag, for whoever needs to wait for the process to let go of
    /// the files (deleting the app's data).
    pub fn ended(&self) -> Arc<AtomicBool> {
        self.ended.clone()
    }

    /// The process id while it runs (Settings › System shows its usage).
    pub fn pid(&self) -> Option<u32> {
        if self.ended.load(Ordering::SeqCst) {
            return None;
        }
        self.child.lock().ok()?.as_ref().map(|c| c.id())
    }

    pub fn send(&self, msg: &ToApp) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(msg.line());
        }
    }

    /// Asks it to exit (`stop` and EOF) and, if it doesn't exit in time, kills the group. Doesn't wait.
    pub fn stop(&mut self) {
        let Some(tx) = self.tx.take() else { return };
        self.stopping.store(true, Ordering::SeqCst);
        let _ = tx.send(ToApp::Stop.line());
        drop(tx);
        let (child, ended) = (self.child.clone(), self.ended.clone());
        let _ = std::thread::Builder::new().name("app-stop".into()).spawn(move || {
            finish(&child, STOP_GRACE);
            ended.store(true, Ordering::SeqCst);
        });
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Reads the lines to the end. `Some(error)` if it stopped because of a problem with the app (a line too long).
fn read_messages(stdout: impl Read, id: &str, on: &impl Fn(Incoming)) -> Option<String> {
    let mut reader = BufReader::new(stdout);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.by_ref().take(MAX_LINE as u64).read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => return None,
            Ok(n) if n >= MAX_LINE && buf.last() != Some(&b'\n') => {
                return Some(tr!("the app sent a line longer than {} MiB", MAX_LINE >> 20));
            }
            Ok(_) => {}
        }
        let line = String::from_utf8_lossy(&buf);
        if line.trim().is_empty() {
            continue;
        }
        match parse_line(&line) {
            Ok(msg) => on(Incoming::Msg(msg)),
            Err(e) => {
                let shown: String = line.trim().chars().take(200).collect();
                log::warn!("app {id}: line ignored ({e}): {shown}");
            }
        }
    }
}

/// Waits up to `grace` for the process to exit; then kills the group. Returns the exit status.
fn finish(child: &Mutex<Option<Child>>, grace: Duration) -> String {
    let Some(mut child) = child.lock().ok().and_then(|mut c| c.take()) else { return tr!("already ended").into() };
    let deadline = Instant::now() + grace;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.to_string(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                exec::kill_group(&mut child);
                return tr!("killed by the host").into();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_messages() {
        assert_eq!(ToApp::Tick.line(), "{\"type\":\"tick\"}\n");
        let a = ToApp::Action { name: "sort".into(), arg: "mem".into() }.line();
        assert_eq!(a, "{\"type\":\"action\",\"name\":\"sort\",\"arg\":\"mem\"}\n");
        assert_eq!(
            parse_line(r#"{"type":"set","prop":"n","value":[1,"a"]}"#).unwrap(),
            FromApp::Set { prop: "n".into(), value: serde_json::json!([1, "a"]) }
        );
        assert_eq!(
            parse_line(r#"{"type":"fact","kind":"strain","subject":"CPU","value":93}"#).unwrap(),
            FromApp::Fact { kind: "strain".into(), subject: "CPU".into(), value: Some(93.0) }
        );
        assert_eq!(parse_line(r#"{"type":"notify","title":"hi"}"#).unwrap(), FromApp::Notify { title: "hi".into(), body: String::new() });
        assert!(parse_line("debug print").is_err());
        assert!(parse_line(r#"{"type":"delete_everything"}"#).is_err());
        assert_eq!(parse_line(r#"{"type":"migrated","version":2}"#).unwrap(), FromApp::Migrated { version: 2 });
        let start = ToApp::Start { api: 1, id: "a.b".into(), os: "linux", data_dir: "/d".into(), data_version: 3, lang: "en", settings: serde_json::json!({}) };
        assert!(start.line().contains("\"data_version\":3"));
    }

    #[test]
    fn bad_lines_are_ignored_and_a_huge_line_stops() {
        let got = Mutex::new(Vec::new());
        let on = |i: Incoming| got.lock().unwrap().push(i);
        let text = "junk\n\n{\"type\":\"mood\",\"mood\":\"happy\"}\n{\"type\":\"summary\",\"text\":\"ok\"}";
        assert_eq!(read_messages(text.as_bytes(), "t", &on), None);
        assert_eq!(
            *got.lock().unwrap(),
            [Incoming::Msg(FromApp::Mood { mood: "happy".into() }), Incoming::Msg(FromApp::Summary { text: "ok".into() })]
        );
        let huge = vec![b'x'; MAX_LINE + 10];
        assert!(read_messages(huge.as_slice(), "t", &on).unwrap().contains("MiB"));
    }

    /// A real "app" (a shell script): gets the start, answers and exits on stop.
    #[cfg(unix)]
    #[test]
    fn process_talks_and_stops() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("miniutils-app-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let prog = dir.join("app.sh");
        let script = r#"#!/bin/sh
echo "starting" >&2
while IFS= read -r line; do
  case "$line" in
    *'"start"'*) printf '{"type":"set","prop":"id","value":"%s"}\n' "$MINIUTILS_APP_ID" ;;
    *'"tick"'*) echo '{"type":"mood","mood":"busy"}' ;;
    *'"stop"'*) exit 0 ;;
  esac
done
"#;
        std::fs::write(&prog, script).unwrap();
        std::fs::set_permissions(&prog, std::fs::Permissions::from_mode(0o755)).unwrap();
        let (tx, rx) = channel();
        let data = dir.join("data");
        let launch = Launch { id: "io.x.app", program: &prog, dir: &dir, data_dir: &data };
        let mut p = Process::spawn(&launch, move |i| {
            let _ = tx.send(i);
        })
        .unwrap();
        assert!(data.is_dir(), "the data folder is created");
        p.send(&ToApp::Start {
            api: PROTOCOL,
            id: "io.x.app".into(),
            os: "linux",
            data_dir: String::new(),
            data_version: 1,
            lang: "pt-BR",
            settings: serde_json::json!({}),
        });
        p.send(&ToApp::Tick);
        let t = Duration::from_secs(5);
        assert_eq!(rx.recv_timeout(t).unwrap(), Incoming::Msg(FromApp::Set { prop: "id".into(), value: "io.x.app".into() }));
        assert_eq!(rx.recv_timeout(t).unwrap(), Incoming::Msg(FromApp::Mood { mood: "busy".into() }));
        let ended = p.ended();
        assert!(!ended.load(Ordering::SeqCst));
        p.stop();
        // stopped on request: no "exited on its own"
        assert!(rx.recv_timeout(Duration::from_millis(500)).is_err());
        // and signals when it is gone for good (the hub waits for this to delete the data)
        let until = Instant::now() + Duration::from_secs(5);
        while !ended.load(Ordering::SeqCst) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(ended.load(Ordering::SeqCst));

        // an app that exits on its own becomes a warning
        std::fs::write(&prog, "#!/bin/sh\nexit 3\n").unwrap();
        let (tx, rx) = channel();
        let _p = Process::spawn(&launch, move |i| {
            let _ = tx.send(i);
        })
        .unwrap();
        match rx.recv_timeout(t).unwrap() {
            Incoming::Exited(msg) => assert!(msg.contains('3'), "{msg}"),
            other => panic!("{other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
