//! Vital signs mini-app: CPU, RAM, GPU, disk and network, each with a mascot, and
//! warnings when something stays high for a while (not on spikes).

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Model, ModelRc, VecModel};

use crate::core::bus::Event;
use crate::core::chatter::{self, Fact, FactKind, Voice};
use crate::core::i18n;
use crate::core::manifest::Manifest;
use crate::core::miniapp::{Host, MiniApp};
use crate::mascot::anim::{Animator, Reaction};
use crate::mascot::{Mood, render, sprites};
use crate::providers::docker::format_bytes;
use crate::providers::gpu::GpuSnapshot;
use crate::providers::sys::SysSnapshot;
use crate::{tr, tr_noop};
use crate::ui::{HubWindow, VitalCard, VitalsApp};

const HISTORY: usize = 30; // ~1 minute with readings every 2 s
const CPU: usize = 0;
const RAM: usize = 1;
const GPU: usize = 2;
const DISK: usize = 3;
const NET: usize = 4;
const N: usize = 5;
/// Card titles (translated where shown, with [`title`]).
const TITLES: [&str; N] = ["CPU", "RAM", "GPU", tr_noop!("DISK"), tr_noop!("NETWORK")];
const MASCOT_KEYS: [&str; N] = ["cpu_mascot", "ram_mascot", "gpu_mascot", "disk_mascot", "net_mascot"];

/// Disk usage from which it warns (and the slack to warn again).
const DISK_FULL: f32 = 90.0;
/// How long something must stay high to become a warning.
const HOLD_CPU: Duration = Duration::from_secs(120);
const HOLD_RAM: Duration = Duration::from_secs(60);
const HOLD_TEMP: Duration = Duration::from_secs(30);
/// Below the threshold minus this, the warning "re-arms".
const SLACK: f32 = 10.0;
/// Minimum scale of the network chart (bytes/s): below it the bar doesn't fill.
const NET_FLOOR: f64 = 128.0 * 1024.0;

#[derive(Default, Clone)]
struct Vital {
    available: bool,
    value: f32,
    /// Big text ("37%", "↓ 2.5M/s").
    value_text: String,
    detail: String,
    temp: f32,
    mood: Mood,
    hot: bool,
    history: VecDeque<f32>,
}

/// A condition that only warns after lasting `hold`, once, until it goes back to normal.
#[derive(Default)]
struct Watch {
    since: Option<Instant>,
    fired: bool,
}

impl Watch {
    /// `high`: past the threshold; `clear`: back below the slack (re-arms the warning).
    /// Returns `true` only once, when it has been high for `hold` in a row.
    fn check(&mut self, high: bool, clear: bool, now: Instant, hold: Duration) -> bool {
        if clear {
            *self = Watch::default();
            return false;
        }
        if !high {
            self.since = None;
            return false;
        }
        let since = *self.since.get_or_insert(now);
        if !self.fired && now.duration_since(since) >= hold {
            self.fired = true;
            return true;
        }
        false
    }
}

#[derive(Default)]
struct Watches {
    cpu: Watch,
    ram: Watch,
    cpu_temp: Watch,
    gpu_temp: Watch,
    disks: HashMap<String, Watch>,
}

pub struct VitalsMini {
    host: Rc<Host>,
    manifest: Manifest,
    vitals: [Vital; N],
    gpu_vendor: &'static str,
    /// Network in bytes/s (↓ + ↑), for the chart scale.
    net_history: VecDeque<f64>,
    /// Swap in use (0..1), for the RAM mood.
    swap_frac: f32,
    watches: Watches,
    cards: Rc<VecModel<VitalCard>>,
    /// Last frame of each mascot: the image is only swapped when the pixels change.
    mascots: [render::Cache; N],
    /// Blinking and reactions of each mascot (each at its own pace).
    anims: [Animator; N],
    /// Reaction shown now on each card.
    reactions: [Option<Reaction>; N],
    /// Facts for the chat between mascots, until the hub collects them.
    facts: Vec<Fact>,
}

impl VitalsMini {
    pub fn create(host: Rc<Host>, ui: &HubWindow) -> Rc<RefCell<Self>> {
        let manifest = Manifest::parse(include_str!("../../miniapps/vitals/manifest.toml"))
            .expect("invalid Vital signs manifest");
        let cards = Rc::new(VecModel::from(vec![VitalCard::default(); N]));
        ui.global::<VitalsApp>().set_cards(ModelRc::from(cards.clone()));
        let app = Rc::new(RefCell::new(VitalsMini {
            host,
            manifest,
            vitals: Default::default(),
            gpu_vendor: "",
            net_history: VecDeque::new(),
            swap_frac: 0.0,
            watches: Watches::default(),
            cards,
            mascots: Default::default(),
            anims: std::array::from_fn(|i| Animator::new(i as u32)),
            reactions: [None; N],
            facts: Vec::new(),
        }));

        // easter egg: poke the mascot a few times
        let a = Rc::downgrade(&app);
        ui.global::<VitalsApp>().on_mascot_poked(move |i| {
            let Some(a) = a.upgrade() else { return };
            let mut a = a.borrow_mut();
            let i = i as usize;
            if i >= N || a.host.config.borrow().reduce_motion {
                return;
            }
            let mood = a.vitals[i].mood;
            let voice = a.voice(i);
            a.facts.push(Fact { kind: FactKind::Poked, voice, subject: String::new(), value: None });
            if a.anims[i].poke(mood, Instant::now()) && a.refresh_mascot(i) {
                let card = a.card(i);
                a.cards.set_row_data(i, card);
            }
        });
        app
    }

    /// Redraws mascot `i` now; `true` if the image or the reaction changed.
    fn refresh_mascot(&mut self, i: usize) -> bool {
        let key = self.mascot_of(i);
        let theme = self.host.theme_for(&self.manifest);
        let sprite = sprites::get(&key);
        let look = self.anims[i].look_with(sprite, self.vitals[i].mood, Instant::now());
        let reaction_changed = std::mem::replace(&mut self.reactions[i], look.reaction) != look.reaction;
        self.mascots[i].update_look(sprite, theme, &look) || reaction_changed
    }

    /// The mascot of card `i`, for the chat between mascots.
    fn voice(&self, i: usize) -> Voice {
        Voice {
            app: self.manifest.id.clone(),
            name: title(i).to_string(),
            sprite: sprites::get(&self.mascot_of(i)),
            theme: self.host.theme_for(&self.manifest),
            mood: self.vitals[i].mood,
        }
    }

    fn mascot_of(&self, i: usize) -> String {
        let cfg = self.host.config.borrow();
        let choice = cfg.string(&self.manifest, MASCOT_KEYS[i]);
        if choice == "auto" {
            if self.gpu_vendor == "amd" { "amd".into() } else { "nvidia".into() }
        } else {
            choice
        }
    }

    fn apply_sys(&mut self, s: &SysSnapshot) {
        let pct = |used: u64, total: u64| if total > 0 { used as f32 / total as f32 * 100.0 } else { 0.0 };
        let size = |b: u64| format_bytes(b as f64);
        let rate = |b: f64| format!("{}/s", format_bytes(b));

        // CPU: frequency and load average, instead of the number of cores
        let cpu = &mut self.vitals[CPU];
        cpu.available = true;
        cpu.value = s.cpu;
        cpu.temp = s.cpu_temp;
        let (l1, l5, l15) = s.load;
        let load = tr!("load {} {} {}", format!("{l1:.1}"), format!("{l5:.1}"), format!("{l15:.1}"));
        cpu.detail = if s.cpu_mhz > 0.0 { format!("{:.1} GHz · {load}", s.cpu_mhz / 1000.0) } else { load };

        // RAM: how much is actually free, and the swap (swap going up = short on memory)
        let ram = &mut self.vitals[RAM];
        ram.available = s.mem_total > 0;
        ram.value = pct(s.mem_used, s.mem_total);
        let free = tr!("{} free of {}", size(s.mem_total.saturating_sub(s.mem_used)), size(s.mem_total));
        ram.detail = if s.swap_total > 0 {
            format!("{free} · swap {} / {}", size(s.swap_used), size(s.swap_total))
        } else {
            format!("{free} · {}", tr!("no swap"))
        };
        self.swap_frac = if s.swap_total > 0 { s.swap_used as f32 / s.swap_total as f32 } else { 0.0 };

        // Disk: the fullest partition rules; the detail shows read/write and the partitions
        let disk = &mut self.vitals[DISK];
        disk.available = !s.disks.is_empty();
        let mut by_use: Vec<_> = s.disks.iter().collect();
        by_use.sort_by(|a, b| b.percent().total_cmp(&a.percent()));
        disk.value = by_use.first().map_or(0.0, |d| d.percent());
        let parts: Vec<String> = by_use.iter().map(|d| format!("{} {:.0}%", d.mount, d.percent())).collect();
        disk.detail = tr!("read {} · write {} · {}", rate(s.disk_read), rate(s.disk_write), parts.join(" · "));

        // Network: its own card, scaled by the peak of the last minute
        let total = s.net_rx + s.net_tx;
        self.net_history.push_back(total);
        while self.net_history.len() > HISTORY {
            self.net_history.pop_front();
        }
        let scale = self.net_history.iter().copied().fold(NET_FLOOR, f64::max);
        let net = &mut self.vitals[NET];
        net.available = !s.net_iface.is_empty();
        net.value = (total / scale * 100.0).clamp(0.0, 100.0) as f32;
        net.value_text = format!("↓ {}", rate(s.net_rx));
        let kind = if s.net_wireless { "Wi-Fi" } else { tr!("wired") };
        let ip = if s.net_ip.is_empty() { String::new() } else { format!(" · {}", s.net_ip) };
        net.detail = if net.available {
            format!("↑ {} · {kind} {}{ip}", rate(s.net_tx), s.net_iface)
        } else {
            tr!("no connection").into()
        };
        // the network chart is redrawn entirely at the current scale
        net.history = self.net_history.iter().map(|v| (v / scale * 100.0) as f32).collect();

        for i in [CPU, RAM, DISK] {
            self.push_history(i);
        }
        self.check_alerts(s);
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

    /// Sustained warnings: CPU and RAM high for a while, a partition almost full, critical temperature.
    fn check_alerts(&mut self, s: &SysSnapshot) {
        let (on, alarm, critical) = {
            let cfg = self.host.config.borrow();
            let m = &self.manifest;
            (cfg.bool(m, "alerts"), cfg.int(m, "alarm") as f32, cfg.int(m, "temp_critical") as f32)
        };
        if !on {
            self.watches = Watches::default();
            return;
        }
        let now = Instant::now();
        let mut out: Vec<(String, String, String)> = Vec::new();
        let w = &mut self.watches;

        let cpu = self.vitals[CPU].value;
        if w.cpu.check(cpu >= alarm, cpu < alarm - SLACK, now, HOLD_CPU) {
            out.push(("vitals:cpu".into(), tr!("CPU at {}% for 2 minutes", format!("{cpu:.0}")), tr!("See what is weighing it down in the system monitor.").into()));
        }
        let ram = self.vitals[RAM].value;
        if w.ram.check(ram >= alarm, ram < alarm - SLACK, now, HOLD_RAM) {
            out.push(("vitals:ram".into(), tr!("Memory at {}%", format!("{ram:.0}")), self.vitals[RAM].detail.clone()));
        }
        let t = s.cpu_temp;
        if w.cpu_temp.check(t >= critical, t < critical - SLACK, now, HOLD_TEMP) {
            out.push(("vitals:cpu-temp".into(), tr!("CPU at {} °C", format!("{t:.0}")), tr!("Critical temperature for 30 seconds.").into()));
        }
        let t = self.vitals[GPU].temp;
        if self.vitals[GPU].available && w.gpu_temp.check(t >= critical, t < critical - SLACK, now, HOLD_TEMP) {
            out.push(("vitals:gpu-temp".into(), tr!("GPU at {} °C", format!("{t:.0}")), tr!("Critical temperature for 30 seconds.").into()));
        }
        // a full partition warns right away (it doesn't swing like the CPU)
        w.disks.retain(|mount, _| s.disks.iter().any(|d| &d.mount == mount));
        for d in &s.disks {
            let p = d.percent();
            let watch = w.disks.entry(d.mount.clone()).or_default();
            if watch.check(p >= DISK_FULL, p < DISK_FULL - 2.0, now, Duration::ZERO) {
                let free = format_bytes(d.total.saturating_sub(d.used) as f64);
                out.push((format!("vitals:disk:{}", d.mount), tr!("{} is {}% full", d.mount, format!("{p:.0}")), tr!("{} left.", free)));
            }
        }

        let mut n = self.host.notifier.borrow_mut();
        for (key, title, body) in out {
            n.send(&key, &title, &body, true);
        }
    }

    /// Recomputes the mood of each card from the thresholds in the config.
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
        let swap = self.swap_frac;
        let before: [Mood; N] = std::array::from_fn(|i| self.vitals[i].mood);
        for (i, v) in self.vitals.iter_mut().enumerate() {
            if i == NET {
                // the network isn't a worry: sleeps when idle, happy when there's traffic
                v.mood = if !v.available || v.value < 1.0 { Mood::Sleep } else if v.value >= 60.0 { Mood::Happy } else { Mood::Idle };
                continue;
            }
            // a full disk is another scale: only a worry near the end
            let (w, a) = if i == DISK { (85.0, 95.0) } else { (warn, alarm) };
            let mut mood = Mood::from_level(v.value, w, a, v.available);
            if i == DISK && v.available && mood == Mood::Sleep {
                mood = Mood::Idle; // a disk doesn't "sleep" for being empty
            }
            // heavy swap use means short on memory, even with the RAM "ok"
            if i == RAM && swap >= 0.5 {
                mood = mood.worst(Mood::Busy);
            }
            v.hot = v.temp >= hot;
            if v.temp >= critical {
                mood = mood.worst(Mood::Alarm);
            } else if v.hot {
                mood = mood.worst(Mood::Busy);
            }
            v.mood = mood;
        }
        // chat between mascots: whoever got worse or better speaks (the network isn't a worry)
        for (i, &was) in before.iter().enumerate().take(NET) {
            let v = &self.vitals[i];
            if let Some(f) = chatter::mood_fact(was, v.mood, self.voice(i), title(i), Some(v.value)) {
                self.facts.push(f);
            }
        }
    }

    fn card(&self, i: usize) -> VitalCard {
        let v = &self.vitals[i];
        let show_temps = self.host.config.borrow().bool(&self.manifest, "show_temps");
        VitalCard {
            title: title(i).into(),
            voice: Voice::id_of(&self.manifest.id, title(i)).into(),
            mascot: self.mascots[i].image(),
            mood: v.mood.as_str().into(),
            reaction: self.reactions[i].map_or("", Reaction::as_str).into(),
            available: v.available,
            value: v.value,
            value_text: if v.value_text.is_empty() { format!("{:.0}%", v.value) } else { v.value_text.clone() }.into(),
            detail: v.detail.clone().into(),
            temp_text: if show_temps && v.temp > 0.0 { format!("{:.0} °C", v.temp).into() } else { "".into() },
            hot: v.hot,
            spark: spark_path(&v.history).into(),
        }
    }

    fn push_cards(&mut self) {
        for i in 0..N {
            self.refresh_mascot(i);
            self.cards.set_row_data(i, self.card(i));
        }
    }
}

/// Title of card `i` in the current language.
fn title(i: usize) -> &'static str {
    i18n::tr(TITLES[i])
}

/// History (0..100) -> SVG path in a 1000×100 box.
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
        ui.global::<VitalsApp>().set_theme(render::colors(self.host.theme_for(&self.manifest)));
        self.push_cards();
    }

    fn disable(&mut self, _ui: &HubWindow) {
        for v in &mut self.vitals {
            v.history.clear();
        }
        self.net_history.clear();
        self.watches = Watches::default();
    }

    fn on_event(&mut self, ev: &Event, _ui: &HubWindow) {
        match ev {
            Event::Sys(s) => self.apply_sys(s),
            Event::Gpu(g) => self.apply_gpu(g),
            _ => return,
        }
        self.update_moods();
        self.push_cards();
    }

    fn tick(&mut self, _frame: u32, _ui: &HubWindow) {
        // only redraws the mascots that changed; the rest changes with the readings.
        // The pace comes from each `Animator`'s clock, not from the hub frame.
        for i in 0..N {
            if self.refresh_mascot(i)
                && let Some(mut c) = self.cards.row_data(i) {
                    c.mascot = self.mascots[i].image();
                    c.reaction = self.reactions[i].map_or("", Reaction::as_str).into();
                    self.cards.set_row_data(i, c);
                }
        }
    }

    fn settings_changed(&mut self, ui: &HubWindow) {
        ui.global::<VitalsApp>().set_theme(render::colors(self.host.theme_for(&self.manifest)));
        self.update_moods();
        self.push_cards();
    }

    fn mood(&self) -> Mood {
        // in the hub, vital signs are only a worry when something saturates or heats up
        let worst = self.vitals[..NET].iter().map(|v| v.mood).fold(Mood::Sleep, Mood::worst);
        if worst >= Mood::Busy { worst } else { Mood::Sleep }
    }

    fn take_facts(&mut self) -> Vec<Fact> {
        std::mem::take(&mut self.facts)
    }

    fn voices(&self) -> Vec<Voice> {
        (0..N).filter(|&i| self.vitals[i].available).map(|i| self.voice(i)).collect()
    }

    fn lines(&self) -> &'static str {
        include_str!("../../miniapps/vitals/lines.toml")
    }

    fn mascot_key(&self) -> String {
        // the mascot of the resource in the worst state (the network doesn't count)
        let worst = (0..NET).max_by_key(|&i| self.vitals[i].mood).unwrap_or(CPU);
        self.mascot_of(worst)
    }

    fn summary(&self) -> String {
        let [cpu, ram, gpu, ..] = &self.vitals;
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
    fn sparkline_path() {
        let h: VecDeque<f32> = [0.0, 50.0, 100.0].into_iter().collect();
        assert_eq!(spark_path(&h), "M 0.0 98.0 L 500.0 50.0 L 1000.0 2.0");
        assert_eq!(spark_path(&VecDeque::new()), "M 0 98 L 1000 98");
    }

    #[test]
    fn warns_only_after_lasting_and_once() {
        let t0 = Instant::now();
        let hold = Duration::from_secs(120);
        let mut w = Watch::default();
        assert!(!w.check(true, false, t0, hold));
        assert!(!w.check(true, false, t0 + Duration::from_secs(60), hold));
        assert!(w.check(true, false, t0 + Duration::from_secs(120), hold));
        // still high: doesn't repeat
        assert!(!w.check(true, false, t0 + Duration::from_secs(600), hold));
    }

    #[test]
    fn short_spike_does_not_warn_and_slack_rearms() {
        let t0 = Instant::now();
        let hold = Duration::from_secs(120);
        let mut w = Watch::default();
        w.check(true, false, t0, hold);
        // dropped between the threshold and the slack: the clock restarts
        assert!(!w.check(false, false, t0 + Duration::from_secs(100), hold));
        assert!(!w.check(true, false, t0 + Duration::from_secs(110), hold));
        assert!(!w.check(true, false, t0 + Duration::from_secs(200), hold));
        assert!(w.check(true, false, t0 + Duration::from_secs(230), hold));
        // back to normal: re-arms and can warn again
        w.check(false, true, t0 + Duration::from_secs(300), hold);
        assert!(w.check(true, false, t0 + Duration::from_secs(300), Duration::ZERO));
    }
}
