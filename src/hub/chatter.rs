//! Director of the conversation between mascots.
//!
//! Takes the facts from the apps (`MiniApp::take_facts`), builds a short conversation (who
//! tells and who answers) from the lines in the `lines.toml` files and plays one line at a
//! time, in the dialog box of the speaker's app. The director knows no app: what each one
//! says comes from its own `lines.toml`, and whatever is missing comes from the generic
//! lines (`lines.toml` next to this file). Format of the lines: `src/core/chatter.rs`.
//!
//! Talks only with the window open on the panel and the "mascots chat" option on. The one
//! who answers is always a mascot of the open page; the one who tells may be on another
//! page (only alarms and recoveries), and then the line shows in the answerer's app, with
//! the page name. When everything has been calm for a while, some idle small talk comes up.
//!
//! To keep it from turning into chatter: a cooldown between fact-driven conversations, a
//! wait before repeating the same topic, and pokes with their own short cooldown. A fact
//! that cannot be shown is dropped (facts are changes; they don't wait around going stale).

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use slint::{ComponentHandle, TimerMode};

use super::Hub;
use crate::core::chatter::{Fact, FactKind, Lang, Lines, Say, Voice};
use crate::mascot::render;
use crate::ui::Chatter;

/// Generic lines: what an app did not declare in its own `lines.toml`.
const GENERIC: &str = include_str!("lines.toml");

/// Frame interval while a line is on screen.
const CHAT_FRAME: Duration = Duration::from_millis(100);
/// Letters per second when "typing" the line.
const TYPE_RATE: f32 = 28.0;
/// How long the line stays on screen after being typed: base + a bit per letter (up to the cap).
const HOLD_BASE: Duration = Duration::from_millis(1800);
const HOLD_PER_CHAR: Duration = Duration::from_millis(40);
const HOLD_MAX: Duration = Duration::from_millis(5500);
/// Pause between one line and the next.
const GAP: Duration = Duration::from_millis(350);
/// Minimum interval between poke conversations.
const POKE_COOLDOWN: Duration = Duration::from_secs(10);
/// How many spoken lines the director remembers so as not to repeat them (see `pick_text`).
const RECENT: usize = 32;
/// Time before commenting on the same topic again (app + subject + kind).
const TOPIC_COOLDOWN: Duration = Duration::from_secs(600);

/// How much the mascots chat (`chatter` config option).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pace {
    Off,
    Some,
    Lots,
}

impl Pace {
    pub const KEYS: [&str; 3] = ["off", "some", "lots"];

    pub fn from_key(key: &str) -> Pace {
        match key {
            "off" => Pace::Off,
            "lots" => Pace::Lots,
            _ => Pace::Some,
        }
    }

    /// Minimum interval between fact-driven conversations (alarms jump the queue).
    fn talk_cooldown(self) -> Duration {
        match self {
            Pace::Lots => Duration::from_secs(45),
            _ => Duration::from_secs(150),
        }
    }

    /// How long it must be calm before idle small talk (drawn between the two).
    fn idle_range(self) -> (u64, u64) {
        match self {
            Pace::Lots => (120, 240),
            _ => (360, 600),
        }
    }
}

/// A ready line.
#[derive(Clone)]
pub struct Line {
    pub voice: Voice,
    pub text: String,
    /// Who listens (turns to look at the speaker).
    pub listener: Option<Voice>,
    /// Which app the dialog box shows in: the speaker's, or the answerer's when the speaker
    /// is on another page.
    pub show_in: String,
}

/// What the dialog box shows now.
pub struct View {
    /// Changes with every new line (so portrait and name are swapped only when needed).
    pub seq: u64,
    pub line: Line,
    /// Part already "typed".
    pub typed: String,
    /// Finished typing.
    pub done: bool,
}

pub struct Director {
    generic: Lines,
    /// Lines of each app (id -> lines).
    lines: HashMap<String, Lines>,
    queue: VecDeque<Line>,
    current: Option<(Line, Instant)>,
    seq: u64,
    gap_until: Option<Instant>,
    last_talk: Option<Instant>,
    last_poke: Option<Instant>,
    topics: HashMap<String, Instant>,
    /// Since when everything has been calm (no conversation, with the panel in view).
    quiet_since: Option<Instant>,
    /// Calm needed before the next idle small talk (drawn at random).
    idle_wait: Duration,
    /// Last lines spoken (the template, before filling the blanks), from oldest to newest.
    recent: VecDeque<String>,
    rng: u64,
}

impl Default for Director {
    fn default() -> Self {
        let seed = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0x9e37_79b9, |d| d.as_nanos() as u64);
        Director::with_seed(seed)
    }
}

impl Director {
    pub fn with_seed(seed: u64) -> Self {
        Director {
            generic: Lines::parse(GENERIC).unwrap_or_default(),
            lines: HashMap::new(),
            queue: VecDeque::new(),
            current: None,
            seq: 0,
            gap_until: None,
            last_talk: None,
            last_poke: None,
            topics: HashMap::new(),
            quiet_since: None,
            idle_wait: Duration::from_secs(480),
            recent: VecDeque::new(),
            rng: seed | 1,
        }
    }

    /// An app's own lines (its `lines.toml`).
    pub fn add_lines(&mut self, app: &str, lines: Lines) {
        self.lines.insert(app.to_string(), lines);
    }

    /// Advances the conversation. `voices` are the mascots of the open page; `visible` says
    /// whether anything can be shown (window open on the panel).
    pub fn step(&mut self, now: Instant, facts: Vec<Fact>, voices: &[Voice], visible: bool, pace: Pace) {
        if !visible || pace == Pace::Off {
            self.clear();
            self.quiet_since = None;
            return;
        }
        // page changed in the middle of the conversation: the box is no longer on screen
        let on_page = |app: &str| voices.iter().any(|v| v.app == app);
        let gone = self.current.as_ref().is_some_and(|(l, _)| !on_page(&l.show_in))
            || self.queue.iter().any(|l| !on_page(&l.show_in));
        if gone {
            self.clear();
        }
        let quiet = *self.quiet_since.get_or_insert(now);
        if self.current.is_none() && self.queue.is_empty() {
            let started = self.start(now, facts, voices, pace)
                || (now.duration_since(quiet) >= self.idle_wait && self.idle(now, voices, pace));
            if started {
                self.quiet_since = Some(now);
                let (lo, hi) = pace.idle_range();
                self.idle_wait = Duration::from_secs(lo + self.next() % (hi - lo + 1));
            }
        }
        // end of the current line: pause, then the next one
        if let Some((line, started)) = &self.current
            && now >= *started + line_time(&line.text) {
                self.current = None;
                self.gap_until = Some(now + GAP);
            }
        if self.current.is_none() && self.gap_until.is_none_or(|t| now >= t)
            && let Some(line) = self.queue.pop_front() {
                self.seq += 1;
                self.current = Some((line, now));
                self.gap_until = None;
            }
        // a conversation on screen counts as "not calm"
        if self.current.is_some() || !self.queue.is_empty() {
            self.quiet_since = Some(now);
        }
    }

    /// A conversation is on (a line, or others queued): the screen needs frames.
    pub fn busy(&self) -> bool {
        self.current.is_some() || !self.queue.is_empty()
    }

    /// What to show now (nothing between lines).
    pub fn view(&self, now: Instant) -> Option<View> {
        let (line, started) = self.current.as_ref()?;
        let total = line.text.chars().count();
        let n = ((now - *started).as_secs_f32() * TYPE_RATE) as usize;
        Some(View {
            seq: self.seq,
            line: line.clone(),
            typed: line.text.chars().take(n.min(total)).collect(),
            done: n >= total,
        })
    }

    /// Skips to the next line (click on the box).
    pub fn skip(&mut self, now: Instant) {
        if self.current.take().is_some() {
            self.gap_until = Some(now + GAP);
        }
    }

    fn clear(&mut self) {
        self.queue.clear();
        self.current = None;
        self.gap_until = None;
    }

    /// Picks the most important fact that can become a conversation and queues its lines.
    fn start(&mut self, now: Instant, facts: Vec<Fact>, voices: &[Voice], pace: Pace) -> bool {
        let rank = |k: FactKind| match k {
            FactKind::Poked => 0,
            FactKind::Trouble => 1,
            FactKind::Recovered => 2,
            FactKind::Forecast => 3,
            FactKind::Strain => 4,
            FactKind::Idle => 5,
        };
        let mut facts = facts;
        facts.sort_by_key(|f| rank(f.kind));
        let cooled = |t: Option<Instant>, d: Duration| t.is_none_or(|t| now.duration_since(t) >= d);
        for fact in facts {
            // from another page, only alarm and recovery (the rest is not worth interrupting for)
            let remote = !voices.iter().any(|v| v.app == fact.voice.app);
            if remote && !matches!(fact.kind, FactKind::Trouble | FactKind::Recovered) {
                continue;
            }
            let topic = format!("{}:{}:{:?}", fact.voice.app, fact.subject, fact.kind);
            let ok = match fact.kind {
                FactKind::Poked => cooled(self.last_poke, POKE_COOLDOWN),
                // an alarm skips the cooldown between conversations (but does not repeat the topic)
                FactKind::Trouble => cooled(self.topics.get(&topic).copied(), TOPIC_COOLDOWN),
                _ => {
                    cooled(self.last_talk, pace.talk_cooldown())
                        && cooled(self.topics.get(&topic).copied(), TOPIC_COOLDOWN)
                }
            };
            if !ok {
                continue;
            }
            let lines = self.compose(fact.kind, &fact.voice, &fact.subject, fact.value, voices);
            if lines.is_empty() {
                continue;
            }
            if fact.kind == FactKind::Poked {
                self.last_poke = Some(now);
            } else {
                self.last_talk = Some(now);
                self.topics.insert(topic, now);
            }
            self.queue.extend(lines);
            return true;
        }
        false
    }

    /// Idle small talk: a mascot of the page starts a topic and another answers.
    fn idle(&mut self, now: Instant, voices: &[Voice], pace: Pace) -> bool {
        let cooled = self.last_talk.is_none_or(|t| now.duration_since(t) >= pace.talk_cooldown());
        let Some(a) = (if cooled { self.pick(voices).cloned() } else { None }) else { return false };
        let lines = self.compose(FactKind::Idle, &a, "", None, voices);
        if lines.is_empty() {
            return false;
        }
        self.last_talk = Some(now);
        self.queue.extend(lines);
        true
    }

    /// Who answers, and the filled-in lines. Empty if the conversation cannot be built.
    pub fn compose(&mut self, kind: FactKind, a: &Voice, subject: &str, value: Option<f32>, voices: &[Voice]) -> Vec<Line> {
        let subject = if subject.is_empty() { a.name.clone() } else { subject.to_string() };
        let remote = !voices.iter().any(|v| v.app == a.app);
        let b = self.responder(kind, a, &subject, voices);
        // from another page, the line only shows if someone here answers (it goes in their box)
        if remote && b.is_none() {
            return Vec::new();
        }
        let ctx = Ctx { value, has_b: b.is_some() };
        let a_say = self.own_say(kind, &a.app, ctx);
        let b_say = b.as_ref().map(|b| self.answer_say(kind, &a.app, &b.app, &subject, ctx));
        let lang = self.lang(&a_say, b_say.as_ref(), ctx);
        let fill = |t: &str| {
            t.replace("{a}", &a.name)
                .replace("{b}", b.as_ref().map_or("", |b| b.name.as_str()))
                .replace("{s}", &subject)
                .replace("{v}", &value.map_or(String::new(), |v| format!("{v:.0}")))
        };
        let mut out = Vec::new();
        let show_a = match (&b, remote) {
            (Some(b), true) => b.app.clone(),
            _ => a.app.clone(),
        };
        if let Some(t) = self.pick_text(&a_say, lang, ctx) {
            out.push(Line { voice: a.clone(), text: fill(&t), listener: b.clone(), show_in: show_a });
        }
        if let (Some(b), Some(say)) = (&b, &b_say)
            && let Some(t) = self.pick_text(say, lang, ctx) {
                out.push(Line { voice: b.clone(), text: fill(&t), listener: Some(a.clone()), show_in: b.app.clone() });
            }
        out
    }

    /// Who answers: a mascot of the page other than the teller. First the ones with their
    /// own reply for the case (Docker taking the blame for the CPU), then those of another
    /// app, last those of the same app.
    fn responder(&mut self, kind: FactKind, a: &Voice, subject: &str, voices: &[Voice]) -> Option<Voice> {
        let others: Vec<&Voice> = voices.iter().filter(|v| !v.same(a)).collect();
        let own_reply: Vec<&Voice> = others
            .iter()
            .copied()
            .filter(|v| v.app != a.app && self.lines.get(&v.app).and_then(|l| l.reply(kind)).is_some_and(|r| r.fits(subject)))
            .collect();
        let other_app: Vec<&Voice> = others.iter().copied().filter(|v| v.app != a.app).collect();
        let pool = if !own_reply.is_empty() { own_reply } else if !other_app.is_empty() { other_app } else { others };
        self.pick(&pool).map(|v| (*v).clone())
    }

    /// What the teller says: from its app, if it has a usable line; otherwise the generic one.
    fn own_say(&self, kind: FactKind, app: &str, ctx: Ctx) -> Say {
        self.lines
            .get(app)
            .and_then(|l| l.own(kind))
            .map(|e| e.say())
            .filter(|s| usable(s, Lang::Pt, ctx))
            .or_else(|| self.generic.own(kind).map(|e| e.say()))
            .unwrap_or_default()
    }

    /// What the answerer says: its app's own reply (if it fits the subject), the reply the
    /// teller's app suggests (`answer`), or the generic one.
    fn answer_say(&self, kind: FactKind, a_app: &str, b_app: &str, subject: &str, ctx: Ctx) -> Say {
        let own = self.lines.get(b_app).and_then(|l| l.reply(kind)).filter(|r| r.fits(subject)).cloned();
        let suggested = self.lines.get(a_app).and_then(|l| l.own(kind)).and_then(|e| e.answer.clone());
        [own, suggested, self.generic.reply(kind).cloned()]
            .into_iter()
            .flatten()
            .find(|s| usable(s, Lang::Pt, ctx))
            .unwrap_or_default()
    }

    /// Language of the whole conversation: mostly the interface language, sometimes the other
    /// one, German now and then (see `pick_lang`).
    fn lang(&mut self, a: &Say, b: Option<&Say>, ctx: Ctx) -> Lang {
        let roll = self.next() % 100;
        pick_lang(roll, crate::core::i18n::current(), a, b, ctx)
    }

    /// Draws a variant without repeating the last ones spoken: first among those not said
    /// recently; if all were, among the half said longest ago (with two variants they
    /// alternate; with only one, nothing can be done).
    fn pick_text(&mut self, say: &Say, lang: Lang, ctx: Ctx) -> Option<String> {
        let options: Vec<&String> = say.variants(lang).iter().filter(|t| ctx.allows(t)).collect();
        // position in memory: the higher, the more recent; `None` = not said recently
        let said = |t: &String| self.recent.iter().rposition(|r| r == t);
        let fresh: Vec<&String> = options.iter().copied().filter(|t| said(t).is_none()).collect();
        let pool = if fresh.is_empty() {
            let mut old = options.clone();
            old.sort_by_key(|t| said(t));
            old.truncate(options.len().div_ceil(2));
            old
        } else {
            fresh
        };
        let text = self.pick(&pool).map(|t| t.to_string())?;
        self.recent.retain(|r| *r != text);
        self.recent.push_back(text.clone());
        if self.recent.len() > RECENT {
            self.recent.pop_front();
        }
        Some(text)
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> Option<&'a T> {
        if items.is_empty() {
            return None;
        }
        let i = (self.next() % items.len() as u64) as usize;
        items.get(i)
    }

    /// xorshift64: enough for drawing lines.
    fn next(&mut self) -> u64 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng = x;
        x
    }
}

/// What the conversation has to fill the blanks.
#[derive(Clone, Copy)]
struct Ctx {
    value: Option<f32>,
    has_b: bool,
}

impl Ctx {
    /// The variant uses only blanks that can be filled.
    fn allows(self, t: &str) -> bool {
        (self.value.is_some() || !t.contains("{v}")) && (self.has_b || !t.contains("{b}"))
    }
}

/// Language for a conversation from a `roll` in `0..100`: the interface language most of the
/// time, the other one sometimes, German rarely. Only a language that suits both lines is
/// picked: if the rolled one does not, the interface language, then the other one, then
/// Portuguese (the lines always have it).
fn pick_lang(roll: u64, interface: crate::core::i18n::Language, a: &Say, b: Option<&Say>, ctx: Ctx) -> Lang {
    let (first, second) = match interface {
        crate::core::i18n::Language::PtBr => (Lang::Pt, Lang::En),
        crate::core::i18n::Language::En => (Lang::En, Lang::Pt),
    };
    let rolled = if roll < 55 { first } else if roll < 90 { second } else { Lang::De };
    [rolled, first, second]
        .into_iter()
        .find(|&l| usable(a, l, ctx) && b.is_none_or(|b| usable(b, l, ctx)))
        .unwrap_or(Lang::Pt)
}

/// Has some usable variant in this language.
fn usable(say: &Say, lang: Lang, ctx: Ctx) -> bool {
    say.variants(lang).iter().any(|t| ctx.allows(t))
}

/// How long a line takes: typing + staying on screen.
fn line_time(text: &str) -> Duration {
    let n = text.chars().count();
    let typing = Duration::from_secs_f32(n as f32 / TYPE_RATE);
    typing + (HOLD_BASE + HOLD_PER_CHAR * n as u32).min(HOLD_MAX)
}

fn voice_id(v: &Voice) -> String {
    v.id()
}

impl Hub {
    /// Loads the lines of each app (called once, at setup).
    pub(super) fn load_lines(&self) {
        let mut d = self.chatter.borrow_mut();
        for (app, src) in self.registry.lines() {
            match Lines::parse(src) {
                Ok(lines) => {
                    for p in lines.problems() {
                        log::warn!("lines of {app}: {p}");
                    }
                    d.add_lines(&app, lines);
                }
                Err(e) => log::warn!("invalid lines of {app}: {e}"),
            }
        }
    }

    /// Collects the apps' facts, advances the conversation and puts the current line on screen.
    pub(super) fn chat_tick(&self) {
        let facts = self.registry.take_facts();
        let pace = Pace::from_key(&self.host.config.borrow().chatter);
        let visible = self.shown.get() && self.ui.get_current() == "panel";
        let voices = if visible {
            let ids = self.host.config.borrow().page_apps(self.current_page());
            self.registry.voices(&ids)
        } else {
            Vec::new()
        };
        let now = Instant::now();
        let (view, busy) = {
            let mut d = self.chatter.borrow_mut();
            d.step(now, facts, &voices, visible, pace);
            (d.view(now), d.busy())
        };
        // the line is "typed" at 28 letters/s: with a conversation on, frames every 100 ms;
        // without one, the animation frame is enough to start the next talk
        if !busy {
            self.chat_timer.stop();
        } else if !self.chat_timer.running() {
            self.chat_timer.start(TimerMode::Repeated, CHAT_FRAME, || super::with_hub(Hub::chat_tick));
        }
        let key = view.as_ref().map(|v| (v.seq, v.typed.len(), v.done));
        if self.chat_shown.replace(key) == key {
            return;
        }
        let g = self.ui.global::<Chatter>();
        let Some(v) = view else {
            g.set_app("".into());
            g.set_speaker("".into());
            g.set_listener("".into());
            return;
        };
        if g.get_seq() != v.seq as i32 {
            let voice = &v.line.voice;
            // the speaker is on another page: the name carries the page along
            let remote = voice.app != v.line.show_in;
            let name = if remote {
                let cfg = self.host.config.borrow();
                let page = cfg.page_of(&voice.app).and_then(|p| cfg.pages.get(p)).map(|p| p.name.clone());
                match page {
                    Some(page) => format!("{} · {}", voice.name, page),
                    None => voice.name.clone(),
                }
            } else {
                voice.name.clone()
            };
            g.set_seq(v.seq as i32);
            g.set_name(name.into());
            g.set_portrait(render::image(voice.sprite, voice.theme, voice.mood, 0));
            g.set_theme(render::colors(voice.theme));
            g.set_speaker(if remote { String::new() } else { voice_id(voice) }.into());
            g.set_listener(v.line.listener.as_ref().map_or(String::new(), voice_id).into());
        }
        g.set_text(v.typed.into());
        g.set_done(v.done);
        g.set_app(v.line.show_in.into());
    }

    /// Click on the dialog box: skips the line.
    pub(super) fn chat_skip(&self) {
        self.chatter.borrow_mut().skip(Instant::now());
        self.chat_tick();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mascot::{Mood, sprites, themes};

    const VITALS: &str = "cyber.vitals";
    const DOCKER: &str = "cyber.docker";
    const AI: &str = "cyber.ai_usage";

    fn voice(app: &str, name: &str) -> Voice {
        Voice { app: app.into(), name: name.into(), sprite: sprites::get("neko"), theme: themes::get("neon"), mood: Mood::Idle }
    }

    fn fact(kind: FactKind, v: Voice, subject: &str, value: Option<f32>) -> Fact {
        Fact { kind, voice: v, subject: subject.into(), value }
    }

    /// Director with the apps' real lines.
    fn director(seed: u64) -> Director {
        let mut d = Director::with_seed(seed);
        d.add_lines(DOCKER, Lines::parse(include_str!("../../miniapps/docker/lines.toml")).unwrap());
        d.add_lines(VITALS, Lines::parse(include_str!("../../miniapps/vitals/lines.toml")).unwrap());
        d.add_lines(AI, Lines::parse(include_str!("../../miniapps/ai_usage/lines.toml")).unwrap());
        d
    }

    /// Runs the conversation for `secs` and returns who spoke, in order.
    fn run(d: &mut Director, t: &mut Instant, voices: &[Voice], secs: u64) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for _ in 0..secs * 10 {
            *t += Duration::from_millis(100);
            d.step(*t, Vec::new(), voices, true, Pace::Some);
            if let Some(v) = d.view(*t)
                && names.last() != Some(&v.line.voice.name) {
                    names.push(v.line.voice.name.clone());
                }
        }
        names
    }

    #[test]
    fn generic_and_app_lines_are_valid() {
        for (name, src) in [
            ("generic", GENERIC),
            ("docker", include_str!("../../miniapps/docker/lines.toml")),
            ("vitals", include_str!("../../miniapps/vitals/lines.toml")),
            ("ai_usage", include_str!("../../miniapps/ai_usage/lines.toml")),
        ] {
            let lines = Lines::parse(src).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(lines.problems(), Vec::<String>::new(), "{name}");
        }
        // the generic lines cover every case, on both sides
        let g = Lines::parse(GENERIC).unwrap();
        for k in FactKind::ALL {
            assert!(g.own(k).is_some() && g.reply(k).is_some(), "generic lines lack {}", k.key());
        }
    }

    #[test]
    fn poke_becomes_a_conversation_between_apps() {
        let cpu = voice(VITALS, "CPU");
        let docker = voice(DOCKER, "Docker");
        let voices = [cpu.clone(), docker.clone()];
        let mut d = director(7);
        let mut t = Instant::now();
        d.step(t, vec![fact(FactKind::Poked, cpu.clone(), "", None)], &voices, true, Pace::Some);
        let first = d.view(t).expect("the first line starts at once");
        assert_eq!(first.line.voice.name, "CPU");
        assert_eq!(first.line.listener.as_ref().map(|l| l.name.as_str()), Some("Docker"));
        let mut names = vec!["CPU".to_string()];
        names.extend(run(&mut d, &mut t, &voices, 20).into_iter().filter(|n| n != "CPU"));
        assert_eq!(names, ["CPU", "Docker"]);
        assert!(d.view(t).is_none(), "the conversation ends");
    }

    #[test]
    fn english_interface_never_falls_back_to_portuguese_when_english_fits() {
        use crate::core::i18n::Language;
        let ctx = Ctx { value: None, has_b: true };
        let a = Say { pt: vec!["oi".into()], en: vec!["hi".into()], ..Default::default() };
        let b = Say { pt: vec!["opa".into()], en: vec!["hey".into()], ..Default::default() };
        for roll in 0..100 {
            let lang = pick_lang(roll, Language::En, &a, Some(&b), ctx);
            // the interface language, or Portuguese only when it was rolled as the "other" one
            let expected = if (55..90).contains(&roll) { Lang::Pt } else { Lang::En };
            assert_eq!(lang, expected, "roll {roll}");
        }
        // a Portuguese interface may roll English, but never German here (no `de` lines)
        assert!((0..100).all(|r| pick_lang(r, Language::PtBr, &a, Some(&b), ctx) != Lang::De));
        // English only usable without the `{v}` blank: still not Portuguese for an English interface
        let v = Say { pt: vec!["{v}%".into()], en: vec!["{v}%".into(), "high".into()], ..Default::default() };
        assert_eq!(pick_lang(10, Language::En, &v, None, ctx), Lang::En);
        // nothing but Portuguese fits: Portuguese
        let pt_only = Say { pt: vec!["só".into()], ..Default::default() };
        assert_eq!(pick_lang(10, Language::En, &pt_only, None, ctx), Lang::Pt);
    }

    #[test]
    fn does_not_repeat_the_same_line_in_a_row() {
        let ctx = Ctx { value: None, has_b: true };
        let say = |n: usize| Say { pt: (0..n).map(|i| format!("line {i}")).collect(), ..Default::default() };
        for seed in 1..10 {
            // four variants: all four come out before any repetition, and after that
            // never the same twice in a row (not even with only one in between)
            let mut d = Director::with_seed(seed);
            let four = say(4);
            let said: Vec<String> = (0..40).map(|_| d.pick_text(&four, Lang::Pt, ctx).unwrap()).collect();
            let mut first: Vec<&String> = said[..4].iter().collect();
            first.sort();
            first.dedup();
            assert_eq!(first.len(), 4, "{said:?}");
            assert!(said.windows(3).all(|w| w[0] != w[1] && w[0] != w[2]), "{said:?}");
            // two variants alternate; a single one keeps coming out
            let two = say(2);
            let said: Vec<String> = (0..10).map(|_| d.pick_text(&two, Lang::Pt, ctx).unwrap()).collect();
            assert!(said.windows(2).all(|w| w[0] != w[1]), "{said:?}");
            assert!(d.pick_text(&say(1), Lang::Pt, ctx).is_some());
            assert!(d.pick_text(&say(1), Lang::Pt, ctx).is_some());
        }
    }

    #[test]
    fn docker_takes_the_blame_only_for_the_cpu() {
        let cpu = voice(VITALS, "CPU");
        let disk = voice(VITALS, "DISK");
        let voices = [cpu.clone(), disk.clone(), voice(DOCKER, "Docker"), voice(AI, "Claude")];
        for seed in 1..20 {
            let mut d = director(seed);
            let lines = d.compose(FactKind::Strain, &cpu, "CPU", Some(91.0), &voices);
            assert_eq!(lines.len(), 2);
            assert_eq!(lines[1].voice.app, DOCKER, "the one with its own reply answers");
            // a full disk is not Docker's fault: anyone from another app answers
            let lines = d.compose(FactKind::Strain, &disk, "DISK", Some(91.0), &voices);
            assert_ne!(lines[1].voice.app, VITALS);
            assert!(lines.iter().all(|l| !l.text.contains('{')));
        }
    }

    #[test]
    fn app_lines_win_and_without_a_number_fall_back_to_one_without_it() {
        let claude = voice(AI, "Claude");
        let cpu = voice(VITALS, "CPU");
        for seed in 1..30 {
            let mut d = director(seed);
            // alone on the page: only the first line, with no number and no `{b}`
            let lines = d.compose(FactKind::Trouble, &claude, "Claude", None, std::slice::from_ref(&claude));
            assert_eq!(lines.len(), 1);
            assert!(!lines[0].text.contains('%') && !lines[0].text.contains('{'), "{}", lines[0].text);
            // the answer to the AI alarm is the one the AI app suggests (`trouble.answer`)
            let lines = d.compose(FactKind::Trouble, &claude, "Claude", Some(95.0), &[claude.clone(), cpu.clone()]);
            let ai = Lines::parse(include_str!("../../miniapps/ai_usage/lines.toml")).unwrap();
            let answer = ai.trouble.unwrap().answer.unwrap();
            let all: Vec<String> = Lang::ALL.iter().flat_map(|&l| answer.variants(l).iter().map(|t| t.replace("{a}", "Claude"))).collect();
            assert!(all.contains(&lines[1].text), "{}", lines[1].text);
        }
    }

    #[test]
    fn alarm_from_another_page_shows_in_the_answerers_app() {
        let web = voice(DOCKER, "Docker"); // Docker is on another page
        let cpu = voice(VITALS, "CPU");
        let voices = [cpu.clone()];
        let mut d = director(5);
        let t = Instant::now();
        d.step(t, vec![fact(FactKind::Trouble, web.clone(), "postgres", None)], &voices, true, Pace::Some);
        let v = d.view(t).expect("an alarm from another page gets in");
        assert_eq!(v.line.voice.app, DOCKER);
        assert_eq!(v.line.show_in, VITALS, "the box shows where it can be seen");
        // a poke or strain from another page does not interrupt
        let mut d = director(5);
        d.step(t, vec![fact(FactKind::Strain, web, "postgres", Some(80.0))], &voices, true, Pace::Some);
        assert!(d.view(t).is_none());
    }

    #[test]
    fn idle_talk_after_a_calm_while() {
        let voices = [voice(VITALS, "CPU"), voice(DOCKER, "Docker")];
        let mut d = director(11);
        let mut t = Instant::now();
        // calm start: nothing before the minimum time
        assert!(run(&mut d, &mut t, &voices, 100).is_empty());
        // by the maximum of the draw a talk comes out, with both mascots
        let names = run(&mut d, &mut t, &voices, 600);
        assert_eq!(names.len(), 2, "{names:?}");
        // off: never
        let mut d = director(11);
        for _ in 0..7000 {
            t += Duration::from_millis(100);
            d.step(t, Vec::new(), &voices, true, Pace::Off);
            assert!(d.view(t).is_none());
        }
    }

    #[test]
    fn does_not_repeat_a_topic_nor_talk_while_hidden() {
        let cpu = voice(VITALS, "CPU");
        let voices = [cpu.clone(), voice(DOCKER, "Docker")];
        let trouble = || vec![fact(FactKind::Trouble, cpu.clone(), "CPU", Some(99.0))];
        let mut d = director(3);
        let mut t = Instant::now();
        // window closed: dropped
        d.step(t, trouble(), &voices, false, Pace::Some);
        assert!(d.view(t).is_none());
        d.step(t, trouble(), &voices, true, Pace::Some);
        assert!(d.view(t).is_some());
        // let the whole conversation finish
        run(&mut d, &mut t, &voices, 30);
        assert!(d.view(t).is_none());
        // the same alarm right after does not start another conversation
        d.step(t, trouble(), &voices, true, Pace::Some);
        assert!(d.view(t).is_none());
        // nor does a poke from an app off the page
        d.step(t, vec![fact(FactKind::Poked, voice("cyber.cd", "CD"), "", None)], &voices, true, Pace::Some);
        assert!(d.view(t).is_none());
        // but a poke on the page does
        d.step(t, vec![fact(FactKind::Poked, cpu.clone(), "", None)], &voices, true, Pace::Some);
        assert!(d.view(t).is_some());
    }
}
