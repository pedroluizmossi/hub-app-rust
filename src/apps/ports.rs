//! Mini-app Portas: o que está escutando em cada porta, com abrir no navegador e encerrar.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Model, ModelRc, VecModel};

use crate::core::bus::{Event, Topic};
use crate::core::exec;
use crate::core::manifest::Manifest;
use crate::core::miniapp::{Host, MiniApp};
use crate::mascot::anim::{Animator, Reaction};
use crate::mascot::{Mood, render, sprites};
use crate::providers::ports::{Listener, PortsSnapshot};
use crate::ui::{HubWindow, PortRow, PortsApp};

const SPEECH_TTL: Duration = Duration::from_secs(30);
/// Tempo para o segundo clique confirmar o "encerrar".
const ARM_TTL: Duration = Duration::from_secs(4);
/// Uma porta que abriu há menos que isso aparece marcada como nova.
const FRESH: Duration = Duration::from_secs(60);

pub struct PortsMini {
    host: Rc<Host>,
    manifest: Manifest,
    snap: PortsSnapshot,
    /// Portas já vistas (proto, porta): o que não estiver aqui é novo.
    seen: HashSet<(String, u16)>,
    /// Primeira leitura depois de ligar: não acusa nada como novo.
    baseline: bool,
    fresh: Vec<((String, u16), Instant)>,
    /// Pid com o "encerrar" armado, esperando o segundo clique.
    armed: Option<(u32, Instant)>,
    search: String,
    speech: Option<(String, Instant)>,
    mood: Mood,
    rows: Rc<VecModel<PortRow>>,
    mascot: render::Cache,
    anim: Animator,
    reaction: Option<Reaction>,
}

impl PortsMini {
    pub fn create(host: Rc<Host>, ui: &HubWindow) -> Rc<RefCell<Self>> {
        let manifest =
            Manifest::parse(include_str!("../../miniapps/ports/manifest.toml")).expect("manifesto de Portas inválido");
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
            speech: None,
            mood: Mood::Sleep,
            rows,
            mascot: Default::default(),
            anim: Animator::new(11),
            reaction: None,
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
                if a.host.config.borrow().reduce_motion {
                    return;
                }
                let mood = a.mood;
                if a.anim.poke(mood, Instant::now()) {
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
                {
                    let mut cfg = a.host.config.borrow_mut();
                    let v = cfg.bool(&a.manifest, "show_udp");
                    cfg.set(&a.manifest.id, "show_udp", toml::Value::Boolean(!v));
                    cfg.save();
                }
                a.host.settings_dirty.set(true);
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

    fn say(&mut self, text: String) {
        self.speech = Some((text, Instant::now()));
    }

    /// Primeiro clique arma; o segundo, dentro de `ARM_TTL`, manda SIGTERM.
    fn kill(&mut self, pid: u32, ui: &HubWindow) {
        // só pids que estão na leitura atual (nada de pid vindo de fora) e nunca o próprio hub
        let Some(l) = self.snap.listeners.iter().find(|l| l.pid == pid && pid > 1) else { return };
        let name = l.process.clone();
        if pid == std::process::id() {
            self.say("Esse sou eu! Não vou me encerrar.".into());
        } else if self.armed.is_some_and(|(p, at)| p == pid && at.elapsed() < ARM_TTL) {
            self.armed = None;
            // SAFETY: `kill` só envia um sinal a um processo; o kernel recusa se não for do usuário.
            let r = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
            if r == 0 {
                self.say(format!("Pedi para {name} encerrar."));
                self.anim.trigger(Reaction::Celebrate, Instant::now());
            } else {
                let e = std::io::Error::last_os_error();
                self.say(if e.raw_os_error() == Some(libc::EPERM) {
                    format!("Sem permissão para encerrar {name}.")
                } else {
                    format!("Não consegui encerrar {name}: {e}")
                });
            }
            self.host.bus.poke(Topic::Ports);
        } else {
            self.armed = Some((pid, Instant::now()));
            self.say(format!("Clique de novo para encerrar {name} (pid {pid})."));
        }
        self.refresh_rows(ui);
        self.push_mascot(ui);
    }

    fn ingest(&mut self, snap: PortsSnapshot) {
        let now = Instant::now();
        let cur: HashSet<(String, u16)> = snap.listeners.iter().map(|l| (l.proto.clone(), l.port)).collect();
        if self.baseline && snap.ok {
            let notify = self.host.config.borrow().bool(&self.manifest, "notify_new");
            let show_udp = self.host.config.borrow().bool(&self.manifest, "show_udp");
            for l in &snap.listeners {
                let key = (l.proto.clone(), l.port);
                if self.seen.contains(&key) || (l.proto == "udp" && !show_udp) {
                    continue;
                }
                self.fresh.push((key, now));
                let who = if l.process.is_empty() { "Algum serviço".to_string() } else { l.process.clone() };
                self.say(format!("{who} abriu a porta {}.", l.port));
                if notify {
                    self.host.notifier.borrow_mut().send(
                        &format!("ports:{}:{}", l.proto, l.port),
                        "Porta aberta",
                        &format!("{who} está escutando em :{} ({})", l.port, l.proto),
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
        if warn && s.listeners.iter().any(|l| l.proto == "tcp" && l.pid > 0 && l.exposed()) {
            return Mood::Busy;
        }
        if s.listeners.iter().any(|l| l.pid > 0) { Mood::Idle } else { Mood::Sleep }
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
            .filter(|l| show_udp || l.proto == "tcp")
            .filter(|l| !hide_others || l.pid > 0)
            .filter(|l| {
                q.is_empty()
                    || l.port.to_string().contains(&q)
                    || l.process.to_lowercase().contains(&q)
                    || l.cmd.to_lowercase().contains(&q)
            })
            .collect();
        // os seus primeiro (dá para agir neles), depois por porta
        list.sort_by_key(|l| (l.pid == 0, l.port, l.proto.clone()));
        list
    }

    fn refresh_rows(&self, ui: &HubWindow) {
        let armed = self.armed.filter(|(_, at)| at.elapsed() < ARM_TTL).map(|(p, _)| p);
        let rows: Vec<PortRow> = self
            .visible()
            .into_iter()
            .map(|l| {
                let fresh = self.fresh.iter().any(|((p, n), _)| *p == l.proto && *n == l.port);
                row(l, armed == Some(l.pid) && l.pid > 0, fresh)
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
        let mine = tcp.iter().filter(|l| l.pid > 0).count();
        let exposed = tcp.iter().filter(|l| l.exposed()).count();
        g.set_total(tcp.len() as i32);
        g.set_mine(mine as i32);
        g.set_exposed(exposed as i32);
        g.set_ok(self.snap.ok);
        g.set_missing(self.snap.missing);
        g.set_error_text(if self.snap.missing {
            "O comando ss (iproute2) não foi encontrado.".into()
        } else {
            self.snap.error.as_str().into()
        });
        g.set_mood(self.mood.as_str().into());
        g.set_show_udp(self.host.config.borrow().bool(&self.manifest, "show_udp"));
        let speech = match &self.speech {
            Some((t, at)) if at.elapsed() < SPEECH_TTL => t.clone(),
            _ if tcp.is_empty() => "Nenhuma porta aberta… zzz".into(),
            _ if mine == 0 => format!("{} do sistema escutando.", plural(tcp.len(), "porta", "portas")),
            _ => format!("Vigiando {}, {mine} {}.", plural(tcp.len(), "porta", "portas"), if mine == 1 { "sua" } else { "suas" }),
        };
        g.set_speech(speech.into());
    }

    fn refresh_mascot(&mut self) -> bool {
        let key = self.mascot_key();
        let theme = self.host.theme_for(&self.manifest);
        let look = self.anim.look(self.mood, Instant::now());
        let reaction_changed = std::mem::replace(&mut self.reaction, look.reaction) != look.reaction;
        self.mascot.update_look(sprites::get(&key), theme, &look) || reaction_changed
    }

    fn push_mascot(&mut self, ui: &HubWindow) {
        self.refresh_mascot();
        let g = ui.global::<PortsApp>();
        g.set_mascot(self.mascot.image());
        g.set_reaction(self.reaction.map_or("", Reaction::as_str).into());
    }
}

/// "1 porta", "3 portas".
pub fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// Como o endereço aparece: "só local", "rede toda" ou o IP específico.
pub fn addr_text(l: &Listener) -> String {
    if l.exposed() {
        "rede toda".into()
    } else if l.local_only() {
        "só local".into()
    } else {
        l.addrs.join(", ")
    }
}

fn row(l: &Listener, armed: bool, fresh: bool) -> PortRow {
    PortRow {
        port: l.port as i32,
        proto: l.proto.clone().into(),
        process: if l.process.is_empty() { "(outro usuário)".into() } else { l.process.clone().into() },
        cmd: l.cmd.clone().into(),
        pid: l.pid as i32,
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
        // o "encerrar" armado desarma sozinho
        if self.armed.is_some_and(|(_, at)| at.elapsed() >= ARM_TTL) {
            self.armed = None;
            self.refresh_rows(ui);
        }
        if self.refresh_mascot() {
            let g = ui.global::<PortsApp>();
            g.set_mascot(self.mascot.image());
            g.set_reaction(self.reaction.map_or("", Reaction::as_str).into());
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
            return "Portas: ss não instalado".into();
        }
        let tcp: Vec<&Listener> = self.snap.listeners.iter().filter(|l| l.proto == "tcp").collect();
        let mine = tcp.iter().filter(|l| l.pid > 0).count();
        format!("Portas: {} · {mine} {}", plural(tcp.len(), "aberta", "abertas"), if mine == 1 { "sua" } else { "suas" })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l(addrs: &[&str]) -> Listener {
        Listener { proto: "tcp".into(), port: 80, addrs: addrs.iter().map(|a| a.to_string()).collect(), ..Default::default() }
    }

    #[test]
    fn texto_do_endereco() {
        assert_eq!(addr_text(&l(&["127.0.0.1", "::1"])), "só local");
        assert_eq!(addr_text(&l(&["*", "0.0.0.0"])), "rede toda");
        assert_eq!(addr_text(&l(&["100.1.2.3"])), "100.1.2.3");
    }
}
