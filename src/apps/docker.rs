//! Mini-app Docker: lista de containers, eventos (queda, loop, unhealthy, volta) e ações.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::core::bus::Event;
use crate::core::exec::{self, Exec};
use crate::core::manifest::Manifest;
use crate::core::miniapp::{Host, MiniApp};
use crate::mascot::{Mood, render, sprites};
use crate::providers::docker::{
    Container, DockerSnapshot, format_bytes, valid_container_name,
};
use crate::ui::{ContainerRow, DockerApp, HubWindow};

const CRASH_WINDOW: Duration = Duration::from_secs(5 * 60);
const SPEECH_TTL: Duration = Duration::from_secs(45);
const HAPPY_FOR: Duration = Duration::from_secs(6);

#[derive(Clone, PartialEq)]
pub struct Prev {
    pub state: String,
    pub health: String,
    pub problem: bool,
}

#[derive(Debug, PartialEq)]
pub enum DockerEvent {
    Crash { name: String, code: i32 },
    Restarting(String),
    Unhealthy(String),
    Recovered(String),
}

pub struct DockerMini {
    host: Rc<Host>,
    manifest: Manifest,
    exec: Exec,
    snap: DockerSnapshot,
    prev: HashMap<String, Prev>,
    baseline: bool,
    crashes: HashMap<String, Instant>,
    speech: Option<(String, Instant)>,
    happy_until: Option<Instant>,
    search: String,
    mood: Mood,
    rows: Rc<VecModel<ContainerRow>>,
}

impl DockerMini {
    pub fn create(host: Rc<Host>, ui: &HubWindow) -> Rc<RefCell<Self>> {
        let manifest = Manifest::parse(include_str!("../../miniapps/docker/manifest.toml"))
            .expect("manifesto do Docker inválido");
        let rows = Rc::new(VecModel::default());
        ui.global::<DockerApp>().set_rows(ModelRc::from(rows.clone()));
        let app = Rc::new(RefCell::new(DockerMini {
            exec: host.exec_for(&manifest),
            host,
            manifest,
            snap: DockerSnapshot { ok: true, ..Default::default() },
            prev: HashMap::new(),
            baseline: false,
            crashes: HashMap::new(),
            speech: None,
            happy_until: None,
            search: String::new(),
            mood: Mood::Sleep,
            rows,
        }));
        Self::bind(&app, ui);
        app
    }

    fn bind(app: &Rc<RefCell<Self>>, ui: &HubWindow) {
        let g = ui.global::<DockerApp>();
        let weak_ui = ui.as_weak();

        let a = Rc::downgrade(app);
        let w = weak_ui.clone();
        g.on_action(move |verb, name| {
            if let (Some(a), Some(ui)) = (a.upgrade(), w.upgrade()) {
                a.borrow_mut().action(&verb, &name, &ui);
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
        g.on_toggle_hide_stopped(move || {
            if let (Some(a), Some(ui)) = (a.upgrade(), w.upgrade()) {
                let a = a.borrow_mut();
                {
                    let mut cfg = a.host.config.borrow_mut();
                    let v = cfg.bool(&a.manifest, "hide_stopped");
                    cfg.set(&a.manifest.id, "hide_stopped", toml::Value::Boolean(!v));
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
                a.borrow().host.bus.poke("docker");
            }
        });
    }

    fn say(&mut self, text: String) {
        self.speech = Some((text, Instant::now()));
    }

    fn action(&mut self, verb: &str, name: &str, ui: &HubWindow) {
        if !valid_container_name(name) {
            return;
        }
        let result = match verb {
            "start" | "stop" | "restart" => {
                let poker = self.host.bus.poker("docker");
                let weak = ui.as_weak();
                let label = match verb {
                    "start" => "Iniciando",
                    "stop" => "Parando",
                    _ => "Reiniciando",
                };
                self.say(format!("{label} {name}…"));
                let name2 = name.to_string();
                self.exec.run_async("docker", &[verb, name], move |r| {
                    poker.poke();
                    if let Err(e) = r {
                        let msg = format!("Falhou em {name2}: {}", e.to_string().lines().next().unwrap_or_default());
                        let _ = weak.upgrade_in_event_loop(move |ui| ui.global::<DockerApp>().set_speech(msg.into()));
                    }
                });
                Ok(())
            }
            "logs" => self.exec.open_terminal(&format!("{name} logs"), &["docker", "logs", "-f", "--tail", "300", name]),
            "shell" => self.exec.open_terminal(
                name,
                &["docker", "exec", "-it", name, "sh", "-c", "command -v bash >/dev/null && exec bash || exec sh"],
            ),
            _ => Ok(()),
        };
        if let Err(e) = result {
            self.say(e.to_string());
        }
        self.push_header(ui);
    }

    // ---- regras -------------------------------------------------------------

    fn crashed(&self, c: &Container) -> bool {
        self.crashes.get(&c.name).is_some_and(|t| t.elapsed() < CRASH_WINDOW)
    }

    fn is_problem(&self, c: &Container) -> bool {
        c.state == "restarting" || c.state == "dead" || c.health == "unhealthy" || self.crashed(c)
    }

    fn ingest(&mut self, snap: DockerSnapshot) -> Vec<DockerEvent> {
        let now = Instant::now();
        // queda = estava rodando e saiu com código de erro (137/143 = parado de propósito)
        for c in &snap.containers {
            let was_running = self.prev.get(&c.name).is_some_and(|p| p.state == "running");
            if was_running && c.state == "exited" && ![0, 137, 143].contains(&c.exit_code) {
                self.crashes.insert(c.name.clone(), now);
            }
            if c.state == "running" {
                self.crashes.remove(&c.name);
            }
        }
        self.crashes.retain(|_, t| t.elapsed() < CRASH_WINDOW);

        let cur: HashMap<String, Prev> = snap
            .containers
            .iter()
            .map(|c| {
                let p = Prev { state: c.state.clone(), health: c.health.clone(), problem: self.is_problem(c) };
                (c.name.clone(), p)
            })
            .collect();
        let events = if self.baseline { detect_events(&self.prev, &snap.containers, &cur) } else { Vec::new() };
        self.prev = cur;
        self.baseline = true;
        self.snap = snap;
        events
    }

    fn handle_events(&mut self, events: Vec<DockerEvent>) {
        let (notify, notify_recovery) = {
            let cfg = self.host.config.borrow();
            (cfg.bool(&self.manifest, "notify"), cfg.bool(&self.manifest, "notify_recovery"))
        };
        // a mascote comenta o evento mais grave (problemas antes de recuperações)
        let mut said = false;
        for ev in &events {
            let (key, title, body, urgent) = match ev {
                DockerEvent::Crash { name, code } => (
                    format!("crash:{name}"),
                    format!("💥 {name} caiu"),
                    format!("O container saiu com código {code}."),
                    true,
                ),
                DockerEvent::Restarting(n) => (
                    format!("restarting:{n}"),
                    format!("🔁 {n} está em loop de reinício"),
                    "O container não para de reiniciar.".into(),
                    true,
                ),
                DockerEvent::Unhealthy(n) => (
                    format!("unhealthy:{n}"),
                    format!("🩺 {n} está unhealthy"),
                    "O healthcheck do container está falhando.".into(),
                    true,
                ),
                DockerEvent::Recovered(n) => (
                    format!("recovered:{n}"),
                    format!("♥ {n} voltou!"),
                    "O container está rodando normalmente de novo.".into(),
                    false,
                ),
            };
            if !said && (urgent || events.len() == 1) {
                self.say(title.clone());
                said = true;
            }
            if !urgent {
                self.happy_until = Some(Instant::now() + HAPPY_FOR);
            }
            if notify && (urgent || notify_recovery) {
                self.host.notifier.borrow_mut().send(&key, &title, &body, urgent);
            }
        }
    }

    fn compute_mood(&self) -> Mood {
        let s = &self.snap;
        if !s.ok || s.containers.iter().any(|c| self.is_problem(c)) {
            return Mood::Alarm;
        }
        if self.happy_until.is_some_and(|t| Instant::now() < t) {
            return Mood::Happy;
        }
        let busy_cpu = self.host.config.borrow().int(&self.manifest, "busy_cpu") as f32;
        if s.stats.values().any(|st| st.cpu >= busy_cpu) {
            return Mood::Busy;
        }
        if s.containers.iter().any(|c| c.state == "running") { Mood::Idle } else { Mood::Sleep }
    }

    // ---- tela ---------------------------------------------------------------

    fn mascot_image(&self, frame: u32) -> slint::Image {
        render::image(sprites::get(&self.mascot_key()), self.host.theme_for(&self.manifest), self.mood, frame)
    }

    fn counts(&self) -> (i32, i32, i32) {
        let cs = &self.snap.containers;
        let running = cs.iter().filter(|c| c.state == "running").count() as i32;
        let errors = cs.iter().filter(|c| self.is_problem(c)).count() as i32;
        (running, cs.len() as i32 - running, errors)
    }

    fn push_header(&self, ui: &HubWindow) {
        let g = ui.global::<DockerApp>();
        let (running, stopped, errors) = self.counts();
        g.set_running(running);
        g.set_stopped(stopped);
        g.set_errors(errors);
        g.set_ok(self.snap.ok);
        g.set_error_text(self.snap.error.clone().into());
        g.set_mood(self.mood.as_str().into());
        g.set_hide_stopped(self.host.config.borrow().bool(&self.manifest, "hide_stopped"));

        let speech = match &self.speech {
            Some((t, at)) if at.elapsed() < SPEECH_TTL => t.clone(),
            _ if errors > 0 => format!("{errors} container(s) com problema!"),
            _ if running == 0 => "Nenhum container rodando… zzz".into(),
            _ => format!("Tudo certo: {running} rodando."),
        };
        g.set_speech(speech.into());

        let cpu: f32 = self.snap.stats.values().map(|s| s.cpu).sum();
        let mem: u64 = self.snap.stats.values().map(|s| s.mem).sum();
        g.set_usage_text(if running > 0 {
            format!("Docker usando {cpu:.0}% de CPU e {} de RAM", format_bytes(mem as f64)).into()
        } else {
            SharedString::default()
        });
    }

    fn refresh_rows(&self, ui: &HubWindow) {
        let hide_stopped = self.host.config.borrow().bool(&self.manifest, "hide_stopped");
        let q = self.search.trim().to_lowercase();
        let mut list: Vec<&Container> = self
            .snap
            .containers
            .iter()
            .filter(|c| !hide_stopped || c.state == "running" || self.is_problem(c))
            .filter(|c| {
                q.is_empty()
                    || c.name.to_lowercase().contains(&q)
                    || c.image.to_lowercase().contains(&q)
                    || c.project.to_lowercase().contains(&q)
            })
            .collect();
        let rank = |c: &Container| {
            if self.is_problem(c) { 0 } else if c.state == "running" { 1 } else if c.state == "paused" { 2 } else { 3 }
        };
        list.sort_by(|a, b| rank(a).cmp(&rank(b)).then_with(|| a.name.cmp(&b.name)));

        let rows: Vec<ContainerRow> = list.into_iter().map(|c| self.row(c)).collect();
        // só troca o modelo quando algo mudou: evita reconstruir a lista à toa
        let same = self.rows.row_count() == rows.len()
            && rows.iter().enumerate().all(|(i, r)| self.rows.row_data(i).as_ref() == Some(r));
        if !same {
            self.rows.set_vec(rows);
        }
        self.push_header(ui);
    }

    fn row(&self, c: &Container) -> ContainerRow {
        let problem = self.is_problem(c);
        let state_text = if c.health == "unhealthy" {
            "unhealthy".to_string()
        } else if c.state == "restarting" {
            "em loop de reinício".into()
        } else if problem {
            format!("caiu (código {})", c.exit_code)
        } else {
            match c.state.as_str() {
                "running" if c.health == "healthy" => "rodando · saudável".into(),
                "running" => "rodando".into(),
                "exited" => format!("parado ({})", c.exit_code),
                "paused" => "pausado".into(),
                "created" => "criado".into(),
                s => s.to_string(),
            }
        };
        let st = self.snap.stats.get(&c.name).copied().unwrap_or_default();
        let running = c.state == "running";
        let (port_text, url) = match c.ports.first() {
            Some(p) if c.ports.len() > 1 => (format!(":{} +{}", p.port, c.ports.len() - 1), p.url()),
            Some(p) => (format!(":{}", p.port), p.url()),
            None => (String::new(), String::new()),
        };
        ContainerRow {
            name: c.name.clone().into(),
            image: c.image.clone().into(),
            status: c.status.clone().into(),
            project: c.project.clone().into(),
            running,
            problem,
            state_text: state_text.into(),
            cpu: st.cpu,
            usage_text: if running { format!("{:.0}% · {}", st.cpu, format_bytes(st.mem as f64)).into() } else { "".into() },
            port_text: port_text.into(),
            url: if running { url.into() } else { "".into() },
        }
    }
}

/// Compara a leitura anterior com a atual e devolve o que vale avisar.
pub fn detect_events(prev: &HashMap<String, Prev>, now: &[Container], cur: &HashMap<String, Prev>) -> Vec<DockerEvent> {
    let mut out = Vec::new();
    for c in now {
        let (Some(p), Some(n)) = (prev.get(&c.name), cur.get(&c.name)) else { continue };
        if p.state == "running" && c.state == "exited" && ![0, 137, 143].contains(&c.exit_code) {
            out.push(DockerEvent::Crash { name: c.name.clone(), code: c.exit_code });
        } else if p.state != "restarting" && c.state == "restarting" {
            out.push(DockerEvent::Restarting(c.name.clone()));
        } else if p.health != "unhealthy" && c.health == "unhealthy" {
            out.push(DockerEvent::Unhealthy(c.name.clone()));
        } else if p.problem && !n.problem && c.state == "running" {
            out.push(DockerEvent::Recovered(c.name.clone()));
        }
    }
    // problemas antes de recuperações
    out.sort_by_key(|e| matches!(e, DockerEvent::Recovered(_)));
    out
}

impl MiniApp for DockerMini {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn enable(&mut self, ui: &HubWindow) {
        self.baseline = false; // sem histórico: não acusa eventos antigos
        ui.global::<DockerApp>().set_mascot(self.mascot_image(1));
        self.push_header(ui);
    }

    fn disable(&mut self, _ui: &HubWindow) {
        self.prev.clear();
        self.crashes.clear();
        self.rows.set_vec(Vec::new());
    }

    fn on_event(&mut self, ev: &Event, ui: &HubWindow) {
        let Event::Docker(snap) = ev else { return };
        let events = self.ingest(snap.clone());
        self.handle_events(events);
        self.mood = self.compute_mood();
        self.refresh_rows(ui);
    }

    fn tick(&mut self, frame: u32, ui: &HubWindow) {
        // o humor "feliz" e a fala expiram sozinhos
        let mood = self.compute_mood();
        if mood != self.mood {
            self.mood = mood;
            self.push_header(ui);
        }
        ui.global::<DockerApp>().set_mascot(self.mascot_image(frame));
    }

    fn settings_changed(&mut self, ui: &HubWindow) {
        self.mood = self.compute_mood();
        ui.global::<DockerApp>().set_mascot(self.mascot_image(1));
        self.refresh_rows(ui);
    }

    fn mood(&self) -> Mood {
        self.mood
    }

    fn mascot_key(&self) -> String {
        self.host.config.borrow().string(&self.manifest, "mascot")
    }

    fn summary(&self) -> String {
        if !self.snap.ok {
            return "Docker: fora do ar".into();
        }
        let (running, stopped, errors) = self.counts();
        let mut s = format!("Docker: {running}/{} rodando", running + stopped);
        if errors > 0 {
            s += &format!(" · {errors} com problema");
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(name: &str, state: &str, code: i32, health: &str) -> Container {
        Container { name: name.into(), state: state.into(), exit_code: code, health: health.into(), ..Default::default() }
    }
    fn p(state: &str, health: &str, problem: bool) -> Prev {
        Prev { state: state.into(), health: health.into(), problem }
    }

    #[test]
    fn eventos() {
        let prev = HashMap::from([
            ("a".to_string(), p("running", "", false)),
            ("b".to_string(), p("running", "", false)),
            ("c".to_string(), p("running", "healthy", false)),
            ("d".to_string(), p("exited", "", true)),
            ("e".to_string(), p("running", "", false)),
        ]);
        let now = vec![
            c("a", "exited", 1, ""),       // caiu
            c("b", "exited", 137, ""),     // parado de propósito
            c("c", "running", 0, "unhealthy"),
            c("d", "running", 0, ""),      // voltou
            c("e", "restarting", 0, ""),
        ];
        let cur = HashMap::from([
            ("a".to_string(), p("exited", "", true)),
            ("b".to_string(), p("exited", "", false)),
            ("c".to_string(), p("running", "unhealthy", true)),
            ("d".to_string(), p("running", "", false)),
            ("e".to_string(), p("restarting", "", true)),
        ]);
        let ev = detect_events(&prev, &now, &cur);
        assert_eq!(ev, vec![
            DockerEvent::Crash { name: "a".into(), code: 1 },
            DockerEvent::Unhealthy("c".into()),
            DockerEvent::Restarting("e".into()),
            DockerEvent::Recovered("d".into()),
        ]);
    }
}
