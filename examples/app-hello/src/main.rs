//! App externo de exemplo do MiniUtils.
//!
//! Um programa comum: lê mensagens do hub na entrada padrão (JSON, uma por linha) e
//! responde na saída padrão. Tem acesso total, como qualquer programa do usuário: aqui ele
//! lê a pasta pessoal e grava um arquivo na pasta de dados que o hub dá
//! (`MINIUTILS_APP_DATA`). Protocolo completo: topo de `src/apps/external.rs` no repositório
//! do MiniUtils.
//!
//! Regras de convivência:
//! - a saída padrão é só do protocolo: depuração vai para a saída de erro (`eprintln!`),
//!   que o hub manda para o registro dele;
//! - entrada fechada (EOF) ou `{"type":"stop"}`: saia logo.

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::time::Instant;

use serde_json::{Value, json};

/// Manda uma mensagem ao hub.
fn send(msg: Value) {
    let mut out = std::io::stdout().lock();
    // sem o hub do outro lado não há o que fazer: o laço principal sai no EOF
    let _ = writeln!(out, "{msg}").and_then(|_| out.flush());
}

fn set(prop: &str, value: impl Into<Value>) {
    send(json!({ "type": "set", "prop": prop, "value": value.into() }));
}

struct App {
    started: Instant,
    data: Option<PathBuf>,
    count: i64,
    show_home: bool,
    cpu_high: bool,
}

impl App {
    fn state_file(&self) -> Option<PathBuf> {
        self.data.as_ref().map(|d| d.join("estado.json"))
    }

    fn load(&mut self) {
        let saved = self.state_file().and_then(|f| std::fs::read_to_string(f).ok());
        let parsed = saved.and_then(|t| serde_json::from_str::<Value>(&t).ok());
        self.count = parsed.and_then(|v| v["count"].as_i64()).unwrap_or(0);
    }

    fn save(&self) {
        if let Some(f) = self.state_file() {
            if let Err(e) = std::fs::write(&f, json!({ "count": self.count }).to_string()) {
                send(json!({ "type": "error", "message": format!("não consegui gravar {}: {e}", f.display()) }));
            }
        }
    }

    /// A pasta pessoal e quantos itens ela tem (o que um plugin sem acesso total não lê).
    fn home(&self) -> String {
        if !self.show_home {
            return String::new();
        }
        let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) else {
            return "sem pasta pessoal".into();
        };
        let home = PathBuf::from(home);
        let n = std::fs::read_dir(&home).map(|d| d.count()).unwrap_or(0);
        format!("{} ({n} itens)", home.display())
    }

    fn redraw(&self) {
        set("count", self.count);
        set("home", self.home());
        send(json!({ "type": "summary", "text": format!("{} clique(s)", self.count) }));
    }

    fn handle(&mut self, msg: &Value) -> bool {
        match msg["type"].as_str().unwrap_or_default() {
            "start" => {
                self.data = msg["data_dir"].as_str().filter(|d| !d.is_empty()).map(PathBuf::from);
                self.show_home = msg["settings"]["show_home"].as_bool().unwrap_or(true);
                self.load();
                eprintln!("iniciado no {} (protocolo {})", msg["os"], msg["api"]);
                set("status", "rodando como programa próprio");
                send(json!({ "type": "mood", "mood": "happy" }));
                self.redraw();
            }
            "tick" => {
                let s = self.started.elapsed().as_secs();
                set("clock", format!("ligado há {}m{:02}s", s / 60, s % 60));
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
    let mut app = App { started: Instant::now(), data: None, count: 0, show_home: true, cpu_high: false };
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            eprintln!("mensagem ilegível: {line}");
            continue;
        };
        if !app.handle(&msg) {
            break;
        }
    }
}
