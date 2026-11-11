//! Processos no Linux, direto de /proc.
//!
//! O uso de CPU de cada processo é quanto o tempo dele (`utime + stime`, em ticks) cresceu
//! entre duas leituras, dividido por quanto o tempo total de todos os núcleos cresceu (a
//! linha "cpu" de /proc/stat): dá a fração da máquina toda.

use std::collections::HashMap;
use std::fs;

use super::{Proc, ProcsSnapshot, snapshot};

#[derive(Default)]
pub struct ProcsProvider {
    /// Tempo total da CPU (todos os núcleos) na leitura anterior.
    prev_total: Option<u64>,
    /// Ticks de cada processo na leitura anterior.
    prev: HashMap<u32, u64>,
}

impl ProcsProvider {
    pub fn collect(&mut self) -> ProcsSnapshot {
        let Some(total) = fs::read_to_string("/proc/stat").ok().and_then(|t| total_ticks(&t)) else {
            return ProcsSnapshot::default();
        };
        let mem_total = fs::read_to_string("/proc/meminfo").ok().and_then(|t| mem_total(&t)).unwrap_or(0);
        // SAFETY: `sysconf` só consulta um valor do sistema.
        let page = match unsafe { libc::sysconf(libc::_SC_PAGESIZE) } {
            p if p > 0 => p as u64,
            _ => 4096,
        };
        let dt = self.prev_total.map(|p| total.saturating_sub(p)).filter(|d| *d > 0);
        self.prev_total = Some(total);

        let mut now = HashMap::new();
        let mut all = Vec::new();
        for entry in fs::read_dir("/proc").into_iter().flatten().flatten() {
            let Some(pid) = entry.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else { continue };
            // o processo pode ter saído entre a listagem e a leitura
            let Some(st) = fs::read_to_string(entry.path().join("stat")).ok().and_then(|t| parse_stat(&t)) else {
                continue;
            };
            now.insert(pid, st.ticks);
            // threads do kernel não têm memória própria
            if st.rss_pages == 0 {
                continue;
            }
            let cpu = match (dt, self.prev.get(&pid)) {
                (Some(dt), Some(&before)) => (st.ticks.saturating_sub(before) as f64 * 100.0 / dt as f64) as f32,
                _ => 0.0,
            };
            let mem_bytes = st.rss_pages * page;
            let mem = if mem_total > 0 { (mem_bytes as f64 * 100.0 / mem_total as f64) as f32 } else { 0.0 };
            all.push(Proc { pid, name: st.name, cpu: cpu.clamp(0.0, 100.0), mem, mem_bytes });
        }
        self.prev = now;
        snapshot(all)
    }
}

/// O que interessa de /proc/<pid>/stat.
#[derive(Debug, PartialEq)]
struct Stat {
    name: String,
    /// `utime + stime`, em ticks.
    ticks: u64,
    rss_pages: u64,
}

/// /proc/<pid>/stat: "pid (nome) estado ppid …". O nome pode ter espaço e parêntese, então
/// vai até o ÚLTIMO ")"; depois dele, o campo 3 (estado) é o índice 0, `utime` (campo 14)
/// o 11, `stime` o 12 e `rss` (campo 24) o 21.
fn parse_stat(text: &str) -> Option<Stat> {
    let open = text.find('(')?;
    let close = text.rfind(')')?;
    let name = text.get(open + 1..close)?.to_string();
    let rest: Vec<&str> = text.get(close + 1..)?.split_whitespace().collect();
    let field = |i: usize| rest.get(i).and_then(|v| v.parse::<u64>().ok());
    Some(Stat { name, ticks: field(11)? + field(12)?, rss_pages: field(21)? })
}

/// Linha "cpu" de /proc/stat -> ticks de todos os núcleos somados (sem guest, já contado
/// em user).
fn total_ticks(text: &str) -> Option<u64> {
    let line = text.lines().find(|l| l.starts_with("cpu "))?;
    Some(line.split_whitespace().skip(1).take(8).filter_map(|v| v.parse::<u64>().ok()).sum())
}

/// `MemTotal` de /proc/meminfo, em bytes.
fn mem_total(text: &str) -> Option<u64> {
    let line = text.lines().find(|l| l.starts_with("MemTotal:"))?;
    line.split_whitespace().nth(1)?.parse::<u64>().ok().map(|kb| kb * 1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn le_stat_com_nome_estranho() {
        let t = "1234 (Web Content (x)) S 1 1234 1234 0 -1 4194560 100 0 0 0 250 50 0 0 20 0 30 0 5000 900000000 12345 18446744073709551615 0";
        assert_eq!(parse_stat(t), Some(Stat { name: "Web Content (x)".into(), ticks: 300, rss_pages: 12345 }));
    }

    #[test]
    fn stat_cortado_nao_vale() {
        assert_eq!(parse_stat("1 (init) S 0 1"), None);
        assert_eq!(parse_stat(""), None);
    }

    #[test]
    fn total_e_memoria() {
        assert_eq!(total_ticks("cpu  10 0 5 100 2 0 1 0 7 0\ncpu0 1 1 1 1"), Some(118));
        assert_eq!(mem_total("MemTotal:       16000000 kB\nMemFree: 1 kB"), Some(16_384_000_000));
    }

    #[test]
    fn le_o_proc_desta_maquina() {
        let mut p = ProcsProvider::default();
        p.collect();
        let s = p.collect();
        assert!(s.count > 0, "ao menos este processo de teste");
        assert!(s.by_mem.iter().all(|p| p.mem_bytes > 0 && !p.name.is_empty()));
    }
}
