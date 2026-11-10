//! Mini-app Sinais vitais: CPU, RAM, GPU e disco, cada um com uma mascote.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use slint::{ComponentHandle, Model, ModelRc, VecModel};

use crate::core::bus::Event;
use crate::core::manifest::Manifest;
use crate::core::miniapp::{Host, MiniApp};
use crate::mascot::{Mood, render, sprites};
use crate::providers::docker::format_bytes;
use crate::providers::gpu::GpuSnapshot;
use crate::providers::sys::SysSnapshot;
use crate::ui::{HubWindow, VitalCard, VitalsApp};

const HISTORY: usize = 30; // ~1 minuto com leituras a cada 2 s
const CPU: usize = 0;
const RAM: usize = 1;
const GPU: usize = 2;
const DISK: usize = 3;

#[derive(Default, Clone)]
struct Vital {
    available: bool,
    value: f32,
    detail: String,
    temp: f32,
    mood: Mood,
    hot: bool,
    history: VecDeque<f32>,
}

pub struct VitalsMini {
    host: Rc<Host>,
    manifest: Manifest,
    vitals: [Vital; 4],
    gpu_vendor: &'static str,
    net: (f64, f64),
    frame: u32,
    cards: Rc<VecModel<VitalCard>>,
}

impl VitalsMini {
    pub fn create(host: Rc<Host>, ui: &HubWindow) -> Rc<RefCell<Self>> {
        let manifest = Manifest::parse(include_str!("../../miniapps/vitals/manifest.toml"))
            .expect("manifesto dos Sinais vitais inválido");
        let cards = Rc::new(VecModel::from(vec![VitalCard::default(); 4]));
        ui.global::<VitalsApp>().set_cards(ModelRc::from(cards.clone()));
        Rc::new(RefCell::new(VitalsMini {
            host,
            manifest,
            vitals: Default::default(),
            gpu_vendor: "",
            net: (0.0, 0.0),
            frame: 1,
            cards,
        }))
    }

    fn mascot_of(&self, i: usize) -> String {
        let cfg = self.host.config.borrow();
        let key = ["cpu_mascot", "ram_mascot", "gpu_mascot", "disk_mascot"][i];
        let choice = cfg.string(&self.manifest, key);
        if choice == "auto" {
            if self.gpu_vendor == "amd" { "amd".into() } else { "nvidia".into() }
        } else {
            choice
        }
    }

    fn apply_sys(&mut self, s: &SysSnapshot) {
        let pct = |used: u64, total: u64| if total > 0 { used as f32 / total as f32 * 100.0 } else { 0.0 };
        let gib = |b: u64| format_bytes(b as f64);

        let cpu = &mut self.vitals[CPU];
        cpu.available = true;
        cpu.value = s.cpu;
        cpu.temp = s.cpu_temp;
        cpu.detail = format!("{} núcleos", std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1));

        let ram = &mut self.vitals[RAM];
        ram.available = s.mem_total > 0;
        ram.value = pct(s.mem_used, s.mem_total);
        ram.detail = format!("{} / {}", gib(s.mem_used), gib(s.mem_total));

        let disk = &mut self.vitals[DISK];
        disk.available = s.disk_total > 0;
        disk.value = pct(s.disk_used, s.disk_total);
        disk.detail = format!("{} / {} em /", gib(s.disk_used), gib(s.disk_total));

        self.net = (s.net_rx, s.net_tx);
        for i in [CPU, RAM, DISK] {
            self.push_history(i);
        }
    }

    fn apply_gpu(&mut self, g: &GpuSnapshot) {
        self.gpu_vendor = g.vendor;
        let gpu = &mut self.vitals[GPU];
        gpu.available = g.available;
        gpu.value = g.util;
        gpu.temp = g.temp;
        gpu.detail = if g.available {
            format!(
                "VRAM {} / {}",
                format_bytes(g.vram_used_mib as f64 * 1048576.0),
                format_bytes(g.vram_total_mib as f64 * 1048576.0)
            )
        } else {
            String::new()
        };
        self.push_history(GPU);
    }

    fn push_history(&mut self, i: usize) {
        let v = &mut self.vitals[i];
        v.history.push_back(v.value);
        while v.history.len() > HISTORY {
            v.history.pop_front();
        }
    }

    /// Recalcula o humor de cada cartão pelos limites da configuração.
    fn update_moods(&mut self) {
        let (warn, alarm, hot, critical) = {
            let cfg = self.host.config.borrow();
            let m = &self.manifest;
            (
                cfg.int(m, "warn") as f32,
                cfg.int(m, "alarm") as f32,
                cfg.int(m, "temp_hot") as f32,
                cfg.int(m, "temp_critical") as f32,
            )
        };
        for (i, v) in self.vitals.iter_mut().enumerate() {
            // disco cheio é outra escala: só preocupa perto do fim
            let (w, a) = if i == DISK { (85.0, 95.0) } else { (warn, alarm) };
            let mut mood = Mood::from_level(v.value, w, a, v.available);
            if i == DISK && v.available && mood == Mood::Sleep {
                mood = Mood::Idle; // disco não "dorme" por estar vazio
            }
            v.hot = v.temp >= hot;
            if v.temp >= critical {
                mood = mood.worst(Mood::Alarm);
            } else if v.hot {
                mood = mood.worst(Mood::Busy);
            }
            v.mood = mood;
        }
    }

    fn card(&self, i: usize) -> VitalCard {
        let v = &self.vitals[i];
        let show_temps = self.host.config.borrow().bool(&self.manifest, "show_temps");
        let title = ["CPU", "RAM", "GPU", "DISCO"][i];
        VitalCard {
            title: title.into(),
            mascot: render::image(sprites::get(&self.mascot_of(i)), self.host.theme_for(&self.manifest), v.mood, self.frame),
            mood: v.mood.as_str().into(),
            available: v.available,
            value: v.value,
            value_text: format!("{:.0}%", v.value).into(),
            detail: v.detail.clone().into(),
            temp_text: if show_temps && v.temp > 0.0 { format!("{:.0} °C", v.temp).into() } else { "".into() },
            hot: v.hot,
            spark: spark_path(&v.history).into(),
        }
    }

    fn push_cards(&self, ui: &HubWindow) {
        for i in 0..4 {
            self.cards.set_row_data(i, self.card(i));
        }
        let (rx, tx) = self.net;
        ui.global::<VitalsApp>().set_net_text(
            format!("rede  ↓ {}/s   ↑ {}/s", format_bytes(rx), format_bytes(tx)).into(),
        );
    }
}

/// Histórico (0..100) -> caminho SVG num quadro 1000×100.
pub fn spark_path(h: &VecDeque<f32>) -> String {
    if h.len() < 2 {
        return "M 0 98 L 1000 98".into();
    }
    let n = (h.len() - 1) as f32;
    h.iter()
        .enumerate()
        .map(|(i, v)| {
            let x = i as f32 * 1000.0 / n;
            let y = 98.0 - v.clamp(0.0, 100.0) * 0.96;
            format!("{} {x:.1} {y:.1}", if i == 0 { "M" } else { "L" })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

impl MiniApp for VitalsMini {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn enable(&mut self, ui: &HubWindow) {
        self.push_cards(ui);
    }

    fn disable(&mut self, _ui: &HubWindow) {
        for v in &mut self.vitals {
            v.history.clear();
        }
    }

    fn on_event(&mut self, ev: &Event, ui: &HubWindow) {
        match ev {
            Event::Sys(s) => self.apply_sys(s),
            Event::Gpu(g) => self.apply_gpu(g),
            _ => return,
        }
        self.update_moods();
        self.push_cards(ui);
    }

    fn tick(&mut self, frame: u32, _ui: &HubWindow) {
        // só redesenha as mascotes; o resto muda com as leituras
        self.frame = frame;
        for i in 0..4 {
            if let Some(mut c) = self.cards.row_data(i) {
                let v = &self.vitals[i];
                c.mascot = render::image(sprites::get(&self.mascot_of(i)), self.host.theme_for(&self.manifest), v.mood, frame);
                self.cards.set_row_data(i, c);
            }
        }
    }

    fn settings_changed(&mut self, ui: &HubWindow) {
        self.update_moods();
        self.push_cards(ui);
    }

    fn mood(&self) -> Mood {
        // no hub, sinais vitais só preocupam quando algo satura ou esquenta
        let worst = self.vitals.iter().map(|v| v.mood).fold(Mood::Sleep, Mood::worst);
        if worst >= Mood::Busy { worst } else { Mood::Sleep }
    }

    fn mascot_key(&self) -> String {
        // a mascote do recurso em pior estado
        let worst = (0..4).max_by_key(|&i| self.vitals[i].mood).unwrap_or(CPU);
        self.mascot_of(worst)
    }

    fn summary(&self) -> String {
        let [cpu, ram, gpu, _] = &self.vitals;
        let mut s = format!("CPU {:.0}% · RAM {:.0}%", cpu.value, ram.value);
        if gpu.available {
            s += &format!(" · GPU {:.0}%", gpu.value);
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caminho_da_sparkline() {
        let h: VecDeque<f32> = [0.0, 50.0, 100.0].into_iter().collect();
        assert_eq!(spark_path(&h), "M 0.0 98.0 L 500.0 50.0 L 1000.0 2.0");
        assert_eq!(spark_path(&VecDeque::new()), "M 0 98 L 1000 98");
    }
}
