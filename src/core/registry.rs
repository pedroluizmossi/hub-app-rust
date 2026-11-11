//! Registro dos mini-apps: liga, desliga e entrega leituras dos provedores.

use std::cell::RefCell;
use std::rc::Rc;

use super::bus::Event;
use super::chatter::{Fact, Voice};
use super::miniapp::{AppRef, Host, Report};
use crate::mascot::Mood;
use crate::ui::HubWindow;

pub struct Registry {
    host: Rc<Host>,
    /// Apps conhecidos. Plugins entram e saem com o hub rodando (ver `add`/`remove`).
    apps: RefCell<Vec<AppRef>>,
}

impl Registry {
    pub fn new(host: Rc<Host>) -> Self {
        Registry { host, apps: RefCell::new(Vec::new()) }
    }

    /// Acrescenta um app (ou troca o de mesmo id). Se ele já estava ligado na config
    /// (um plugin reinstalado), é ligado agora.
    pub fn add(&self, app: AppRef, ui: Option<&HubWindow>) {
        let id = app.borrow().manifest().id.clone();
        self.remove(&id, ui);
        self.apps.borrow_mut().push(app.clone());
        if let Some(ui) = ui
            && self.is_enabled(&id) {
                self.activate(&app, ui);
            }
    }

    /// Tira um app (plugin removido ou atualizado): desliga antes, se estava ligado. A
    /// config continua lembrando dele (posição, opções) para quando voltar.
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

    /// Apps ligados, na ordem das abas (a ordem da config).
    pub fn enabled(&self) -> Vec<AppRef> {
        let cfg = self.host.config.borrow();
        cfg.enabled.iter().filter_map(|id| self.find(id)).collect()
    }

    fn is_enabled(&self, id: &str) -> bool {
        self.host.config.borrow().is_enabled(id)
    }

    /// Liga os apps marcados na config (na inicialização).
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

    /// Entrega cada leitura só aos apps ligados que assinam aquele provedor.
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

    /// Fatos novos de todos os apps ligados, para a conversa entre mascotes.
    pub fn take_facts(&self) -> Vec<Fact> {
        self.enabled().iter().flat_map(|app| app.borrow_mut().take_facts()).collect()
    }

    /// Falas próprias de cada app (id, conteúdo do `lines.toml`), ligado ou não.
    pub fn lines(&self) -> Vec<(String, &'static str)> {
        self.apps.borrow().iter().map(|a| {
            let a = a.borrow();
            (a.manifest().id.clone(), a.lines())
        }).collect()
    }

    /// Mascotes que podem falar, dos apps `ids` (os da página aberta).
    pub fn voices(&self, ids: &[String]) -> Vec<Voice> {
        ids.iter().filter_map(|id| self.find(id)).flat_map(|app| app.borrow().voices()).collect()
    }

    /// Entrega o resultado das ações em segundo plano ao app que as pediu (se ainda ligado).
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

    /// O app em pior estado entre `apps` (o primeiro, em caso de empate).
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

    /// O pior humor entre `apps`.
    pub fn mood_of(apps: &[AppRef]) -> Mood {
        apps.iter().map(|a| a.borrow().mood()).fold(Mood::Sleep, Mood::worst)
    }

    /// Uma linha por app, para o tooltip da bandeja.
    pub fn summary_of(apps: &[AppRef]) -> String {
        let lines: Vec<String> = apps.iter().map(|a| a.borrow().summary()).collect();
        if lines.is_empty() { "Nenhum mini-app ligado".into() } else { lines.join("\n") }
    }
}
