//! Coleta no Windows, pelo crate `sysinfo`.
//!
//! Fica de fora o que o Windows não entrega sem privilégio ou sem driver: carga média e
//! temperatura da CPU (ambas em zero no snapshot).

use std::net::{IpAddr, UdpSocket};
use std::time::Instant;

use sysinfo::{Disks, Networks, System};

use super::{Disk, SysSnapshot};

/// Interfaces que não são "a rede" da máquina: laço local e as virtuais (Hyper-V, WSL,
/// Docker, VirtualBox, VMware).
const SKIP_NET: &[&str] = &["loopback", "vethernet", "virtualbox", "vmware", "bluetooth"];

pub struct SysProvider {
    sys: Option<System>,
    nets: Option<Networks>,
    disks: Option<Disks>,
    /// Quando foi a leitura anterior (as taxas são o que mudou desde então).
    prev: Option<Instant>,
}

impl Default for SysProvider {
    fn default() -> Self {
        SysProvider {
            sys: System::new().ok(),
            nets: Networks::new_with_refreshed_list().ok(),
            disks: Disks::new_with_refreshed_list().ok(),
            prev: None,
        }
    }
}

impl SysProvider {
    pub fn collect(&mut self) -> SysSnapshot {
        let mut s = SysSnapshot::default();
        let now = Instant::now();
        // segundos desde a leitura anterior; a primeira não tem com o que comparar
        let dt = self.prev.map(|at| now.duration_since(at).as_secs_f64().max(0.001));
        self.prev = Some(now);

        if let Some(sys) = &mut self.sys {
            sys.refresh_cpu_usage();
            sys.refresh_cpu_frequency();
            sys.refresh_memory();
            // o uso é medido entre duas leituras: a primeira não vale
            if dt.is_some() {
                s.cpu = sys.global_cpu_usage().clamp(0.0, 100.0);
            }
            let cpus = sys.cpus();
            if !cpus.is_empty() {
                s.cpu_mhz = cpus.iter().map(|c| c.frequency() as f32).sum::<f32>() / cpus.len() as f32;
            }
            s.mem_total = sys.total_memory();
            s.mem_used = s.mem_total.saturating_sub(sys.available_memory());
            s.swap_total = sys.total_swap();
            s.swap_used = sys.used_swap();
        }

        if let Some(nets) = &mut self.nets {
            nets.refresh(true);
            let local = default_ipv4();
            let (mut rx, mut tx) = (0u64, 0u64);
            for (name, data) in nets.iter() {
                let name = name.to_string_lossy();
                let lower = name.to_lowercase();
                if SKIP_NET.iter().any(|p| lower.contains(p)) {
                    continue;
                }
                rx += data.received();
                tx += data.transmitted();
                // a interface da rota padrão é a que tem o endereço por onde saímos
                if local.is_some_and(|ip| data.ip_networks().iter().any(|n| n.addr == ip)) {
                    s.net_wireless = ["wi-fi", "wifi", "wireless", "wlan"].iter().any(|w| lower.contains(w));
                    s.net_ip = local.map(|ip| ip.to_string()).unwrap_or_default();
                    s.net_iface = name.into_owned();
                }
            }
            if let Some(dt) = dt {
                s.net_rx = rx as f64 / dt;
                s.net_tx = tx as f64 / dt;
            }
        }

        if let Some(disks) = &mut self.disks {
            disks.refresh(true);
            let (mut read, mut written) = (0u64, 0u64);
            for d in disks.iter() {
                let usage = d.usage();
                read += usage.read_bytes;
                written += usage.written_bytes;
                let total = d.total_space();
                if total > 0 {
                    s.disks.push(Disk {
                        // "C:\" -> "C:"
                        mount: d.mount_point().to_string_lossy().trim_end_matches('\\').to_string(),
                        used: total.saturating_sub(d.available_space()),
                        total,
                    });
                }
            }
            s.disks.sort_by(|a, b| a.mount.cmp(&b.mount));
            s.disks.dedup_by(|a, b| a.mount == b.mount);
            if let Some(dt) = dt {
                s.disk_read = read as f64 / dt;
                s.disk_write = written as f64 / dt;
            }
        }
        s
    }
}

/// IPv4 por onde a máquina sai para a internet. "Conectar" um socket UDP não manda nada:
/// só faz o sistema escolher a interface da rota padrão, e o endereço local é o dela.
fn default_ipv4() -> Option<IpAddr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_unspecified() && !ip.is_loopback()).then_some(ip)
}
