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

pub use imp::ProcsProvider;

/// How many processes go in each list.
pub const TOP: usize = 10;

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
    fn short_names() {
        assert_eq!(short_name("chrome.exe"), "chrome");
        assert_eq!(short_name("C:\\Windows\\explorer.EXE"), "explorer");
        assert_eq!(short_name("/usr/bin/firefox"), "firefox");
        assert_eq!(short_name("exe"), "exe");
        assert_eq!(short_name("ação.exe"), "ação");
    }
}
