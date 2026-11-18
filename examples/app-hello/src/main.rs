//! MiniUtils example external app.
//!
//! An ordinary program: it reads messages from the hub on standard input (JSON, one per
//! line) and answers on standard output. It has full access, like any of the user's
//! programs: here it reads the home folder and writes a file in the data folder the hub
//! gives it (`MINIUTILS_APP_DATA`). Full protocol: top of `src/apps/external.rs` in the
//! MiniUtils repository.
//!
//! House rules:
//! - standard output belongs to the protocol: debugging goes to standard error
//!   (`eprintln!`), which the hub sends to its log;
//! - input closed (EOF) or `{"type":"stop"}`: exit right away.
//!
//! Language: `start` brings `lang` ("en", "pt-BR"). The texts below are English and are
//! translated with the app's own `i18n/<lang>.po` (the same file the hub uses for the view
//! and the manifest). Apps built on the SDK use `miniutils_app::i18n::Catalog` instead of
//! the small reader here.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::time::Instant;

use serde_json::{Value, json};

/// Sends a message to the hub.
fn send(msg: Value) {
    let mut out = std::io::stdout().lock();
    // with no hub on the other side there is nothing to do: the main loop exits on EOF
    let _ = writeln!(out, "{msg}").and_then(|_| out.flush());
}

fn set(prop: &str, value: impl Into<Value>) {
    send(json!({ "type": "set", "prop": prop, "value": value.into() }));
}

/// Translations of one language (msgid → msgstr), read from a gettext `.po`.
#[derive(Default)]
struct Catalog(HashMap<String, String>);

impl Catalog {
    /// `i18n/<lang>.po` in the app's folder (the process runs there). English, or no file:
    /// empty, and the English texts are shown.
    fn load(lang: &str) -> Catalog {
        if lang == "en" || lang.contains(['/', '\\', '.']) {
            return Catalog::default();
        }
        let text = std::fs::read_to_string(format!("i18n/{lang}.po")).unwrap_or_default();
        // only one-line `msgid "…"` / `msgstr "…"` pairs: enough for this app's file
        let unquote = |s: &str| s.trim().trim_matches('"').replace("\\\"", "\"").replace("\\n", "\n");
        let mut map = HashMap::new();
        let mut id = None;
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("msgid ") {
                id = Some(unquote(rest));
            } else if let (Some(rest), Some(i)) = (line.strip_prefix("msgstr "), id.take()) {
                let s = unquote(rest);
                if !i.is_empty() && !s.is_empty() {
                    map.insert(i, s);
                }
            }
        }
        Catalog(map)
    }

    /// The translation of `msgid`, with each `{}` filled in order by `args`.
    fn tr(&self, msgid: &str, args: &[&dyn std::fmt::Display]) -> String {
        let mut text = self.0.get(msgid).map_or(msgid, String::as_str).to_string();
        for a in args {
            if let Some(i) = text.find("{}") {
                text.replace_range(i..i + 2, &a.to_string());
            }
        }
        text
    }
}

struct App {
    started: Instant,
    data: Option<PathBuf>,
    count: i64,
    show_home: bool,
    cpu_high: bool,
    texts: Catalog,
}

impl App {
    fn state_file(&self) -> Option<PathBuf> {
        self.data.as_ref().map(|d| d.join("state.json"))
    }

    fn load(&mut self) {
        // a change in this format goes through [data] version in the manifest and the
        // `migrated` message, not through reading old file names
        let saved = self.state_file().and_then(|f| std::fs::read_to_string(f).ok());
        let parsed = saved.and_then(|t| serde_json::from_str::<Value>(&t).ok());
        self.count = parsed.and_then(|v| v["count"].as_i64()).unwrap_or(0);
    }

    fn save(&self) {
        if let Some(f) = self.state_file()
            && let Err(e) = std::fs::write(&f, json!({ "count": self.count }).to_string())
        {
            let message = self.texts.tr("could not write {}: {}", &[&f.display(), &e]);
            send(json!({ "type": "error", "message": message }));
        }
    }

    /// The home folder and how many items it has (what a plugin without full access cannot read).
    fn home(&self) -> String {
        if !self.show_home {
            return String::new();
        }
        let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) else {
            return self.texts.tr("no home folder", &[]);
        };
        let home = PathBuf::from(home);
        let n = std::fs::read_dir(&home).map(|d| d.count()).unwrap_or(0);
        self.texts.tr("{} ({} items)", &[&home.display(), &n])
    }

    fn redraw(&self) {
        set("count", self.count);
        set("home", self.home());
        send(json!({ "type": "summary", "text": self.texts.tr("clicks: {}", &[&self.count]) }));
    }

    fn handle(&mut self, msg: &Value) -> bool {
        match msg["type"].as_str().unwrap_or_default() {
            "start" => {
                self.texts = Catalog::load(msg["lang"].as_str().unwrap_or("en"));
                self.data = msg["data_dir"].as_str().filter(|d| !d.is_empty()).map(PathBuf::from);
                self.show_home = msg["settings"]["show_home"].as_bool().unwrap_or(true);
                self.load();
                eprintln!("started on {} (protocol {})", msg["os"], msg["api"]);
                set("status", self.texts.tr("running as its own program", &[]));
                send(json!({ "type": "mood", "mood": "happy" }));
                self.redraw();
            }
            "tick" => {
                let s = self.started.elapsed().as_secs();
                let uptime = format!("{}m{:02}s", s / 60, s % 60);
                set("clock", self.texts.tr("up for {}", &[&uptime]));
            }
            "action" => {
                match msg["name"].as_str().unwrap_or_default() {
                    "count" => self.count += 1,
                    "reset" => self.count = 0,
                    _ => return true,
                }
                self.save();
                self.redraw();
            }
            "settings" => {
                self.show_home = msg["settings"]["show_home"].as_bool().unwrap_or(true);
                self.redraw();
            }
            "event" if msg["topic"] == "sys" => {
                let cpu = msg["data"]["cpu"].as_f64().unwrap_or(0.0);
                set("cpu", format!("{cpu:.0}%"));
                let high = cpu > 80.0;
                if high != self.cpu_high {
                    self.cpu_high = high;
                    send(json!({ "type": "mood", "mood": if high { "busy" } else { "happy" } }));
                    if high {
                        send(json!({ "type": "fact", "kind": "strain", "subject": "CPU", "value": cpu }));
                    }
                }
            }
            "stop" => return false,
            _ => {}
        }
        true
    }
}

fn main() {
    let mut app =
        App { started: Instant::now(), data: None, count: 0, show_home: true, cpu_high: false, texts: Catalog::default() };
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            eprintln!("unreadable message: {line}");
            continue;
        };
        if !app.handle(&msg) {
            break;
        }
    }
}
