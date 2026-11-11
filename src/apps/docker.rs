//! Mini-app Docker: lista de containers, eventos (queda, loop, unhealthy, volta) e ações.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::core::bus::Event;
use crate::core::exec::{self, Exec};
use crate::core::manifest::Manifest;
use crate::core::miniapp::{Host, MiniApp};
use crate::mascot::{Mood, render, sprites};
use crate::providers::docker::{
    Container, DockerSnapshot, Stats, format_age, format_bytes, valid_container_name, valid_project_name,
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
    /// Projetos com o grupo recolhido ("" = avulsos).
    collapsed: HashSet<String>,
    mood: Mood,
    rows: Rc<VecModel<ContainerRow>>,
    /// Último quadro da mascote: só troca a imagem quando os pixels mudam.
    mascot: render::Cache,
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
            collapsed: HashSet::new(),
            mood: Mood::Sleep,
            rows,
            mascot: Default::default(),
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
        g.on_stack_action(move |verb, project| {
            if let (Some(a), Some(ui)) = (a.upgrade(), w.upgrade()) {
                a.borrow_mut().stack_action(&verb, &project, &ui);
            }
        });

        let a = Rc::downgrade(app);
        let w = weak_ui.clone();
        g.on_toggle_group(move |project| {
            if let (Some(a), Some(ui)) = (a.upgrade(), w.upgrade()) {
                let mut a = a.borrow_mut();
                let project = project.to_string();
                if !a.collapsed.remove(&project) {
                    a.collapsed.insert(project);
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
        g.on_cycle_sort(move || {
            if let (Some(a), Some(ui)) = (a.upgrade(), w.upgrade()) {
                let a = a.borrow_mut();
                {
                    let mut cfg = a.host.config.borrow_mut();
                    let next = next_sort(&cfg.string(&a.manifest, "sort"));
                    cfg.set(&a.manifest.id, "sort", toml::Value::String(next.into()));
                    cfg.save();
                }
                a.host.settings_dirty.set(true);
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
            "start" | "stop" | "restart" | "pause" | "unpause" => {
                let poker = self.host.bus.poker("docker");
                let weak = ui.as_weak();
                let label = match verb {
                    "start" => "Iniciando",
                    "stop" => "Parando",
                    "pause" => "Pausando",
                    "unpause" => "Retomando",
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

    /// Ação na stack inteira: `docker compose -p <projeto> start|stop|restart`.
    fn stack_action(&mut self, verb: &str, project: &str, ui: &HubWindow) {
        if !valid_project_name(project) {
            return;
        }
        let label = match verb {
            "start" => "Subindo",
            "stop" => "Parando",
            "restart" => "Reiniciando",
            _ => return,
        };
        self.say(format!("{label} stack {project}…"));
        let poker = self.host.bus.poker("docker");
        let weak = ui.as_weak();
        let project2 = project.to_string();
        // start/stop/restart funcionam só com o nome do projeto, sem o arquivo do compose
        self.exec.run_async("docker", &["compose", "-p", project, verb], move |r| {
            poker.poke();
            if let Err(e) = r {
                let msg = format!("Falhou na stack {project2}: {}", e.to_string().lines().next().unwrap_or_default());
                let _ = weak.upgrade_in_event_loop(move |ui| ui.global::<DockerApp>().set_speech(msg.into()));
            }
        });
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

    /// Redesenha a mascote no quadro dado; `true` se a imagem mudou.
    fn refresh_mascot(&mut self, frame: u32) -> bool {
        let key = self.mascot_key();
        let theme = self.host.theme_for(&self.manifest);
        self.mascot.update(sprites::get(&key), theme, self.mood, frame)
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
        g.set_sort(self.host.config.borrow().string(&self.manifest, "sort").into());

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
            // pausado continua visível: senão não dá para retomar com "só ativos"
            .filter(|c| !hide_stopped || c.state == "running" || c.state == "paused" || self.is_problem(c))
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
        let sort = self.host.config.borrow().string(&self.manifest, "sort");
        sort_containers(&mut list, &sort, &self.snap.stats, rank);

        // agrupa por stack; sem nenhum projeto, lista simples (sem cabeçalhos)
        let groups = group_by_project(&self.snap.containers, &list, |c| self.is_problem(c));
        let grouped = groups.iter().any(|g| !g.project.is_empty());
        let mut rows: Vec<ContainerRow> = Vec::with_capacity(list.len() + groups.len());
        for g in &groups {
            if grouped {
                // com busca ativa, mostra tudo: senão o resultado fica escondido no grupo recolhido
                let collapsed = q.is_empty() && self.collapsed.contains(g.project);
                rows.push(header_row(g, collapsed));
                if collapsed {
                    continue;
                }
            }
            rows.extend(g.items.iter().map(|c| self.row(c)));
        }
        ui.global::<DockerApp>().set_grouped(grouped);
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
            paused: c.state == "paused",
            problem,
            state_text: state_text.into(),
            cpu: st.cpu,
            usage_text: if running { format!("{:.0}% · {}", st.cpu, format_bytes(st.mem as f64)).into() } else { "".into() },
            port_text: port_text.into(),
            url: if running { url.into() } else { "".into() },
            age_text: age_text(c, unix_now()).into(),
            restarts: c.restarts.min(i32::MAX as u32) as i32,
            restarts_text: restarts_text(c.restarts).into(),
            header: false,
            collapsed: false,
            active: false,
        }
    }
}

/// Grupo da lista: um projeto do Compose ("" = avulsos) e seus containers visíveis.
#[derive(Debug, PartialEq)]
pub struct Group<'a> {
    pub project: &'a str,
    pub items: Vec<&'a Container>,
    /// Contagens da stack inteira (não só do que passou no filtro).
    pub running: usize,
    /// Rodando, pausados ou reiniciando: o que `compose stop` ainda tem a parar.
    pub active: usize,
    pub total: usize,
    pub problems: usize,
}

/// Modos de ordenação, na ordem do ciclo do botão.
pub const SORTS: [&str; 4] = ["estado", "nome", "cpu", "ram"];

/// Próximo modo do ciclo (desconhecido volta ao começo).
pub fn next_sort(cur: &str) -> &'static str {
    let i = SORTS.iter().position(|s| *s == cur).map_or(0, |i| i + 1);
    SORTS[i % SORTS.len()]
}

/// Ordena os visíveis. "estado" usa `rank` (problema primeiro); cpu/ram do maior
/// para o menor; empate sempre por nome. Os grupos mantêm essa ordem por dentro.
pub fn sort_containers(list: &mut [&Container], mode: &str, stats: &HashMap<String, Stats>, rank: impl Fn(&Container) -> u8) {
    let st = |c: &Container| stats.get(&c.name).copied().unwrap_or_default();
    list.sort_by(|a, b| {
        let by = match mode {
            "nome" => std::cmp::Ordering::Equal,
            "cpu" => st(b).cpu.total_cmp(&st(a).cpu),
            "ram" => st(b).mem.cmp(&st(a).mem),
            _ => rank(a).cmp(&rank(b)),
        };
        by.then_with(|| a.name.cmp(&b.name))
    });
}

/// Agrupa os visíveis (já ordenados) por projeto, mantendo a ordem dentro do grupo.
/// Stacks com problema primeiro, depois por nome; avulsos no fim. Grupo sem visíveis some.
pub fn group_by_project<'a>(
    all: &'a [Container],
    visible: &[&'a Container],
    problem: impl Fn(&Container) -> bool,
) -> Vec<Group<'a>> {
    let mut by: BTreeMap<&str, Vec<&Container>> = BTreeMap::new();
    for c in visible {
        by.entry(c.project.as_str()).or_default().push(c);
    }
    let mut out: Vec<Group> = by
        .into_iter()
        .map(|(project, items)| {
            let members = all.iter().filter(|c| c.project == project);
            let (mut running, mut active, mut total, mut problems) = (0, 0, 0, 0);
            for c in members {
                total += 1;
                running += (c.state == "running") as usize;
                active += matches!(c.state.as_str(), "running" | "paused" | "restarting") as usize;
                problems += problem(c) as usize;
            }
            Group { project, items, running, active, total, problems }
        })
        .collect();
    out.sort_by_key(|g| (g.project.is_empty(), g.problems == 0, g.project));
    out
}

/// "2/3 rodando · 1 com problema".
pub fn group_text(g: &Group) -> String {
    let mut s = format!("{}/{} rodando", g.running, g.total);
    if g.problems > 0 {
        s += &format!(" · {} com problema", g.problems);
    }
    s
}

/// Linha de cabeçalho do grupo (`project` vazio = avulsos, sem ações de stack).
fn header_row(g: &Group, collapsed: bool) -> ContainerRow {
    ContainerRow {
        header: true,
        collapsed,
        name: if g.project.is_empty() { "Avulsos".into() } else { g.project.into() },
        project: g.project.into(),
        running: g.running > 0,
        active: g.active > 0,
        problem: g.problems > 0,
        state_text: group_text(g).into(),
        ..Default::default()
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Há quanto tempo está no estado atual: "há 3 d" (rodando desde / parado desde). "" se não sei.
pub fn age_text(c: &Container, now: i64) -> String {
    let since = match c.state.as_str() {
        "running" => c.started_at,
        "exited" | "dead" => c.finished_at,
        _ => 0,
    };
    if since > 0 { format!("há {}", format_age(now - since)) } else { String::new() }
}

/// 0 -> "", 1 -> "1 reinício", 2 -> "2 reinícios".
pub fn restarts_text(n: u32) -> String {
    match n {
        0 => String::new(),
        1 => "1 reinício".into(),
        n => format!("{n} reinícios"),
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
        ui.global::<DockerApp>().set_theme(render::colors(self.host.theme_for(&self.manifest)));
        self.refresh_mascot(1);
        ui.global::<DockerApp>().set_mascot(self.mascot.image());
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
        if self.refresh_mascot(frame) {
            ui.global::<DockerApp>().set_mascot(self.mascot.image());
        }
    }

    fn settings_changed(&mut self, ui: &HubWindow) {
        ui.global::<DockerApp>().set_theme(render::colors(self.host.theme_for(&self.manifest)));
        self.mood = self.compute_mood();
        self.refresh_mascot(1);
        ui.global::<DockerApp>().set_mascot(self.mascot.image());
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

    fn cp(name: &str, state: &str, project: &str) -> Container {
        Container { project: project.into(), ..c(name, state, 0, "") }
    }

    #[test]
    fn ordena() {
        let (a, b, z) = (c("a", "running", 0, ""), c("b", "exited", 1, ""), c("z", "running", 0, ""));
        let mut stats = HashMap::new();
        stats.insert("a".to_string(), Stats { cpu: 5.0, mem: 300 });
        stats.insert("z".to_string(), Stats { cpu: 50.0, mem: 100 });
        let rank = |c: &Container| if c.state == "exited" { 0 } else { 1 };
        let names = |mode: &str| {
            let mut l = vec![&z, &a, &b];
            sort_containers(&mut l, mode, &stats, rank);
            l.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(",")
        };
        assert_eq!(names("estado"), "b,a,z");
        assert_eq!(names("nome"), "a,b,z");
        assert_eq!(names("cpu"), "z,a,b");
        assert_eq!(names("ram"), "a,z,b");
        assert_eq!(next_sort("estado"), "nome");
        assert_eq!(next_sort("ram"), "estado");
        assert_eq!(next_sort("???"), "estado");
    }

    #[test]
    fn agrupa_por_projeto() {
        let all = vec![
            cp("loja-web", "running", "loja"),
            cp("loja-db", "exited", "loja"),
            cp("api-app", "running", "api"),
            cp("api-cache", "restarting", "api"),
            cp("solto", "running", ""),
            cp("blog-web", "exited", "blog"),
        ];
        let problem = |c: &Container| c.state == "restarting";
        // "esconder parados": blog some inteiro, loja-db some mas conta no total
        let visible: Vec<&Container> = all.iter().filter(|c| c.state != "exited").collect();
        let gs = group_by_project(&all, &visible, problem);
        let names: Vec<(&str, Vec<&str>)> =
            gs.iter().map(|g| (g.project, g.items.iter().map(|c| c.name.as_str()).collect())).collect();
        assert_eq!(names, vec![
            ("api", vec!["api-app", "api-cache"]), // com problema primeiro
            ("loja", vec!["loja-web"]),
            ("", vec!["solto"]),                   // avulsos no fim
        ]);
        assert_eq!((gs[0].running, gs[0].total, gs[0].problems), (1, 2, 1));
        assert_eq!((gs[1].running, gs[1].total, gs[1].problems), (1, 2, 0));
        // reiniciando conta como ativo (dá para parar a stack em crash loop)
        assert_eq!((gs[0].active, gs[1].active), (2, 1));
        assert_eq!(group_text(&gs[0]), "1/2 rodando · 1 com problema");
        assert_eq!(group_text(&gs[1]), "1/2 rodando");

        // ordem dentro do grupo é a da lista recebida
        let visible: Vec<&Container> = vec![&all[1], &all[0]];
        let gs = group_by_project(&all, &visible, problem);
        assert_eq!(gs.len(), 1);
        assert_eq!(gs[0].items[0].name, "loja-db");

        // nada visível: nenhum grupo
        assert!(group_by_project(&all, &[], problem).is_empty());
    }

    #[test]
    fn idade_e_reinicios() {
        let now = 1_000_000;
        let mut r = c("web", "running", 0, "");
        r.started_at = now - 3 * 86400 - 60;
        assert_eq!(age_text(&r, now), "há 3 d");
        let mut x = c("db", "exited", 1, "");
        x.started_at = now - 9000;
        x.finished_at = now - 2 * 3600;
        assert_eq!(age_text(&x, now), "há 2 h");
        // sem inspect, ou estados sem tempo útil
        assert_eq!(age_text(&c("a", "running", 0, ""), now), "");
        let mut p = c("p", "paused", 0, "");
        p.started_at = now - 10;
        assert_eq!(age_text(&p, now), "");
        assert_eq!(restarts_text(0), "");
        assert_eq!(restarts_text(1), "1 reinício");
        assert_eq!(restarts_text(7), "7 reinícios");
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

    #[test]
    fn pausar_e_retomar_nao_gera_evento() {
        // rodando -> pausado e pausado -> rodando: nem queda nem "voltou"
        let prev = HashMap::from([
            ("a".to_string(), p("running", "", false)),
            ("b".to_string(), p("paused", "", false)),
        ]);
        let now = vec![c("a", "paused", 0, ""), c("b", "running", 0, "")];
        let cur = HashMap::from([
            ("a".to_string(), p("paused", "", false)),
            ("b".to_string(), p("running", "", false)),
        ]);
        assert!(detect_events(&prev, &now, &cur).is_empty());
    }
}
