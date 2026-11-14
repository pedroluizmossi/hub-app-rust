//! Registry of the mini-apps: enables, disables and delivers provider readings to them.

use std::cell::RefCell;
use std::rc::Rc;

use super::bus::Event;
use super::chatter::{Fact, Voice};
use super::miniapp::{AppRef, Host, Report};
use crate::mascot::Mood;
use crate::ui::HubWindow;

pub struct Registry {
    host: Rc<Host>,
    /// Known apps. Plugins come and go while the hub runs (see `add`/`remove`).
    apps: RefCell<Vec<AppRef>>,
}

impl Registry {
    pub fn new(host: Rc<Host>) -> Self {
        Registry { host, apps: RefCell::new(Vec::new()) }
    }

    /// Adds an app (or replaces the one with the same id). If it was already enabled in the
    /// config (a reinstalled plugin), it is enabled now.
    pub fn add(&self, app: AppRef, ui: Option<&HubWindow>) {
        let id = app.borrow().manifest().id.clone();
        self.remove(&id, ui);
        self.apps.borrow_mut().push(app.clone());
        if let Some(ui) = ui
            && self.is_enabled(&id) {
                self.activate(&app, ui);
            }
    }

    /// Takes an app out (plugin removed or updated): disables it first if it was enabled.
    /// The config still remembers it (position, options) for when it comes back.
    pub fn remove(&self, id: &str, ui: Option<&HubWindow>) -> bool {
        let Some(app) = self.find(id) else { return false };
        if let Some(ui) = ui
            && self.is_enabled(id) {
                self.deactivate(&app, ui);
            }
        self.apps.borrow_mut().retain(|a| !Rc::ptr_eq(a, &app));
        true
    }

    pub fn all(&self) -> Vec<AppRef> {
        self.apps.borrow().clone()
    }

    pub fn find(&self, id: &str) -> Option<AppRef> {
        self.apps.borrow().iter().find(|a| a.borrow().manifest().id == id).cloned()
    }

    /// Enabled apps, in tab order (the config's order).
    pub fn enabled(&self) -> Vec<AppRef> {
        let cfg = self.host.config.borrow();
        cfg.enabled.iter().filter_map(|id| self.find(id)).collect()
    }

    fn is_enabled(&self, id: &str) -> bool {
        self.host.config.borrow().is_enabled(id)
    }

    /// Enables the apps marked in the config (at startup).
    pub fn start(&self, ui: &HubWindow) {
        for app in self.enabled() {
            self.activate(&app, ui);
        }
    }

    fn activate(&self, app: &AppRef, ui: &HubWindow) {
        let mut a = app.borrow_mut();
        for p in &a.manifest().uses {
            self.host.bus.subscribe(*p);
        }
        a.enable(ui);
    }

    fn deactivate(&self, app: &AppRef, ui: &HubWindow) {
        let mut a = app.borrow_mut();
        for p in &a.manifest().uses {
            self.host.bus.unsubscribe(*p);
        }
        a.disable(ui);
    }

    pub fn set_enabled(&self, id: &str, on: bool, ui: &HubWindow) {
        let Some(app) = self.find(id) else { return };
        if self.is_enabled(id) == on {
            return;
        }
        {
            let mut cfg = self.host.config.borrow_mut();
            if on {
                cfg.enabled.push(id.to_string());
            } else {
                cfg.enabled.retain(|e| e != id);
            }
            cfg.save();
        }
        if on {
            self.activate(&app, ui);
        } else {
            self.deactivate(&app, ui);
        }
    }

    /// Delivers each reading only to the enabled apps subscribed to that provider.
    pub fn dispatch(&self, events: &[Event], ui: &HubWindow) {
        for ev in events {
            let topic = ev.topic();
            for app in self.enabled() {
                let uses = app.borrow().manifest().uses.contains(&topic);
                if uses {
                    app.borrow_mut().on_event(ev, ui);
                }
            }
        }
    }

    /// New facts from every enabled app, for the mascot chatter.
    pub fn take_facts(&self) -> Vec<Fact> {
        self.enabled().iter().flat_map(|app| app.borrow_mut().take_facts()).collect()
    }

    /// Each app's own lines (id, content of `lines.toml`), enabled or not.
    pub fn lines(&self) -> Vec<(String, &'static str)> {
        self.apps.borrow().iter().map(|a| {
            let a = a.borrow();
            (a.manifest().id.clone(), a.lines())
        }).collect()
    }

    /// Mascots that may speak, from apps `ids` (those of the open page).
    pub fn voices(&self, ids: &[String]) -> Vec<Voice> {
        ids.iter().filter_map(|id| self.find(id)).flat_map(|app| app.borrow().voices()).collect()
    }

    /// Delivers the results of background actions to the app that asked (if still enabled).
    pub fn deliver(&self, reports: &[(String, Report)], ui: &HubWindow) {
        for (id, report) in reports {
            if !self.is_enabled(id) {
                continue;
            }
            if let Some(app) = self.find(id) {
                app.borrow_mut().on_report(report, ui);
            }
        }
    }

    /// The app in the worst state among `apps` (the first one on a tie).
    pub fn worst_of(apps: &[AppRef]) -> Option<AppRef> {
        let mut best: Option<(Mood, &AppRef)> = None;
        for app in apps {
            let m = app.borrow().mood();
            if best.as_ref().is_none_or(|(b, _)| m > *b) {
                best = Some((m, app));
            }
        }
        best.map(|(_, a)| a.clone())
    }

    /// The worst mood among `apps`.
    pub fn mood_of(apps: &[AppRef]) -> Mood {
        apps.iter().map(|a| a.borrow().mood()).fold(Mood::Sleep, Mood::worst)
    }

    /// One line per app, for the tray tooltip.
    pub fn summary_of(apps: &[AppRef]) -> String {
        let lines: Vec<String> = apps.iter().map(|a| a.borrow().summary()).collect();
        if lines.is_empty() { crate::tr!("No mini-apps enabled").into() } else { lines.join("\n") }
    }
}
