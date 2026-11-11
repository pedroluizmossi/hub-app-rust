//! CPU, RAM, temperatura da CPU, rede e discos, lidos direto de /proc e /sys.

use std::collections::HashSet;
use std::fs;
use std::time::Instant;

#[derive(Debug, Clone, Default)]
pub struct SysSnapshot {
    /// Uso de CPU em % desde a leitura anterior.
    pub cpu: f32,
    /// Frequência média dos núcleos em MHz (0 = desconhecida).
    pub cpu_mhz: f32,
    /// Carga média de 1, 5 e 15 minutos.
    pub load: (f32, f32, f32),
    pub mem_total: u64,
    /// Total menos o disponível (`MemAvailable`, que já desconta cache).
    pub mem_used: u64,
    pub swap_total: u64,
    pub swap_used: u64,
    /// °C; 0 = sem sensor.
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

#[derive(Debug, Clone, Default, PartialEq)]
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

#[derive(Default)]
pub struct SysProvider {
    prev_cpu: Option<(u64, u64)>,
    prev_net: Option<(u64, u64, Instant)>,
    prev_io: Option<(u64, u64, Instant)>,
}

impl SysProvider {
    pub fn collect(&mut self) -> SysSnapshot {
        let mut s = SysSnapshot::default();
        let now = Instant::now();
        if let Some(cur) = fs::read_to_string("/proc/stat").ok().and_then(|t| parse_cpu(&t)) {
            s.cpu = cpu_percent(self.prev_cpu, cur);
            self.prev_cpu = Some(cur);
        }
        s.cpu_mhz = cpu_mhz();
        if let Ok(t) = fs::read_to_string("/proc/loadavg") {
            s.load = parse_loadavg(&t);
        }
        if let Ok(t) = fs::read_to_string("/proc/meminfo") {
            let m = parse_mem(&t);
            s.mem_total = m.total;
            s.mem_used = m.total.saturating_sub(m.available);
            s.swap_total = m.swap_total;
            s.swap_used = m.swap_total.saturating_sub(m.swap_free);
        }
        s.cpu_temp = cpu_temp();
        if let Ok(t) = fs::read_to_string("/proc/net/dev") {
            let (rx, tx) = parse_net(&t);
            if let Some((prx, ptx, at)) = self.prev_net {
                let dt = now.duration_since(at).as_secs_f64().max(0.001);
                s.net_rx = rx.saturating_sub(prx) as f64 / dt;
                s.net_tx = tx.saturating_sub(ptx) as f64 / dt;
            }
            self.prev_net = Some((rx, tx, now));
        }
        if let Some(iface) = fs::read_to_string("/proc/net/route").ok().and_then(|t| default_iface(&t)) {
            s.net_wireless = std::path::Path::new(&format!("/sys/class/net/{iface}/wireless")).exists();
            s.net_ip = ipv4_of(&iface).unwrap_or_default();
            s.net_iface = iface;
        }
        if let Ok(t) = fs::read_to_string("/proc/mounts") {
            s.disks = parse_mounts(&t)
                .into_iter()
                .filter_map(|mount| {
                    let (total, used) = disk_usage(&mount);
                    (total > 0).then_some(Disk { mount, used, total })
                })
                .collect();
        }
        if let Ok(t) = fs::read_to_string("/proc/diskstats") {
            let (r, w) = parse_diskstats(&t);
            if let Some((pr, pw, at)) = self.prev_io {
                let dt = now.duration_since(at).as_secs_f64().max(0.001);
                s.disk_read = r.saturating_sub(pr) as f64 / dt;
                s.disk_write = w.saturating_sub(pw) as f64 / dt;
            }
            self.prev_io = Some((r, w, now));
        }
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

/// Campos de /proc/meminfo que interessam, em bytes.
#[derive(Debug, Default, PartialEq)]
pub struct MemInfo {
    pub total: u64,
    pub available: u64,
    pub swap_total: u64,
    pub swap_free: u64,
}

pub fn parse_mem(text: &str) -> MemInfo {
    let field = |name: &str| {
        text.lines()
            .find(|l| l.starts_with(name))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0)
            * 1024
    };
    MemInfo {
        total: field("MemTotal:"),
        available: field("MemAvailable:"),
        swap_total: field("SwapTotal:"),
        swap_free: field("SwapFree:"),
    }
}

/// /proc/loadavg -> carga de 1, 5 e 15 min.
pub fn parse_loadavg(text: &str) -> (f32, f32, f32) {
    let mut f = text.split_whitespace().map(|x| x.parse::<f32>().unwrap_or(0.0));
    (f.next().unwrap_or(0.0), f.next().unwrap_or(0.0), f.next().unwrap_or(0.0))
}

/// Frequência média dos núcleos (cpufreq; senão /proc/cpuinfo), em MHz.
fn cpu_mhz() -> f32 {
    let mut khz = Vec::new();
    if let Ok(dir) = fs::read_dir("/sys/devices/system/cpu") {
        for e in dir.flatten() {
            let name = e.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("cpu") && name[3..].chars().all(|c| c.is_ascii_digit()) && name.len() > 3 {
                if let Some(v) = fs::read_to_string(e.path().join("cpufreq/scaling_cur_freq"))
                    .ok()
                    .and_then(|t| t.trim().parse::<f32>().ok())
                {
                    khz.push(v);
                }
            }
        }
    }
    if !khz.is_empty() {
        return khz.iter().sum::<f32>() / khz.len() as f32 / 1000.0;
    }
    let text = fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    let mhz: Vec<f32> = text
        .lines()
        .filter(|l| l.starts_with("cpu MHz"))
        .filter_map(|l| l.split(':').nth(1)?.trim().parse().ok())
        .collect();
    if mhz.is_empty() { 0.0 } else { mhz.iter().sum::<f32>() / mhz.len() as f32 }
}

/// Pontos de montagem de /proc/mounts que valem mostrar: dispositivos de bloco de verdade,
/// um por dispositivo (subvolumes btrfs do mesmo disco contam uma vez só, pelo caminho
/// mais curto).
pub fn parse_mounts(text: &str) -> Vec<String> {
    const SKIP_FS: &[&str] = &["squashfs", "iso9660", "udf", "overlay"];
    const SKIP_AT: &[&str] = &["/snap", "/var/lib/docker", "/run", "/boot/efi", "/efi"];
    let mut rows: Vec<(String, String)> = text
        .lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            let (dev, at, fs) = (f.next()?, f.next()?, f.next()?);
            let at = unescape_mount(at);
            let ok = dev.starts_with("/dev/")
                && !dev.starts_with("/dev/loop")
                && !SKIP_FS.contains(&fs)
                && !SKIP_AT.iter().any(|p| at == *p || at.starts_with(&format!("{p}/")));
            ok.then(|| (dev.to_string(), at))
        })
        .collect();
    rows.sort_by_key(|(_, at)| at.len());
    let mut seen = HashSet::new();
    let mut out: Vec<String> = rows.into_iter().filter(|(dev, _)| seen.insert(dev.clone())).map(|(_, at)| at).collect();
    out.sort();
    out
}

/// Espaços e afins vêm escapados em octal em /proc/mounts (`\040`).
fn unescape_mount(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c)) {
            out.push((b[i + 1] - b'0') * 64 + (b[i + 2] - b'0') * 8 + (b[i + 3] - b'0'));
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// /proc/diskstats -> bytes (lidos, gravados) acumulados dos discos físicos (sem partições).
pub fn parse_diskstats(text: &str) -> (u64, u64) {
    let mut r = 0;
    let mut w = 0;
    for line in text.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 10 || !is_whole_disk(f[2]) {
            continue;
        }
        r += f[5].parse::<u64>().unwrap_or(0) * 512;
        w += f[9].parse::<u64>().unwrap_or(0) * 512;
    }
    (r, w)
}

/// `sda`, `vdb`, `nvme0n1`, `mmcblk0` sim; `sda1`, `nvme0n1p2`, `loop0`, `dm-0` não.
fn is_whole_disk(name: &str) -> bool {
    let digits_after = |p: &str| name.strip_prefix(p).is_some_and(|r| !r.is_empty() && r.chars().all(|c| c.is_ascii_digit()));
    if let Some(rest) = name.strip_prefix("nvme") {
        // nvme<N>n<M>
        return rest.split_once('n').is_some_and(|(a, b)| {
            !a.is_empty() && a.chars().all(|c| c.is_ascii_digit()) && !b.is_empty() && b.chars().all(|c| c.is_ascii_digit())
        });
    }
    if digits_after("mmcblk") {
        return true;
    }
    ["sd", "vd", "hd", "xvd"].iter().any(|p| {
        name.strip_prefix(p).is_some_and(|r| !r.is_empty() && r.chars().all(|c| c.is_ascii_lowercase()))
    })
}

/// Interface da rota padrão em /proc/net/route.
pub fn default_iface(text: &str) -> Option<String> {
    text.lines().skip(1).find_map(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        (f.len() > 2 && f[1] == "00000000").then(|| f[0].to_string())
    })
}

/// IPv4 de uma interface (getifaddrs).
fn ipv4_of(iface: &str) -> Option<String> {
    let mut addrs: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs preenche a lista, que é liberada com freeifaddrs logo abaixo; só
    // lemos os nós enquanto ela existe.
    unsafe {
        if libc::getifaddrs(&mut addrs) != 0 {
            return None;
        }
        let mut found = None;
        let mut p = addrs;
        while !p.is_null() {
            let a = &*p;
            if !a.ifa_addr.is_null()
                && (*a.ifa_addr).sa_family as i32 == libc::AF_INET
                && std::ffi::CStr::from_ptr(a.ifa_name).to_str() == Ok(iface)
            {
                let sin = &*(a.ifa_addr as *const libc::sockaddr_in);
                found = Some(std::net::Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr)).to_string());
                break;
            }
            p = a.ifa_next;
        }
        libc::freeifaddrs(addrs);
        found
    }
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
        let m = parse_mem("MemTotal: 1000 kB\nMemFree: 1 kB\nMemAvailable: 400 kB\nSwapTotal: 200 kB\nSwapFree: 50 kB");
        assert_eq!(m, MemInfo { total: 1024000, available: 409600, swap_total: 204800, swap_free: 51200 });
    }

    #[test]
    fn carga() {
        assert_eq!(parse_loadavg("1.25 0.90 0.50 2/1000 1234\n"), (1.25, 0.9, 0.5));
    }

    #[test]
    fn montagens_reais_uma_por_disco() {
        let t = "/dev/nvme0n1p2 / btrfs rw 0 0\n\
                 /dev/nvme0n1p2 /home btrfs rw 0 0\n\
                 /dev/nvme0n1p1 /boot/efi vfat rw 0 0\n\
                 tmpfs /tmp tmpfs rw 0 0\n\
                 /dev/loop3 /snap/core/1 squashfs ro 0 0\n\
                 /dev/sdb1 /mnt/HD\\040Externo ext4 rw 0 0\n";
        assert_eq!(parse_mounts(t), vec!["/".to_string(), "/mnt/HD Externo".to_string()]);
    }

    #[test]
    fn diskstats_so_discos_inteiros() {
        let t = "   8       0 sda 10 0 100 0 20 0 200 0 0 0 0\n\
                    8       1 sda1 10 0 100 0 20 0 200 0 0 0 0\n\
                  259       0 nvme0n1 1 0 4 0 2 0 8 0 0 0 0\n\
                  259       1 nvme0n1p1 1 0 4 0 2 0 8 0 0 0 0\n\
                    7       0 loop0 1 0 999 0 0 0 999 0 0 0 0\n";
        assert_eq!(parse_diskstats(t), (104 * 512, 208 * 512));
        assert!(is_whole_disk("mmcblk0") && !is_whole_disk("mmcblk0p1") && !is_whole_disk("dm-0"));
    }

    #[test]
    fn rota_padrao() {
        let t = "Iface\tDestination\tGateway\nwlp3s0\t0000A8C0\t00000000\nwlp3s0\t00000000\t0100A8C0\n";
        assert_eq!(default_iface(t), Some("wlp3s0".into()));
        assert_eq!(default_iface("Iface\tDestination\n"), None);
    }

    #[test]
    fn rede_ignora_loopback_e_docker() {
        let t = "h1\nh2\n    lo: 999 0 0 0 0 0 0 0 999 0\n  eth0: 100 0 0 0 0 0 0 0 50 0\ndocker0: 7 0 0 0 0 0 0 0 7 0\n";
        assert_eq!(parse_net(t), (100, 50));
    }
}
