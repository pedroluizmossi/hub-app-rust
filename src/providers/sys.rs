//! CPU, RAM, temperatura da CPU, rede e disco, lidos direto de /proc e /sys.

use std::fs;
use std::time::Instant;

#[derive(Debug, Clone, Default)]
pub struct SysSnapshot {
    /// Uso de CPU em % desde a leitura anterior.
    pub cpu: f32,
    pub mem_used: u64,
    pub mem_total: u64,
    /// °C; 0 = sem sensor.
    pub cpu_temp: f32,
    /// Bytes/s (sem loopback nem interfaces do Docker/VMs).
    pub net_rx: f64,
    pub net_tx: f64,
    pub disk_used: u64,
    pub disk_total: u64,
}

#[derive(Default)]
pub struct SysProvider {
    prev_cpu: Option<(u64, u64)>,
    prev_net: Option<(u64, u64, Instant)>,
}

impl SysProvider {
    pub fn collect(&mut self) -> SysSnapshot {
        let mut s = SysSnapshot::default();
        if let Some(cur) = fs::read_to_string("/proc/stat").ok().and_then(|t| parse_cpu(&t)) {
            s.cpu = cpu_percent(self.prev_cpu, cur);
            self.prev_cpu = Some(cur);
        }
        if let Ok(t) = fs::read_to_string("/proc/meminfo") {
            (s.mem_total, s.mem_used) = parse_mem(&t);
        }
        s.cpu_temp = cpu_temp();
        if let Ok(t) = fs::read_to_string("/proc/net/dev") {
            let (rx, tx) = parse_net(&t);
            let now = Instant::now();
            if let Some((prx, ptx, at)) = self.prev_net {
                let dt = now.duration_since(at).as_secs_f64().max(0.001);
                s.net_rx = rx.saturating_sub(prx) as f64 / dt;
                s.net_tx = tx.saturating_sub(ptx) as f64 / dt;
            }
            self.prev_net = Some((rx, tx, now));
        }
        (s.disk_total, s.disk_used) = disk_usage("/");
        s
    }
}

/// Primeira linha de /proc/stat -> (ocupado, total) acumulados.
pub fn parse_cpu(text: &str) -> Option<(u64, u64)> {
    let line = text.lines().find(|l| l.starts_with("cpu "))?;
    let v: Vec<u64> = line.split_whitespace().skip(1).map(|x| x.parse().unwrap_or(0)).collect();
    let idle = v.get(3).copied().unwrap_or(0) + v.get(4).copied().unwrap_or(0); // idle + iowait
    let total: u64 = v.iter().take(8).sum(); // sem guest (já contado em user)
    Some((total - idle, total))
}

pub fn cpu_percent(prev: Option<(u64, u64)>, cur: (u64, u64)) -> f32 {
    let Some((pb, pt)) = prev else { return 0.0 };
    let dt = cur.1.saturating_sub(pt);
    if dt == 0 {
        return 0.0;
    }
    (cur.0.saturating_sub(pb) as f32 / dt as f32 * 100.0).clamp(0.0, 100.0)
}

/// /proc/meminfo -> (total, usado) em bytes.
pub fn parse_mem(text: &str) -> (u64, u64) {
    let field = |name: &str| {
        text.lines()
            .find(|l| l.starts_with(name))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0)
            * 1024
    };
    let total = field("MemTotal:");
    (total, total.saturating_sub(field("MemAvailable:")))
}

/// /proc/net/dev -> bytes (rx, tx) acumulados das interfaces "de verdade".
pub fn parse_net(text: &str) -> (u64, u64) {
    const SKIP: &[&str] = &["lo", "docker", "veth", "br-", "virbr"];
    let mut rx = 0;
    let mut tx = 0;
    for line in text.lines().skip(2) {
        let Some((name, rest)) = line.split_once(':') else { continue };
        if SKIP.iter().any(|p| name.trim().starts_with(p)) {
            continue;
        }
        let f: Vec<u64> = rest.split_whitespace().map(|x| x.parse().unwrap_or(0)).collect();
        rx += f.first().copied().unwrap_or(0);
        tx += f.get(8).copied().unwrap_or(0);
    }
    (rx, tx)
}

/// Temperatura da CPU pelo hwmon (Intel, AMD, Raspberry).
fn cpu_temp() -> f32 {
    let Ok(dir) = fs::read_dir("/sys/class/hwmon") else { return 0.0 };
    for entry in dir.flatten() {
        let p = entry.path();
        let name = fs::read_to_string(p.join("name")).unwrap_or_default();
        if matches!(name.trim(), "coretemp" | "k10temp" | "zenpower" | "cpu_thermal") {
            return fs::read_to_string(p.join("temp1_input"))
                .ok()
                .and_then(|t| t.trim().parse::<f32>().ok())
                .map(|m| m / 1000.0)
                .unwrap_or(0.0);
        }
    }
    0.0
}

/// (total, usado) em bytes do sistema de arquivos de `path`.
fn disk_usage(path: &str) -> (u64, u64) {
    let c = std::ffi::CString::new(path).unwrap();
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` é uma string C válida e `st` aponta para memória nossa do tamanho certo.
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return (0, 0);
    }
    let bs = st.f_frsize as u64;
    let total = st.f_blocks as u64 * bs;
    (total, total.saturating_sub(st.f_bfree as u64 * bs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu() {
        let a = parse_cpu("cpu  100 0 100 700 100 0 0 0 0 0\ncpu0 1 2 3").unwrap();
        let b = parse_cpu("cpu  200 0 200 800 100 0 0 0 0 0").unwrap();
        assert_eq!(cpu_percent(Some(a), b), 200.0 / 300.0 * 100.0);
        assert_eq!(cpu_percent(None, b), 0.0);
    }

    #[test]
    fn mem() {
        assert_eq!(parse_mem("MemTotal: 1000 kB\nMemFree: 1 kB\nMemAvailable: 400 kB"), (1024000, 614400));
    }

    #[test]
    fn rede_ignora_loopback_e_docker() {
        let t = "h1\nh2\n    lo: 999 0 0 0 0 0 0 0 999 0\n  eth0: 100 0 0 0 0 0 0 0 50 0\ndocker0: 7 0 0 0 0 0 0 0 7 0\n";
        assert_eq!(parse_net(t), (100, 50));
    }
}
