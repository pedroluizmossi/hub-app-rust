//! CPU, RAM, CPU temperature, network and disks.
//!
//! What the apps receive (`SysSnapshot`) is the same on both systems; the collection
//! (`SysProvider`) differs: `/proc` and `/sys` on Linux (`linux.rs`), the `sysinfo` crate on
//! Windows (`windows.rs`). Whatever a system lacks stays at zero (see each field).

#[cfg(target_os = "linux")]
#[path = "sys/linux.rs"]
mod imp;
#[cfg(windows)]
#[path = "sys/windows.rs"]
mod imp;

pub use imp::SysProvider;

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct SysSnapshot {
    /// CPU usage in % since the previous reading.
    pub cpu: f32,
    /// Average core frequency in MHz (0 = unknown).
    pub cpu_mhz: f32,
    /// 1, 5 and 15 minute load average (Windows has none: stays at zero).
    pub load: (f32, f32, f32),
    pub mem_total: u64,
    /// Total minus available (`MemAvailable`, which already excludes cache).
    pub mem_used: u64,
    pub swap_total: u64,
    pub swap_used: u64,
    /// °C; 0 = no sensor (not read on Windows).
    pub cpu_temp: f32,
    /// Bytes/s (without loopback or Docker/VM interfaces).
    pub net_rx: f64,
    pub net_tx: f64,
    /// Interface of the default route ("" = no network), whether it is Wi-Fi, and its IPv4.
    pub net_iface: String,
    pub net_wireless: bool,
    pub net_ip: String,
    /// Real mounted partitions (no loop, snap, tmpfs…), one per device.
    pub disks: Vec<Disk>,
    /// Physical disk reads and writes, in bytes/s.
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
