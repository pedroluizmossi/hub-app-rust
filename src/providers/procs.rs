//! Processes using the most CPU and memory (for plugins: `uses = ["procs"]`).
//!
//! It exists so plugins don't depend on `ps`, which Windows doesn't have. What they receive
//! (`ProcsSnapshot`) is the same on both systems; the collection differs: `/proc` on Linux
//! (`linux.rs`), the `sysinfo` crate on Windows (`windows.rs`).
//!
//! CPU and memory are in % of the whole machine (0 to 100), like the `cpu` of the `sys`
//! provider: a process using one full core on an 8-core machine shows up as 12.5%.

#[cfg(target_os = "linux")]
#[path = "procs/linux.rs"]
mod imp;
#[cfg(windows)]
#[path = "procs/windows.rs"]
mod imp;

use std::collections::HashMap;
use std::time::{Duration, Instant};

pub use imp::{ProcsProvider, usage};

/// How many processes go in each list.
pub const TOP: usize = 10;

/// What one process has used so far (`usage`, read per OS: `/proc/<pid>/stat` on Linux,
/// `sysinfo` on Windows). Settings › System shows it for the hub and its external apps.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    /// CPU time since it started (user + system, all cores), in ms.
    pub cpu_ms: u64,
    /// Memory in use (Linux: resident; Windows: working set), in bytes.
    pub mem_bytes: u64,
}

/// Measures a few processes over time: CPU in % of the whole machine between two reads.
#[derive(Default)]
pub struct Meter {
    prev: HashMap<u32, (Instant, u64)>,
}

impl Meter {
    /// Reads `pids` now: CPU (% of the machine since the last read; 0 on a pid's first
    /// read) and memory in bytes. A pid that is gone is left out.
    pub fn read(&mut self, pids: &[u32]) -> HashMap<u32, (f32, u64)> {
        let now = Instant::now();
        let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1) as u32;
        let mut out = HashMap::new();
        let mut next = HashMap::new();
        for (pid, u) in usage(pids) {
            let cpu = self.prev.get(&pid).map_or(0.0, |&(t, ms)| cpu_percent(u.cpu_ms.saturating_sub(ms), now - t, cores));
            out.insert(pid, (cpu, u.mem_bytes));
            next.insert(pid, (now, u.cpu_ms));
        }
        self.prev = next;
        out
    }
}

/// `used_ms` of CPU time over `elapsed` on a machine with `cores`, in % of the machine.
fn cpu_percent(used_ms: u64, elapsed: Duration, cores: u32) -> f32 {
    let available = elapsed.as_secs_f64() * 1000.0 * cores.max(1) as f64;
    if available <= 0.0 { 0.0 } else { (used_ms as f64 * 100.0 / available).clamp(0.0, 100.0) as f32 }
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ProcsSnapshot {
    /// The heaviest CPU users, largest first (up to `TOP`). On the first reading they are
    /// all at 0: usage is measured between two readings.
    pub by_cpu: Vec<Proc>,
    /// The heaviest memory users, largest first (up to `TOP`).
    pub by_mem: Vec<Proc>,
    /// How many processes there were (with their own memory; no kernel threads).
    pub count: usize,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct Proc {
    pub pid: u32,
    /// Short program name ("firefox", without path or ".exe").
    pub name: String,
    /// % of the whole machine's CPU since the previous reading.
    pub cpu: f32,
    /// % of the machine's memory.
    pub mem: f32,
    /// Resident memory in bytes.
    pub mem_bytes: u64,
}

/// Builds the snapshot from every process read: both lists, sorted and cut at `TOP` (ties:
/// lowest pid first, so the list doesn't jump around for nothing).
pub fn snapshot(all: Vec<Proc>) -> ProcsSnapshot {
    let count = all.len();
    let top = |key: fn(&Proc) -> f32| {
        let mut v = all.clone();
        v.sort_by(|a, b| key(b).total_cmp(&key(a)).then(a.pid.cmp(&b.pid)));
        v.truncate(TOP);
        v
    };
    ProcsSnapshot { by_cpu: top(|p| p.cpu), by_mem: top(|p| p.mem), count }
}

/// Display name on Windows: no folder and no ".exe" (the name comes as "chrome.exe").
/// Linux doesn't need it: `comm` is already short (and may contain "/", as in "kworker/0:1").
#[cfg(any(windows, test))]
pub fn short_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    match base.len().checked_sub(4) {
        Some(cut) if base.is_char_boundary(cut) && base[cut..].eq_ignore_ascii_case(".exe") => base[..cut].to_string(),
        _ => base.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pid: u32, cpu: f32, mem: f32) -> Proc {
        Proc { pid, name: format!("p{pid}"), cpu, mem, mem_bytes: 0 }
    }

    #[test]
    fn sorts_and_cuts_both_lists() {
        let all: Vec<Proc> = (1..=15).map(|i| p(i, i as f32, (16 - i) as f32)).collect();
        let s = snapshot(all);
        assert_eq!(s.count, 15);
        assert_eq!(s.by_cpu.len(), TOP);
        assert_eq!(s.by_cpu[0].pid, 15);
        assert_eq!(s.by_mem[0].pid, 1);
        assert!(s.by_cpu.windows(2).all(|w| w[0].cpu >= w[1].cpu));
    }

    #[test]
    fn ties_are_ordered_by_pid() {
        let s = snapshot(vec![p(9, 0.0, 0.0), p(3, 0.0, 0.0), p(5, 0.0, 0.0)]);
        let pids: Vec<u32> = s.by_cpu.iter().map(|p| p.pid).collect();
        assert_eq!(pids, [3, 5, 9]);
    }

    #[test]
    fn cpu_percent_of_the_machine() {
        // one full core for 2 s on 4 cores = 25%
        assert_eq!(cpu_percent(2000, Duration::from_secs(2), 4), 25.0);
        assert_eq!(cpu_percent(0, Duration::from_secs(2), 4), 0.0);
        assert_eq!(cpu_percent(10, Duration::ZERO, 4), 0.0);
        assert_eq!(cpu_percent(99_000, Duration::from_secs(1), 1), 100.0);
    }

    #[test]
    fn meter_reads_this_process() {
        let me = std::process::id();
        let mut m = Meter::default();
        let first = m.read(&[me]);
        let (cpu, mem) = first[&me];
        assert_eq!(cpu, 0.0);
        assert!(mem > 0);
        assert!(m.read(&[me]).contains_key(&me));
        // a pid that does not exist is left out
        assert!(m.read(&[u32::MAX - 1]).is_empty());
    }

    #[test]
    fn short_names() {
        assert_eq!(short_name("chrome.exe"), "chrome");
        assert_eq!(short_name("C:\\Windows\\explorer.EXE"), "explorer");
        assert_eq!(short_name("/usr/bin/firefox"), "firefox");
        assert_eq!(short_name("exe"), "exe");
        assert_eq!(short_name("ação.exe"), "ação");
    }
}
