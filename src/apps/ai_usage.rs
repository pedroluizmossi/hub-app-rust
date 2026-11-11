//! Cartões das assinaturas de IA; dados vêm dos provedores na thread de coleta.
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};
use slint::{ComponentHandle, ModelRc, VecModel};

use crate::core::bus::{Event, Topic};
use crate::core::manifest::Manifest;
use crate::core::miniapp::{Host, MiniApp};
use crate::mascot::{Mood, render};
use crate::providers::ai_usage::{UsageSnapshot, UsageWindow};
use crate::ui::{AiUsageApp, HubWindow, UsageCard, UsageLine};

/// Uso (em %) a partir do qual uma janela preocupa. Valem para a cor de cada barra e
/// para o humor do app.
const BUSY_AT: f64 = 75.0;
const ALARM_AT: f64 = 90.0;

/// Nível de uma janela de uso.
fn level(used: f64) -> Mood {
    if used >= ALARM_AT {
        Mood::Alarm
    } else if used >= BUSY_AT {
        Mood::Busy
    } else {
        Mood::Idle
    }
}

pub struct AiUsageMini {
    host: Rc<Host>,
    manifest: Manifest,
    snap: UsageSnapshot,
}

impl AiUsageMini {
    pub fn create(host: Rc<Host>, ui: &HubWindow) -> Rc<RefCell<Self>> {
        let app = Rc::new(RefCell::new(Self {
            host,
            manifest: Manifest::parse(include_str!("../../miniapps/ai_usage/manifest.toml")).expect("manifesto inválido"),
            snap: UsageSnapshot::default(),
        }));
        let weak = Rc::downgrade(&app);
        ui.global::<AiUsageApp>().on_refresh(move || {
            if let Some(app) = weak.upgrade() { app.borrow().host.bus.poke(Topic::AiUsage); }
        });
        app
    }

    fn push(&self, ui: &HubWindow) {
        let g = ui.global::<AiUsageApp>();
        g.set_theme(render::colors(self.host.theme_for(&self.manifest)));
        let cards = self.snap.cards.iter().map(|card| {
            let lines: Vec<UsageLine> = card.windows.iter().map(|w| UsageLine {
                label: w.label.clone().into(), value: w.used as f32,
                detail: if w.detail.is_empty() { format!("{:.1}% usado", w.used) } else { w.detail.clone() }.into(),
                reset: reset_text(w).into(),
                level: level(w.used).as_str().into(),
            }).collect();
            let status = match card.updated_at {
                Some(at) => format!("{} · {}", card.status, age(at)),
                None => card.status.clone(),
            };
            UsageCard { name: card.name.clone().into(), status: status.into(), plan: card.plan.clone().into(),
                stale: card.stale, lines: ModelRc::new(VecModel::from(lines)) }
        }).collect::<Vec<_>>();
        g.set_cards(ModelRc::new(VecModel::from(cards)));
    }
}

fn age(at: i64) -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    let minutes = now.saturating_sub(at) / 60;
    if minutes < 1 { "agora".into() } else if minutes < 60 { format!("há {minutes} min") }
    else { format!("há {} h", minutes / 60) }
}

fn reset_text(w: &UsageWindow) -> String {
    if !w.reset_date.is_empty() { return format!("Renova em {}", w.reset_date); }
    let Some(at) = w.resets_at.filter(|t| *t > 0) else { return String::new() };
    let time = at as libc::time_t;
    let mut local: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: ponteiros válidos para dados locais; a saída pertence à chamada.
    if unsafe { libc::localtime_r(&time, &mut local) }.is_null() { return String::new() }
    format!("Renova {:02}/{:02} às {:02}:{:02}", local.tm_mday, local.tm_mon + 1, local.tm_hour, local.tm_min)
}

impl MiniApp for AiUsageMini {
    fn manifest(&self) -> &Manifest { &self.manifest }
    fn enable(&mut self, ui: &HubWindow) {
        self.host.bus.poke(Topic::AiUsage);
        self.push(ui);
    }
    fn on_event(&mut self, event: &Event, ui: &HubWindow) {
        if let Event::AiUsage(snap) = event { self.snap = snap.clone(); self.push(ui); }
    }
    fn settings_changed(&mut self, ui: &HubWindow) { self.push(ui); }
    fn mood(&self) -> Mood {
        let highest = self.snap.cards.iter().filter(|c| !c.stale).flat_map(|c| &c.windows)
            .map(|w| w.used).fold(0.0, f64::max);
        level(highest)
    }
    fn summary(&self) -> String {
        let active = self.snap.cards.iter().filter(|c| !c.windows.is_empty()).count();
        format!("Assinaturas de IA: {active}/{} fontes disponíveis", self.snap.cards.len())
    }
}
