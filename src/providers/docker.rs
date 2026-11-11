//! Docker pela CLI: `docker ps -a`, `docker stats` e `docker events`, tudo em JSON.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::core::bus::Poker;
use crate::core::exec;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Port {
    pub host: String,
    pub port: u16,
}

impl Port {
    pub fn url(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Container {
    pub id: String,
    pub name: String,
    pub image: String,
    /// running | exited | paused | restarting | dead | created
    pub state: String,
    pub status: String,
    pub exit_code: i32,
    /// healthy | unhealthy | starting | ""
    pub health: String,
    pub ports: Vec<Port>,
    /// Projeto do Docker Compose (stack), se houver.
    pub project: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Stats {
    pub cpu: f32,
    pub mem: u64,
}

#[derive(Debug, Clone, Default)]
pub struct DockerSnapshot {
    pub ok: bool,
    pub error: String,
    pub containers: Vec<Container>,
    /// Por nome do container.
    pub stats: HashMap<String, Stats>,
}

/// `docker ps` vira só rede de segurança: as mudanças vêm do `docker events`.
const PS_EVERY: Duration = Duration::from_secs(15);
/// `docker stats` roda à parte, e só quando há container rodando.
const STATS_EVERY: Duration = Duration::from_secs(5);
/// Espera antes de reconectar o `docker events`: cresce de 2 s até 30 s.
const RETRY_FIRST: Duration = Duration::from_secs(2);
const RETRY_MAX: Duration = Duration::from_secs(30);
/// Eventos em rajada (ex.: `compose up`) viram uma única leitura.
const DEBOUNCE: Duration = Duration::from_millis(300);
/// Ações de container que mudam o que o mini-app mostra.
const RELEVANT: &[&str] = &[
    "create", "start", "restart", "die", "stop", "kill", "oom", "pause", "unpause", "destroy", "rename", "update",
    "health_status",
];

/// Estado do provedor Docker entre as leituras.
#[derive(Default)]
pub struct DockerProvider {
    containers: Vec<Container>,
    stats: HashMap<String, Stats>,
    ok: bool,
    error: String,
    last_ps: Option<Instant>,
    last_stats: Option<Instant>,
    /// A thread do `docker events` está viva (ela se encerra sozinha sem assinantes).
    watching: Arc<AtomicBool>,
}

impl DockerProvider {
    /// Chamado a cada segundo enquanto há assinantes. Devolve um snapshot só se algo foi lido.
    pub fn collect(&mut self, poker: &Poker, poked: bool) -> Option<DockerSnapshot> {
        if !self.watching.swap(true, Ordering::Relaxed) {
            let (p, flag) = (poker.clone(), self.watching.clone());
            std::thread::Builder::new()
                .name("docker-events".into())
                .spawn(move || watch(p, flag))
                .expect("não consegui criar a thread do docker events");
        }

        let now = Instant::now();
        let mut changed = false;

        // pedido de leitura (evento, ação do usuário) força tudo; senão, ps a cada 15 s
        if poked || self.last_ps.is_none_or(|t| now - t >= PS_EVERY) {
            self.last_ps = Some(now);
            changed = true;
            match exec::run("docker", &args(&["ps", "-a", "--no-trunc", "--format", "{{json .}}"])) {
                Ok(out) => {
                    self.containers = parse_ps(&out);
                    self.ok = true;
                    self.error.clear();
                }
                Err(e) => {
                    self.containers.clear();
                    self.ok = false;
                    self.error = first_line(&e.to_string());
                }
            }
        }

        if self.containers.iter().any(|c| c.state == "running") {
            if poked || self.last_stats.is_none_or(|t| now - t >= STATS_EVERY) {
                self.last_stats = Some(now);
                self.stats = exec::run("docker", &args(&["stats", "--no-stream", "--format", "{{json .}}"]))
                    .map(|o| parse_stats(&o))
                    .unwrap_or_default();
                changed = true;
            }
        } else {
            self.last_stats = None;
            if !self.stats.is_empty() {
                self.stats.clear();
                changed = true;
            }
        }

        changed.then(|| DockerSnapshot {
            ok: self.ok,
            error: self.error.clone(),
            containers: self.containers.clone(),
            stats: self.stats.clone(),
        })
    }
}

/// Lê `docker events` enquanto o provedor tem assinantes; se cair, tenta de novo com espera.
fn watch(poker: Poker, watching: Arc<AtomicBool>) {
    let mut retry = RETRY_FIRST;
    let mut first = true;
    while poker.subscribed() {
        let started = Instant::now();
        let spawned = Command::new("docker")
            .args(["events", "--filter", "type=container", "--format", "{{json .}}"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let Ok(mut child) = spawned else {
            if !sleep_subscribed(&poker, retry) {
                break;
            }
            retry = (retry * 2).min(RETRY_MAX);
            continue;
        };
        // reconectou: eventos perdidos enquanto estava fora são lidos num ps
        if !first {
            poker.poke();
        }
        first = false;

        if let Some(stdout) = child.stdout.take() {
            let (tx, rx) = mpsc::channel::<String>();
            std::thread::spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            });
            let mut due: Option<Instant> = None;
            loop {
                if !poker.subscribed() {
                    break;
                }
                match rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(line) => {
                        if relevant(&line) && due.is_none() {
                            due = Some(Instant::now() + DEBOUNCE);
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break, // docker events terminou
                }
                if due.is_some_and(|t| Instant::now() >= t) {
                    due = None;
                    poker.poke();
                }
            }
        }
        let _ = child.kill();
        let _ = child.wait();

        // rodou um bom tempo: a próxima queda começa a espera de novo em 2 s
        if started.elapsed() >= RETRY_MAX {
            retry = RETRY_FIRST;
        }
        if !sleep_subscribed(&poker, retry) {
            break;
        }
        retry = (retry * 2).min(RETRY_MAX);
    }
    watching.store(false, Ordering::Relaxed);
}

/// Dorme `d`, a menos que o provedor perca os assinantes. `false` se perdeu.
fn sleep_subscribed(poker: &Poker, d: Duration) -> bool {
    let end = Instant::now() + d;
    while Instant::now() < end {
        if !poker.subscribed() {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    poker.subscribed()
}

/// Diz se uma linha do `docker events` muda o que o mini-app mostra.
fn relevant(line: &str) -> bool {
    let Ok(j) = serde_json::from_str::<Value>(line) else { return false };
    if j.get("Type").and_then(Value::as_str).is_some_and(|t| t != "container") {
        return false;
    }
    let action = j.get("Action").or(j.get("status")).and_then(Value::as_str).unwrap_or_default();
    // "health_status: healthy" -> "health_status"; "exec_start: sh" fica de fora
    let name = action.split(':').next().unwrap_or_default().trim();
    RELEVANT.contains(&name)
}

fn args(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or_default().chars().take(160).collect()
}

fn str_of(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or_default().to_string()
}

pub fn parse_ps(out: &str) -> Vec<Container> {
    out.lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .map(|j| {
            let status = str_of(&j, "Status");
            let health = match str_of(&j, "HealthStatus").as_str() {
                "" | "none" if status.contains("(unhealthy)") => "unhealthy".to_string(),
                "" | "none" if status.contains("(healthy)") => "healthy".to_string(),
                "" | "none" if status.contains("(health: starting)") => "starting".to_string(),
                "" | "none" => String::new(),
                h => h.to_string(),
            };
            let labels = str_of(&j, "Labels");
            Container {
                id: str_of(&j, "ID"),
                name: str_of(&j, "Names"),
                image: str_of(&j, "Image"),
                state: str_of(&j, "State"),
                exit_code: exit_code(&status),
                status,
                health,
                ports: parse_ports(&str_of(&j, "Ports")),
                project: labels
                    .split(',')
                    .find_map(|l| l.strip_prefix("com.docker.compose.project="))
                    .unwrap_or_default()
                    .to_string(),
            }
        })
        .collect()
}

/// "Exited (137) 2 hours ago" -> 137
fn exit_code(status: &str) -> i32 {
    status
        .split_once("Exited (")
        .and_then(|(_, r)| r.split_once(')'))
        .and_then(|(n, _)| n.parse().ok())
        .unwrap_or(0)
}

pub fn parse_stats(out: &str) -> HashMap<String, Stats> {
    out.lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .map(|j| {
            let cpu = str_of(&j, "CPUPerc").trim_end_matches('%').parse().unwrap_or(0.0);
            let mem_usage = str_of(&j, "MemUsage");
            let mem = parse_bytes(mem_usage.split('/').next().unwrap_or_default());
            (str_of(&j, "Name"), Stats { cpu, mem })
        })
        .collect()
}

/// "1.4GiB" | "184MiB" | "18.5kB" -> bytes
pub fn parse_bytes(text: &str) -> u64 {
    let t = text.trim();
    let split = t.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(t.len());
    let (num, unit) = t.split_at(split);
    let n: f64 = num.trim().parse().unwrap_or(0.0);
    let mult: f64 = match unit.trim().to_ascii_uppercase().as_str() {
        "B" => 1.0,
        "KB" => 1e3,
        "MB" => 1e6,
        "GB" => 1e9,
        "TB" => 1e12,
        "KIB" => 1024.0,
        "MIB" => 1048576.0,
        "GIB" => 1073741824.0,
        "TIB" => 1099511627776.0,
        _ => 0.0,
    };
    (n * mult) as u64
}

/// "127.0.0.1:8090->8080/tcp, [::]:5432->5432/tcp, 6379/tcp" -> portas TCP publicadas,
/// sem duplicar IPv4/IPv6, ordenadas.
pub fn parse_ports(text: &str) -> Vec<Port> {
    let mut out: Vec<Port> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for part in text.split(',').map(str::trim) {
        let Some((left, right)) = part.split_once("->") else { continue };
        if !right.ends_with("/tcp") {
            continue;
        }
        let (ip, port) = match left.rsplit_once(':') {
            Some((ip, p)) => (ip, p),
            None => ("", left),
        };
        let Ok(port) = port.split('-').next().unwrap_or_default().parse::<u16>() else { continue };
        let wildcard = matches!(ip, "" | "0.0.0.0" | "[::]" | "::");
        let local = wildcard || ip == "127.0.0.1" || ip == "[::1]";
        let host = if wildcard { "localhost".to_string() } else { ip.to_string() };
        let key = format!("{}:{port}", if local { "local" } else { &host });
        if seen.insert(key) {
            out.push(Port { host, port });
        }
    }
    out.sort_by_key(|p| p.port);
    out
}

/// Bytes -> "184M" / "1.4G" (compacto, base 1024).
pub fn format_bytes(bytes: f64) -> String {
    if bytes <= 0.0 {
        return "0".into();
    }
    for (suffix, size) in [("T", 1099511627776.0), ("G", 1073741824.0), ("M", 1048576.0), ("K", 1024.0)] {
        if bytes >= size {
            let v = bytes / size;
            return if v >= 10.0 { format!("{v:.0}{suffix}") } else { format!("{v:.1}{suffix}") };
        }
    }
    format!("{bytes:.0}B")
}

/// Nomes seguros para passar como argumento (não podem começar com "-").
pub fn valid_container_name(n: &str) -> bool {
    let mut chars = n.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portas() {
        let p = parse_ports("127.0.0.1:8090->8080/tcp, 0.0.0.0:5432->5432/tcp, [::]:5432->5432/tcp, 6379/tcp, 0.0.0.0:53->53/udp");
        assert_eq!(p, vec![
            Port { host: "localhost".into(), port: 5432 },
            Port { host: "127.0.0.1".into(), port: 8090 },
        ]);
    }

    #[test]
    fn bytes() {
        assert_eq!(parse_bytes("1.5GiB"), 1610612736);
        assert_eq!(parse_bytes("18.5kB"), 18500);
        assert_eq!(parse_bytes("lixo"), 0);
        assert_eq!(format_bytes(1610612736.0), "1.5G");
        assert_eq!(format_bytes(184.0 * 1048576.0), "184M");
    }

    #[test]
    fn ps_json() {
        let line = r#"{"ID":"abc","Names":"web","Image":"nginx","State":"exited","Status":"Exited (1) 3 minutes ago","Ports":"","Labels":"a=b,com.docker.compose.project=loja"}"#;
        let c = &parse_ps(line)[0];
        assert_eq!((c.name.as_str(), c.exit_code, c.project.as_str()), ("web", 1, "loja"));
        let h = &parse_ps(r#"{"Names":"db","State":"running","Status":"Up 2 hours (unhealthy)"}"#)[0];
        assert_eq!(h.health, "unhealthy");
    }

    #[test]
    fn stats_json() {
        let s = parse_stats(r#"{"Name":"web","CPUPerc":"12.5%","MemUsage":"184MiB / 15.5GiB"}"#);
        assert_eq!(s["web"], Stats { cpu: 12.5, mem: 184 * 1048576 });
    }

    #[test]
    fn eventos_que_disparam_leitura() {
        // ações de container que mudam a tela
        assert!(relevant(r#"{"Type":"container","Action":"start","status":"start"}"#));
        assert!(relevant(r#"{"Type":"container","Action":"die"}"#));
        assert!(relevant(r#"{"Type":"container","Action":"health_status: unhealthy"}"#));
        assert!(relevant(r#"{"Type":"container","Action":"oom"}"#));
        // sem "Action", usa "status"
        assert!(relevant(r#"{"status":"stop","Type":"container"}"#));
        // exec, anexos e o resto ficam de fora
        assert!(!relevant(r#"{"Type":"container","Action":"exec_start: sh -c ls"}"#));
        assert!(!relevant(r#"{"Type":"container","Action":"attach"}"#));
        assert!(!relevant(r#"{"Type":"image","Action":"start"}"#));
        assert!(!relevant(r#"{"Type":"container"}"#));
        assert!(!relevant("lixo"));
        assert!(!relevant(""));
    }

    #[test]
    fn nomes() {
        assert!(valid_container_name("my_app-1.web"));
        assert!(!valid_container_name("-rf"));
        assert!(!valid_container_name("a b"));
    }
}
