//! Kubernetes mini-app: the workloads of the local cluster by namespace, events (a workload
//! fails, restarts, comes back) and actions (stop/start by scaling, restart, logs).

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use super::docker::restarts_text;
use super::kit::{HeaderMascot, Speech, unix_now};
use crate::core::bus::{Event, Topic};
use crate::core::chatter::{Fact, FactKind, Voice};
use crate::core::exec::{self, Exec};
use crate::core::manifest::Manifest;
use crate::core::miniapp::{Host, MiniApp, Report};
use crate::mascot::anim::Reaction;
use crate::mascot::{Mood, render, sprites};
use crate::providers::docker::format_age;
use crate::providers::kube::{Kind, KubeSnapshot, REPLICAS_ANNOTATION, Workload, valid_name};
use crate::tr;
use crate::ui::{HubWindow, KubeApp, KubeRow};

const SPEECH_TTL: Duration = Duration::from_secs(45);
/// Namespaces of the cluster itself, hidden by the `hide_system` option.
const SYSTEM_NAMESPACES: [&str; 3] = ["kube-system", "kube-public", "kube-node-lease"];
/// The second step of "stop" (scaling to 0, after saving the replicas).
const SCALE_TIMEOUT: Duration = Duration::from_secs(60);

/// What a workload looked like at the previous reading.
#[derive(Debug, Clone, PartialEq)]
struct Prev {
    problem: bool,
    restarts: u32,
}

#[derive(Debug, PartialEq)]
enum KubeEvent {
    /// It started failing: (name, what the pod says).
    Failing(String, String),
    /// A container restarted without staying broken: (name, why it went down).
    Restarted(String, String),
    Recovered(String),
}

pub struct KubeMini {
    host: Rc<Host>,
    manifest: Manifest,
    exec: Exec,
    snap: KubeSnapshot,
    prev: HashMap<String, Prev>,
    baseline: bool,
    speech: Speech,
    /// Speech of an action in progress ("Stopping web…"): goes away when the action ends.
    working: Option<String>,
    search: String,
    /// Namespaces whose group is collapsed.
    collapsed: HashSet<String>,
    mood: Mood,
    rows: Rc<VecModel<KubeRow>>,
    face: HeaderMascot,
    /// Facts for the chat between mascots, until the hub collects them.
    facts: Vec<Fact>,
}

impl KubeMini {
    pub fn create(host: Rc<Host>, ui: &HubWindow) -> Rc<RefCell<Self>> {
        let manifest = Manifest::parse(include_str!("../../miniapps/kube/manifest.toml")).expect("invalid Kubernetes manifest");
        let rows = Rc::new(VecModel::default());
        ui.global::<KubeApp>().set_rows(ModelRc::from(rows.clone()));
        let app = Rc::new(RefCell::new(KubeMini {
            exec: host.exec_for(&manifest),
            host,
            manifest,
            snap: KubeSnapshot { ok: true, ..Default::default() },
            prev: HashMap::new(),
            baseline: false,
            speech: Speech::new(SPEECH_TTL),
            working: None,
            search: String::new(),
            collapsed: HashSet::new(),
            mood: Mood::Sleep,
            rows,
            face: HeaderMascot::new(11),
            facts: Vec::new(),
        }));
        Self::bind(&app, ui);
        app
    }

    fn bind(app: &Rc<RefCell<Self>>, ui: &HubWindow) {
        let g = ui.global::<KubeApp>();
        let weak_ui = ui.as_weak();

        let a = Rc::downgrade(app);
        let w = weak_ui.clone();
        g.on_mascot_poked(move || {
            if let (Some(a), Some(ui)) = (a.upgrade(), w.upgrade()) {
                let mut a = a.borrow_mut();
                let (mood, reduce) = (a.mood, a.host.config.borrow().reduce_motion);
                let voice = a.voice();
                a.facts.push(Fact { kind: FactKind::Poked, voice, subject: String::new(), value: None });
                if a.face.poke(mood, reduce) {
                    a.push_mascot(&ui);
                }
            }
        });

        let a = Rc::downgrade(app);
        let w = weak_ui.clone();
        g.on_action(move |verb, key| {
            if let (Some(a), Some(ui)) = (a.upgrade(), w.upgrade()) {
                a.borrow_mut().action(&verb, &key, &ui);
            }
        });

        let a = Rc::downgrade(app);
        let w = weak_ui.clone();
        g.on_toggle_group(move |ns| {
            if let (Some(a), Some(ui)) = (a.upgrade(), w.upgrade()) {
                let mut a = a.borrow_mut();
                let ns = ns.to_string();
                if !a.collapsed.remove(&ns) {
                    a.collapsed.insert(ns);
                }
                a.refresh_rows(&ui);
            }
        });

        let a = Rc::downgrade(app);
        let w = weak_ui.clone();
        g.on_search_edited(move |text| {
            if let (Some(a), Some(ui)) = (a.upgrade(), w.upgrade()) {
                let mut a = a.borrow_mut();
                a.search = text.to_string();
                a.refresh_rows(&ui);
            }
        });

        let a = Rc::downgrade(app);
        let w = weak_ui.clone();
        g.on_toggle_hide_system(move || {
            if let (Some(a), Some(ui)) = (a.upgrade(), w.upgrade()) {
                let a = a.borrow_mut();
                a.host.toggle_setting(&a.manifest, "hide_system");
                a.refresh_rows(&ui);
            }
        });

        let a = Rc::downgrade(app);
        g.on_refresh(move || {
            if let Some(a) = a.upgrade() {
                a.borrow().host.bus.poke(Topic::Kube);
            }
        });
    }

    fn say_working(&mut self, text: String) {
        self.working = Some(text.clone());
        self.speech.say(text);
    }

    fn action(&mut self, verb: &str, key: &str, ui: &HubWindow) {
        let Some(w) = self.snap.workloads.iter().find(|w| w.key() == key).cloned() else { return };
        if !valid_name(&w.namespace) || !valid_name(&w.name) || w.pods.iter().any(|p| !valid_name(&p.name)) {
            return;
        }
        let target = w.kind.resource().map(|r| format!("{r}/{}", w.name));
        let ns = w.namespace.as_str();
        let poker = self.host.bus.poker(Topic::Kube);
        let reporter = self.host.reporter(&self.manifest);
        let prefix = tr!("Failed on {}", w.name);
        let result = match (verb, target) {
            ("start", Some(target)) if w.kind.scalable() => {
                let replicas = format!("--replicas={}", w.saved_replicas.filter(|&n| n > 0).unwrap_or(1));
                self.say_working(tr!("Starting {}…", w.name));
                self.exec.run_async("kubectl", &["scale", "-n", ns, &target, &replicas], move |r| {
                    poker.poke();
                    reporter.result(&prefix, &r);
                });
                Ok(())
            }
            // stopping is scaling to 0; the replicas there were go to an annotation, so
            // starting brings them back
            ("stop", Some(target)) if w.kind.scalable() => {
                self.say_working(tr!("Stopping {}…", w.name));
                let note = format!("{REPLICAS_ANNOTATION}={}", w.desired.max(1));
                let (scale_ns, scale_target) = (ns.to_string(), target.clone());
                self.exec.run_async("kubectl", &["annotate", "--overwrite", "-n", ns, &target, &note], move |r| {
                    let r = r.and_then(|_| {
                        exec::run_timeout("kubectl", &["scale", "-n", &scale_ns, &scale_target, "--replicas=0"], SCALE_TIMEOUT)
                    });
                    poker.poke();
                    reporter.result(&prefix, &r);
                });
                Ok(())
            }
            ("restart", Some(target)) => {
                self.say_working(tr!("Restarting {}…", w.name));
                self.exec.run_async("kubectl", &["rollout", "restart", "-n", ns, &target], move |r| {
                    poker.poke();
                    reporter.result(&prefix, &r);
                });
                Ok(())
            }
            // pods of another controller: deleting them makes it create them again
            ("restart", None) if w.kind == Kind::Owned && !w.pods.is_empty() => {
                self.say_working(tr!("Restarting {}…", w.name));
                let mut args = vec!["delete", "pod", "--wait=false", "-n", ns];
                args.extend(w.pods.iter().map(|p| p.name.as_str()));
                self.exec.run_async("kubectl", &args, move |r| {
                    poker.poke();
                    reporter.result(&prefix, &r);
                });
                Ok(())
            }
            ("logs", _) => match logs_pod(&w) {
                Some(pod) => {
                    let target = format!("pod/{pod}");
                    let title = tr!("{} logs", w.name);
                    let argv = ["kubectl", "logs", "-f", "--tail=300", "--all-containers", "--prefix", "-n", ns, &target];
                    self.exec.open_terminal(&title, &argv)
                }
                None => Ok(()),
            },
            _ => Ok(()),
        };
        if let Err(e) = result {
            self.speech.say(e.to_string());
        }
        self.push_header(ui);
    }

    // ---- rules ---------------------------------------------------------------

    fn ingest(&mut self, snap: KubeSnapshot) -> Vec<KubeEvent> {
        let cur: HashMap<String, Prev> =
            snap.workloads.iter().map(|w| (w.key(), Prev { problem: w.problem(), restarts: w.restarts() })).collect();
        let events = if self.baseline && snap.ok { detect_events(&self.prev, &snap.workloads) } else { Vec::new() };
        // a failed reading (cluster stopped) keeps the comparison base: no "back!" storm later
        if snap.ok {
            self.prev = cur;
            self.baseline = true;
        }
        self.snap = snap;
        events
    }

    fn handle_events(&mut self, events: Vec<KubeEvent>) {
        let (notify, notify_recovery) = {
            let cfg = self.host.config.borrow();
            (cfg.bool(&self.manifest, "notify"), cfg.bool(&self.manifest, "notify_recovery"))
        };
        let mut said = false;
        for ev in &events {
            let (key, title, body, urgent) = match ev {
                KubeEvent::Failing(n, why) => {
                    (format!("kube-failing:{n}"), tr!("💥 {} is failing", n), tr!("The pod says: {}.", why), true)
                }
                KubeEvent::Restarted(n, why) => (
                    format!("kube-restarted:{n}"),
                    tr!("🔁 {} restarted", n),
                    if why.is_empty() { tr!("A container went down and came back.").into() } else { tr!("A container went down ({}) and came back.", why) },
                    true,
                ),
                KubeEvent::Recovered(n) => {
                    (format!("kube-recovered:{n}"), tr!("♥ {} is back!", n), tr!("Every pod is running again.").into(), false)
                }
            };
            let (kind, subject) = match ev {
                KubeEvent::Failing(n, _) | KubeEvent::Restarted(n, _) => (FactKind::Trouble, n),
                KubeEvent::Recovered(n) => (FactKind::Recovered, n),
            };
            let voice = self.voice();
            self.facts.push(Fact { kind, voice, subject: subject.clone(), value: None });
            if !said && (urgent || events.len() == 1) {
                self.speech.say(title.clone());
                said = true;
            }
            if !urgent && !self.host.config.borrow().reduce_motion {
                self.face.react(Reaction::Celebrate);
            }
            if notify && (urgent || notify_recovery) {
                self.host.notifier.borrow_mut().send(&key, &title, &body, urgent);
            }
        }
    }

    fn voice(&self) -> Voice {
        Voice {
            app: self.manifest.id.clone(),
            name: self.manifest.name.clone(),
            sprite: sprites::get(&self.mascot_key()),
            theme: self.host.theme_for(&self.manifest),
            mood: self.mood,
        }
    }

    fn compute_mood(&self) -> Mood {
        let s = &self.snap;
        // a local cluster switched off is normal: sleep, don't alarm
        if s.missing || !s.ok {
            return Mood::Sleep;
        }
        let shown: Vec<&Workload> = s.workloads.iter().filter(|w| self.shown_namespace(&w.namespace)).collect();
        if shown.iter().any(|w| w.problem()) {
            Mood::Alarm
        } else if shown.iter().any(|w| state(w) == "starting") {
            Mood::Busy
        } else if shown.iter().any(|w| state(w) == "running") {
            Mood::Idle
        } else {
            Mood::Sleep
        }
    }

    /// The namespace is not hidden by the `hide_system` option.
    fn shown_namespace(&self, ns: &str) -> bool {
        !SYSTEM_NAMESPACES.contains(&ns) || !self.host.config.borrow().bool(&self.manifest, "hide_system")
    }

    // ---- screen ---------------------------------------------------------------

    fn refresh_mascot(&mut self) -> bool {
        let sprite = sprites::get(&self.mascot_key());
        self.face.refresh(sprite, self.host.theme_for(&self.manifest), self.mood)
    }

    fn push_mascot(&mut self, ui: &HubWindow) {
        self.refresh_mascot();
        let g = ui.global::<KubeApp>();
        g.set_mascot(self.face.image());
        g.set_reaction(self.face.reaction());
    }

    /// (running, stopped, with problems) among the workloads of the shown namespaces.
    fn counts(&self) -> (i32, i32, i32) {
        let shown = self.snap.workloads.iter().filter(|w| self.shown_namespace(&w.namespace));
        let (mut running, mut stopped, mut errors) = (0, 0, 0);
        for w in shown {
            match state(w) {
                "problem" => errors += 1,
                "stopped" => stopped += 1,
                _ => running += 1,
            }
        }
        (running, stopped, errors)
    }

    fn push_header(&self, ui: &HubWindow) {
        let g = ui.global::<KubeApp>();
        let (running, stopped, errors) = self.counts();
        g.set_running(running);
        g.set_stopped(stopped);
        g.set_errors(errors);
        g.set_ok(self.snap.ok);
        g.set_missing(self.snap.missing);
        g.set_context(self.snap.context.as_str().into());
        g.set_error_text(if self.snap.missing {
            tr!("The kubectl command was not found in PATH.").into()
        } else {
            SharedString::from(self.snap.error.as_str())
        });
        g.set_mood(self.mood.as_str().into());
        g.set_hide_system(self.host.config.borrow().bool(&self.manifest, "hide_system"));
        let speech = match self.speech.current() {
            Some(t) => t.to_string(),
            _ if errors > 0 => tr!("{} workload(s) with problems!", errors),
            _ if running == 0 => tr!("Nothing running in the cluster… zzz").into(),
            _ => tr!("All good: {} running.", running),
        };
        g.set_speech(speech.into());
        g.set_voice(Voice::id_of(&self.manifest.id, &self.manifest.name).into());
    }

    fn refresh_rows(&self, ui: &HubWindow) {
        let q = self.search.trim().to_lowercase();
        // a search looks in the hidden namespaces too
        let mut list: Vec<&Workload> = self
            .snap
            .workloads
            .iter()
            .filter(|w| !q.is_empty() || self.shown_namespace(&w.namespace))
            .filter(|w| {
                q.is_empty()
                    || w.name.to_lowercase().contains(&q)
                    || w.namespace.to_lowercase().contains(&q)
                    || w.image.to_lowercase().contains(&q)
            })
            .collect();
        list.sort_by(|a, b| rank(a).cmp(&rank(b)).then_with(|| a.name.cmp(&b.name)));
        let now = unix_now();
        let mut rows = Vec::with_capacity(list.len() + 4);
        for (ns, items) in group_by_namespace(&list) {
            let collapsed = q.is_empty() && self.collapsed.contains(ns);
            rows.push(header_row(ns, &items, collapsed));
            if !collapsed {
                rows.extend(items.iter().map(|w| row(w, now)));
            }
        }
        let same = self.rows.row_count() == rows.len()
            && rows.iter().enumerate().all(|(i, r)| self.rows.row_data(i).as_ref() == Some(r));
        if !same {
            self.rows.set_vec(rows);
        }
        self.push_header(ui);
    }
}

/// "running", "starting", "problem" or "stopped".
fn state(w: &Workload) -> &'static str {
    if w.problem() {
        "problem"
    } else if w.stopped() {
        "stopped"
    } else if w.ready < w.desired || w.pods.iter().any(|p| p.status != "Running") {
        "starting"
    } else {
        "running"
    }
}

/// Order in the list: problems, starting, running, stopped.
fn rank(w: &Workload) -> u8 {
    match state(w) {
        "problem" => 0,
        "starting" => 1,
        "running" => 2,
        _ => 3,
    }
}

/// The pod whose logs to open: a broken one (that is what you want to read), else the first.
fn logs_pod(w: &Workload) -> Option<&str> {
    w.pods.iter().find(|p| p.problem).or(w.pods.first()).map(|p| p.name.as_str())
}

/// Short type name, as kubectl abbreviates it; another controller's pods carry its kind.
fn kind_text(w: &Workload) -> String {
    match w.kind {
        Kind::Deployment => "deploy".into(),
        Kind::StatefulSet => "sts".into(),
        Kind::DaemonSet => "ds".into(),
        Kind::Owned => w.owner_kind.clone(),
        Kind::Pod => "pod".into(),
    }
}

/// What the row says about the state: the broken pod's reason, why it isn't ready yet, or
/// running/stopped.
fn state_text(w: &Workload) -> String {
    match state(w) {
        "problem" => {
            let p = w.pods.iter().find(|p| p.problem);
            let status = p.map_or("", |p| p.status.as_str());
            match p.map(|p| p.last_reason.as_str()) {
                // CrashLoopBackOff says it keeps falling; the last exit says why
                Some(why) if !why.is_empty() && why != status => format!("{status} ({why})"),
                _ => status.to_string(),
            }
        }
        "stopped" => tr!("stopped").into(),
        "starting" => match w.pods.iter().find(|p| p.status != "Running" || !p.ready) {
            Some(p) if p.status != "Running" => tr!("starting: {}", p.status),
            _ => tr!("starting").into(),
        },
        _ => tr!("running").into(),
    }
}

fn row(w: &Workload, now: i64) -> KubeRow {
    KubeRow {
        key: w.key().into(),
        name: w.name.as_str().into(),
        kind: kind_text(w).into(),
        state: state(w).into(),
        state_text: state_text(w).into(),
        ready_text: if w.stopped() { SharedString::default() } else { format!("{}/{}", w.ready, w.desired).into() },
        age_text: if w.created_at > 0 { tr!("age {}", format_age(now - w.created_at)).into() } else { SharedString::default() },
        restarts: w.restarts().min(i32::MAX as u32) as i32,
        restarts_text: restarts_text(w.restarts()).into(),
        image: w.image.as_str().into(),
        can_scale: w.kind.scalable(),
        can_restart: w.kind.resource().is_some() || (w.kind == Kind::Owned && !w.pods.is_empty()),
        can_logs: !w.pods.is_empty(),
        header: false,
        collapsed: false,
    }
}

/// The visible workloads (already sorted) by namespace, keeping the order inside: namespaces
/// with problems first, then by name.
fn group_by_namespace<'a>(list: &[&'a Workload]) -> Vec<(&'a str, Vec<&'a Workload>)> {
    let mut by: BTreeMap<&str, Vec<&Workload>> = BTreeMap::new();
    for w in list {
        by.entry(w.namespace.as_str()).or_default().push(w);
    }
    let mut out: Vec<(&str, Vec<&Workload>)> = by.into_iter().collect();
    out.sort_by_key(|(ns, items)| (!items.iter().any(|w| w.problem()), *ns));
    out
}

/// Namespace header: "2/3 running · 1 with problems".
fn header_row(ns: &str, items: &[&Workload], collapsed: bool) -> KubeRow {
    let total = items.len();
    let running = items.iter().filter(|w| matches!(state(w), "running" | "starting")).count();
    let problems = items.iter().filter(|w| w.problem()).count();
    let mut text = tr!("{}/{} running", running, total);
    if problems > 0 {
        text += " · ";
        text += &tr!("{} with problems", problems);
    }
    KubeRow {
        header: true,
        collapsed,
        key: ns.into(),
        name: ns.into(),
        state: if problems > 0 { "problem" } else if running > 0 { "running" } else { "stopped" }.into(),
        state_text: text.into(),
        ..Default::default()
    }
}

/// Compares the previous reading with the current one: what is worth a warning.
fn detect_events(prev: &HashMap<String, Prev>, now: &[Workload]) -> Vec<KubeEvent> {
    let mut out = Vec::new();
    for w in now {
        let Some(p) = prev.get(&w.key()) else { continue };
        let problem = w.problem();
        if problem && !p.problem {
            let why = w.pods.iter().find(|p| p.problem).map(|p| p.status.clone()).unwrap_or_default();
            out.push(KubeEvent::Failing(w.name.clone(), why));
        } else if !problem && w.restarts() > p.restarts {
            let why = w.pods.iter().map(|p| p.last_reason.as_str()).find(|r| !r.is_empty()).unwrap_or_default();
            out.push(KubeEvent::Restarted(w.name.clone(), why.to_string()));
        } else if p.problem && !problem && state(w) == "running" {
            out.push(KubeEvent::Recovered(w.name.clone()));
        }
    }
    // problems before recoveries
    out.sort_by_key(|e| matches!(e, KubeEvent::Recovered(_)));
    out
}

impl MiniApp for KubeMini {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// The window opened: read again, so the screen doesn't start out stale.
    fn window_shown(&mut self, _ui: &HubWindow) {
        self.host.bus.poke(Topic::Kube);
    }

    fn enable(&mut self, ui: &HubWindow) {
        self.baseline = false;
        ui.global::<KubeApp>().set_theme(render::colors(self.host.theme_for(&self.manifest)));
        self.push_mascot(ui);
        self.push_header(ui);
    }

    fn disable(&mut self, _ui: &HubWindow) {
        self.prev.clear();
        self.rows.set_vec(Vec::new());
    }

    fn on_event(&mut self, ev: &Event, ui: &HubWindow) {
        let Event::Kube(snap) = ev else { return };
        let events = self.ingest(snap.clone());
        self.handle_events(events);
        self.mood = self.compute_mood();
        self.refresh_rows(ui);
    }

    fn on_report(&mut self, report: &Report, ui: &HubWindow) {
        let working = self.working.take();
        match report {
            Report::Error(msg) => self.speech.say(msg.clone()),
            Report::Done | Report::Output(_) => {
                if working.is_some_and(|w| self.speech.is(&w)) {
                    self.speech.clear();
                }
            }
        }
        self.push_header(ui);
    }

    fn tick(&mut self, _frame: u32, ui: &HubWindow) {
        if self.speech.take_expired() {
            self.push_header(ui);
        }
        if self.refresh_mascot() {
            self.push_mascot(ui);
        }
    }

    fn settings_changed(&mut self, ui: &HubWindow) {
        ui.global::<KubeApp>().set_theme(render::colors(self.host.theme_for(&self.manifest)));
        self.mood = self.compute_mood();
        self.push_mascot(ui);
        self.refresh_rows(ui);
    }

    fn mood(&self) -> Mood {
        self.mood
    }

    fn mascot_key(&self) -> String {
        self.host.config.borrow().string(&self.manifest, "mascot")
    }

    fn take_facts(&mut self) -> Vec<Fact> {
        std::mem::take(&mut self.facts)
    }

    fn voices(&self) -> Vec<Voice> {
        vec![self.voice()]
    }

    fn summary(&self) -> String {
        if self.snap.missing {
            return tr!("Kubernetes: kubectl not installed").into();
        }
        if !self.snap.ok {
            return tr!("Kubernetes: cluster offline").into();
        }
        let (running, stopped, errors) = self.counts();
        let mut s = tr!("Kubernetes: {}/{} running", running, running + stopped + errors);
        if errors > 0 {
            s += " · ";
            s += &tr!("{} with problems", errors);
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::kube::Pod;

    fn pod(name: &str, status: &str, problem: bool, restarts: u32) -> Pod {
        Pod { name: name.into(), status: status.into(), problem, ready: status == "Running", restarts, ..Default::default() }
    }

    fn deploy(name: &str, desired: u32, pods: Vec<Pod>) -> Workload {
        let ready = pods.iter().filter(|p| p.ready).count() as u32;
        Workload { namespace: "app".into(), name: name.into(), desired, ready, pods, ..Default::default() }
    }

    #[test]
    fn state_in_words() {
        let ok = deploy("web", 1, vec![pod("web-a", "Running", false, 0)]);
        assert_eq!((state(&ok), state_text(&ok).as_str()), ("running", "running"));
        let crash = deploy("api", 1, vec![Pod { last_reason: "OOMKilled".into(), ..pod("api-a", "CrashLoopBackOff", true, 3) }]);
        assert_eq!((state(&crash), state_text(&crash).as_str()), ("problem", "CrashLoopBackOff (OOMKilled)"));
        let boot = deploy("db", 1, vec![pod("db-a", "ContainerCreating", false, 0)]);
        assert_eq!((state(&boot), state_text(&boot).as_str()), ("starting", "starting: ContainerCreating"));
        let off = deploy("idle", 0, vec![]);
        assert_eq!((state(&off), state_text(&off).as_str(), row(&off, 0).ready_text.as_str()), ("stopped", "stopped", ""));
        // a DaemonSet with no pods isn't "stopped": it can't be scaled
        let ds = Workload { kind: Kind::DaemonSet, ..deploy("proxy", 0, vec![]) };
        assert_eq!(state(&ds), "running");
    }

    #[test]
    fn actions_each_kind_allows() {
        let r = row(&deploy("web", 1, vec![pod("web-a", "Running", false, 0)]), 0);
        assert!(r.can_scale && r.can_restart && r.can_logs);
        let ds = row(&Workload { kind: Kind::DaemonSet, ..deploy("proxy", 1, vec![pod("p", "Running", false, 0)]) }, 0);
        assert!(!ds.can_scale && ds.can_restart);
        let cnpg = Workload { kind: Kind::Owned, owner_kind: "Cluster".into(), ..deploy("db", 1, vec![pod("db-1", "Running", false, 0)]) };
        let r = row(&cnpg, 0);
        assert_eq!((r.kind.as_str(), r.can_scale, r.can_restart), ("Cluster", false, true));
        let loose = row(&Workload { kind: Kind::Pod, ..deploy("debug", 1, vec![pod("debug", "Running", false, 0)]) }, 0);
        assert!(!loose.can_scale && !loose.can_restart && loose.can_logs);
        // logs: the broken pod first
        let w = deploy("web", 2, vec![pod("web-a", "Running", false, 0), pod("web-b", "Error", true, 1)]);
        assert_eq!(logs_pod(&w), Some("web-b"));
    }

    #[test]
    fn events_between_readings() {
        let key = |w: &Workload| w.key();
        let ok = deploy("web", 1, vec![pod("web-a", "Running", false, 0)]);
        let crash = deploy("web", 1, vec![pod("web-a", "CrashLoopBackOff", true, 1)]);
        let prev = |w: &Workload| HashMap::from([(key(w), Prev { problem: w.problem(), restarts: w.restarts() })]);
        assert_eq!(detect_events(&prev(&ok), std::slice::from_ref(&crash)), [KubeEvent::Failing("web".into(), "CrashLoopBackOff".into())]);
        assert_eq!(detect_events(&prev(&crash), std::slice::from_ref(&ok)), [KubeEvent::Recovered("web".into())]);
        // went down and came back between two readings
        let bounced = deploy("web", 1, vec![Pod { last_reason: "Error".into(), ..pod("web-a", "Running", false, 2) }]);
        assert_eq!(detect_events(&prev(&ok), &[bounced]), [KubeEvent::Restarted("web".into(), "Error".into())]);
        assert!(detect_events(&prev(&ok), std::slice::from_ref(&ok)).is_empty());
        // something new is not an event
        assert!(detect_events(&HashMap::new(), &[crash]).is_empty());
    }

    #[test]
    fn namespaces_with_problems_first() {
        let a = Workload { namespace: "a".into(), ..deploy("x", 1, vec![pod("x", "Running", false, 0)]) };
        let z = Workload { namespace: "z".into(), ..deploy("y", 1, vec![pod("y", "Error", true, 0)]) };
        let groups = group_by_namespace(&[&a, &z]);
        assert_eq!(groups.iter().map(|(ns, _)| *ns).collect::<Vec<_>>(), ["z", "a"]);
        let h = header_row("z", &groups[0].1, false);
        assert_eq!((h.state.as_str(), h.state_text.as_str()), ("problem", "0/1 running · 1 with problems"));
    }
}
