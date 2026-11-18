//! Docker through the CLI: `docker ps -a`, `docker stats` and `docker events`, all in JSON.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::core::bus::Poker;
use crate::core::exec;

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct Port {
    pub host: String,
    pub port: u16,
}

impl Port {
    pub fn url(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
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
    /// Docker Compose project (stack), if any.
    pub project: String,
    /// When it last started / stopped (Unix seconds; 0 = unknown). Comes from `docker inspect`.
    pub started_at: i64,
    pub finished_at: i64,
    /// How many times Docker restarted the container (restart policy).
    pub restarts: u32,
    /// The kernel killed it for running out of memory (exit 137 then is not a deliberate stop).
    pub oom_killed: bool,
}

/// What `docker inspect` adds to `docker ps`.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Inspect {
    pub started_at: i64,
    pub finished_at: i64,
    pub restarts: u32,
    pub oom_killed: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize)]
pub struct Stats {
    pub cpu: f32,
    pub mem: u64,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct DockerSnapshot {
    pub ok: bool,
    pub error: String,
    /// The `docker` command doesn't exist on this machine (not a failure: nothing to watch).
    pub missing: bool,
    pub containers: Vec<Container>,
    /// By container name.
    pub stats: HashMap<String, Stats>,
}

/// `docker ps` is only a safety net: changes come from `docker events`.
const PS_EVERY: Duration = Duration::from_secs(15);
/// `docker stats` runs separately, and only when some container is running.
const STATS_EVERY: Duration = Duration::from_secs(5);
/// With the window hidden the data only feeds the tray's mood: each call is a CLI process
/// (and `stats` also waits for the second sample), so it collects more slowly. State
/// changes still arrive right away through `docker events`.
const PS_EVERY_HIDDEN: Duration = Duration::from_secs(60);
const STATS_EVERY_HIDDEN: Duration = Duration::from_secs(30);
/// Deadline of a CLI call: a stuck daemon or an unreachable `DOCKER_HOST` doesn't hold the
/// provider thread (`stats --no-stream` takes 1 to 2 s on its own).
const CLI_TIMEOUT: Duration = Duration::from_secs(10);
const STATS_TIMEOUT: Duration = Duration::from_secs(15);
/// Container actions that change what the mini-app shows.
const RELEVANT: &[&str] = &[
    "create", "start", "restart", "die", "stop", "kill", "oom", "pause", "unpause", "destroy", "rename", "update",
    "health_status",
];

/// State of the Docker provider between readings.
#[derive(Default)]
pub struct DockerProvider {
    containers: Vec<Container>,
    stats: HashMap<String, Stats>,
    ok: bool,
    error: String,
    missing: bool,
    last_ps: Option<Instant>,
    last_stats: Option<Instant>,
    /// `docker inspect` cache by id, along with the state it was read in.
    inspected: HashMap<String, (String, Inspect)>,
    /// The `docker events` thread is alive (it ends by itself with no subscribers).
    watching: Arc<AtomicBool>,
}

impl DockerProvider {
    /// Called every second while there are subscribers. Returns a snapshot only if something was read.
    pub fn collect(&mut self, poker: &Poker, poked: bool) -> Option<DockerSnapshot> {
        let now = Instant::now();
        let (ps_every, stats_every) =
            if poker.visible() { (PS_EVERY, STATS_EVERY) } else { (PS_EVERY_HIDDEN, STATS_EVERY_HIDDEN) };
        let mut changed = false;

        // a read request (event, user action, window opening) forces everything
        if poked || self.last_ps.is_none_or(|t| now - t >= ps_every) {
            self.last_ps = Some(now);
            changed = true;
            match exec::run_timeout("docker", &["ps", "-a", "--no-trunc", "--format", "{{json .}}"], CLI_TIMEOUT) {
                Ok(out) => {
                    self.containers = parse_ps(&out);
                    self.ok = true;
                    self.missing = false;
                    self.error.clear();
                    self.inspect(poked);
                }
                Err(e) => {
                    self.containers.clear();
                    self.ok = false;
                    self.missing = e.not_found();
                    self.error = e.brief();
                }
            }
        }

        // without the command there is nothing to watch; the thread starts once it shows up
        if !self.missing && !self.watching.swap(true, Ordering::Relaxed) {
            let (p, flag) = (poker.clone(), self.watching.clone());
            let spawned = std::thread::Builder::new()
                .name("docker-events".into())
                .spawn(move || super::watch::follow(p, flag, "docker", &events_args(), relevant));
            if let Err(e) = spawned {
                // polling with `ps` keeps working; the next collect tries again
                log::warn!("docker: could not start the events watcher: {e}");
                self.watching.store(false, Ordering::Relaxed);
            }
        }

        if self.containers.iter().any(|c| c.state == "running") {
            if poked || self.last_stats.is_none_or(|t| now - t >= stats_every) {
                self.last_stats = Some(now);
                match exec::run_timeout("docker", &["stats", "--no-stream", "--format", "{{json .}}"], STATS_TIMEOUT) {
                    Ok(out) => {
                        self.stats = parse_stats(&out);
                        changed = true;
                    }
                    // a slow or failed reading keeps the last good one (minus containers that are gone)
                    Err(e) => {
                        log::debug!("docker stats failed: {}", e.brief());
                        let before = self.stats.len();
                        let names: std::collections::HashSet<&str> =
                            self.containers.iter().map(|c| c.name.as_str()).collect();
                        self.stats.retain(|name, _| names.contains(name.as_str()));
                        changed |= self.stats.len() != before;
                    }
                }
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
            missing: self.missing,
            containers: self.containers.clone(),
            stats: self.stats.clone(),
        })
    }

    /// Fills the containers with start/end and restarts. Only re-inspects (in a single
    /// `docker inspect`) the new ones, the ones whose state changed, or all when an event arrived.
    fn inspect(&mut self, all: bool) {
        let stale: Vec<String> = self
            .containers
            .iter()
            .filter(|c| valid_id(&c.id))
            .filter(|c| all || self.inspected.get(&c.id).is_none_or(|(st, _)| *st != c.state))
            .map(|c| c.id.clone())
            .collect();
        if !stale.is_empty() {
            let mut args = vec!["inspect", "--format", INSPECT_FORMAT, "--"];
            args.extend(stale.iter().map(String::as_str));
            // an id that vanished makes inspect exit with an error, but the others come on stdout
            if let Ok(out) = exec::output_timeout("docker", &args, CLI_TIMEOUT) {
                let got = parse_inspect(&out.stdout);
                for c in &self.containers {
                    if let Some(i) = got.get(&c.id) {
                        self.inspected.insert(c.id.clone(), (c.state.clone(), *i));
                    }
                }
            }
        }
        // forget the ones that no longer exist
        let ids: std::collections::HashSet<&str> = self.containers.iter().map(|c| c.id.as_str()).collect();
        self.inspected.retain(|id, _| ids.contains(id.as_str()));
        for c in &mut self.containers {
            if let Some((_, i)) = self.inspected.get(&c.id) {
                c.started_at = i.started_at;
                c.finished_at = i.finished_at;
                c.restarts = i.restarts;
                c.oom_killed = i.oom_killed;
            }
        }
    }
}

/// One line per container: "<id> <restarts> <start> <end> <oom-killed>".
const INSPECT_FORMAT: &str =
    "{{.Id}} {{.RestartCount}} {{.State.StartedAt}} {{.State.FinishedAt}} {{.State.OOMKilled}}";

/// Docker ids are hexadecimal (with --no-trunc, 64 digits).
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_hexdigit())
}

/// Output of `docker inspect --format INSPECT_FORMAT` -> by id.
pub fn parse_inspect(out: &str) -> HashMap<String, Inspect> {
    out.lines()
        .filter_map(|line| {
            let mut f = line.split_whitespace();
            let id = f.next()?;
            let restarts = f.next()?.parse().ok()?;
            let started_at = f.next().and_then(parse_time).unwrap_or(0);
            let finished_at = f.next().and_then(parse_time).unwrap_or(0);
            let oom_killed = f.next() == Some("true");
            Some((id.to_string(), Inspect { started_at, finished_at, restarts, oom_killed }))
        })
        .collect()
}

/// Docker's RFC 3339 -> Unix seconds. "0001-01-01T00:00:00Z" (never) becomes `None`.
/// E.g. "2026-11-09T15:02:34.006791228Z", "2026-11-09T12:02:34-03:00".
pub fn parse_time(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s.trim()).ok().map(|d| d.timestamp()).filter(|&t| t >= 0)
}

/// Seconds -> "40 s", "5 min", "3 h", "2 d" (short, rounded down).
pub fn format_age(secs: i64) -> String {
    let s = secs.max(0);
    match s {
        0..60 => format!("{s} s"),
        60..3600 => format!("{} min", s / 60),
        3600..86400 => format!("{} h", s / 3600),
        _ => format!("{} d", s / 86400),
    }
}

/// Arguments of `docker events`: the filter goes to the daemon, so irrelevant events (every
/// `exec_*` of a healthcheck, for example) don't even wake the thread up.
/// The daemon matches `health_status` by prefix ("health_status: healthy").
fn events_args() -> Vec<String> {
    let mut a: Vec<String> = vec!["events".into(), "--filter".into(), "type=container".into()];
    for action in RELEVANT {
        a.push("--filter".into());
        a.push(format!("event={action}"));
    }
    a.extend(["--format".into(), "{{json .}}".into()]);
    a
}

/// Tells whether a `docker events` line changes what the mini-app shows.
fn relevant(line: &str) -> bool {
    let Ok(j) = serde_json::from_str::<Value>(line) else { return false };
    if j.get("Type").and_then(Value::as_str).is_some_and(|t| t != "container") {
        return false;
    }
    let action = j.get("Action").or(j.get("status")).and_then(Value::as_str).unwrap_or_default();
    // "health_status: healthy" -> "health_status"; "exec_start: sh" is left out
    let name = action.split(':').next().unwrap_or_default().trim();
    RELEVANT.contains(&name)
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
                ..Default::default()
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

/// "127.0.0.1:8090->8080/tcp, [::]:5432->5432/tcp, 6379/tcp" -> published TCP ports,
/// without IPv4/IPv6 duplicates, sorted.
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

/// Bytes -> "184M" / "1.4G" (compact, base 1024).
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

/// Names safe to pass as an argument (they can't start with "-").
pub fn valid_container_name(n: &str) -> bool {
    let mut chars = n.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
}

/// Compose project name: lowercase letters, digits, `-` and `_`, starting with a letter or digit.
pub fn valid_project_name(n: &str) -> bool {
    let mut chars = n.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "_-".contains(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_name() {
        assert!(valid_project_name("shop"));
        assert!(valid_project_name("1app_dev-2"));
        assert!(!valid_project_name(""));
        assert!(!valid_project_name("-p"));
        assert!(!valid_project_name("Shop"));
        assert!(!valid_project_name("a b"));
        assert!(!valid_project_name("a;rm"));
        assert!(!valid_project_name("a.b"));
    }

    #[test]
    fn ports() {
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
        assert_eq!(parse_bytes("garbage"), 0);
        assert_eq!(format_bytes(1610612736.0), "1.5G");
        assert_eq!(format_bytes(184.0 * 1048576.0), "184M");
    }

    #[test]
    fn ps_json() {
        let line = r#"{"ID":"abc","Names":"web","Image":"nginx","State":"exited","Status":"Exited (1) 3 minutes ago","Ports":"","Labels":"a=b,com.docker.compose.project=shop"}"#;
        let c = &parse_ps(line)[0];
        assert_eq!((c.name.as_str(), c.exit_code, c.project.as_str()), ("web", 1, "shop"));
        let h = &parse_ps(r#"{"Names":"db","State":"running","Status":"Up 2 hours (unhealthy)"}"#)[0];
        assert_eq!(h.health, "unhealthy");
    }

    #[test]
    fn stats_json() {
        let s = parse_stats(r#"{"Name":"web","CPUPerc":"12.5%","MemUsage":"184MiB / 15.5GiB"}"#);
        assert_eq!(s["web"], Stats { cpu: 12.5, mem: 184 * 1048576 });
    }

    #[test]
    fn events_that_trigger_a_reading() {
        // container actions that change the screen
        assert!(relevant(r#"{"Type":"container","Action":"start","status":"start"}"#));
        assert!(relevant(r#"{"Type":"container","Action":"die"}"#));
        assert!(relevant(r#"{"Type":"container","Action":"health_status: unhealthy"}"#));
        assert!(relevant(r#"{"Type":"container","Action":"oom"}"#));
        // without "Action", uses "status"
        assert!(relevant(r#"{"status":"stop","Type":"container"}"#));
        // exec, attach and the rest are left out
        assert!(!relevant(r#"{"Type":"container","Action":"exec_start: sh -c ls"}"#));
        assert!(!relevant(r#"{"Type":"container","Action":"attach"}"#));
        assert!(!relevant(r#"{"Type":"image","Action":"start"}"#));
        assert!(!relevant(r#"{"Type":"container"}"#));
        assert!(!relevant("garbage"));
        assert!(!relevant(""));
    }

    #[test]
    fn times() {
        assert_eq!(parse_time("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_time("2026-11-09T15:02:34.006791228Z"), Some(1794236554));
        // same instant with a -03:00 offset
        assert_eq!(parse_time("2026-11-09T12:02:34.5-03:00"), Some(1794236554));
        assert_eq!(parse_time("2024-02-29T00:00:00+01:00"), Some(1709161200));
        assert_eq!(parse_time("0001-01-01T00:00:00Z"), None);
        assert_eq!(parse_time("garbage"), None);
        assert_eq!(parse_time(""), None);
    }

    #[test]
    fn inspect() {
        let out = "abc123 2 2026-11-09T15:02:34.006791228Z 0001-01-01T00:00:00Z false\n\
                   def 0 2026-11-09T15:00:00Z 2026-11-09T16:00:00Z true\n\
                   f00 1 2026-11-09T15:00:00Z\ngarbage\n";
        let i = parse_inspect(out);
        assert_eq!(i.len(), 3);
        assert_eq!(i["abc123"], Inspect { started_at: 1794236554, finished_at: 0, restarts: 2, oom_killed: false });
        assert_eq!(i["def"].finished_at - i["def"].started_at, 3600);
        assert!(i["def"].oom_killed);
        // missing fields at the end: unknown, not an error
        assert_eq!((i["f00"].finished_at, i["f00"].oom_killed), (0, false));
        assert!(valid_id("abc123"));
        assert!(!valid_id("-f"));
        assert!(!valid_id(""));
    }

    #[test]
    fn age() {
        assert_eq!(format_age(-5), "0 s");
        assert_eq!(format_age(42), "42 s");
        assert_eq!(format_age(60), "1 min");
        assert_eq!(format_age(3599), "59 min");
        assert_eq!(format_age(2 * 3600 + 59), "2 h");
        assert_eq!(format_age(3 * 86400 + 7200), "3 d");
    }

    #[test]
    fn names() {
        assert!(valid_container_name("my_app-1.web"));
        assert!(!valid_container_name("-rf"));
        assert!(!valid_container_name("a b"));
    }
}
