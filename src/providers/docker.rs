//! Docker pela CLI: `docker ps -a` e `docker stats` em JSON.

use std::collections::HashMap;

use serde_json::Value;

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

pub fn collect() -> DockerSnapshot {
    let ps = exec::run("docker", &args(&["ps", "-a", "--no-trunc", "--format", "{{json .}}"]));
    let containers = match ps {
        Ok(out) => parse_ps(&out),
        Err(e) => {
            return DockerSnapshot { ok: false, error: first_line(&e.to_string()), ..Default::default() };
        }
    };
    let stats = if containers.iter().any(|c| c.state == "running") {
        exec::run("docker", &args(&["stats", "--no-stream", "--format", "{{json .}}"]))
            .map(|o| parse_stats(&o))
            .unwrap_or_default()
    } else {
        HashMap::new()
    };
    DockerSnapshot { ok: true, error: String::new(), containers, stats }
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
    fn nomes() {
        assert!(valid_container_name("my_app-1.web"));
        assert!(!valid_container_name("-rf"));
        assert!(!valid_container_name("a b"));
    }
}
