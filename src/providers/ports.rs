//! Portas abertas: o que está escutando em TCP e UDP.
//!
//! No Linux, pelo `ss` (iproute2). Sem root, o `ss` só diz o processo das portas do
//! próprio usuário; as outras vêm sem nome nem pid (serviços do sistema, containers).
//!
//! No Windows, pelo `netstat -ano` (que sempre dá o pid) mais o nome e a linha de comando
//! do processo; de processos de outro usuário ou do sistema a linha de comando vem vazia.

use std::collections::BTreeMap;
use std::time::Duration;

use crate::core::exec;

/// Uma porta escutando (endereços IPv4 e IPv6 do mesmo processo viram uma entrada só).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Listener {
    /// "tcp" | "udp"
    pub proto: String,
    pub port: u16,
    /// Endereços em que escuta: "127.0.0.1", "::1", "0.0.0.0", "*"…
    pub addrs: Vec<String>,
    /// Nome do programa ("" = de outro usuário, o `ss` não mostra).
    pub process: String,
    /// Linha de comando completa ("" se não deu para ler).
    pub cmd: String,
    /// 0 = desconhecido.
    pub pid: u32,
}

impl Listener {
    /// Só aceita conexões da própria máquina.
    pub fn local_only(&self) -> bool {
        !self.addrs.is_empty() && self.addrs.iter().all(|a| is_loopback(a))
    }

    /// Escuta em todas as interfaces: qualquer um na rede alcança.
    pub fn exposed(&self) -> bool {
        self.addrs.iter().any(|a| is_wildcard(a))
    }

    /// Endereço para abrir no navegador (só TCP).
    pub fn url(&self) -> Option<String> {
        if self.proto != "tcp" {
            return None;
        }
        // escuta em local ou em tudo: localhost serve; senão, o IP específico
        let host = if self.local_only() || self.exposed() {
            "localhost".to_string()
        } else {
            let a = self.addrs.first()?;
            if a.contains(':') { format!("[{a}]") } else { a.clone() }
        };
        Some(format!("http://{host}:{}", self.port))
    }
}

pub fn is_loopback(a: &str) -> bool {
    a.starts_with("127.") || a == "::1" || a == "localhost"
}

pub fn is_wildcard(a: &str) -> bool {
    matches!(a, "*" | "0.0.0.0" | "::" | "[::]")
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct PortsSnapshot {
    pub ok: bool,
    /// O `ss` não está instalado.
    pub missing: bool,
    pub error: String,
    pub listeners: Vec<Listener>,
}

#[cfg(not(windows))]
pub fn collect() -> PortsSnapshot {
    match exec::run_timeout("ss", &["-H", "-l", "-n", "-p", "-t", "-u"], Duration::from_secs(5)) {
        Ok(out) => {
            let mut listeners = parse(&out);
            for l in &mut listeners {
                if l.pid > 0
                    && let Some((name, cmd)) = read_cmdline(l.pid) {
                        // o `ss` corta o nome em 15 letras; a linha de comando tem o inteiro
                        if !name.is_empty() {
                            l.process = name;
                        }
                        l.cmd = cmd;
                    }
            }
            PortsSnapshot { ok: true, listeners, ..Default::default() }
        }
        Err(e) => PortsSnapshot { ok: false, missing: e.not_found(), error: e.brief(), listeners: Vec::new() },
    }
}

#[cfg(windows)]
pub fn collect() -> PortsSnapshot {
    match exec::run_timeout("netstat", &["-a", "-n", "-o"], Duration::from_secs(10)) {
        Ok(out) => {
            let mut listeners = parse_netstat(&out);
            fill_processes(&mut listeners);
            PortsSnapshot { ok: true, listeners, ..Default::default() }
        }
        Err(e) => PortsSnapshot { ok: false, missing: e.not_found(), error: e.brief(), listeners: Vec::new() },
    }
}

/// Nome do programa e linha de comando de cada pid, pelo `sysinfo`.
#[cfg(windows)]
fn fill_processes(listeners: &mut [Listener]) {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let mut pids: Vec<Pid> = listeners.iter().filter(|l| l.pid > 0).map(|l| Pid::from_u32(l.pid)).collect();
    pids.sort();
    pids.dedup();
    let Ok(mut sys) = System::new() else { return };
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&pids),
        true,
        ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always),
    );
    for l in listeners {
        let Some(p) = sys.process(Pid::from_u32(l.pid)) else { continue };
        let name = p.name().to_string_lossy();
        l.process = name.strip_suffix(".exe").unwrap_or(&name).to_string();
        l.cmd = p.cmd().iter().map(|a| a.to_string_lossy()).collect::<Vec<_>>().join(" ");
    }
}

/// Lê a saída de `netstat -a -n -o`:
/// `  TCP    127.0.0.1:5432    0.0.0.0:0    LISTENING    1234`
/// `  UDP    [::]:5353         *:*                       2345`
#[cfg(any(windows, test))]
pub fn parse_netstat(out: &str) -> Vec<Listener> {
    // (proto, porta, pid) → endereços
    let mut merged: BTreeMap<(String, u16, u32), Vec<String>> = BTreeMap::new();
    for line in out.lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        let (proto, local, pid) = match cols.as_slice() {
            // escutando = sem ponta remota. Não dá para ler a coluna do estado: o texto
            // dela ("LISTENING") muda com o idioma do Windows
            ["TCP", local, remote, _state, pid] if remote.ends_with(":0") => ("tcp", local, pid),
            ["UDP", local, _remote, pid] => ("udp", local, pid),
            _ => continue,
        };
        let Some((addr, port)) = local.rsplit_once(':') else { continue };
        let (Ok(port), Ok(pid)) = (port.parse::<u16>(), pid.parse::<u32>()) else { continue };
        // "[fe80::1%12]" → "fe80::1"
        let addr = addr.strip_prefix('[').and_then(|a| a.strip_suffix(']')).unwrap_or(addr);
        let addr = addr.split('%').next().unwrap_or(addr);
        let addrs = merged.entry((proto.to_string(), port, pid)).or_default();
        if !addrs.iter().any(|a| a == addr) {
            addrs.push(addr.to_string());
        }
    }
    merged
        .into_iter()
        .map(|((proto, port, pid), addrs)| Listener { proto, port, addrs, pid, ..Default::default() })
        .collect()
}

/// Nome do programa e linha de comando, de `/proc/<pid>/cmdline`.
#[cfg(not(windows))]
fn read_cmdline(pid: u32) -> Option<(String, String)> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let args: Vec<String> =
        raw.split(|b| *b == 0).filter(|a| !a.is_empty()).map(|a| String::from_utf8_lossy(a).into_owned()).collect();
    let first = args.first()?;
    let name = first.rsplit('/').next().unwrap_or(first).to_string();
    Some((name, args.join(" ")))
}

/// Lê a saída de `ss -H -l -n -p -t -u`:
/// `tcp LISTEN 0 128 127.0.0.1:5432 0.0.0.0:* users:(("postgres",pid=12,fd=5))`
#[cfg(any(not(windows), test))]
pub fn parse(out: &str) -> Vec<Listener> {
    // (proto, porta, pid, processo) → endereços
    let mut merged: BTreeMap<(String, u16, u32, String), Vec<String>> = BTreeMap::new();
    for line in out.lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 6 {
            continue;
        }
        let proto = cols[0].to_string();
        if proto != "tcp" && proto != "udp" {
            continue;
        }
        let Some((addr, port)) = cols[4].rsplit_once(':') else { continue };
        let Ok(port) = port.parse::<u16>() else { continue };
        // "127.0.0.53%lo" → "127.0.0.53"; "[::1]" → "::1"
        let addr = addr.split('%').next().unwrap_or(addr);
        let addr = addr.strip_prefix('[').and_then(|a| a.strip_suffix(']')).unwrap_or(addr);
        let (process, pid) = cols.get(6).map(|u| parse_users(u)).unwrap_or_default();
        let addrs = merged.entry((proto, port, pid, process)).or_default();
        if !addrs.iter().any(|a| a == addr) {
            addrs.push(addr.to_string());
        }
    }
    merged
        .into_iter()
        .map(|((proto, port, pid, process), addrs)| Listener { proto, port, addrs, process, cmd: String::new(), pid })
        .collect()
}

/// `users:(("postgres",pid=12,fd=5),(…))` → ("postgres", 12): só o primeiro processo.
#[cfg(any(not(windows), test))]
fn parse_users(s: &str) -> (String, u32) {
    let name = s.split('"').nth(1).unwrap_or_default().to_string();
    let pid = s
        .split("pid=")
        .nth(1)
        .and_then(|r| r.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(0);
    (name, pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
tcp LISTEN 0 4096 127.0.0.1:5432 0.0.0.0:*
tcp LISTEN 0 511 127.0.0.1:47836 0.0.0.0:* users:((\"tidal-hifi\",pid=25284,fd=117))
tcp LISTEN 0 50 *:1716 *:* users:((\"kdeconnectd\",pid=3038,fd=17))
tcp LISTEN 0 50 0.0.0.0:1716 0.0.0.0:* users:((\"kdeconnectd\",pid=3038,fd=18))
tcp LISTEN 0 4096 [::1]:631 [::]:*
tcp LISTEN 0 4096 127.0.0.1:631 0.0.0.0:*
udp UNCONN 0 0 127.0.0.53%lo:53 0.0.0.0:*
tcp LISTEN 0 5 100.105.101.45:18449 0.0.0.0:* users:((\"socat\",pid=2665,fd=5))
lixo
";

    #[test]
    fn le_e_junta_ipv4_e_ipv6() {
        let ls = parse(SAMPLE);
        assert_eq!(ls.len(), 6);
        let pg = ls.iter().find(|l| l.port == 5432).unwrap();
        assert_eq!((pg.process.as_str(), pg.pid), ("", 0));
        assert!(pg.local_only() && !pg.exposed());
        let cups = ls.iter().find(|l| l.port == 631).unwrap();
        assert_eq!(cups.addrs, ["::1", "127.0.0.1"]);
        assert!(cups.local_only());
        let kde = ls.iter().find(|l| l.port == 1716).unwrap();
        assert_eq!((kde.process.as_str(), kde.pid), ("kdeconnectd", 3038));
        assert_eq!(kde.addrs, ["*", "0.0.0.0"]);
        assert!(kde.exposed());
        let dns = ls.iter().find(|l| l.port == 53).unwrap();
        assert_eq!((dns.proto.as_str(), dns.addrs[0].as_str()), ("udp", "127.0.0.53"));
        assert_eq!(dns.url(), None);
    }

    #[test]
    fn netstat_do_windows_so_o_que_escuta() {
        let out = "\r\nConexões ativas\r\n\r\n  Proto  Endereço local  Endereço externo  Estado  PID\r\n\
  TCP    0.0.0.0:135            0.0.0.0:0              LISTENING       1108\r\n\
  TCP    127.0.0.1:5432         0.0.0.0:0              ABHÖREN         4321\r\n\
  TCP    192.168.0.7:51234      140.82.112.26:443      ESTABLISHED     9000\r\n\
  TCP    [::]:135               [::]:0                 LISTENING       1108\r\n\
  TCP    [::1]:5432             [::]:0                 LISTENING       4321\r\n\
  UDP    0.0.0.0:5353           *:*                                    2345\r\n\
  UDP    [fe80::1%12]:1900      *:*                                    2345\r\n";
        let ls = parse_netstat(out);
        assert_eq!(ls.len(), 4, "{ls:?}");
        let rpc = ls.iter().find(|l| l.port == 135).unwrap();
        assert_eq!((rpc.proto.as_str(), rpc.pid), ("tcp", 1108));
        assert_eq!(rpc.addrs, ["0.0.0.0", "::"]);
        assert!(rpc.exposed());
        // o estado em outro idioma não atrapalha; IPv4 e IPv6 do mesmo processo se juntam
        let pg = ls.iter().find(|l| l.port == 5432).unwrap();
        assert_eq!(pg.addrs, ["127.0.0.1", "::1"]);
        assert!(pg.local_only());
        assert_eq!(pg.url().as_deref(), Some("http://localhost:5432"));
        // conexão de saída não é porta aberta
        assert!(ls.iter().all(|l| l.port != 51234));
        let ssdp = ls.iter().find(|l| l.port == 1900).unwrap();
        assert_eq!((ssdp.proto.as_str(), ssdp.addrs[0].as_str()), ("udp", "fe80::1"));
    }

    #[test]
    fn endereco_para_o_navegador() {
        let ls = parse(SAMPLE);
        let url = |p: u16| ls.iter().find(|l| l.port == p).unwrap().url().unwrap();
        assert_eq!(url(47836), "http://localhost:47836");
        assert_eq!(url(1716), "http://localhost:1716");
        assert_eq!(url(18449), "http://100.105.101.45:18449");
    }
}
