//! Cartões das assinaturas de IA; dados vêm dos provedores na thread de coleta.
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use slint::{ComponentHandle, Model, ModelRc, VecModel};

use crate::core::bus::{Event, Topic};
use crate::core::chatter::{self, Fact, FactKind, Voice};
use crate::core::manifest::Manifest;
use crate::core::miniapp::{Host, MiniApp};
use crate::mascot::anim::{Animator, Reaction};
use crate::mascot::{Mood, render};
use crate::providers::ai_usage::{self as provider, UsageSnapshot, UsageWindow};
use crate::ui::{AiUsageApp, HubWindow, UsageCard, UsageLine};

#[path = "ai_mascots.rs"]
pub(super) mod ai_mascots;
#[path = "ai_pace.rs"]
mod ai_pace;
use ai_pace::Forecast;

const N: usize = provider::SOURCES.len();
/// Chave de config que liga cada fonte, na ordem de `provider::SOURCES`.
const SHOW_KEYS: [&str; N] = ["show_codex", "show_claude", "show_kiro", "show_opencode"];
/// Programa de linha de comando de cada fonte, na ordem de `provider::SOURCES`.
const COMMANDS: [&str; N] = ["codex", "claude", "kiro-cli", "opencode"];
/// Abaixo do alarme menos isto, o aviso daquela janela "rearma".
const SLACK: f64 = 10.0;
/// A fala da previsão só diz quantos minutos faltam até este tanto ("em 900 min" não ajuda).
const FORECAST_MINUTES: i64 = 120;

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
    /// Humor de cada fonte pelo % usado (sem a previsão) na última leitura com dados,
    /// para contar quando muda.
    moods: [Mood; N],
    /// Fontes que a previsão já apontava na última leitura com dados.
    strained_before: [bool; N],
    /// Fatos para a conversa entre mascotes, até o hub recolher.
    facts: Vec<Fact>,
    /// Leituras recentes de cada janela de uso (fonte + rótulo), para medir o ritmo.
    pace: HashMap<String, ai_pace::History>,
    /// Previsão de cada janela de cada fonte, na ordem de `windows`.
    forecasts: [Vec<Option<Forecast>>; N],
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
            moods: [Mood::Sleep; N],
            strained_before: [false; N],
            facts: Vec::new(),
            pace: HashMap::new(),
            forecasts: Default::default(),
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
            if let Some(voice) = app.voice(i) {
                app.facts.push(Fact { kind: FactKind::Poked, voice, subject: String::new(), value: None });
            }
            if app.anims[i].poke(mood, Instant::now()) && app.refresh_mascot(i) {
                app.update_mascot_row(row as usize);
            }
        });
        let weak = Rc::downgrade(&app);
        ui.global::<AiUsageApp>().on_launch(move |row| {
            let Some(app) = weak.upgrade() else { return };
            app.borrow().launch(row as usize);
        });
        app
    }

    /// Abre num terminal o programa da fonte mostrada na linha `row`.
    fn launch(&self, row: usize) {
        let Some(&i) = self.rows.get(row) else { return };
        let (name, cmd) = (provider::SOURCES[i], COMMANDS[i]);
        if let Err(e) = self.host.exec_for(&self.manifest).open_terminal(name, &[cmd]) {
            self.host.notifier.borrow_mut().send(&format!("ai_usage:launch:{cmd}"), &format!("Não deu para abrir o {name}"), &e.to_string(), false);
        }
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

    /// A mascote da fonte `i`, para a conversa entre mascotes.
    fn voice(&self, i: usize) -> Option<Voice> {
        let card = self.snap.cards.get(i)?;
        Some(Voice {
            app: self.manifest.id.clone(),
            name: card.name.clone(),
            sprite: ai_mascots::get(&card.name)?,
            theme: self.host.theme_for(&self.manifest),
            mood: self.card_mood(i),
        })
    }

    /// Conta as fontes que pioraram ou melhoraram desde a última leitura. Fonte sem dados
    /// (desatualizada, sem login) não conta: o humor dela é "dormindo" por falta de leitura.
    fn collect_facts(&mut self) {
        for i in 0..self.snap.cards.len().min(N) {
            let card = &self.snap.cards[i];
            if self.enabled & (1 << i) == 0 || card.stale || card.windows.is_empty() {
                continue;
            }
            let used = card.windows.iter().map(|w| w.used).fold(0.0, f64::max);
            // O humor pelo % usado e a previsão são contados à parte: a previsão tem fala
            // própria, e o fim dela (o ritmo caiu) não é "limite renovou".
            let now = self.levels().of(used);
            let before = std::mem::replace(&mut self.moods[i], now);
            let strained = self.strained(i);
            let newly_strained = strained && !std::mem::replace(&mut self.strained_before[i], strained);
            let Some(voice) = self.voice(i) else { continue };
            let name = voice.name.clone();
            if newly_strained && now != Mood::Alarm {
                let minutes = self.forecasts[i].iter().flatten().filter(|f| f.strained)
                    .map(|f| (f.ends_at - self::now() + 59) / 60).min()
                    .filter(|m| (1..=FORECAST_MINUTES).contains(m)).map(|m| m as f32);
                self.facts.push(Fact { kind: FactKind::Forecast, voice, subject: name, value: minutes });
            } else {
                self.facts.extend(chatter::mood_fact(before, now, voice, &name, Some(used as f32)));
            }
        }
    }

    fn card_mood(&self, i: usize) -> Mood {
        let card = &self.snap.cards[i];
        if card.stale || card.windows.is_empty() { return Mood::Sleep; }
        let mood = self.levels().of(card.windows.iter().map(|w| w.used).fold(0.0, f64::max));
        if self.strained(i) { mood.max(Mood::Busy) } else { mood }
    }

    /// Alguma janela da fonte `i` acaba antes de renovar, no ritmo de agora.
    fn strained(&self, i: usize) -> bool {
        self.forecasts[i].iter().flatten().any(|f| f.strained)
    }

    /// Guarda a leitura nova de cada janela (sem horário de renovação, só para o gráfico).
    fn record_pace(&mut self) {
        for card in self.snap.cards.iter().take(N) {
            let Some(at) = card.updated_at.filter(|_| !card.stale) else { continue };
            for w in &card.windows {
                let resets_at = w.resets_at.unwrap_or(0).max(0);
                self.pace.entry(pace_key(&card.name, w)).or_default().record(at, w.used, resets_at);
            }
        }
    }

    /// Refaz as previsões (config "forecast"). Fonte desatualizada não tem previsão.
    fn refresh_forecasts(&mut self) {
        let on = self.host.config.borrow().bool(&self.manifest, "forecast");
        let now = now();
        for (i, card) in self.snap.cards.iter().enumerate().take(N) {
            self.forecasts[i] = card.windows.iter().map(|w| {
                if !on || card.stale || w.resets_at.is_none_or(|t| t <= 0) { return None; }
                self.pace.get(&pace_key(&card.name, w))?.forecast(now)
            }).collect();
        }
    }

    fn refresh_mascot(&mut self, i: usize) -> bool {
        let mood = self.card_mood(i);
        let Some(sprite) = ai_mascots::get(&self.snap.cards[i].name) else { return false };
        let look = self.anims[i].look_with(sprite, mood, Instant::now());
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
        let levels = self.levels();
        self.rows = self.visible_rows();
        for i in self.rows.clone() { self.refresh_mascot(i); }
        let cards = self.rows.iter().map(|&i| {
            let card = &self.snap.cards[i];
            let lines: Vec<UsageLine> = card.windows.iter().enumerate().map(|(k, w)| UsageLine {
                label: w.label.clone().into(), value: w.used as f32,
                detail: if w.detail.is_empty() { format!("{:.1}% usado", w.used) } else { w.detail.clone() }.into(),
                reset: match self.forecasts[i].get(k).copied().flatten() {
                    Some(f) => forecast_text(w, f, relative),
                    None => reset_text(w, relative),
                }.into(),
                level: levels.of(w.used).as_str().into(),
                spark: if card.stale { String::new() } else {
                    self.pace.get(&pace_key(&card.name, w)).map(ai_pace::History::spark).unwrap_or_default()
                }.into(),
            }).collect();
            let status = match card.updated_at {
                Some(at) => format!("{} · {}", card.status, age(at)),
                None => card.status.clone(),
            };
            let tint = ai_mascots::get(&card.name).and_then(ai_mascots::button_colors).unwrap_or((0x9399a4, 0x26282e));
            UsageCard { name: card.name.clone().into(), voice: Voice::id_of(&self.manifest.id, &card.name).into(), status: status.into(), plan: card.plan.clone().into(),
                stale: card.stale, lines: ModelRc::new(VecModel::from(lines)),
                mascot: self.mascots[i].image(), mood: self.card_mood(i).as_str().into(),
                button: render::color(tint.0), button_edge: render::color(tint.1),
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

fn pace_key(source: &str, w: &UsageWindow) -> String {
    format!("{source}:{}", w.label)
}

/// Data e hora locais de um instante.
fn local(at: i64) -> Option<libc::tm> {
    let time = at as libc::time_t;
    let mut local: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: ponteiros válidos para dados locais; a saída pertence à chamada.
    (!unsafe { libc::localtime_r(&time, &mut local) }.is_null()).then_some(local)
}

fn reset_text(w: &UsageWindow, relative: bool) -> String {
    if !w.reset_date.is_empty() { return format!("Renova em {}", w.reset_date); }
    let Some(at) = w.resets_at.filter(|t| *t > 0) else { return String::new() };
    if relative { return format!("Renova em {}", remaining(at - now())); }
    let Some(local) = local(at) else { return String::new() };
    format!("Renova {:02}/{:02} às {:02}:{:02}", local.tm_mday, local.tm_mon + 1, local.tm_hour, local.tm_min)
}

/// Hora curta: "16:10" se for hoje, "12/11 16:10" se for outro dia.
fn clock(at: i64) -> String {
    let (Some(t), Some(today)) = (local(at), local(now())) else { return String::new() };
    if (t.tm_year, t.tm_yday) == (today.tm_year, today.tm_yday) {
        format!("{:02}:{:02}", t.tm_hour, t.tm_min)
    } else {
        format!("{:02}/{:02} {:02}:{:02}", t.tm_mday, t.tm_mon + 1, t.tm_hour, t.tm_min)
    }
}

/// Texto da janela que, nesse ritmo, acaba antes de renovar. O fim vem primeiro: se a
/// linha não couber, o que se perde é a renovação.
fn forecast_text(w: &UsageWindow, f: Forecast, relative: bool) -> String {
    let resets_at = w.resets_at.unwrap_or(0);
    if relative {
        let now = now();
        format!("Acaba em ~{} · renova em {}", remaining(f.ends_at - now), remaining(resets_at - now))
    } else {
        format!("Acaba ~{} · renova {}", clock(f.ends_at), clock(resets_at))
    }
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
            self.record_pace();
            self.refresh_forecasts();
            self.check_alerts();
            self.collect_facts();
            self.push(ui);
        }
    }
    fn tick(&mut self, _frame: u32, _ui: &HubWindow) {
        for row in 0..self.rows.len() {
            if self.refresh_mascot(self.rows[row]) { self.update_mascot_row(row); }
        }
    }
    fn settings_changed(&mut self, ui: &HubWindow) {
        self.refresh_forecasts();
        self.push(ui);
    }
    fn take_facts(&mut self) -> Vec<Fact> { std::mem::take(&mut self.facts) }
    fn voices(&self) -> Vec<Voice> { self.rows.iter().filter_map(|&i| self.voice(i)).collect() }
    fn lines(&self) -> &'static str { include_str!("../../miniapps/ai_usage/lines.toml") }
    fn mood(&self) -> Mood {
        let levels = self.levels();
        let highest = self.snap.cards.iter().enumerate()
            .filter(|(i, c)| self.enabled & (1 << i) != 0 && !c.stale).flat_map(|(_, c)| &c.windows)
            .map(|w| w.used).fold(0.0, f64::max);
        let strained = (0..N).any(|i| self.enabled & (1 << i) != 0 && self.strained(i));
        if strained { levels.of(highest).max(Mood::Busy) } else { levels.of(highest) }
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
    fn botoes_do_terminal_tem_permissao() {
        let m = Manifest::parse(include_str!("../../miniapps/ai_usage/manifest.toml")).unwrap();
        for cmd in COMMANDS {
            assert!(m.permissions.exec.iter().any(|p| p == cmd), "{cmd} fora de permissions.exec");
        }
    }

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
