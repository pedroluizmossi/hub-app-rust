//! AI Agents mini-app: monitors sessions from Claude Code, Codex, OpenCode and Oh My Pi.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use slint::{ComponentHandle, Model, ModelRc, VecModel};

use super::ai_usage::ai_mascots;
use crate::core::bus::Event;
use crate::core::i18n::{self, Language};
use crate::core::manifest::Manifest;
use crate::core::miniapp::{Host, MiniApp};
use super::kit::{HeaderMascot, Speech, plural};
use crate::mascot::anim::Reaction;
use crate::mascot::stroll::{Pose, Script, Stage};
use crate::mascot::themes::Theme;
use crate::mascot::{Mood, anim, render, sprites, walkers};
use crate::core::bus::Topic;
use crate::providers::agents::{self, Agent, AgentsSnapshot, Artifact, Lease, State, Step, StepKind, Tool, WORKFLOW, clip};
use crate::tr;
use crate::ui::{
    AgentDetail, AgentDetailWindow, AgentPhase, AgentRow, AgentRun, AgentRunAgent, AgentStep, AgentSub, AgentWalker, AgentsApp,
    Anim, HubWindow,
};

const SPEECH_TTL: Duration = Duration::from_secs(30);
/// Subagents drawn walking in a session's strip (the counter shows all of them).
const MAX_WALKERS: usize = 6;
/// Subagents listed on a session's card.
const MAX_SUB_LINES: usize = 5;
/// Agents of a phase listed in an expanded workflow run.
const MAX_RUN_AGENTS: usize = 12;

pub struct AgentsMini {
    host: Rc<Host>,
    manifest: Manifest,
    snap: AgentsSnapshot,
    /// State of each session at the previous reading: the change is what becomes speech
    /// and a notification.
    prev: HashMap<(Tool, String), State>,
    /// First reading after enabling: nothing counts as a change.
    baseline: bool,
    speech: Speech,
    mood: Mood,
    rows: Rc<VecModel<AgentRow>>,
    /// Session with its options open (the clicked row). By session, not by index: the
    /// list reorders itself.
    selected: Option<(Tool, String)>,
    checking_resume: HashSet<String>,
    /// What each row on screen shows now: only the row that changed is replaced (replacing
    /// the whole list recreates every row and the screen flickers).
    cards: Vec<Card>,
    /// Mascot of each tool in each state (still: drawn once per theme).
    faces: HashMap<(Tool, State), slint::Image>,
    /// Subagents walking with the imported mascots.
    heroes: Heroes,
    face: HeaderMascot,
    /// Detail window (only one: opening another session swaps what it shows). Created on
    /// first use and only hidden when closed.
    detail: Option<Detail>,
    /// How the workflow runs are shown, by run id.
    runs: RunView,
}

/// Which workflow runs are expanded, and the phase picked in each (none picked: the one
/// running, or the last one that started).
#[derive(Debug, Default)]
struct RunView {
    open: HashSet<String>,
    phase: HashMap<String, usize>,
}

/// The detail window and the session it shows.
struct Detail {
    win: AgentDetailWindow,
    /// Session shown.
    key: (Tool, String),
    /// Open (closing only hides the window and stops reading the conversation).
    open: bool,
    /// Steps on screen, newest to oldest.
    steps: Rc<VecModel<AgentStep>>,
    /// The steps read last (the list is only replaced when they change).
    shown: Vec<Step>,
    /// The timeline of this session has arrived.
    loaded: bool,
}

impl AgentsMini {
    pub fn create(host: Rc<Host>, ui: &HubWindow) -> Rc<RefCell<Self>> {
        let manifest =
            Manifest::parse(include_str!("../../miniapps/agents/manifest.toml")).expect("invalid Agents manifest");
        let rows = Rc::new(VecModel::default());
        ui.global::<AgentsApp>().set_rows(ModelRc::from(rows.clone()));
        let app = Rc::new(RefCell::new(AgentsMini {
            host,
            manifest,
            snap: AgentsSnapshot::default(),
            prev: HashMap::new(),
            baseline: false,
            speech: Speech::new(SPEECH_TTL),
            mood: Mood::Sleep,
            rows,
            selected: None,
            checking_resume: HashSet::new(),
            cards: Vec::new(),
            faces: HashMap::new(),
            heroes: Heroes::default(),
            face: HeaderMascot::new(13),
            detail: None,
            runs: RunView::default(),
        }));
        let a = Rc::downgrade(&app);
        let w = ui.as_weak();
        ui.global::<AgentsApp>().on_mascot_poked(move || {
            if let (Some(a), Some(ui)) = (a.upgrade(), w.upgrade()) {
                let mut a = a.borrow_mut();
                let (mood, reduce) = (a.mood, a.host.config.borrow().reduce_motion);
                if a.face.poke(mood, reduce) {
                    a.push_mascot(&ui);
                }
            }
        });
        let a = Rc::downgrade(&app);
        let w = ui.as_weak();
        ui.global::<AgentsApp>().on_toggle(move |key| {
            if let (Some(a), Some(ui)) = (a.upgrade(), w.upgrade()) {
                let mut a = a.borrow_mut();
                let key = a.find(&key).map(|s| (s.tool, s.id.clone()));
                a.selected = if a.selected == key { None } else { key };
                a.refresh(&ui);
            }
        });
        let a = Rc::downgrade(&app);
        let w = ui.as_weak();
        ui.global::<AgentsApp>().on_toggle_run(move |key| {
            if let (Some(a), Some(ui)) = (a.upgrade(), w.upgrade()) {
                let mut a = a.borrow_mut();
                let key = key.to_string();
                if !a.runs.open.remove(&key) {
                    a.runs.open.insert(key);
                }
                a.refresh(&ui);
            }
        });
        let a = Rc::downgrade(&app);
        let w = ui.as_weak();
        ui.global::<AgentsApp>().on_run_phase(move |key, index| {
            if let (Some(a), Some(ui)) = (a.upgrade(), w.upgrade()) {
                let mut a = a.borrow_mut();
                a.runs.phase.insert(key.to_string(), index.max(0) as usize);
                a.refresh(&ui);
            }
        });
        let a = Rc::downgrade(&app);
        let w = ui.as_weak();
        ui.global::<AgentsApp>().on_open_terminal(move |key| {
            if let (Some(a), Some(ui)) = (a.upgrade(), w.upgrade()) {
                a.borrow_mut().open_terminal(&key, &ui);
            }
        });
        let a = Rc::downgrade(&app);
        let w = ui.as_weak();
        ui.global::<AgentsApp>().on_open_dir(move |key| {
            if let (Some(a), Some(ui)) = (a.upgrade(), w.upgrade()) {
                a.borrow_mut().open_dir(&key, &ui);
            }
        });
        let a = Rc::downgrade(&app);
        let w = ui.as_weak();
        ui.global::<AgentsApp>().on_resume(move |key| {
            if let (Some(a), Some(ui)) = (a.upgrade(), w.upgrade()) {
                a.borrow_mut().resume(&key, &ui);
            }
        });
        let a = Rc::downgrade(&app);
        let w = ui.as_weak();
        ui.global::<AgentsApp>().on_resume_checked(move |key, free| {
            if let (Some(a), Some(ui)) = (a.upgrade(), w.upgrade()) {
                let mut a = a.borrow_mut();
                if !a.checking_resume.remove(key.as_str()) {
                    return;
                }
                if free {
                    a.launch_resume(&key, &ui);
                } else {
                    a.host.bus.poke(Topic::Agents);
                    a.speech.say(tr!("The OMP session is open or its status could not be checked.").into());
                    a.refresh(&ui);
                }
            }
        });
        let a = Rc::downgrade(&app);
        let w = ui.as_weak();
        ui.global::<AgentsApp>().on_details(move |key| {
            if let (Some(a), Some(ui)) = (a.upgrade(), w.upgrade()) {
                AgentsMini::open_detail(&a, &key, &ui);
            }
        });
        app
    }

    /// Session with key `key` (see `session_key`), among everything the last reading saw:
    /// the detail window may show one that is no longer on a card (a subagent, or one past
    /// the `recent` option).
    fn find(&self, key: &str) -> Option<&Agent> {
        self.snap.agents.iter().find(|a| session_key(a) == key)
    }

    /// Opens the session's folder in the file manager.
    fn open_dir(&mut self, key: &str, ui: &HubWindow) {
        let Some(a) = self.find(key) else { return };
        if !crate::core::exec::open_dir(std::path::Path::new(&a.cwd)) {
            self.speech.say(tr!("Couldn't find the folder {}.", a.cwd));
        }
        self.selected = None;
        self.refresh(ui);
    }

    /// Opens a terminal in the session's folder.
    fn open_terminal(&mut self, key: &str, ui: &HubWindow) {
        let Some(a) = self.find(key) else { return };
        if let Err(e) = crate::core::exec::open_terminal_at(std::path::Path::new(&a.cwd)) {
            self.speech.say(tr!("Couldn't open the terminal: {}", e));
        }
        self.selected = None;
        self.refresh(ui);
    }

    /// Resumes a stopped session in a new terminal, in its folder.
    fn resume(&mut self, key: &str, ui: &HubWindow) {
        let Some(a) = self.find(key) else { return };
        if a.tool != Tool::Omp {
            self.launch_resume(key, ui);
            return;
        }
        if resume_argv(a).is_none() {
            return;
        }
        let id = a.id.clone();
        if !self.checking_resume.insert(key.to_owned()) {
            return;
        }
        let w = ui.as_weak();
        let key = key.to_owned();
        // Recheck off the UI thread just before launching; the last poll may be stale.
        std::thread::spawn(move || {
            let free = agents::session_leases(std::iter::once(id.as_str())).is_some_and(|leases| leases.get(&id) == Lease::Free);
            let _ = w.upgrade_in_event_loop(move |ui| ui.global::<AgentsApp>().invoke_resume_checked(key.into(), free));
        });
    }

    fn launch_resume(&mut self, key: &str, ui: &HubWindow) {
        let Some(a) = self.find(key).cloned() else { return };
        let Some(argv) = resume_argv(&a) else { return };
        let dir = std::path::Path::new(&a.cwd);
        let opened = self.host.exec_for(&self.manifest).open_terminal_in(a.tool.name(), Some(dir), &argv);
        self.speech.say(match opened {
            Ok(()) => tr!("Resuming in the terminal: {}", a.title),
            Err(e) => tr!("Couldn't open {}: {}", a.tool.name(), e),
        });
        self.selected = None;
        self.refresh(ui);
    }

    /// Compares with the previous reading: who finished and who started waiting for the user.
    fn ingest(&mut self, mut snap: AgentsSnapshot) {
        carry_over(&self.snap, &mut snap);
        let cur: HashMap<(Tool, String), State> = snap.agents.iter().map(|a| ((a.tool, a.id.clone()), a.state)).collect();
        if self.baseline {
            let (notify_waiting, notify_done) = {
                let cfg = self.host.config.borrow();
                (cfg.bool(&self.manifest, "notify_waiting"), cfg.bool(&self.manifest, "notify_done"))
            };
            // what each change says, to sum up in one speech line afterwards
            let mut said: Vec<(Change, String)> = Vec::new();
            // subagents coming and going is noise: only the sessions the user opened
            for a in snap.agents.iter().filter(|a| a.parent.is_none()) {
                let before = self.prev.get(&(a.tool, a.id.clone())).copied();
                let Some(change) = change(before, a) else { continue };
                let (title, body, notify, urgent) = match change {
                    Change::Waiting => {
                        self.face.react(Reaction::Surprised);
                        (tr!("Agent waiting for you"), tr!("{} is waiting for you: {}", a.tool.name(), a.title), notify_waiting, true)
                    }
                    Change::Done => {
                        self.face.react(Reaction::Celebrate);
                        (tr!("Agent finished"), tr!("{} finished: {}", a.tool.name(), a.title), notify_done, false)
                    }
                    Change::Stopped => {
                        (tr!("Agent stopped"), tr!("{} stopped ({}): {}", a.tool.name(), a.detail, a.title), notify_done, false)
                    }
                };
                if notify {
                    self.host.notifier.borrow_mut().send(&notify_key(&change, a), title, &body, urgent);
                }
                said.push((change, body));
            }
            if let Some(line) = speech_of(said) {
                self.speech.say(line);
            }
        }
        self.prev = cur;
        self.baseline = true;
        self.snap = snap;
    }

    /// The sessions that show up: active ones always; stopped ones, only within the
    /// option's time limit.
    fn visible(&self, now: i64) -> Vec<&Agent> {
        let recent = self.host.config.borrow().int(&self.manifest, "recent").max(1) * 60_000;
        visible(&self.snap.agents, now, recent)
    }

    fn face(&mut self, tool: Tool, state: State) -> slint::Image {
        if let Some(img) = self.faces.get(&(tool, state)) {
            return img.clone();
        }
        let mood = match state {
            State::Working => Mood::Happy,
            State::Waiting => Mood::Busy,
            State::Idle => Mood::Sleep,
        };
        let theme = self.host.theme_for(&self.manifest);
        let img = ai_mascots::for_tool(tool).map(|s| render::image(s, theme, mood, 0)).unwrap_or_default();
        self.faces.insert((tool, state), img.clone());
        img
    }

    fn refresh(&mut self, ui: &HubWindow) {
        let now = now_ms();
        let home = dirs::home_dir().map(|h| h.to_string_lossy().into_owned()).unwrap_or_default();
        let all: Vec<Agent> = self.visible(now).into_iter().cloned().collect();
        // one row per session; subagents stay on the card of whoever started them
        let list = group(&all);
        let counts = Counts::of(&list);
        self.mood = counts.mood();
        // the session with open options left the list: close them
        if self.selected.as_ref().is_some_and(|(t, id)| !list.iter().any(|(a, _)| a.tool == *t && a.id == *id)) {
            self.selected = None;
        }
        let selected = self.selected.clone();
        // runs that left the list forget how they were shown
        let present = |id: &String| all.iter().any(|a| a.id == *id && a.details.kind == WORKFLOW);
        self.runs.open.retain(present);
        self.runs.phase.retain(|id, _| present(id));
        let cards: Vec<Card> = list
            .iter()
            .map(|(a, subs)| {
                let open = selected.as_ref().is_some_and(|(t, id)| a.tool == *t && a.id == *id);
                card(a, subs, &short_dir(&a.cwd, &home), now, open, &self.runs)
            })
            .collect();
        if cards.len() != self.cards.len() {
            let rows: Vec<AgentRow> = cards.iter().map(|c| self.row(c)).collect();
            self.rows.set_vec(rows);
        } else {
            for (i, c) in cards.iter().enumerate() {
                if self.cards[i] != *c {
                    let row = self.row(c);
                    self.rows.set_row_data(i, row);
                }
            }
        }
        // walking subagents: who arrived, who left (skip) and who is calling tools
        let on = self.host.config.borrow().bool(&self.manifest, "heroes");
        let walking: Vec<(String, Vec<(String, bool)>)> = list
            .iter()
            .map(|(a, subs)| {
                let working = walkers(subs).take(MAX_WALKERS);
                (session_key(a), working.map(|s| (s.id.clone(), self.heroes.calling_tools(s, now))).collect())
            })
            .collect();
        let theme = self.host.theme_for(&self.manifest);
        self.heroes.sync(on, &walking, theme);
        self.cards = cards;

        let Counts { working, waiting, idle } = counts;
        let g = ui.global::<AgentsApp>();
        g.set_working(working as i32);
        g.set_waiting(waiting as i32);
        g.set_idle(idle as i32);
        g.set_missing(self.baseline && self.snap.found.is_empty());
        let errors: Vec<String> = self.snap.errors.iter().map(|(t, e)| format!("{}: {e}", t.name())).collect();
        g.set_error_text(errors.join(" · ").into());
        g.set_mood(self.mood.as_str().into());
        let speech = match self.speech.current() {
            Some(t) => t.to_string(),
            _ if waiting == 1 => tr!("1 agent waiting for you!").into(),
            _ if waiting > 0 => tr!("{} agents waiting for you!", waiting),
            _ if working == 1 => tr!("1 agent working.").into(),
            _ if working > 0 => tr!("{} agents working.", working),
            _ if idle > 0 => tr!("All quiet around here.").into(),
            _ => tr!("No agents around… zzz").into(),
        };
        g.set_speech(speech.into());
    }

    /// The card in the screen's format (with the mascot images).
    fn row(&mut self, c: &Card) -> AgentRow {
        let subs: Vec<AgentSub> = c
            .subs
            .iter()
            .map(|s| AgentSub {
                key: s.key.as_str().into(),
                depth: s.depth,
                kind: s.kind.as_str().into(),
                title: s.title.as_str().into(),
                state: state_key(s.state).into(),
                state_text: s.state_text.as_str().into(),
            })
            .collect();
        AgentRow {
            key: c.key.as_str().into(),
            title: c.title.as_str().into(),
            info: c.info.as_str().into(),
            state: state_key(c.state).into(),
            state_text: c.state_text.as_str().into(),
            sub: c.sub,
            mascot: self.face(c.tool, c.state),
            can_resume: c.can_resume,
            open: c.open,
            walkers: c.walkers as i32,
            walker: self.face(c.tool, State::Working),
            heroes: self.heroes.model(&c.key),
            model: c.model.as_str().into(),
            effort: c.effort.as_str().into(),
            branch: c.branch.as_str().into(),
            has_tokens: c.tokens.is_some(),
            tok_in: c.tokens.as_ref().map_or("", |t| t[0].as_str()).into(),
            tok_out: c.tokens.as_ref().map_or("", |t| t[1].as_str()).into(),
            tok_cache: c.tokens.as_ref().map_or("", |t| t[2].as_str()).into(),
            turn: c.turn.as_str().into(),
            turn_note: c.turn_note.as_str().into(),
            prompt: c.prompt.as_str().into(),
            subs: ModelRc::from(Rc::new(VecModel::from(subs))),
            more_subs: c.more_subs as i32,
            run_lines: c.runs.iter().map(|r| r.lines).sum::<usize>() as i32,
            runs: ModelRc::from(Rc::new(VecModel::from(c.runs.iter().map(run_row).collect::<Vec<_>>()))),
        }
    }

    /// Redraws the mascot; `true` if it changed.
    fn refresh_mascot(&mut self) -> bool {
        let sprite = sprites::get(&self.mascot_key());
        self.face.refresh(sprite, self.host.theme_for(&self.manifest), self.mood)
    }

    fn push_mascot(&mut self, ui: &HubWindow) {
        self.refresh_mascot();
        let g = ui.global::<AgentsApp>();
        g.set_mascot(self.face.image());
        g.set_reaction(self.face.reaction());
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Change {
    /// Started waiting for the user.
    Waiting,
    /// Was working and finished the turn.
    Done,
    /// Was working and stopped without finishing (failed, was interrupted).
    Stopped,
}

/// What changed in a session since the previous reading (`before` = the state it had).
fn change(before: Option<State>, a: &Agent) -> Option<Change> {
    match (before, a.state) {
        (Some(State::Waiting), State::Waiting) => None,
        // a session that shows up already waiting counts too: it's time to warn
        (_, State::Waiting) => Some(Change::Waiting),
        (Some(State::Working), State::Idle) if a.detail.is_empty() => Some(Change::Done),
        (Some(State::Working), State::Idle) => Some(Change::Stopped),
        _ => None,
    }
}

/// Notification key of a change: one per kind, so a "finished" right after a "waiting"
/// isn't dropped by the notifier's once-a-minute limit for the same key.
fn notify_key(change: &Change, a: &Agent) -> String {
    let kind = match change {
        Change::Waiting => "waiting",
        Change::Done => "done",
        Change::Stopped => "stopped",
    };
    format!("agents:{kind}:{}:{}", a.tool.name(), a.id)
}

/// One speech line for the changes of a reading: a single change keeps its own text;
/// several are summed up, with whoever waits for the user first.
fn speech_of(said: Vec<(Change, String)>) -> Option<String> {
    if said.len() <= 1 {
        return said.into_iter().next().map(|(_, body)| body);
    }
    let (waiting, ended): (Vec<_>, Vec<_>) = said.into_iter().partition(|(c, _)| *c == Change::Waiting);
    let mut parts = Vec::new();
    match waiting.len() {
        0 => {}
        1 => parts.push(waiting[0].1.clone()),
        n => parts.push(tr!("{} agents waiting for you!", n)),
    }
    match ended.len() {
        0 => {}
        1 => parts.push(ended[0].1.clone()),
        n => parts.push(tr!("{} agents finished their turn", n)),
    }
    Some(parts.join(" · "))
}

/// A tool that couldn't be read this time keeps the sessions of the previous reading: they
/// don't vanish from the list for a moment, nor come back as new (and warn again).
fn carry_over(prev: &AgentsSnapshot, snap: &mut AgentsSnapshot) {
    for (tool, _) in &snap.errors {
        if snap.agents.iter().any(|a| a.tool == *tool) {
            continue;
        }
        snap.agents.extend(prev.agents.iter().filter(|a| a.tool == *tool).cloned());
    }
}

/// How many rows (sessions with their subagents) are in each state. A row counts as
/// working when its subagents work, as in the list order (`group`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Counts {
    working: usize,
    waiting: usize,
    idle: usize,
}

impl Counts {
    fn of(list: &[(&Agent, Vec<&Agent>)]) -> Counts {
        let mut c = Counts::default();
        for (a, subs) in list {
            match row_state(a, subs) {
                State::Waiting => c.waiting += 1,
                State::Working => c.working += 1,
                State::Idle => c.idle += 1,
            }
        }
        c
    }

    fn mood(self) -> Mood {
        if self.waiting > 0 {
            Mood::Busy
        } else if self.working > 0 {
            Mood::Happy
        } else if self.idle > 0 {
            Mood::Idle
        } else {
            Mood::Sleep
        }
    }
}

/// What goes on screen: who is not stopped, or stopped less than `recent` ms ago, and
/// whoever started one of those (a session quiet for a while whose workflow still runs, or
/// just ended, keeps its card; otherwise the run would show up loose).
fn visible(all: &[Agent], now: i64, recent: i64) -> Vec<&Agent> {
    let mut keep: HashSet<(Tool, &str)> =
        all.iter().filter(|a| a.state != State::Idle || a.details.lease == Lease::Held || now - a.since_ms <= recent)
            .map(|a| (a.tool, a.id.as_str())).collect();
    loop {
        let parents: Vec<(Tool, &str)> = all
            .iter()
            .filter(|a| keep.contains(&(a.tool, a.id.as_str())))
            .filter_map(|a| a.parent.as_deref().map(|p| (a.tool, p)))
            .filter(|k| !keep.contains(k))
            .collect();
        if parents.is_empty() {
            break;
        }
        keep.extend(parents);
    }
    all.iter().filter(|a| keep.contains(&(a.tool, a.id.as_str()))).collect()
}

/// State of a row: the session's, or working when it waits on a working subagent.
fn row_state(a: &Agent, subs: &[&Agent]) -> State {
    if a.state == State::Idle && subs.iter().any(|s| s.state == State::Working) { State::Working } else { a.state }
}

/// Attaches each subagent to the session that started it: (session, its subagents). A
/// subagent whose session is not on the list (stopped long ago) gets its own row. The agents
/// of a workflow run hang from the run, itself a subagent: they come after the session's own
/// subagents (the card lists only those; see `direct`).
/// In screen order: who needs you, then who is working (or has a subagent working); within
/// each group, the most recent first.
fn group(all: &[Agent]) -> Vec<(&Agent, Vec<&Agent>)> {
    let is_top = |a: &Agent| match &a.parent {
        Some(p) => !all.iter().any(|o| o.tool == a.tool && o.id == *p),
        None => true,
    };
    let mut list: Vec<(&Agent, Vec<&Agent>)> = all
        .iter()
        .filter(|a| is_top(a))
        .map(|a| {
            let mut subs = Vec::new();
            descendants(all, a, &mut subs);
            (a, subs)
        })
        .collect();
    list.sort_by_key(|(a, subs)| (rank(row_state(a, subs)), -a.since_ms));
    list
}

/// Parent-first order keeps each subtree contiguous in the card.
fn descendants<'a>(all: &'a [Agent], parent: &Agent, out: &mut Vec<&'a Agent>) {
    for child in all.iter().filter(|s| s.tool == parent.tool && s.parent.as_deref() == Some(parent.id.as_str())) {
        if out.iter().any(|seen| std::ptr::eq(*seen, child)) {
            continue;
        }
        out.push(child);
        descendants(all, child, out);
    }
}

/// A workflow run in the screen's format.
fn run_row(r: &RunCard) -> AgentRun {
    let phases: Vec<AgentPhase> = r
        .phases
        .iter()
        .map(|p| AgentPhase {
            title: p.title.as_str().into(),
            state: p.state.into(),
            count: p.count.as_str().into(),
            fill: p.fill_permille as f32 / 1000.0,
            selected: p.selected,
        })
        .collect();
    let agents: Vec<AgentRunAgent> = r
        .agents
        .iter()
        .map(|g| AgentRunAgent { title: g.title.as_str().into(), state: g.state.into(), text: g.text.as_str().into() })
        .collect();
    AgentRun {
        key: r.key.as_str().into(),
        title: r.title.as_str().into(),
        state: state_key(r.state).into(),
        state_text: r.state_text.as_str().into(),
        caption: r.caption.as_str().into(),
        open: r.open,
        phases: ModelRc::from(Rc::new(VecModel::from(phases))),
        agents: ModelRc::from(Rc::new(VecModel::from(agents))),
        more_agents: r.more_agents as i32,
        top: r.top as i32,
        lines: r.lines as i32,
    }
}

/// The session's own subagents: without the agents of its workflow runs (whose parent is
/// another item of the list).
fn direct<'a>(subs: &[&'a Agent]) -> Vec<&'a Agent> {
    subs.iter().copied().filter(|s| !subs.iter().any(|o| s.parent.as_deref() == Some(o.id.as_str()))).collect()
}

/// Who walks on the card: the working subagents and workflow agents, not the runs (those
/// are already represented by their agents).
fn walkers<'a>(subs: &[&'a Agent]) -> impl Iterator<Item = &'a Agent> {
    subs.iter().copied().filter(|s| s.state == State::Working && !subs.iter().any(|o| o.parent.as_deref() == Some(s.id.as_str())))
}

/// Second line: tool, subagents and what it's doing now (when stopped, the folder).
fn info(a: &Agent, subs: &[&Agent], runs: usize, dir: &str) -> String {
    let mut parts = vec![a.tool.name().to_string()];
    if a.parent.is_some() {
        parts.push(tr!("subagent").into());
    }
    let working = subs.iter().filter(|s| s.state == State::Working).count();
    match (subs.len(), working) {
        (0, _) => {}
        (n, 0) => parts.push(plural(n, tr!("subagent"), tr!("subagents"))),
        (n, w) if n == w => parts.push(plural(n, tr!("active subagent"), tr!("active subagents"))),
        (n, w) => parts.push(tr!("{} of {} subagents active", w, n)),
    }
    if runs > 0 {
        parts.push(plural(runs, tr!("workflow"), tr!("workflows")));
    }
    if a.state != State::Idle && !a.action.is_empty() {
        parts.push(a.action.clone());
    } else if !dir.is_empty() {
        parts.push(dir.to_string());
    }
    parts.join(" · ")
}

/// A subagent on the card of the session that started it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SubLine {
    key: String,
    depth: i32,
    /// Kind of subagent ("" = the tool doesn't say, or it equals the title).
    kind: String,
    title: String,
    state: State,
    state_text: String,
}

/// Everything a row on screen shows, without the images: it can be compared with what is
/// already there. The detail fields come from `Details` and stay empty when the tool doesn't
/// report them; the screen hides whatever is empty.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Card {
    /// Identifies the session in clicks (see `session_key`).
    key: String,
    /// Working subagents shown walking (up to `MAX_WALKERS`), by id.
    walking: Vec<String>,
    tool: Tool,
    state: State,
    title: String,
    info: String,
    state_text: String,
    sub: bool,
    can_resume: bool,
    open: bool,
    walkers: usize,
    /// Without the vendor prefix: "fable-5-2".
    model: String,
    effort: String,
    branch: String,
    /// Input, output and cache, already abbreviated.
    tokens: Option<[String; 3]>,
    /// Duration of the last turn ("" = unknown).
    turn: String,
    /// What else is known about the last turn: messages in the conversation, why it stopped.
    turn_note: String,
    prompt: String,
    subs: Vec<SubLine>,
    /// Subagents beyond the ones listed.
    more_subs: usize,
    runs: Vec<RunCard>,
}

/// A workflow run on the card of the session that started it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RunCard {
    /// The run id: clicks arrive with it.
    key: String,
    title: String,
    state: State,
    state_text: String,
    /// Where it is: "phase 3 of 4: Review · 4/5".
    caption: String,
    open: bool,
    phases: Vec<PhaseLine>,
    /// Agents of the selected phase (expanded only).
    agents: Vec<RunAgentLine>,
    more_agents: usize,
    /// Lines of the runs above it on the card, and its own.
    top: usize,
    lines: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PhaseLine {
    title: String,
    /// "done", "current", "stopped" or "todo" (see `AgentPhase` in the view).
    state: &'static str,
    /// "4/5" ("" before it starts).
    count: String,
    /// Part done, in thousandths.
    fill_permille: u16,
    selected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RunAgentLine {
    title: String,
    /// "working", "done" or "stopped".
    state: &'static str,
    text: String,
}

/// Run `run` for the card. `subs`: everything under the session (its agents among them).
fn run_card(run: &Agent, subs: &[&Agent], now: i64, view: &RunView) -> RunCard {
    let phases = &run.details.phases;
    let mine = |id: &str| subs.iter().copied().find(|s| s.id == id && s.parent.as_deref() == Some(run.id.as_str()));
    let working = |p: &agents::Phase| p.agents.iter().any(|id| mine(id).is_some_and(|s| s.state == State::Working));
    let states: Vec<&'static str> = phases
        .iter()
        .map(|p| match p.done as usize {
            _ if p.agents.is_empty() => "todo",
            _ if working(p) => "current",
            d if d >= p.agents.len() => "done",
            _ if run.state == State::Working => "current",
            _ => "stopped",
        })
        .collect();
    // where it is: the phase running (the last one, if several), or the last that started
    let now_at = states.iter().rposition(|s| *s == "current").or_else(|| phases.iter().rposition(|p| !p.agents.is_empty()));
    let picked = view.phase.get(&run.id).copied().filter(|&i| i < phases.len()).or(now_at);
    let open = view.open.contains(&run.id);
    let caption = match now_at {
        Some(i) => {
            let p = &phases[i];
            tr!("phase {} of {}: {} · {}/{}", i + 1, phases.len(), p.title, p.done, p.agents.len())
        }
        None => run.action.clone(),
    };
    let lines: Vec<PhaseLine> = phases
        .iter()
        .zip(&states)
        .enumerate()
        .map(|(i, (p, state))| PhaseLine {
            title: if p.title.is_empty() { "—".into() } else { p.title.clone() },
            state,
            count: if p.agents.is_empty() { String::new() } else { format!("{}/{}", p.done, p.agents.len()) },
            fill_permille: (p.done as usize * 1000).checked_div(p.agents.len()).unwrap_or(0).min(1000) as u16,
            selected: open && Some(i) == picked,
        })
        .collect();
    let listed: Vec<&Agent> = match picked.filter(|_| open) {
        Some(i) => phases[i].agents.iter().filter_map(|id| mine(id)).collect(),
        None => Vec::new(),
    };
    let agents: Vec<RunAgentLine> = listed.iter().take(MAX_RUN_AGENTS).map(|s| run_agent_line(s, now)).collect();
    let more_agents = listed.len().saturating_sub(MAX_RUN_AGENTS);
    RunCard {
        key: run.id.clone(),
        title: run.title.clone(),
        state: run.state,
        state_text: state_text(run, now),
        caption,
        open,
        lines: 2 + if open { lines.len() + agents.len() + usize::from(more_agents > 0) } else { 0 },
        phases: lines,
        agents,
        more_agents,
        top: 0,
    }
}

/// An agent of an expanded run: working, what it is calling; done, model and output
/// tokens (the total would count the cache reads, the same context again on every call);
/// stopped, why.
fn run_agent_line(s: &Agent, now: i64) -> RunAgentLine {
    let (state, text) = match s.state {
        State::Working if !s.action.is_empty() => ("working", s.action.clone()),
        State::Working | State::Waiting => ("working", state_text(s, now)),
        State::Idle if s.detail.is_empty() => {
            let model = s.details.model.strip_prefix("claude-").unwrap_or(&s.details.model);
            let tokens = s.details.tokens.map(|t| tr!("{} output", compact(t.output)));
            let parts: Vec<String> = [Some(model.to_string()), tokens].into_iter().flatten().filter(|p| !p.is_empty()).collect();
            ("done", parts.join(" · "))
        }
        State::Idle => ("stopped", s.detail.clone()),
    };
    RunAgentLine { title: s.title.clone(), state, text }
}

/// The subagents walking with the imported mascots: one strip (`Stage`) per card, each
/// subagent with the mascot the rotation gave it.
#[derive(Default)]
struct Heroes {
    /// Mascots that walk and their frames, from the mascot-list generation they were read
    /// in (reloading the folder replaces everything).
    cast: Option<((u64, &'static str), Vec<Hero>)>,
    roster: Roster,
    /// Strip of each card, by session key.
    stages: HashMap<String, Stage>,
    /// What the screen shows for each card (the same model while the card exists: the row
    /// isn't recreated every frame).
    models: HashMap<String, Rc<VecModel<AgentWalker>>>,
    /// Last action of each subagent and until when (ms) it counts as calling tools.
    busy: HashMap<String, (String, i64)>,
    clock: Option<Instant>,
}

/// A mascot that walks (PNG sprite or code mascot, see `mascot::walkers`): the script and
/// the frames (right, left) of each animation.
type Hero = walkers::Walker;

/// Wrote to its log less than this ago (or has a new action less than `BUSY_MS` ago): it
/// is calling tools, and the mascot runs.
const WROTE_MS: i64 = 4000;
const BUSY_MS: i64 = 5000;

impl Heroes {
    /// Clock of the strips (ms since first use; only moves forward).
    fn now(&mut self) -> u64 {
        self.clock.get_or_insert_with(Instant::now).elapsed().as_millis() as u64
    }

    /// The mascots that walk, of both kinds, drawn in the app's theme (read again when the
    /// mascots are reloaded or the theme changes).
    fn load(&mut self, theme: &Theme) -> usize {
        let generation = (sprites::generation(), theme.key);
        if self.cast.as_ref().is_none_or(|(g, _)| *g != generation) {
            let list = walkers::all(theme);
            self.cast = Some((generation, list));
            self.stages.clear();
        }
        self.cast.as_ref().map_or(0, |(_, l)| l.len())
    }

    /// The subagent is calling tools: it wrote to its log recently, or its action changed
    /// recently (for the tools that don't say when they wrote).
    fn calling_tools(&mut self, sub: &Agent, now: i64) -> bool {
        let wrote = sub.details.wrote_ms;
        let until = match self.busy.get(&sub.id) {
            Some((action, until)) if *action == sub.action => *until,
            _ if sub.action.is_empty() => 0,
            _ => now + BUSY_MS,
        };
        self.busy.insert(sub.id.clone(), (sub.action.clone(), until));
        (wrote > 0 && now - wrote < WROTE_MS) || now < until
    }

    /// The model the screen shows for card `key`.
    fn model(&mut self, key: &str) -> ModelRc<AgentWalker> {
        ModelRc::from(self.models.entry(key.to_string()).or_default().clone())
    }

    /// Who walks on each card now: (card, [(subagent, calling tools)]).
    /// When off (or with no mascot that walks), the strips empty out and the screen uses the
    /// tool's mascot.
    fn sync(&mut self, on: bool, walking: &[(String, Vec<(String, bool)>)], theme: &Theme) {
        let n = if on { self.load(theme) } else { 0 };
        self.models.retain(|key, _| walking.iter().any(|(k, _)| k == key));
        self.busy.retain(|id, _| walking.iter().any(|(_, subs)| subs.iter().any(|(s, _)| s == id)));
        // a subagent that stopped walking frees its slot (the next new one follows the rotation)
        self.roster.keep(walking.iter().flat_map(|(_, subs)| subs.iter().map(|(id, _)| id)));
        if n == 0 {
            self.stages.clear();
            for m in self.models.values() {
                if m.row_count() > 0 {
                    m.set_vec(Vec::new());
                }
            }
            return;
        }
        let now = self.now();
        let Some((_, heroes)) = &self.cast else { return };
        let scripts: Vec<&Script> = heroes.iter().map(|h| &h.script).collect();
        self.stages.retain(|key, _| walking.iter().any(|(k, _)| k == key));
        for (key, subs) in walking {
            let here: Vec<(String, usize, bool)> =
                subs.iter().map(|(id, busy)| (id.clone(), self.roster.pick(id, n), *busy)).collect();
            let stage = self.stages.entry(key.clone()).or_default();
            stage.cast(now, &here, &scripts);
            stage.step(now, &scripts);
        }
        self.draw(now);
    }

    /// One frame: advances time and redraws the strips that have someone on them.
    fn tick(&mut self) {
        if self.stages.values().all(Stage::is_empty) {
            return;
        }
        let now = self.now();
        if let Some((_, heroes)) = &self.cast {
            let scripts: Vec<&Script> = heroes.iter().map(|h| &h.script).collect();
            for stage in self.stages.values_mut() {
                stage.step(now, &scripts);
            }
        }
        self.draw(now);
    }

    /// Passes the poses of each strip to the screen and asks the hub for the animation pace.
    fn draw(&mut self, now: u64) {
        let Some((_, heroes)) = &self.cast else { return };
        let scripts: Vec<&Script> = heroes.iter().map(|h| &h.script).collect();
        let calm = anim::calm();
        for (key, stage) in &self.stages {
            let Some(model) = self.models.get(key) else { continue };
            let rows: Vec<AgentWalker> = stage
                .poses(now, &scripts, calm)
                .into_iter()
                .map(|Pose { hero, clip, frame, right, x }| {
                    let (r, l) = &heroes[hero].frames[clip];
                    AgentWalker { frame: if right { &r[frame] } else { &l[frame] }.clone(), x }
                })
                .collect();
            if model.row_count() == rows.len() {
                for (i, row) in rows.into_iter().enumerate() {
                    model.set_row_data(i, row);
                }
            } else {
                model.set_vec(rows);
            }
            if !calm && let Some(ms) = stage.frame_ms(&scripts) {
                anim::request_frame_ms(ms);
            }
        }
    }
}

/// Which imported mascot walks for each subagent: each new subagent takes the next one, in
/// rotation, and keeps it while it is walking.
#[derive(Default)]
struct Roster {
    of: HashMap<String, usize>,
    next: usize,
}

impl Roster {
    /// The mascot (0..`n`) of subagent `id`.
    fn pick(&mut self, id: &str, n: usize) -> usize {
        if let Some(i) = self.of.get(id) {
            return i % n;
        }
        let i = self.next % n;
        self.next = self.next.wrapping_add(1);
        self.of.insert(id.to_string(), i);
        i
    }

    /// Forgets whoever is no longer walking.
    fn keep<'a>(&mut self, walking: impl Iterator<Item = &'a String>) {
        let alive: std::collections::HashSet<&String> = walking.collect();
        self.of.retain(|id, _| alive.contains(id));
    }
}

/// Stable key of a session for the screen: clicks arrive with it, not with the row's
/// position, which changes when the list reorders between readings.
fn session_key(a: &Agent) -> String {
    format!("{}:{}", a.tool.name(), a.id)
}

fn card(a: &Agent, subs: &[&Agent], dir: &str, now: i64, open: bool, view: &RunView) -> Card {
    let d = &a.details;
    let mut note = Vec::new();
    if let Some((_, messages)) = d.last_turn.filter(|(_, m)| *m > 0) {
        note.push(tr!("{} messages", messages));
    }
    // the reason belongs to the last reply: mid-turn it isn't "why it stopped" yet
    if a.state == State::Idle && !d.stop_reason.is_empty() {
        note.push(stop_text(&d.stop_reason));
    }
    let walking: Vec<String> = walkers(subs).take(MAX_WALKERS).map(|s| s.id.clone()).collect();
    let mut runs: Vec<RunCard> = Vec::new();
    for r in direct(subs).into_iter().filter(|s| s.details.kind == WORKFLOW) {
        let mut c = run_card(r, subs, now, view);
        c.top = runs.iter().map(|r| r.lines).sum();
        runs.push(c);
    }
    let subs: Vec<&Agent> = if a.tool == Tool::Omp {
        subs.to_vec()
    } else {
        direct(subs).into_iter().filter(|s| s.details.kind != WORKFLOW).collect()
    };
    let max_sub_lines = if a.tool == Tool::Omp { subs.len() } else { MAX_SUB_LINES };
    Card {
        key: session_key(a),
        walkers: walking.len(),
        walking,
        tool: a.tool,
        state: a.state,
        title: a.title.clone(),
        info: info(a, &subs, runs.len(), dir),
        state_text: state_text(a, now),
        sub: a.parent.is_some(),
        can_resume: resume_argv(a).is_some(),
        open,
        model: d.model.strip_prefix("claude-").unwrap_or(&d.model).to_string(),
        effort: d.effort.clone(),
        branch: d.branch.clone(),
        tokens: d.tokens.map(|t| [compact(t.input), compact(t.output), compact(t.cache_read.saturating_add(t.cache_write))]),
        turn: d.last_turn.map(|(ms, _)| duration(ms)).unwrap_or_default(),
        turn_note: note.join(" · "),
        prompt: clip(&d.last_prompt, 140),
        subs: subs
            .iter()
            .take(max_sub_lines)
            .map(|s| SubLine {
                key: if a.tool == Tool::Omp { session_key(s) } else { String::new() },
                depth: if a.tool == Tool::Omp { s.details.depth.unwrap_or(1).saturating_sub(1).min(4) as i32 } else { 0 },
                kind: if s.details.kind == s.title { String::new() } else { s.details.kind.clone() },
                // a workflow run says its phase and progress
                title: if s.action.is_empty() { s.title.clone() } else { format!("{} · {}", s.title, s.action) },
                state: s.state,
                state_text: state_text(s, now),
            })
            .collect(),
        more_subs: subs.len().saturating_sub(max_sub_lines),
        runs,
    }
}

/// 950 → "950", 12 345 → "12 k", 1 234 567 → "1.2 M" (decimal comma in Portuguese: "1,2 M").
fn compact(n: u64) -> String {
    let decimal = |s: String| if i18n::current() == Language::PtBr { s.replace('.', ",") } else { s };
    match n {
        0..1_000 => n.to_string(),
        // from 9 950 on, one decimal would round up to "10.0 k"
        1_000..9_950 => decimal(format!("{:.1} k", n as f64 / 1e3)),
        9_950..10_000 => "10 k".to_string(),
        10_000..1_000_000 => format!("{} k", n / 1_000),
        _ => decimal(format!("{:.1} M", n as f64 / 1e6)),
    }
}

/// "8 s", "2 min 05 s", "2 h 05 min".
fn duration(ms: i64) -> String {
    let s = (ms / 1000).max(0);
    match s {
        0..60 => format!("{s} s"),
        60..3600 => format!("{} min {:02} s", s / 60, s % 60),
        _ => format!("{} h {:02} min", s / 3600, s % 3600 / 60),
    }
}

/// Why it stopped, in words (the raw name if it's one I don't know).
fn stop_text(reason: &str) -> String {
    match reason {
        "end_turn" => tr!("complete reply").into(),
        "tool_use" => tr!("stopped at a tool").into(),
        "max_tokens" => tr!("hit the token limit").into(),
        "stop_sequence" => tr!("stopped at a stop sequence").into(),
        "refusal" => tr!("refused the request").into(),
        "pause_turn" => tr!("turn paused").into(),
        other => tr!("stopped by {}", other),
    }
}

/// Resume idle top-level sessions. OMP takes its own writer lease; a session still
/// owned by another process isn't overwritten.
fn resume_argv(a: &Agent) -> Option<[&str; 3]> {
    if a.state != State::Idle || a.parent.is_some() {
        return None;
    }
    // Codex and OpenCode take a session ID, never an option or path.
    let safe_id = || !a.id.is_empty() && !a.id.starts_with('-') && a.id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_');
    match a.tool {
        Tool::Codex if safe_id() => Some(["codex", "resume", a.id.as_str()]),
        Tool::OpenCode if safe_id() => Some(["opencode", "-s", a.id.as_str()]),
        Tool::Omp => {
            if a.details.lease != Lease::Free {
                return None;
            }
            let path = a.log.as_deref().filter(|p| p.is_absolute())?;
            Some(["omp", "--resume", path.to_str()?])
        }
        _ => None,
    }
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

fn rank(s: State) -> u8 {
    match s {
        State::Waiting => 0,
        State::Working => 1,
        State::Idle => 2,
    }
}

fn state_key(s: State) -> &'static str {
    match s {
        State::Working => "working",
        State::Waiting => "waiting",
        State::Idle => "idle",
    }
}

/// Time since `since_ms`: "3 min", "2 h"; `None` under a minute or with no known time.
fn elapsed(since_ms: i64, now: i64) -> Option<String> {
    let min = (now - since_ms) / 60_000;
    match min {
        _ if since_ms <= 0 || min < 1 => None,
        1..60 => Some(format!("{min} min")),
        _ => Some(format!("{} h", min / 60)),
    }
}

fn state_text(a: &Agent, now: i64) -> String {
    let elapsed = elapsed(a.since_ms, now);
    match a.state {
        State::Working if a.since_ms <= 0 => tr!("working").into(),
        State::Working => elapsed.map_or(tr!("just started").into(), |t| tr!("working for {}", t)),
        State::Waiting if a.detail.is_empty() => tr!("waiting for you").into(),
        State::Waiting => tr!("waiting for you: {}", a.detail),
        State::Idle if a.tool == Tool::Omp && a.parent.is_none() => match a.details.lease {
            Lease::Held => tr!("Open in OMP").into(),
            Lease::Free if a.detail.is_empty() => tr!("OMP session closed").into(),
            Lease::Free => tr!("OMP session closed · {}", a.detail),
            Lease::Unknown => tr!("OMP session status unknown").into(),
        },
        State::Idle => {
            let what = if a.detail.is_empty() { tr!("stopped") } else { a.detail.as_str() };
            match elapsed {
                Some(t) => tr!("{} {} ago", what, t),
                None if a.since_ms <= 0 => what.to_string(),
                None if a.detail.is_empty() => tr!("just finished").into(),
                None => tr!("{} just now", what),
            }
        }
    }
}

/// `/home/me/Projects/x` → `~/Projects/x` (`C:\Users\me\x` → `~\x` on Windows, where the
/// case may differ: a tool can record `c:\users\me\x`).
fn short_dir(cwd: &str, home: &str) -> String {
    // a drive letter: a Windows path, compared without case
    let windows = home.as_bytes().get(1) == Some(&b':');
    let rest = match cwd.get(..home.len()) {
        Some(head) if head == home || (windows && head.eq_ignore_ascii_case(home)) => &cwd[home.len()..],
        _ => return cwd.to_string(),
    };
    if !home.is_empty() && (rest.is_empty() || rest.starts_with(['/', '\\'])) { format!("~{rest}") } else { cwd.to_string() }
}

impl AgentsMini {
    /// Opens session `key` in the detail window (creates the window on first use).
    fn open_detail(app: &Rc<RefCell<Self>>, key: &str, ui: &HubWindow) {
        let mut me = app.borrow_mut();
        let Some(id) = me.find(key).map(|a| (a.tool, a.id.clone())) else { return };
        if me.detail.is_none() {
            match AgentDetailWindow::new() {
                Ok(win) => {
                    me.detail = Some(Detail::new(win, app, ui, &id));
                    me.style_detail();
                }
                Err(e) => {
                    log::warn!("couldn't create the detail window: {e}");
                    return;
                }
            }
        }
        let Some(d) = me.detail.as_mut() else { return };
        if d.key != id {
            d.key = id.clone();
            d.win.global::<AgentDetail>().set_section(0);
            d.shown.clear();
            d.loaded = false;
            d.steps.set_vec(Vec::new());
        }
        d.open = true;
        // the provider starts reading this session's conversation; the reading comes right away
        agents::watch(Some(id));
        me.host.bus.poke(Topic::Agents);
        me.selected = None;
        me.refresh(ui);
        me.push_detail();
        // showing takes focus from the hub, which may react right away: the app is no longer borrowed
        let win = me.detail.as_ref().map(|d| d.win.clone_strong());
        // taskbar icon: the app's mascot (set after `show`, when the native window exists)
        let icon = super::kit::window_icon(sprites::get(&me.mascot_key()), me.host.theme_for(&me.manifest), None);
        drop(me);
        if let Some(win) = win {
            if let Err(e) = win.show() {
                log::warn!("couldn't show the detail window: {e}");
            }
            crate::popup::set_icon_after_show(&win, icon);
            crate::popup::label_after_show(&win, crate::popup::Kind::Detail);
            crate::popup::focus(win.window());
        }
    }

    /// Look and theme in the detail window (its globals are separate from the hub's).
    fn style_detail(&mut self) {
        let theme = render::colors(self.host.theme_for(&self.manifest));
        let Some(d) = &self.detail else { return };
        crate::skin::apply_current(&d.win);
        // the window animates nothing; "calm" keeps any mascot still
        d.win.global::<Anim>().set_calm(true);
        d.win.global::<AgentDetail>().set_theme(theme);
    }

    /// The window was closed (by the system): stop reading the conversation.
    fn detail_closed(&mut self) {
        if let Some(d) = &mut self.detail {
            d.open = false;
        }
        agents::watch(None);
    }

    /// Screen key (`session_key`) of the session open in the detail window.
    fn detail_key(&self) -> Option<String> {
        self.detail.as_ref().map(|d| format!("{}:{}", d.key.0.name(), d.key.1))
    }

    /// Passes to the detail window what is known about the session and the timeline.
    fn push_detail(&mut self) {
        let Some(key) = self.detail.as_ref().filter(|d| d.open).map(|d| d.key.clone()) else { return };
        let now = now_ms();
        let found = self.snap.agents.iter().find(|a| a.tool == key.0 && a.id == key.1).cloned();
        let face = found.as_ref().map(|a| self.face(a.tool, a.state));
        let timeline = self.snap.timeline.as_ref().filter(|t| t.tool == key.0 && t.id == key.1);
        let Some(d) = self.detail.as_mut() else { return };
        let g = d.win.global::<AgentDetail>();
        g.set_gone(found.is_none());
        if let (Some(a), Some(face)) = (&found, face) {
            let home = dirs::home_dir().map(|h| h.to_string_lossy().into_owned()).unwrap_or_default();
            let det = &a.details;
            g.set_title(a.title.as_str().into());
            g.set_info(format!("{} · {}", a.tool.name(), short_dir(&a.cwd, &home)).into());
            g.set_state(state_key(a.state).into());
            g.set_state_text(state_text(a, now).into());
            g.set_mascot(face);
            g.set_model(det.model.strip_prefix("claude-").unwrap_or(&det.model).into());
            g.set_effort(det.effort.as_str().into());
            g.set_branch(det.branch.as_str().into());
            g.set_tokens(det.tokens.map_or(String::new(), |t| {
                tr!("in {} · out {} · cache {}", compact(t.input), compact(t.output), compact(t.cache_read.saturating_add(t.cache_write)))
            })
            .into());
            g.set_turn(det.last_turn.map(|(ms, _)| duration(ms)).unwrap_or_default().into());
            g.set_can_resume(resume_argv(a).is_some());
        }
        let (result, patch) = timeline.map_or((None, None), |t| (t.result.as_ref(), t.patch.as_ref()));
        update_artifact(result, g.get_result_text(), |text| g.set_result_text(text));
        update_artifact(patch, g.get_patch_text(), |text| g.set_patch_text(text));
        g.set_result_truncated(result.is_some_and(|a| a.truncated));
        g.set_patch_truncated(patch.is_some_and(|a| a.truncated));
        if (g.get_section() == 1 && result.is_none()) || (g.get_section() == 2 && patch.is_none()) {
            g.set_section(0);
        }
        if let Some(timeline) = timeline {
            let steps = &timeline.steps;
            if *steps != d.shown {
                let today = chrono::Local::now().date_naive();
                let rows: Vec<AgentStep> = steps.iter().rev().map(|s| step_row(s, today)).collect();
                // the date only takes room when some step is from another day
                g.set_dated_steps(rows.iter().any(|r| r.time.len() > 5));
                d.steps.set_vec(rows);
                d.shown.clone_from(steps);
            }
            d.loaded = true;
        }
        g.set_loading(!d.loaded && found.is_some());
    }
}

fn update_artifact(artifact: Option<&Artifact>, current: slint::SharedString, set: impl FnOnce(slint::SharedString)) {
    let text = artifact.map_or("", |a| a.text.as_ref());
    if current.as_str() != text {
        set(text.into());
    }
}

impl Detail {
    /// Wires the window's buttons to the same actions as the card.
    fn new(win: AgentDetailWindow, app: &Rc<RefCell<AgentsMini>>, ui: &HubWindow, key: &(Tool, String)) -> Self {
        let steps = Rc::new(VecModel::default());
        let g = win.global::<AgentDetail>();
        g.set_steps(ModelRc::from(steps.clone()));
        // each button: the session the window shows now, by its screen key
        let action = |run: fn(&mut AgentsMini, &str, &HubWindow)| {
            let a = Rc::downgrade(app);
            let w = ui.as_weak();
            move || {
                if let (Some(a), Some(ui)) = (a.upgrade(), w.upgrade()) {
                    let mut a = a.borrow_mut();
                    if let Some(key) = a.detail_key() {
                        run(&mut a, &key, &ui);
                    }
                }
            }
        };
        g.on_open_dir(action(AgentsMini::open_dir));
        g.on_open_terminal(action(AgentsMini::open_terminal));
        g.on_resume(action(AgentsMini::resume));
        let (a, w) = (Rc::downgrade(app), win.as_weak());
        g.on_close(move || {
            if let (Some(a), Some(win)) = (a.upgrade(), w.upgrade()) {
                let _ = win.hide();
                a.borrow_mut().detail_closed();
            }
        });
        let w = win.as_weak();
        g.on_start_move(move || {
            if let Some(win) = w.upgrade() {
                crate::popup::drag_move(win.window());
            }
        });
        let w = win.as_weak();
        g.on_start_resize(move |dir| {
            if let Some(win) = w.upgrade() {
                crate::popup::drag_resize(win.window(), &dir);
            }
        });
        let a = Rc::downgrade(app);
        win.window().on_close_requested(move || {
            // only hides: the window is reused next time
            if let Some(a) = a.upgrade() {
                a.borrow_mut().detail_closed();
            }
            slint::CloseRequestResponse::HideWindow
        });
        Detail { win, key: key.clone(), open: false, steps, shown: Vec::new(), loaded: false }
    }
}

/// A step in the screen's format; one from a day other than `today` also has the date.
fn step_row(s: &Step, today: chrono::NaiveDate) -> AgentStep {
    use chrono::TimeZone;
    let time = (s.at_ms > 0)
        .then(|| chrono::Local.timestamp_millis_opt(s.at_ms).single())
        .flatten()
        .map(|t| t.format(if t.date_naive() == today { "%H:%M" } else { tr!("%m/%d %H:%M") }).to_string())
        .unwrap_or_default();
    let kind = match s.kind {
        StepKind::User => "user",
        StepKind::Reply => "reply",
        StepKind::Tool => "tool",
        StepKind::Error => "error",
    };
    AgentStep { time: time.into(), kind: kind.into(), text: s.text.as_str().into() }
}

impl MiniApp for AgentsMini {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn enable(&mut self, ui: &HubWindow) {
        self.baseline = false;
        ui.global::<AgentsApp>().set_theme(render::colors(self.host.theme_for(&self.manifest)));
        self.push_mascot(ui);
        self.refresh(ui);
    }

    fn disable(&mut self, _ui: &HubWindow) {
        if let Some(d) = self.detail.take() {
            let _ = d.win.hide();
        }
        agents::watch(None);
        self.snap = AgentsSnapshot::default();
        self.prev.clear();
        self.speech.clear();
        self.mood = Mood::Sleep;
        self.selected = None;
        self.cards.clear();
        self.rows.set_vec(Vec::new());
    }

    fn on_event(&mut self, ev: &Event, ui: &HubWindow) {
        let Event::Agents(snap) = ev else { return };
        self.ingest(snap.clone());
        // also updates the "N min ago", which change without the state changing
        self.refresh(ui);
        self.push_detail();
    }

    fn tick(&mut self, _frame: u32, ui: &HubWindow) {
        // the speech expires on time even without a new reading
        if self.speech.take_expired() {
            self.refresh(ui);
        }
        if self.refresh_mascot() {
            self.push_mascot(ui);
        }
        self.heroes.tick();
    }

    fn settings_changed(&mut self, ui: &HubWindow) {
        ui.global::<AgentsApp>().set_theme(render::colors(self.host.theme_for(&self.manifest)));
        // new theme: the row mascots are redrawn
        self.faces.clear();
        self.cards.clear();
        self.refresh(ui);
        self.push_mascot(ui);
        // new look or theme: the detail window too
        self.style_detail();
        self.push_detail();
    }

    fn mood(&self) -> Mood {
        self.mood
    }

    fn mascot_key(&self) -> String {
        self.host.config.borrow().string(&self.manifest, "mascot")
    }

    fn summary(&self) -> String {
        let all: Vec<Agent> = self.visible(now_ms()).into_iter().cloned().collect();
        let Counts { working, waiting, .. } = Counts::of(&group(&all));
        if waiting > 0 {
            tr!("Agents: {} waiting for you · {} working", waiting, working)
        } else {
            tr!("Agents: {} working", working)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(state: State, since_ms: i64, detail: &str) -> Agent {
        Agent {
            tool: Tool::Claude,
            id: "s".into(),
            title: "t".into(),
            cwd: "/x".into(),
            state,
            since_ms,
            detail: detail.into(),
            action: String::new(),
            parent: None,
            details: Default::default(),
            log: None,
        }
    }

    #[test]
    fn card_only_has_what_the_tool_reports() {
        use crate::providers::agents::{Details, Tokens};
        let now = 10 * 3_600_000;
        // a tool that reports nothing extra: no details on the card
        let bare = card(&agent(State::Idle, 1, ""), &[], "~/p", now, false, &RunView::default());
        assert_eq!((bare.model.as_str(), bare.tokens.is_some(), bare.turn.as_str(), bare.prompt.as_str()), ("", false, "", ""));
        assert!(bare.subs.is_empty() && bare.turn_note.is_empty());

        let full = Agent {
            details: Details {
                last_prompt: "write the\nwrapper".into(),
                model: "claude-fable-5-2".into(),
                effort: "medium".into(),
                branch: "master".into(),
                tokens: Some(Tokens { input: 950, output: 45_300, cache_read: 1_200_000, cache_write: 34_000 }),
                last_turn: Some((125_000, 316)),
                stop_reason: "end_turn".into(),
                ..Details::default()
            },
            ..agent(State::Idle, 1, "")
        };
        let sub = Agent {
            title: "Read the page".into(),
            details: Details { kind: "web-fetch".into(), ..Details::default() },
            ..agent(State::Working, now - 3 * 60_000, "")
        };
        let c = card(&full, &[&sub], "~/p", now, true, &RunView::default());
        assert_eq!((c.model.as_str(), c.effort.as_str(), c.branch.as_str()), ("fable-5-2", "medium", "master"));
        assert_eq!(c.tokens, Some(["950".to_string(), "45 k".to_string(), "1.2 M".to_string()]));
        assert_eq!((c.turn.as_str(), c.turn_note.as_str()), ("2 min 05 s", "316 messages · complete reply"));
        assert_eq!((c.prompt.as_str(), c.open, c.walkers), ("write the wrapper", true, 1));
        let line = &c.subs[0];
        assert_eq!((line.kind.as_str(), line.title.as_str(), line.state_text.as_str()), ("web-fetch", "Read the page", "working for 3 min"));

        // working: the reason of the last reply isn't the reason it stopped yet
        let busy = Agent { state: State::Working, ..full.clone() };
        assert_eq!(card(&busy, &[], "~/p", now, false, &RunView::default()).turn_note, "316 messages");
        // subagents past the limit become a number; a kind equal to the title isn't repeated
        let same = Agent { title: "Explore".into(), details: Details { kind: "Explore".into(), ..Details::default() }, ..sub.clone() };
        let many = vec![&same; MAX_SUB_LINES + 2];
        let c = card(&full, &many, "~/p", now, false, &RunView::default());
        assert_eq!((c.subs.len(), c.more_subs, c.subs[0].kind.as_str()), (MAX_SUB_LINES, 2, ""));
        assert_eq!(c.walkers, MAX_WALKERS.min(MAX_SUB_LINES + 2));
        assert_eq!(c.walking.len(), c.walkers);

        assert_eq!((compact(1_500).as_str(), duration(8_400).as_str()), ("1.5 k", "8 s"));
        assert_eq!((compact(9_949).as_str(), compact(9_960).as_str(), compact(10_000).as_str()), ("9.9 k", "10 k", "10 k"));
        assert_eq!((duration(3_599_000).as_str(), duration(7_505_000).as_str()), ("59 min 59 s", "2 h 05 min"));
        assert_eq!(stop_text("new_reason"), "stopped by new_reason");
    }

    #[test]
    fn subagents_stay_on_the_row_of_who_started_them() {
        let named = |id: &str, parent: Option<&str>, state: State, since: i64| Agent {
            id: id.into(),
            parent: parent.map(String::from),
            ..agent(state, since, "")
        };
        let all = [
            named("a", None, State::Idle, 50),
            named("a1", Some("a"), State::Working, 10),
            named("a2", Some("a"), State::Idle, 20),
            named("b", None, State::Idle, 90),
            named("orphan", Some("gone"), State::Idle, 5),
            Agent { tool: Tool::Codex, ..named("c1", Some("a"), State::Working, 1) },
        ];
        let list = group(&all);
        let ids: Vec<(&str, usize)> = list.iter().map(|(a, subs)| (a.id.as_str(), subs.len())).collect();
        // "a" moves up for having a working subagent; the child from another tool isn't its own
        assert_eq!(ids, [("a", 2), ("c1", 0), ("b", 0), ("orphan", 0)]);

        let (a, subs) = &list[0];
        assert_eq!(info(a, subs, 0, "~/p"), "Claude · 1 of 2 subagents active · ~/p");
        assert_eq!(info(&all[3], &[], 0, "~/p"), "Claude · ~/p");
        assert_eq!(info(&all[4], &[], 0, ""), "Claude · subagent");
        let busy = Agent { action: "Edit: bus.rs".into(), ..named("w", None, State::Working, 1) };
        assert_eq!(info(&busy, &[&all[1]], 0, "~/p"), "Claude · 1 active subagent · Edit: bus.rs");
        // an old action of a stopped session doesn't show
        let stale = Agent { action: "Edit: bus.rs".into(), ..named("w", None, State::Idle, 1) };
        assert_eq!(info(&stale, &[], 0, "~/p"), "Claude · ~/p");
    }

    #[test]
    fn workflow_run_card_with_its_phases_and_agents() {
        let named = |id: &str, parent: Option<&str>, state: State| Agent {
            id: id.into(),
            title: id.into(),
            parent: parent.map(String::from),
            ..agent(state, 10, "")
        };
        let phase = |title: &str, agents: &[&str], done: u32| agents::Phase {
            title: title.into(),
            agents: agents.iter().map(|a| a.to_string()).collect(),
            done,
        };
        let run = Agent {
            title: "sweep".into(),
            details: agents::Details {
                kind: WORKFLOW.into(),
                phase: "Review".into(),
                phases: vec![phase("Find", &["w1"], 1), phase("Review", &["w2", "w3"], 1), phase("Fix", &[], 0)],
                ..Default::default()
            },
            ..named("wf_1", Some("s"), State::Working)
        };
        let done = Agent {
            details: agents::Details {
                model: "claude-opus-5-5".into(),
                tokens: Some(agents::Tokens { input: 1_000, output: 2_000, cache_read: 70_000, cache_write: 0 }),
                ..Default::default()
            },
            ..named("w2", Some("wf_1"), State::Idle)
        };
        let all = [
            named("s", None, State::Idle),
            named("sub", Some("s"), State::Idle),
            run,
            named("w1", Some("wf_1"), State::Idle),
            done,
            Agent { action: "Bash: cargo test".into(), ..named("w3", Some("wf_1"), State::Working) },
        ];
        let list = group(&all);
        // one row: the workflow agents are not rows of their own
        assert_eq!(list.len(), 1);
        let (s, subs) = &list[0];
        assert_eq!((s.id.as_str(), row_state(s, subs)), ("s", State::Working));

        // collapsed: the run leaves the subagent lines and becomes a block with its bar
        let c = card(s, subs, "~/p", 10, false, &RunView::default());
        assert_eq!(c.subs.iter().map(|l| l.title.as_str()).collect::<Vec<_>>(), ["sub"]);
        assert_eq!(c.info, "Claude · 1 subagent · 1 workflow · ~/p");
        assert_eq!(c.runs.len(), 1);
        let r = &c.runs[0];
        assert_eq!((r.caption.as_str(), r.lines, r.open), ("phase 2 of 3: Review · 1/2", 2, false));
        let bar: Vec<(&str, u16)> = r.phases.iter().map(|p| (p.state, p.fill_permille)).collect();
        assert_eq!(bar, [("done", 1000), ("current", 500), ("todo", 0)]);
        assert!(r.agents.is_empty());
        // the run's working agents walk, not the run itself
        assert_eq!(c.walking, ["w3"]);

        // expanded: the running phase's agents, in the order they started
        let mut view = RunView::default();
        view.open.insert("wf_1".into());
        let r = &card(s, subs, "~/p", 10, false, &view).runs[0];
        let agents: Vec<(&str, &str, &str)> = r.agents.iter().map(|g| (g.title.as_str(), g.state, g.text.as_str())).collect();
        assert_eq!(agents, [("w2", "done", "opus-5-5 · 2.0 k output"), ("w3", "working", "Bash: cargo test")]);
        assert_eq!((r.lines, r.phases[1].selected), (2 + 3 + 2, true));
        // another phase picked
        view.phase.insert("wf_1".into(), 0);
        let r = &card(s, subs, "~/p", 10, false, &view).runs[0];
        assert_eq!(r.agents.iter().map(|g| g.title.as_str()).collect::<Vec<_>>(), ["w1"]);
    }

    #[test]
    fn a_quiet_session_stays_while_something_under_it_is_recent() {
        let named = |id: &str, parent: Option<&str>, since: i64| Agent { id: id.into(), parent: parent.map(String::from), ..agent(State::Idle, since, "") };
        let all = [
            named("old", None, 0),
            named("s", None, 0),
            named("wf", Some("s"), 0),
            named("w1", Some("wf"), 90),
            Agent { tool: Tool::Codex, ..named("other", None, 0) },
        ];
        let ids: Vec<&str> = visible(&all, 100, 50).iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["s", "wf", "w1"]);
    }

    #[test]
    fn only_resumes_idle_top_level_sessions_of_supported_tools() {
        let of = |tool: Tool, state: State, id: &str| Agent { tool, id: id.into(), ..agent(state, 1, "") };
        assert_eq!(resume_argv(&of(Tool::Codex, State::Idle, "01a1-c8")), Some(["codex", "resume", "01a1-c8"]));
        assert_eq!(resume_argv(&of(Tool::OpenCode, State::Idle, "ses_f6e8")), Some(["opencode", "-s", "ses_f6e8"]));
        assert_eq!(resume_argv(&of(Tool::Claude, State::Idle, "abc")), None);
        assert_eq!(resume_argv(&of(Tool::Codex, State::Working, "abc")), None);
        assert_eq!(resume_argv(&of(Tool::Codex, State::Idle, "--help")), None);
        assert_eq!(resume_argv(&of(Tool::Codex, State::Idle, "a b")), None);
        let sub = Agent { parent: Some("p".into()), ..of(Tool::Codex, State::Idle, "abc") };
        assert_eq!(resume_argv(&sub), None);
        let path = std::env::temp_dir().join("omp session.jsonl");
        let mut omp = of(Tool::Omp, State::Idle, "abc");
        omp.log = Some(path.clone());
        omp.details.lease = Lease::Free;
        assert_eq!(resume_argv(&omp), Some(["omp", "--resume", path.to_str().unwrap()]));
        for lease in [Lease::Held, Lease::Unknown] {
            let mut blocked = omp.clone();
            blocked.details.lease = lease;
            assert_eq!(resume_argv(&blocked), None);
        }
        assert_eq!(resume_argv(&Agent { state: State::Working, ..omp }), None);
    }

    #[test]
    fn state_text_wording() {
        let now = 10 * 3_600_000;
        let min = 60_000;
        assert_eq!(state_text(&agent(State::Working, now - 3 * min, ""), now), "working for 3 min");
        assert_eq!(state_text(&agent(State::Working, now - 5_000, ""), now), "just started");
        assert_eq!(state_text(&agent(State::Waiting, now - 3 * min, "permission"), now), "waiting for you: permission");
        assert_eq!(state_text(&agent(State::Waiting, now, ""), now), "waiting for you");
        assert_eq!(state_text(&agent(State::Idle, now - 125 * min, ""), now), "stopped 2 h ago");
        assert_eq!(state_text(&agent(State::Idle, now - 5_000, ""), now), "just finished");
        assert_eq!(state_text(&agent(State::Idle, now - 9 * min, "failed"), now), "failed 9 min ago");
        assert_eq!(state_text(&agent(State::Idle, 0, ""), now), "stopped");
        assert_eq!(state_text(&agent(State::Working, 0, ""), now), "working");
    }

    #[test]
    fn only_the_change_becomes_a_warning() {
        let idle = agent(State::Idle, 1, "");
        let waiting = agent(State::Waiting, 1, "");
        assert_eq!(change(Some(State::Working), &idle), Some(Change::Done));
        assert_eq!(change(Some(State::Working), &agent(State::Idle, 1, "failed")), Some(Change::Stopped));
        assert_eq!(change(Some(State::Idle), &idle), None);
        // a new session already stopped (or one that left waiting) isn't "finished"
        assert_eq!(change(None, &idle), None);
        assert_eq!(change(Some(State::Waiting), &idle), None);
        assert_eq!(change(Some(State::Working), &waiting), Some(Change::Waiting));
        assert_eq!(change(None, &waiting), Some(Change::Waiting));
        assert_eq!(change(Some(State::Waiting), &waiting), None);
        assert_eq!(change(Some(State::Idle), &agent(State::Working, 1, "")), None);
    }

    #[test]
    fn shortened_folder() {
        assert_eq!(short_dir("/home/me/Projects/x", "/home/me"), "~/Projects/x");
        assert_eq!(short_dir("/home/me", "/home/me"), "~");
        assert_eq!(short_dir("/home/meg/x", "/home/me"), "/home/meg/x");
        assert_eq!(short_dir("/srv/x", ""), "/srv/x");
        assert_eq!(short_dir(r"C:\Users\me\x", r"C:\Users\me"), r"~\x");
        assert_eq!(short_dir(r"C:\Users\meg", r"C:\Users\me"), r"C:\Users\meg");
        // Windows paths: the case may differ (lowercase drive from some terminals)
        assert_eq!(short_dir(r"c:\users\me\x", r"C:\Users\me"), r"~\x");
        assert_eq!(short_dir(r"c:\users\meg", r"C:\Users\me"), r"c:\users\meg");
        // Unix paths keep their case
        assert_eq!(short_dir("/home/Me/x", "/home/me"), "/home/Me/x");
        assert_eq!(short_dir("/hé", "/h"), "/hé");
    }

    #[test]
    fn each_kind_of_change_has_its_own_notification() {
        let a = agent(State::Idle, 1, "");
        assert_eq!(notify_key(&Change::Waiting, &a), "agents:waiting:Claude:s");
        assert_ne!(notify_key(&Change::Waiting, &a), notify_key(&Change::Done, &a));
        assert_ne!(notify_key(&Change::Done, &a), notify_key(&Change::Stopped, &a));
    }

    #[test]
    fn several_changes_make_one_speech_line() {
        assert_eq!(speech_of(Vec::new()), None);
        assert_eq!(speech_of(vec![(Change::Done, "x finished".into())]).as_deref(), Some("x finished"));
        // whoever waits comes first, even if it changed last
        let line = speech_of(vec![(Change::Done, "a finished".into()), (Change::Waiting, "b waits".into())]);
        assert_eq!(line.as_deref(), Some("b waits · a finished"));
        let line = speech_of(vec![
            (Change::Done, "a".into()),
            (Change::Stopped, "b".into()),
            (Change::Waiting, "c".into()),
            (Change::Waiting, "d".into()),
        ]);
        assert_eq!(line.as_deref(), Some("2 agents waiting for you! · 2 agents finished their turn"));
    }

    #[test]
    fn a_tool_that_failed_to_read_keeps_its_sessions() {
        let waiting = Agent { id: "w".into(), ..agent(State::Waiting, 1, "") };
        let codex = Agent { tool: Tool::Codex, id: "c".into(), ..agent(State::Working, 1, "") };
        let prev = AgentsSnapshot { agents: vec![waiting.clone(), codex.clone()], ..Default::default() };
        // Claude failed this time; Codex was read (and its session finished)
        let done = Agent { state: State::Idle, ..codex.clone() };
        let mut snap = AgentsSnapshot {
            agents: vec![done.clone()],
            errors: vec![(Tool::Claude, "locked".into())],
            ..Default::default()
        };
        carry_over(&prev, &mut snap);
        assert_eq!(snap.agents, [done, waiting.clone()]);
        // the session that was waiting isn't new: no second warning
        assert_eq!(change(Some(State::Waiting), &snap.agents[1]), None);
        // a good reading changes nothing
        let mut good = AgentsSnapshot { agents: vec![waiting.clone()], ..Default::default() };
        carry_over(&prev, &mut good);
        assert_eq!(good.agents, [waiting]);
    }

    #[test]
    fn a_row_with_a_working_subagent_counts_as_working() {
        let parent = Agent { id: "p".into(), ..agent(State::Idle, 1, "") };
        let sub = |id: &str, state: State| Agent { id: id.into(), parent: Some("p".into()), ..agent(state, 1, "") };
        let all = [parent, sub("s1", State::Working), sub("s2", State::Working)];
        let counts = Counts::of(&group(&all));
        assert_eq!(counts, Counts { working: 1, waiting: 0, idle: 0 });
        assert_eq!(counts.mood(), Mood::Happy);
        let waiting = [Agent { state: State::Waiting, ..all[0].clone() }, all[1].clone()];
        assert_eq!(Counts::of(&group(&waiting)), Counts { working: 0, waiting: 1, idle: 0 });
        assert_eq!(Counts::of(&group(&[])).mood(), Mood::Sleep);
    }

    #[test]
    fn steps_from_another_day_have_the_date() {
        use chrono::TimeZone;
        let at = chrono::Local.with_ymd_and_hms(2026, 11, 17, 23, 50, 0).single().unwrap();
        let step = Step { at_ms: at.timestamp_millis(), kind: StepKind::Tool, text: "Edit".into() };
        assert_eq!(step_row(&step, at.date_naive()).time, "23:50");
        assert_eq!(step_row(&step, at.date_naive().succ_opt().unwrap()).time, "11/17 23:50");
        assert_eq!(step_row(&Step { at_ms: 0, ..step }, at.date_naive()).time, "");
    }

    #[test]
    fn each_new_subagent_takes_the_next_hero() {
        let mut r = Roster::default();
        assert_eq!((r.pick("a", 3), r.pick("b", 3), r.pick("c", 3), r.pick("d", 3)), (0, 1, 2, 0));
        // the same subagent keeps its own
        assert_eq!(r.pick("b", 3), 1);
        // who stopped walking leaves; the next new one follows the rotation
        let walking = ["b".to_string()];
        r.keep(walking.iter());
        assert_eq!((r.pick("b", 3), r.pick("e", 3)), (1, 1));
        // fewer heroes than before (the folder changed): stays within the list
        assert!(r.pick("e", 1) == 0);
    }
}
