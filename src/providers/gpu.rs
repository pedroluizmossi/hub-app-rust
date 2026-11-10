//! GPU: NVIDIA via `nvidia-smi`; senão a primeira placa AMD (amdgpu) via sysfs.

use std::fs;
use std::path::PathBuf;

use crate::core::exec;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct GpuSnapshot {
    pub available: bool,
    /// "nvidia" | "amd" | ""
    pub vendor: &'static str,
    pub util: f32,
    pub vram_used_mib: f32,
    pub vram_total_mib: f32,
    /// °C; 0 = sem sensor.
    pub temp: f32,
}

enum Source {
    Nvidia,
    Amd(PathBuf),
    None,
}

pub struct GpuProvider {
    source: Source,
}

impl GpuProvider {
    /// Detecta a placa uma vez só.
    pub fn detect() -> Self {
        let source = if exec::which("nvidia-smi") {
            Source::Nvidia
        } else {
            amd_card().map(Source::Amd).unwrap_or(Source::None)
        };
        GpuProvider { source }
    }

    pub fn collect(&mut self) -> GpuSnapshot {
        match &self.source {
            Source::Nvidia => {
                let args: Vec<String> = [
                    "--query-gpu=utilization.gpu,memory.used,memory.total,temperature.gpu",
                    "--format=csv,noheader,nounits",
                ]
                .iter()
                .map(|s| s.to_string())
                .collect();
                exec::run("nvidia-smi", &args).ok().and_then(|t| parse_nvidia(&t)).unwrap_or_default()
            }
            Source::Amd(dev) => read_amd(dev),
            Source::None => GpuSnapshot::default(),
        }
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
}
