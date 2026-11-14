//! Processes on Windows, through the `sysinfo` crate.
//!
//! `sysinfo` gives each process's CPU usage summed over the cores (above 100 on a machine
//! with several); here it is divided by the number of cores, to be on the same scale as
//! Linux.

use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};

use super::{Proc, ProcsSnapshot, short_name, snapshot};

pub struct ProcsProvider {
    sys: Option<System>,
    cpus: f32,
}

impl Default for ProcsProvider {
    fn default() -> Self {
        let cpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1) as f32;
        ProcsProvider { sys: System::new().ok(), cpus }
    }
}

impl ProcsProvider {
    pub fn collect(&mut self) -> ProcsSnapshot {
        let Some(sys) = &mut self.sys else { return ProcsSnapshot::default() };
        sys.refresh_memory();
        sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing().with_cpu().with_memory(),
        );
        let mem_total = sys.total_memory();
        let all = sys
            .processes()
            .iter()
            .filter_map(|(pid, p)| {
                let pid = pid.as_u32();
                let mem_bytes = p.memory();
                // pid 0 is the "System Idle Process": the free CPU, not a program
                if pid == 0 || mem_bytes == 0 {
                    return None;
                }
                let mem = if mem_total > 0 { (mem_bytes as f64 * 100.0 / mem_total as f64) as f32 } else { 0.0 };
                Some(Proc {
                    pid,
                    name: short_name(&p.name().to_string_lossy()),
                    cpu: (p.cpu_usage() / self.cpus).clamp(0.0, 100.0),
                    mem,
                    mem_bytes,
                })
            })
            .collect();
        snapshot(all)
    }
}
