//! Ports mini-app: what is listening on each port, with open-in-browser and end-process.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Model, ModelRc, VecModel};

use crate::core::bus::{Event, Topic};
use crate::core::exec;
use crate::core::os;
use crate::core::manifest::Manifest;
use crate::core::miniapp::{Host, MiniApp};
use super::kit::{HeaderMascot, Speech, plural};
use crate::mascot::anim::Reaction;
use crate::mascot::{Mood, render, sprites};
use crate::providers::ports::{Listener, PortsSnapshot};
use crate::tr;
use crate::ui::{HubWindow, PortRow, PortsApp};

const SPEECH_TTL: Duration = Duration::from_secs(30);
/// Time for the second click to confirm "end".
const ARM_TTL: Duration = Duration::from_secs(4);
/// A port opened less than this ago shows up marked as new.
const FRESH: Duration = Duration::from_secs(60);

pub struct PortsMini {
    host: Rc<Host>,
    manifest: Manifest,
    snap: PortsSnapshot,
    /// Ports already seen (proto, port): whatever isn't here is new.
    seen: HashSet<(String, u16)>,
    /// First reading after enabling: nothing is reported as new.
    baseline: bool,
    fresh: Vec<((String, u16), Instant)>,
    /// Pid with "end" armed, waiting for the second click.
    armed: Option<(u32, Instant)>,
    search: String,
    speech: Speech,
    mood: Mood,
    rows: Rc<VecModel<PortRow>>,
    face: HeaderMascot,
}

impl PortsMini {
    pub fn create(host: Rc<Host>, ui: &HubWindow) -> Rc<RefCell<Self>> {
        let manifest =
            Manifest::parse(include_str!("../../miniapps/ports/manifest.toml")).expect("invalid Ports manifest");
        let rows = Rc::new(VecModel::default());
        ui.global::<PortsApp>().set_rows(ModelRc::from(rows.clone()));
        let app = Rc::new(RefCell::new(PortsMini {
            host,
            manifest,
            snap: PortsSnapshot { ok: true, ..Default::default() },
            seen: HashSet::new(),
            baseline: false,
            fresh: Vec::new(),
            armed: None,
            search: String::new(),
            speech: Speech::new(SPEECH_TTL),
            mood: Mood::Sleep,
            rows,
            face: HeaderMascot::new(11),
        }));
        Self::bind(&app, ui);
        app
    }

    fn bind(app: &Rc<RefCell<Self>>, ui: &HubWindow) {
        let g = ui.global::<PortsApp>();
        let weak_ui = ui.as_weak();

        let a = Rc::downgrade(app);
        let w = weak_ui.clone();
        g.on_mascot_poked(move || {
            if let (Some(a), Some(ui)) = (a.upgrade(), w.upgrade()) {
                let mut a = a.borrow_mut();
                let (mood, reduce) = (a.mood, a.host.config.borrow().reduce_motion);
                if a.face.poke(mood, reduce) {
                    a.push_mascot(&ui);
                }
            }
        });

        let a = Rc::downgrade(app);
        let w = weak_ui.clone();
        g.on_kill(move |pid| {
            if let (Some(a), Some(ui)) = (a.upgrade(), w.upgrade()) {
                a.borrow_mut().kill(pid.max(0) as u32, &ui);
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
        g.on_toggle_udp(move || {
            if let (Some(a), Some(ui)) = (a.upgrade(), w.upgrade()) {
                let a = a.borrow();
                a.host.toggle_setting(&a.manifest, "show_udp");
                a.refresh_rows(&ui);
            }
        });

        g.on_open_url(|url| exec::open_url(&url));

        let a = Rc::downgrade(app);
        g.on_refresh(move || {
            if let Some(a) = a.upgrade() {
                a.borrow().host.bus.poke(Topic::Ports);
            }
        });
    }

    /// The first click arms; the second, within `ARM_TTL`, asks the process to end (`os::terminate`).
    fn kill(&mut self, pid: u32, ui: &HubWindow) {
        // only pids in the current reading (no pid from outside) and never the hub itself
        let Some(l) = self.snap.listeners.iter().find(|l| l.pid == pid && l.mine && pid > 1) else { return };
        let name = l.process.clone();
        if pid == std::process::id() {
            self.speech.say(tr!("That's me! I won't end myself.").into());
        } else if self.armed.is_some_and(|(p, at)| p == pid && at.elapsed() < ARM_TTL) {
            self.armed = None;
            match os::terminate(pid) {
                Ok(()) => {
                    self.speech.say(tr!("Asked {} to end.", name));
                    self.face.react(Reaction::Celebrate);
                }
                Err(os::KillError::Denied) => self.speech.say(tr!("No permission to end {}.", name)),
                Err(os::KillError::Other(e)) => self.speech.say(tr!("Couldn't end {}: {}", name, e)),
            }
            self.host.bus.poke(Topic::Ports);
        } else {
            self.armed = Some((pid, Instant::now()));
            self.speech.say(tr!("Click again to end {} (pid {}).", name, pid));
        }
        self.refresh_rows(ui);
        self.push_mascot(ui);
    }

    fn ingest(&mut self, snap: PortsSnapshot) {
        let now = Instant::now();
        let cur: HashSet<(String, u16)> = snap.listeners.iter().map(|l| (l.proto.clone(), l.port)).collect();
        if self.baseline && snap.ok {
            let (notify, show_udp, hide_others) = {
                let cfg = self.host.config.borrow();
                let m = &self.manifest;
                (cfg.bool(m, "notify_new"), cfg.bool(m, "show_udp"), cfg.bool(m, "hide_others"))
            };
            for l in &snap.listeners {
                let key = (l.proto.clone(), l.port);
                // only what the list shows is announced
                if self.seen.contains(&key) || !shown(l, show_udp, hide_others) {
                    continue;
                }
                self.fresh.push((key, now));
                let who = if l.process.is_empty() { tr!("Some service").to_string() } else { l.process.clone() };
                self.speech.say(tr!("{} opened port {}.", who, l.port));
                if notify {
                    self.host.notifier.borrow_mut().send(
                        &format!("ports:{}:{}", l.proto, l.port),
                        tr!("Port opened"),
                        &tr!("{} is listening on :{} ({})", who, l.port, l.proto),
                        false,
                    );
                }
            }
        }
        if snap.ok {
            self.seen = cur;
            self.baseline = true;
        }
        self.fresh.retain(|(_, at)| at.elapsed() < FRESH);
        self.snap = snap;
    }

    fn compute_mood(&self) -> Mood {
        let s = &self.snap;
        if s.missing {
            return Mood::Sleep;
        }
        if !s.ok {
            return Mood::Busy;
        }
        let warn = self.host.config.borrow().bool(&self.manifest, "warn_exposed");
        if warn && s.listeners.iter().any(|l| l.proto == "tcp" && l.mine && l.exposed()) {
            return Mood::Busy;
        }
        if s.listeners.iter().any(|l| l.mine) { Mood::Idle } else { Mood::Sleep }
    }

    fn visible(&self) -> Vec<&Listener> {
        let cfg = self.host.config.borrow();
        let show_udp = cfg.bool(&self.manifest, "show_udp");
        let hide_others = cfg.bool(&self.manifest, "hide_others");
        let q = self.search.trim().to_lowercase();
        let mut list: Vec<&Listener> = self
            .snap
            .listeners
            .iter()
            .filter(|l| shown(l, show_udp, hide_others))
            .filter(|l| {
                q.is_empty()
                    || l.port.to_string().contains(&q)
                    || l.process.to_lowercase().contains(&q)
                    || l.cmd.to_lowercase().contains(&q)
            })
            .collect();
        // yours first (you can act on them), then by port
        list.sort_by_key(|l| (!l.mine, l.port, l.proto.clone()));
        list
    }

    fn refresh_rows(&self, ui: &HubWindow) {
        let armed = self.armed.filter(|(_, at)| at.elapsed() < ARM_TTL).map(|(p, _)| p);
        let rows: Vec<PortRow> = self
            .visible()
            .into_iter()
            .map(|l| {
                let fresh = self.fresh.iter().any(|((p, n), _)| *p == l.proto && *n == l.port);
                row(l, armed == Some(l.pid) && l.mine, fresh)
            })
            .collect();
        let same = self.rows.row_count() == rows.len()
            && rows.iter().enumerate().all(|(i, r)| self.rows.row_data(i).as_ref() == Some(r));
        if !same {
            self.rows.set_vec(rows);
        }
        self.push_header(ui);
    }

    fn push_header(&self, ui: &HubWindow) {
        let g = ui.global::<PortsApp>();
        let tcp: Vec<&Listener> = self.snap.listeners.iter().filter(|l| l.proto == "tcp").collect();
        let mine = tcp.iter().filter(|l| l.mine).count();
        let exposed = tcp.iter().filter(|l| l.exposed()).count();
        g.set_total(tcp.len() as i32);
        g.set_mine(mine as i32);
        g.set_exposed(exposed as i32);
        g.set_ok(self.snap.ok);
        g.set_missing(self.snap.missing);
        let (title, hint) = if cfg!(windows) {
            (tr!("netstat not found"), tr!("netstat is needed to see the ports."))
        } else {
            (tr!("ss not installed"), tr!("Install iproute2 to see the ports."))
        };
        g.set_missing_title(title.into());
        g.set_missing_hint(hint.into());
        g.set_error_text(if self.snap.missing {
            if cfg!(windows) { tr!("The netstat command was not found.") } else { tr!("The ss command (iproute2) was not found.") }.into()
        } else {
            self.snap.error.as_str().into()
        });
        g.set_mood(self.mood.as_str().into());
        g.set_show_udp(self.host.config.borrow().bool(&self.manifest, "show_udp"));
        let speech = match self.speech.current() {
            Some(t) => t.to_string(),
            _ if tcp.is_empty() => tr!("No open ports… zzz").into(),
            _ if mine == 0 && tcp.len() == 1 => tr!("1 system port listening.").into(),
            _ if mine == 0 => tr!("{} system ports listening.", tcp.len()),
            _ if mine == 1 => tr!("Watching {}, 1 is yours.", plural(tcp.len(), tr!("port"), tr!("ports"))),
            _ => tr!("Watching {}, {} are yours.", plural(tcp.len(), tr!("port"), tr!("ports")), mine),
        };
        g.set_speech(speech.into());
    }

    /// Redraws the mascot; `true` if it changed.
    fn refresh_mascot(&mut self) -> bool {
        let sprite = sprites::get(&self.mascot_key());
        self.face.refresh(sprite, self.host.theme_for(&self.manifest), self.mood)
    }

    fn push_mascot(&mut self, ui: &HubWindow) {
        self.refresh_mascot();
        let g = ui.global::<PortsApp>();
        g.set_mascot(self.face.image());
        g.set_reaction(self.face.reaction());
    }
}

/// How the address shows up: "local only", "whole network" or the specific IP.
pub fn addr_text(l: &Listener) -> String {
    if l.exposed() {
        tr!("whole network").into()
    } else if l.local_only() {
        tr!("local only").into()
    } else {
        l.addrs.join(", ")
    }
}

fn row(l: &Listener, armed: bool, fresh: bool) -> PortRow {
    PortRow {
        port: l.port as i32,
        proto: l.proto.clone().into(),
        process: if l.process.is_empty() { tr!("(another user)").into() } else { l.process.clone().into() },
        cmd: l.cmd.clone().into(),
        pid: l.pid as i32,
        mine: l.mine,
        addr_text: addr_text(l).into(),
        local: l.local_only(),
        exposed: l.exposed(),
        url: l.url().unwrap_or_default().into(),
        armed,
        fresh,
    }
}

impl MiniApp for PortsMini {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn enable(&mut self, ui: &HubWindow) {
        self.baseline = false;
        ui.global::<PortsApp>().set_theme(render::colors(self.host.theme_for(&self.manifest)));
        self.push_mascot(ui);
        self.push_header(ui);
    }

    fn disable(&mut self, _ui: &HubWindow) {
        self.seen.clear();
        self.fresh.clear();
        self.armed = None;
        self.rows.set_vec(Vec::new());
    }

    fn on_event(&mut self, ev: &Event, ui: &HubWindow) {
        let Event::Ports(snap) = ev else { return };
        self.ingest(snap.clone());
        self.mood = self.compute_mood();
        self.refresh_rows(ui);
    }

    fn tick(&mut self, _frame: u32, ui: &HubWindow) {
        // an armed "end" disarms on its own, and the speech expires on time
        let disarmed = self.armed.is_some_and(|(_, at)| at.elapsed() >= ARM_TTL);
        if disarmed {
            self.armed = None;
        }
        if self.speech.take_expired() || disarmed {
            self.refresh_rows(ui);
        }
        if self.refresh_mascot() {
            self.push_mascot(ui);
        }
    }

    fn settings_changed(&mut self, ui: &HubWindow) {
        ui.global::<PortsApp>().set_theme(render::colors(self.host.theme_for(&self.manifest)));
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

    fn summary(&self) -> String {
        if self.snap.missing {
            return if cfg!(windows) { tr!("Ports: netstat not found") } else { tr!("Ports: ss not installed") }.into();
        }
        let tcp: Vec<&Listener> = self.snap.listeners.iter().filter(|l| l.proto == "tcp").collect();
        let mine = tcp.iter().filter(|l| l.mine).count();
        tr!("Open ports: {} · yours: {}", tcp.len(), mine)
    }
}

/// The list filters ("Show UDP", "Hide other users' ports"), also used for the speech and
/// notifications about new ports.
fn shown(l: &Listener, show_udp: bool, hide_others: bool) -> bool {
    (show_udp || l.proto == "tcp") && (!hide_others || l.mine)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters() {
        let tcp = |mine| Listener { proto: "tcp".into(), mine, ..Default::default() };
        let udp = Listener { proto: "udp".into(), mine: true, ..Default::default() };
        assert!(shown(&tcp(false), false, false));
        assert!(!shown(&tcp(false), false, true), "another user's port is hidden");
        assert!(shown(&tcp(true), false, true));
        assert!(!shown(&udp, false, false));
        assert!(shown(&udp, true, true));
    }

    fn l(addrs: &[&str]) -> Listener {
        Listener { proto: "tcp".into(), port: 80, addrs: addrs.iter().map(|a| a.to_string()).collect(), ..Default::default() }
    }

    #[test]
    fn address_text() {
        assert_eq!(addr_text(&l(&["127.0.0.1", "::1"])), "local only");
        assert_eq!(addr_text(&l(&["*", "0.0.0.0"])), "whole network");
        assert_eq!(addr_text(&l(&["100.1.2.3"])), "100.1.2.3");
    }
}
