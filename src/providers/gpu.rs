//! GPU: NVIDIA through `nvidia-smi`; otherwise the first AMD card (amdgpu) through sysfs.

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::core::exec;

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct GpuSnapshot {
    pub available: bool,
    /// "nvidia" | "amd" | ""
    pub vendor: &'static str,
    pub util: f32,
    pub vram_used_mib: f32,
    pub vram_total_mib: f32,
    /// °C; 0 = no sensor.
    pub temp: f32,
}

/// How often `nvidia-smi` writes a reading (the same pace as the provider).
const NVIDIA_EVERY: Duration = Duration::from_secs(2);
/// With no reading for this long, the one we have is stale: the card shows as unavailable
/// and `nvidia-smi` (probably stuck) is restarted.
const NVIDIA_STALE: Duration = Duration::from_secs(8);
/// With nobody asking for a reading for this long (the app was disabled), `nvidia-smi` is stopped.
const NVIDIA_IDLE: Duration = Duration::from_secs(10);
/// On start, waits up to this long for the first reading, so the card doesn't flash "unavailable".
const NVIDIA_FIRST: Duration = Duration::from_millis(1500);
/// After this many starts in a row without any reading, gives up on `nvidia-smi`.
const NVIDIA_TRIES: u32 = 3;

enum Source {
    Nvidia(Nvidia),
    Amd(PathBuf),
    None,
}

pub struct GpuProvider {
    source: Source,
}

impl GpuProvider {
    /// Detects the card only once.
    pub fn detect() -> Self {
        let source = if exec::which("nvidia-smi") { Source::Nvidia(Nvidia::default()) } else { amd_or_none() };
        GpuProvider { source }
    }

    pub fn collect(&mut self) -> GpuSnapshot {
        if let Source::Nvidia(nvidia) = &mut self.source {
            match nvidia.read() {
                Some(snap) => return snap,
                // `nvidia-smi` on the PATH but not working (missing driver, card turned off):
                // try the AMD card instead of showing "no GPU" forever
                None => self.source = amd_or_none(),
            }
        }
        match &self.source {
            Source::Amd(dev) => read_amd(dev),
            _ => GpuSnapshot::default(),
        }
    }
}

fn amd_or_none() -> Source {
    amd_card().map(Source::Amd).unwrap_or(Source::None)
}

/// What the reader thread and the provider share.
#[derive(Default)]
struct Stream {
    /// The running `nvidia-smi`: the provider kills it through here, the reader reaps it at the end.
    child: Mutex<Option<Child>>,
    /// Last time the provider asked for a reading.
    asked: Mutex<Option<Instant>>,
}

impl Stream {
    fn kill(&self) {
        if let Ok(mut slot) = self.child.lock()
            && let Some(child) = slot.as_mut() {
                let _ = child.kill();
            }
    }

    fn idle(&self) -> bool {
        self.asked.lock().ok().and_then(|t| *t).is_none_or(|t| t.elapsed() >= NVIDIA_IDLE)
    }
}

/// NVIDIA readings through a single long-lived `nvidia-smi -l`, instead of a new process
/// (fork, exec and NVML initialization) on every collection.
#[derive(Default)]
struct Nvidia {
    stream: Arc<Stream>,
    /// Channel of the running reader; `None` = none running.
    rx: Option<Receiver<GpuSnapshot>>,
    last: Option<(GpuSnapshot, Instant)>,
    /// When the current reader started, and whether it has delivered any reading yet.
    started: Option<Instant>,
    delivered: bool,
    /// Starts in a row that yielded no reading.
    failures: u32,
}

impl Nvidia {
    /// Most recent reading. `None` = `nvidia-smi` doesn't work here (give up on it).
    fn read(&mut self) -> Option<GpuSnapshot> {
        if let Ok(mut asked) = self.stream.asked.lock() {
            *asked = Some(Instant::now());
        }
        // first notice whether the previous reader ended (the app was disabled for a while,
        // or `nvidia-smi` died): that way the new one starts in this collection, not the next
        self.drain();
        if self.rx.is_none() {
            if self.failures >= NVIDIA_TRIES {
                return None;
            }
            self.start();
        }
        // running, but silent: it is stuck. Kill it; the reader ends and the next collection restarts.
        let quiet = self.last.as_ref().map_or(self.started, |(_, at)| Some(*at));
        if self.rx.is_some() && quiet.is_some_and(|t| t.elapsed() >= NVIDIA_STALE) {
            self.stream.kill();
        }
        Some(match &self.last {
            Some((snap, at)) if at.elapsed() < NVIDIA_STALE => snap.clone(),
            _ => GpuSnapshot::default(),
        })
    }

    fn start(&mut self) {
        let (tx, rx) = channel();
        let stream = self.stream.clone();
        let spawned = std::thread::Builder::new().name("nvidia-smi".into()).spawn(move || watch(tx, stream));
        if spawned.is_err() {
            self.failures += 1;
            return;
        }
        self.started = Some(Instant::now());
        self.delivered = false;
        // the first line comes out within a fraction of a second
        if let Ok(snap) = rx.recv_timeout(NVIDIA_FIRST) {
            self.accept(snap);
        }
        self.rx = Some(rx);
    }

    fn accept(&mut self, snap: GpuSnapshot) {
        self.last = Some((snap, Instant::now()));
        self.delivered = true;
        self.failures = 0;
    }

    /// Keeps the newest reading and notices when the reader ended.
    fn drain(&mut self) {
        let Some(rx) = self.rx.take() else { return };
        loop {
            match rx.try_recv() {
                Ok(snap) => self.accept(snap),
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    self.rx = Some(rx);
                    return;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    // the reader exited: `nvidia-smi` died (or never started)
                    if !self.delivered {
                        self.failures += 1;
                    }
                    return;
                }
            }
        }
    }
}

impl Drop for Nvidia {
    fn drop(&mut self) {
        self.stream.kill();
    }
}

/// Reader thread: runs `nvidia-smi -l` and delivers each line. Ends when the process ends,
/// when the provider goes away or when nobody has asked for a reading for a while.
fn watch(tx: Sender<GpuSnapshot>, stream: Arc<Stream>) {
    let spawned = exec::command("nvidia-smi")
        .args([
            "--query-gpu=utilization.gpu,memory.used,memory.total,temperature.gpu",
            "--format=csv,noheader,nounits",
            "-l",
            &NVIDIA_EVERY.as_secs().to_string(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = spawned else { return };
    let stdout = child.stdout.take();
    if let Ok(mut slot) = stream.child.lock() {
        *slot = Some(child);
    }
    if let Some(stdout) = stdout {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if stream.idle() {
                break;
            }
            if let Some(snap) = parse_nvidia(&line)
                && tx.send(snap).is_err() {
                    break;
                }
        }
    }
    let child = stream.child.lock().ok().and_then(|mut slot| slot.take());
    if let Some(mut child) = child {
        let _ = child.kill();
        let _ = child.wait();
    }
}

pub fn parse_nvidia(text: &str) -> Option<GpuSnapshot> {
    let f: Vec<f32> = text.lines().next()?.split(',').map(|x| x.trim().parse().unwrap_or(0.0)).collect();
    (f.len() >= 4).then(|| GpuSnapshot {
        available: true,
        vendor: "nvidia",
        util: f[0],
        vram_used_mib: f[1],
        vram_total_mib: f[2],
        temp: f[3],
    })
}

fn amd_card() -> Option<PathBuf> {
    let mut cards: Vec<PathBuf> = fs::read_dir("/sys/class/drm")
        .ok()?
        .flatten()
        .map(|e| e.path().join("device"))
        .filter(|d| d.join("gpu_busy_percent").exists() && d.join("mem_info_vram_total").exists())
        .collect();
    cards.sort();
    cards.into_iter().next()
}

fn read_amd(dev: &std::path::Path) -> GpuSnapshot {
    let num = |name: &str| -> f32 {
        fs::read_to_string(dev.join(name)).ok().and_then(|t| t.trim().parse().ok()).unwrap_or(0.0)
    };
    let temp = fs::read_dir(dev.join("hwmon"))
        .ok()
        .and_then(|mut d| d.next())
        .and_then(|e| e.ok())
        .and_then(|e| fs::read_to_string(e.path().join("temp1_input")).ok())
        .and_then(|t| t.trim().parse::<f32>().ok())
        .map(|m| m / 1000.0)
        .unwrap_or(0.0);
    GpuSnapshot {
        available: true,
        vendor: "amd",
        util: num("gpu_busy_percent"),
        vram_used_mib: num("mem_info_vram_used") / 1048576.0,
        vram_total_mib: num("mem_info_vram_total") / 1048576.0,
        temp,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nvidia() {
        let g = parse_nvidia("37, 2048, 8192, 61\n").unwrap();
        assert_eq!((g.util, g.vram_used_mib, g.vram_total_mib, g.temp), (37.0, 2048.0, 8192.0, 61.0));
        assert!(parse_nvidia("").is_none());
    }

    #[test]
    fn stale_reading_becomes_unavailable_and_repeated_failures_give_up() {
        let mut n = Nvidia::default();
        let snap = parse_nvidia("10, 1, 2, 30").unwrap();
        // reader "running" and a fresh reading
        let (tx, rx) = channel();
        n.rx = Some(rx);
        tx.send(snap.clone()).unwrap();
        assert_eq!(n.read(), Some(snap.clone()));
        // the same reading, too old
        n.last = Some((snap, Instant::now() - NVIDIA_STALE));
        assert_eq!(n.read(), Some(GpuSnapshot::default()));
        // the reader ended without ever delivering anything, several times in a row: give up
        drop(tx);
        let mut n = Nvidia::default();
        n.failures = NVIDIA_TRIES - 1;
        let (tx, rx) = channel::<GpuSnapshot>();
        n.rx = Some(rx);
        n.started = Some(Instant::now());
        drop(tx);
        n.drain();
        assert_eq!(n.failures, NVIDIA_TRIES);
        assert_eq!(n.read(), None);
    }
}
