//! CPU, RAM, temperatura da CPU, rede e discos.
//!
//! O que os apps recebem (`SysSnapshot`) é igual nos dois sistemas; a coleta
//! (`SysProvider`) muda: `/proc` e `/sys` no Linux (`linux.rs`), o crate `sysinfo` no
//! Windows (`windows.rs`). O que um sistema não tem fica em zero (ver cada campo).

#[cfg(target_os = "linux")]
#[path = "sys/linux.rs"]
mod imp;
#[cfg(windows)]
#[path = "sys/windows.rs"]
mod imp;

pub use imp::SysProvider;

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct SysSnapshot {
    /// Uso de CPU em % desde a leitura anterior.
    pub cpu: f32,
    /// Frequência média dos núcleos em MHz (0 = desconhecida).
    pub cpu_mhz: f32,
    /// Carga média de 1, 5 e 15 minutos (o Windows não tem: fica em zero).
    pub load: (f32, f32, f32),
    pub mem_total: u64,
    /// Total menos o disponível (`MemAvailable`, que já desconta cache).
    pub mem_used: u64,
    pub swap_total: u64,
    pub swap_used: u64,
    /// °C; 0 = sem sensor (no Windows não é lida).
    pub cpu_temp: f32,
    /// Bytes/s (sem loopback nem interfaces do Docker/VMs).
    pub net_rx: f64,
    pub net_tx: f64,
    /// Interface da rota padrão ("" = sem rede), se é Wi-Fi e o IPv4 dela.
    pub net_iface: String,
    pub net_wireless: bool,
    pub net_ip: String,
    /// Partições montadas de verdade (sem loop, snap, tmpfs…), uma por dispositivo.
    pub disks: Vec<Disk>,
    /// Leitura e escrita dos discos físicos, em bytes/s.
    pub disk_read: f64,
    pub disk_write: f64,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct Disk {
    pub mount: String,
    pub used: u64,
    pub total: u64,
}

impl Disk {
    pub fn percent(&self) -> f32 {
        if self.total > 0 { self.used as f32 / self.total as f32 * 100.0 } else { 0.0 }
    }
}
