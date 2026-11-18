//! Cards of the AI subscriptions; the data comes from the providers on the collector thread.
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
use crate::tr;
use crate::ui::{AiUsageApp, HubWindow, UsageCard, UsageLine};

#[path = "ai_mascots.rs"]
pub(super) mod ai_mascots;
#[path = "ai_pace.rs"]
mod ai_pace;
use ai_pace::Forecast;

const N: usize = provider::SOURCES.len();
/// Config key that turns each source on, in the order of `provider::SOURCES`.
const SHOW_KEYS: [&str; N] = ["show_codex", "show_claude", "show_kiro", "show_opencode"];
/// Command-line program of each source, in the order of `provider::SOURCES`.
const COMMANDS: [&str; N] = ["codex", "claude", "kiro-cli", "opencode"];
/// Below the alarm minus this, the warning of that window "re-arms".
const SLACK: f64 = 10.0;
/// The forecast line only says how many minutes are left up to this much ("in 900 min" doesn't help).
const FORECAST_MINUTES: i64 = 120;

/// Thresholds (in %) read from the config: busy and alarm.
#[derive(Clone, Copy)]
struct Levels {
    warn: f64,
    alarm: f64,
}

impl Levels {
    /// Level of a usage window. Used for the bar color and for the mood.
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
    /// Source (index into `snap.cards`) shown in each row of the screen.
    rows: Vec<usize>,
    mascots: [render::Cache; N],
    anims: [Animator; N],
    reactions: [Option<Reaction>; N],
    /// Windows that already warned (source + label), until they go back below the alarm.
    alerted: HashSet<String>,
    /// Sources enabled at the last config read (to request a reading of newly enabled ones).
    enabled: u8,
    /// Mood of each source by the % used (without the forecast) at the last reading with
    /// data, to tell when it changes.
    moods: [Mood; N],
    /// Sources the forecast already pointed at in the last reading with data.
    strained_before: [bool; N],
    /// Facts for the chat between mascots, until the hub collects them.
    facts: Vec<Fact>,
    /// Recent readings of each usage window (source + label), to measure the pace.
    pace: HashMap<String, ai_pace::History>,
    /// Forecast of each window of each source, in the order of `windows`.
    forecasts: [Vec<Option<Forecast>>; N],
}

impl AiUsageMini {
    pub fn create(host: Rc<Host>, ui: &HubWindow) -> Rc<RefCell<Self>> {
        let cards = Rc::new(VecModel::from(Vec::<UsageCard>::new()));
        ui.global::<AiUsageApp>().set_cards(ModelRc::from(cards.clone()));
        let app = Rc::new(RefCell::new(Self {
            host,
            manifest: Manifest::parse(include_str!("../../miniapps/ai_usage/manifest.toml")).expect("invalid manifest"),
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

    /// Opens the program of the source shown in row `row` in a terminal.
    fn launch(&self, row: usize) {
        let Some(&i) = self.rows.get(row) else { return };
        let (name, cmd) = (provider::SOURCES[i], COMMANDS[i]);
        if let Err(e) = self.host.exec_for(&self.manifest).open_terminal(name, &[cmd]) {
            self.host.notifier.borrow_mut().send(&format!("ai_usage:launch:{cmd}"), &tr!("Couldn't open {}", name), &e.to_string(), false);
        }
    }

    fn levels(&self) -> Levels {
        let cfg = self.host.config.borrow();
        let warn = cfg.int(&self.manifest, "warn") as f64;
        let alarm = cfg.int(&self.manifest, "alarm") as f64;
        Levels { warn: warn.min(alarm), alarm }
    }

    /// Reads the enabled sources and the interval from the config and passes them to the
    /// provider (another thread). A newly enabled source requests a reading right away.
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

    /// The mascot of source `i`, for the chat between mascots.
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

    /// Records the sources that got worse or better since the last reading. A source without
    /// data (stale, not logged in) doesn't count: its mood is "sleeping" for lack of a reading.
    fn collect_facts(&mut self) {
        for i in 0..self.snap.cards.len().min(N) {
            let card = &self.snap.cards[i];
            if self.enabled & (1 << i) == 0 || card.stale || card.windows.is_empty() {
                continue;
            }
            let used = card.windows.iter().map(|w| w.used).fold(0.0, f64::max);
            // The mood by % used and the forecast are counted separately: the forecast has its
            // own line, and its end (the pace dropped) is not "the limit renewed".
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

    /// Some window of source `i` runs out before it renews, at the current pace.
    fn strained(&self, i: usize) -> bool {
        self.forecasts[i].iter().flatten().any(|f| f.strained)
    }

    /// Stores the new reading of each window (without a renewal time, only for the chart).
    fn record_pace(&mut self) {
        for card in self.snap.cards.iter().take(N) {
            let Some(at) = card.updated_at.filter(|_| !card.stale) else { continue };
            for w in &card.windows {
                let resets_at = w.resets_at.unwrap_or(0).max(0);
                self.pace.entry(pace_key(&card.name, w)).or_default().record(at, w.used, resets_at);
            }
        }
    }

    /// Recomputes the forecasts (config "forecast"). A stale source has no forecast.
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

    /// Sources shown on screen, in card order.
    fn visible_rows(&self) -> Vec<usize> {
        let hide_empty = self.host.config.borrow().bool(&self.manifest, "hide_empty");
        self.snap.cards.iter().enumerate().take(N)
            .filter(|(i, card)| self.enabled & (1 << i) != 0 && !(hide_empty && card.windows.is_empty()))
            .map(|(i, _)| i).collect()
    }

    /// Warns once when a usage window goes past the alarm; re-arms when it drops.
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
                        out.push((key, tr!("{} · {} at {}%", card.name, w.label, format!("{:.0}", w.used)), body));
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
        let cards: Vec<(UsageCard, Vec<UsageLine>)> = self.rows.iter().map(|&i| {
            let card = &self.snap.cards[i];
            let lines: Vec<UsageLine> = card.windows.iter().enumerate().map(|(k, w)| UsageLine {
                label: w.label.clone().into(), value: w.used as f32,
                detail: if w.detail.is_empty() { tr!("{}% used", format!("{:.1}", w.used)) } else { w.detail.clone() }.into(),
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
            (UsageCard { name: card.name.clone().into(), voice: Voice::id_of(&self.manifest.id, &card.name).into(), status: status.into(), plan: card.plan.clone().into(),
                stale: card.stale, lines: ModelRc::default(),
                mascot: self.mascots[i].image(), mood: self.card_mood(i).as_str().into(),
                button: render::color(tint.0), button_edge: render::color(tint.1),
                reaction: self.reactions[i].map_or("", Reaction::as_str).into() }, lines)
        }).collect();
        self.show(cards);
    }

    /// Puts the cards on screen. Each card exists several times in the view (the measuring
    /// copies and the visible one), so rebuilding them all on every reading costs and
    /// flickers: with the same number of rows, only the rows that changed are replaced, and
    /// a row keeps its lines model when the lines are the same.
    fn show(&self, cards: Vec<(UsageCard, Vec<UsageLine>)>) {
        if cards.len() != self.cards.row_count() {
            self.cards.set_vec(cards.into_iter()
                .map(|(card, lines)| UsageCard { lines: ModelRc::new(VecModel::from(lines)), ..card }).collect::<Vec<_>>());
            return;
        }
        for (row, (mut card, lines)) in cards.into_iter().enumerate() {
            let Some(old) = self.cards.row_data(row) else { continue };
            let same_lines = old.lines.row_count() == lines.len() && old.lines.iter().zip(&lines).all(|(a, b)| a == *b);
            card.lines = if same_lines { old.lines.clone() } else { ModelRc::new(VecModel::from(lines)) };
            if card != old { self.cards.set_row_data(row, card); }
        }
    }
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

fn age(at: i64) -> String {
    age_at(at, now())
}

/// How long ago `at` was, seen at `now`: minutes, then hours, then days (from 48 h).
fn age_at(at: i64, now: i64) -> String {
    let minutes = now.saturating_sub(at) / 60;
    if minutes < 1 { tr!("just now").into() } else if minutes < 60 { tr!("{} min ago", minutes) }
    else if minutes < 48 * 60 { tr!("{} h ago", minutes / 60) }
    else { tr!("{} d ago", minutes / 1440) }
}

/// Time left, as short text: "35 min", "2 h 15 min", "3 d 4 h".
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

/// Local date and time of an instant.
fn local(at: i64) -> Option<chrono::DateTime<chrono::Local>> {
    chrono::DateTime::from_timestamp(at, 0).map(|utc| utc.with_timezone(&chrono::Local))
}

fn reset_text(w: &UsageWindow, relative: bool) -> String {
    if !w.reset_date.is_empty() { return tr!("Resets in {}", w.reset_date); }
    let Some(at) = w.resets_at.filter(|t| *t > 0) else { return String::new() };
    if relative { return tr!("Resets in {}", remaining(at - now())); }
    let (Some(local), Some(today)) = (local(at), local(now())) else { return String::new() };
    if local.date_naive() == today.date_naive() { return tr!("Resets at {}", local.format("%H:%M")); }
    // strftime pattern: the catalog gives each language its own date order
    local.format(tr!("Resets %m/%d at %H:%M")).to_string()
}

/// Short time: "16:10" if it's today, "11/12 16:10" (month/day in English) on another day.
fn clock(at: i64) -> String {
    let (Some(t), Some(today)) = (local(at), local(now())) else { return String::new() };
    let format = if t.date_naive() == today.date_naive() { "%H:%M" } else { tr!("%m/%d %H:%M") };
    t.format(format).to_string()
}

/// Text of a window that, at this pace, runs out before it renews. The end comes first: if
/// the line doesn't fit, what gets cut is the renewal.
fn forecast_text(w: &UsageWindow, f: Forecast, relative: bool) -> String {
    let resets_at = w.resets_at.unwrap_or(0);
    if relative {
        let now = now();
        tr!("Runs out in ~{} · resets in {}", remaining(f.ends_at - now), remaining(resets_at - now))
    } else {
        tr!("Runs out ~{} · resets {}", clock(f.ends_at), clock(resets_at))
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
        tr!("AI subscriptions: {}/{} sources available", active, shown)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_buttons_have_permission() {
        let m = Manifest::parse(include_str!("../../miniapps/ai_usage/manifest.toml")).unwrap();
        for cmd in COMMANDS {
            assert!(m.permissions.exec.iter().any(|p| p == cmd), "{cmd} missing from permissions.exec");
        }
    }

    #[test]
    fn short_time_left() {
        assert_eq!(remaining(30), "1 min");
        assert_eq!(remaining(35 * 60), "35 min");
        assert_eq!(remaining(2 * 3600), "2 h");
        assert_eq!(remaining(2 * 3600 + 15 * 60), "2 h 15 min");
        assert_eq!(remaining(3 * 86400), "3 d");
        assert_eq!(remaining(3 * 86400 + 4 * 3600), "3 d 4 h");
        assert_eq!(remaining(-10), "0 min");
    }

    #[test]
    fn age_of_a_reading() {
        assert_eq!(age_at(1000, 1030), "just now");
        assert_eq!(age_at(0, 35 * 60), "35 min ago");
        assert_eq!(age_at(0, 47 * 3600), "47 h ago");
        assert_eq!(age_at(0, 50 * 3600), "2 d ago");
        assert_eq!(age_at(100, 0), "just now");
    }

    #[test]
    fn levels_from_the_config() {
        let l = Levels { warn: 60.0, alarm: 80.0 };
        assert_eq!(l.of(59.0), Mood::Idle);
        assert_eq!(l.of(60.0), Mood::Busy);
        assert_eq!(l.of(80.0), Mood::Alarm);
    }
}
