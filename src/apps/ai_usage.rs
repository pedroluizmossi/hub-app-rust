//! Cartões das assinaturas de IA; dados vêm dos provedores na thread de coleta.
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use slint::{ComponentHandle, Model, ModelRc, VecModel};

use crate::core::bus::{Event, Topic};
use crate::core::manifest::Manifest;
use crate::core::miniapp::{Host, MiniApp};
use crate::mascot::anim::{Animator, Reaction};
use crate::mascot::{Mood, render};
use crate::providers::ai_usage::{self as provider, UsageSnapshot, UsageWindow};
use crate::ui::{AiUsageApp, HubWindow, UsageCard, UsageLine};

#[path = "ai_mascots.rs"]
mod ai_mascots;

const N: usize = provider::SOURCES.len();
/// Chave de config que liga cada fonte, na ordem de `provider::SOURCES`.
const SHOW_KEYS: [&str; N] = ["show_codex", "show_claude", "show_kiro", "show_opencode"];
/// Abaixo do alarme menos isto, o aviso daquela janela "rearma".
const SLACK: f64 = 10.0;

/// Limites (em %) lidos da config: agitado e alarme.
#[derive(Clone, Copy)]
struct Levels {
    warn: f64,
    alarm: f64,
}

impl Levels {
    /// Nível de uma janela de uso. Vale para a cor da barra e para o humor.
    fn of(self, used: f64) -> Mood {
        if used >= self.alarm {
            Mood::Alarm
        } else if used >= self.warn {
            Mood::Busy
        } else {
            Mood::Idle
        }
    }
}

pub struct AiUsageMini {
    host: Rc<Host>,
    manifest: Manifest,
    snap: UsageSnapshot,
    cards: Rc<VecModel<UsageCard>>,
    /// Fonte (índice em `snap.cards`) mostrada em cada linha da tela.
    rows: Vec<usize>,
    mascots: [render::Cache; N],
    anims: [Animator; N],
    reactions: [Option<Reaction>; N],
    /// Janelas que já avisaram (fonte + rótulo), até voltarem abaixo do alarme.
    alerted: HashSet<String>,
    /// Fontes ligadas na última leitura da config (para pedir leitura das recém-ligadas).
    enabled: u8,
}

impl AiUsageMini {
    pub fn create(host: Rc<Host>, ui: &HubWindow) -> Rc<RefCell<Self>> {
        let cards = Rc::new(VecModel::from(Vec::<UsageCard>::new()));
        ui.global::<AiUsageApp>().set_cards(ModelRc::from(cards.clone()));
        let app = Rc::new(RefCell::new(Self {
            host,
            manifest: Manifest::parse(include_str!("../../miniapps/ai_usage/manifest.toml")).expect("manifesto inválido"),
            snap: UsageSnapshot::default(),
            cards,
            rows: Vec::new(),
            mascots: Default::default(),
            anims: std::array::from_fn(|i| Animator::new(i as u32)),
            reactions: [None; N],
            alerted: HashSet::new(),
            enabled: 0b1111,
        }));
        let weak = Rc::downgrade(&app);
        ui.global::<AiUsageApp>().on_refresh(move || {
            if let Some(app) = weak.upgrade() { app.borrow().host.bus.poke(Topic::AiUsage); }
        });
        let weak = Rc::downgrade(&app);
        ui.global::<AiUsageApp>().on_mascot_poked(move |row| {
            let Some(app) = weak.upgrade() else { return };
            let mut app = app.borrow_mut();
            let Some(&i) = app.rows.get(row as usize) else { return };
            if app.host.config.borrow().reduce_motion { return; }
            let mood = app.card_mood(i);
            if app.anims[i].poke(mood, Instant::now()) && app.refresh_mascot(i) {
                app.update_mascot_row(row as usize);
            }
        });
        app
    }

    fn levels(&self) -> Levels {
        let cfg = self.host.config.borrow();
        let warn = cfg.int(&self.manifest, "warn") as f64;
        let alarm = cfg.int(&self.manifest, "alarm") as f64;
        Levels { warn: warn.min(alarm), alarm }
    }

    /// Lê da config as fontes ligadas e o intervalo, e repassa ao provedor (outra thread).
    /// Fonte recém-ligada pede uma leitura na hora.
    fn sync_provider(&mut self) {
        let (enabled, poll) = {
            let cfg = self.host.config.borrow();
            let enabled = SHOW_KEYS.iter().enumerate()
                .fold(0u8, |bits, (i, key)| if cfg.bool(&self.manifest, key) { bits | 1 << i } else { bits });
            (enabled, cfg.int(&self.manifest, "poll").max(1) as u64 * 60)
        };
        provider::ENABLED.store(enabled, Ordering::Relaxed);
        provider::POLL_SECS.store(poll, Ordering::Relaxed);
        if enabled & !self.enabled != 0 { self.host.bus.poke(Topic::AiUsage); }
        self.enabled = enabled;
    }

    fn card_mood(&self, i: usize) -> Mood {
        let card = &self.snap.cards[i];
        if card.stale || card.windows.is_empty() { return Mood::Sleep; }
        self.levels().of(card.windows.iter().map(|w| w.used).fold(0.0, f64::max))
    }

    fn refresh_mascot(&mut self, i: usize) -> bool {
        let mood = self.card_mood(i);
        let Some(sprite) = ai_mascots::get(&self.snap.cards[i].name) else { return false };
        let look = self.anims[i].look(mood, Instant::now());
        let changed = std::mem::replace(&mut self.reactions[i], look.reaction) != look.reaction;
        self.mascots[i].update_look(sprite, self.host.theme_for(&self.manifest), &look) || changed
    }

    fn update_mascot_row(&self, row: usize) {
        let i = self.rows[row];
        if let Some(mut card) = self.cards.row_data(row) {
            card.mascot = self.mascots[i].image();
            card.reaction = self.reactions[i].map_or("", Reaction::as_str).into();
            self.cards.set_row_data(row, card);
        }
    }

    /// Fontes que aparecem na tela, na ordem dos cartões.
    fn visible_rows(&self) -> Vec<usize> {
        let hide_empty = self.host.config.borrow().bool(&self.manifest, "hide_empty");
        self.snap.cards.iter().enumerate().take(N)
            .filter(|(i, card)| self.enabled & (1 << i) != 0 && !(hide_empty && card.windows.is_empty()))
            .map(|(i, _)| i).collect()
    }

    /// Avisa uma vez quando uma janela de uso passa do alarme; rearma quando ela cai.
    fn check_alerts(&mut self) {
        let on = self.host.config.borrow().bool(&self.manifest, "alerts");
        if !on {
            self.alerted.clear();
            return;
        }
        let alarm = self.levels().alarm;
        let mut out = Vec::new();
        for (i, card) in self.snap.cards.iter().enumerate().take(N) {
            if self.enabled & (1 << i) == 0 || card.stale { continue; }
            for w in &card.windows {
                let key = format!("ai_usage:{}:{}", card.name, w.label);
                if w.used >= alarm {
                    if self.alerted.insert(key.clone()) {
                        let body = match reset_text(w, false) {
                            reset if reset.is_empty() => w.detail.clone(),
                            reset => reset,
                        };
                        out.push((key, format!("{} · {} em {:.0}%", card.name, w.label, w.used), body));
                    }
                } else if w.used < alarm - SLACK {
                    self.alerted.remove(&key);
                }
            }
        }
        let mut n = self.host.notifier.borrow_mut();
        for (key, title, body) in out {
            n.send(&key, &title, &body, true);
        }
    }

    fn push(&mut self, ui: &HubWindow) {
        self.sync_provider();
        let g = ui.global::<AiUsageApp>();
        g.set_theme(render::colors(self.host.theme_for(&self.manifest)));
        let (relative, compact) = {
            let cfg = self.host.config.borrow();
            (cfg.bool(&self.manifest, "reset_relative"), cfg.bool(&self.manifest, "compact"))
        };
        g.set_compact(compact);
        g.set_grid(self.host.config.borrow().bool(&self.manifest, "grid"));
        let levels = self.levels();
        self.rows = self.visible_rows();
        for i in self.rows.clone() { self.refresh_mascot(i); }
        let cards = self.rows.iter().map(|&i| {
            let card = &self.snap.cards[i];
            let lines: Vec<UsageLine> = card.windows.iter().map(|w| UsageLine {
                label: w.label.clone().into(), value: w.used as f32,
                detail: if w.detail.is_empty() { format!("{:.1}% usado", w.used) } else { w.detail.clone() }.into(),
                reset: reset_text(w, relative).into(),
                level: levels.of(w.used).as_str().into(),
            }).collect();
            let status = match card.updated_at {
                Some(at) => format!("{} · {}", card.status, age(at)),
                None => card.status.clone(),
            };
            UsageCard { name: card.name.clone().into(), status: status.into(), plan: card.plan.clone().into(),
                stale: card.stale, lines: ModelRc::new(VecModel::from(lines)),
                mascot: self.mascots[i].image(), mood: self.card_mood(i).as_str().into(),
                reaction: self.reactions[i].map_or("", Reaction::as_str).into() }
        }).collect::<Vec<_>>();
        self.cards.set_vec(cards);
    }
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

fn age(at: i64) -> String {
    let minutes = now().saturating_sub(at) / 60;
    if minutes < 1 { "agora".into() } else if minutes < 60 { format!("há {minutes} min") }
    else { format!("há {} h", minutes / 60) }
}

/// Quanto falta, em texto curto: "35 min", "2 h 15 min", "3 d 4 h".
fn remaining(secs: i64) -> String {
    let minutes = (secs.max(0) + 59) / 60;
    let (d, h, m) = (minutes / 1440, minutes / 60 % 24, minutes % 60);
    match (d, h) {
        (0, 0) => format!("{m} min"),
        (0, _) if m == 0 => format!("{h} h"),
        (0, _) => format!("{h} h {m} min"),
        (_, 0) => format!("{d} d"),
        _ => format!("{d} d {h} h"),
    }
}

fn reset_text(w: &UsageWindow, relative: bool) -> String {
    if !w.reset_date.is_empty() { return format!("Renova em {}", w.reset_date); }
    let Some(at) = w.resets_at.filter(|t| *t > 0) else { return String::new() };
    if relative { return format!("Renova em {}", remaining(at - now())); }
    let time = at as libc::time_t;
    let mut local: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: ponteiros válidos para dados locais; a saída pertence à chamada.
    if unsafe { libc::localtime_r(&time, &mut local) }.is_null() { return String::new() }
    format!("Renova {:02}/{:02} às {:02}:{:02}", local.tm_mday, local.tm_mon + 1, local.tm_hour, local.tm_min)
}

impl MiniApp for AiUsageMini {
    fn manifest(&self) -> &Manifest { &self.manifest }
    fn enable(&mut self, ui: &HubWindow) {
        self.sync_provider();
        self.host.bus.poke(Topic::AiUsage);
        self.push(ui);
    }
    fn on_event(&mut self, event: &Event, ui: &HubWindow) {
        if let Event::AiUsage(snap) = event {
            self.snap = snap.clone();
            self.check_alerts();
            self.push(ui);
        }
    }
    fn tick(&mut self, _frame: u32, _ui: &HubWindow) {
        for row in 0..self.rows.len() {
            if self.refresh_mascot(self.rows[row]) { self.update_mascot_row(row); }
        }
    }
    fn settings_changed(&mut self, ui: &HubWindow) { self.push(ui); }
    fn mood(&self) -> Mood {
        let levels = self.levels();
        let highest = self.snap.cards.iter().enumerate()
            .filter(|(i, c)| self.enabled & (1 << i) != 0 && !c.stale).flat_map(|(_, c)| &c.windows)
            .map(|w| w.used).fold(0.0, f64::max);
        levels.of(highest)
    }
    fn summary(&self) -> String {
        let shown = self.rows.len();
        let active = self.rows.iter().filter(|&&i| !self.snap.cards[i].windows.is_empty()).count();
        format!("Assinaturas de IA: {active}/{shown} fontes disponíveis")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tempo_restante_curto() {
        assert_eq!(remaining(30), "1 min");
        assert_eq!(remaining(35 * 60), "35 min");
        assert_eq!(remaining(2 * 3600), "2 h");
        assert_eq!(remaining(2 * 3600 + 15 * 60), "2 h 15 min");
        assert_eq!(remaining(3 * 86400), "3 d");
        assert_eq!(remaining(3 * 86400 + 4 * 3600), "3 d 4 h");
        assert_eq!(remaining(-10), "0 min");
    }

    #[test]
    fn niveis_pela_config() {
        let l = Levels { warn: 60.0, alarm: 80.0 };
        assert_eq!(l.of(59.0), Mood::Idle);
        assert_eq!(l.of(60.0), Mood::Busy);
        assert_eq!(l.of(80.0), Mood::Alarm);
    }
}
