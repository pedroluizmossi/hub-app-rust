//! Mini-app Docker: lista de containers, eventos (queda, loop, unhealthy, volta) e ações.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::core::bus::{Event, Topic};
use crate::core::chatter::{Fact, FactKind, Voice};
use crate::core::exec::{self, Exec};
use crate::core::manifest::Manifest;
use crate::core::miniapp::{Host, MiniApp, Report};
use super::kit::{HeaderMascot, Speech, unix_now};
use crate::mascot::anim::Reaction;
use crate::mascot::{Mood, render, sprites};
use crate::providers::docker::{
    Container, DockerSnapshot, Stats, format_age, format_bytes, valid_container_name, valid_project_name,
};
use crate::ui::{ContainerRow, DockerApp, HubWindow};

const CRASH_WINDOW: Duration = Duration::from_secs(5 * 60);
const SPEECH_TTL: Duration = Duration::from_secs(45);

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
    speech: Speech,
    /// Fala de uma ação em curso ("Parando web…"): sai quando a ação termina.
    working: Option<String>,
    search: String,
    /// Projetos com o grupo recolhido ("" = avulsos).
    collapsed: HashSet<String>,
    mood: Mood,
    rows: Rc<VecModel<ContainerRow>>,
    /// A mascote do cabeçalho (comemora quando um container volta).
    face: HeaderMascot,
    /// Fatos para a conversa entre mascotes, até o hub recolher.
    facts: Vec<Fact>,
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
            speech: Speech::new(SPEECH_TTL),
            working: None,
            search: String::new(),
            collapsed: HashSet::new(),
            mood: Mood::Sleep,
            rows,
            face: HeaderMascot::new(7),
            facts: Vec::new(),
        }));
        Self::bind(&app, ui);
        app
    }

    fn bind(app: &Rc<RefCell<Self>>, ui: &HubWindow) {
        let g = ui.global::<DockerApp>();
        let weak_ui = ui.as_weak();

        // easter egg: cutucar a mascote algumas vezes
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
                let next = next_sort(&a.host.config.borrow().string(&a.manifest, "sort"));
                a.host.set_setting(&a.manifest, "sort", toml::Value::String(next.into()));
                a.refresh_rows(&ui);
            }
        });

        let a = Rc::downgrade(app);
        let w = weak_ui.clone();
        g.on_toggle_hide_stopped(move || {
            if let (Some(a), Some(ui)) = (a.upgrade(), w.upgrade()) {
                let a = a.borrow_mut();
                a.host.toggle_setting(&a.manifest, "hide_stopped");
                a.refresh_rows(&ui);
            }
        });

        g.on_open_url(|url| exec::open_url(&url));

        let a = Rc::downgrade(app);
        g.on_refresh(move || {
            if let Some(a) = a.upgrade() {
                a.borrow().host.bus.poke(Topic::Docker);
            }
        });
    }

    /// Fala enquanto uma ação roda em segundo plano; o resultado chega por `on_report`.
    fn say_working(&mut self, text: String) {
        self.working = Some(text.clone());
        self.speech.say(text);
    }

    fn action(&mut self, verb: &str, name: &str, ui: &HubWindow) {
        if !valid_container_name(name) {
            return;
        }
        let result = match verb {
            "start" | "stop" | "restart" | "pause" | "unpause" => {
                let poker = self.host.bus.poker(Topic::Docker);
                let reporter = self.host.reporter(&self.manifest);
                let label = match verb {
                    "start" => "Iniciando",
                    "stop" => "Parando",
                    "pause" => "Pausando",
                    "unpause" => "Retomando",
                    _ => "Reiniciando",
                };
                self.say_working(format!("{label} {name}…"));
                let prefix = format!("Falhou em {name}");
                self.exec.run_async("docker", &[verb, name], move |r| {
                    poker.poke();
                    reporter.result(&prefix, &r);
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
            self.speech.say(e.to_string());
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
        self.say_working(format!("{label} stack {project}…"));
        let poker = self.host.bus.poker(Topic::Docker);
        let reporter = self.host.reporter(&self.manifest);
        let prefix = format!("Falhou na stack {project}");
        // start/stop/restart funcionam só com o nome do projeto, sem o arquivo do compose
        self.exec.run_async("docker", &["compose", "-p", project, verb], move |r| {
            poker.poke();
            reporter.result(&prefix, &r);
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
            // conversa entre mascotes: o Docker conta o que houve com o container
            let (kind, subject) = match ev {
                DockerEvent::Crash { name, .. } | DockerEvent::Restarting(name) | DockerEvent::Unhealthy(name) => {
                    (FactKind::Trouble, name)
                }
                DockerEvent::Recovered(name) => (FactKind::Recovered, name),
            };
            let voice = self.voice();
            self.facts.push(Fact { kind, voice, subject: subject.clone(), value: None });
            if !said && (urgent || events.len() == 1) {
                self.speech.say(title.clone());
                said = true;
            }
            // container de volta: a mascote comemora (reação curta, não um humor)
            if !urgent && !self.host.config.borrow().reduce_motion {
                self.face.react(Reaction::Celebrate);
            }
            if notify && (urgent || notify_recovery) {
                self.host.notifier.borrow_mut().send(&key, &title, &body, urgent);
            }
        }
    }

    /// A mascote do Docker, para a conversa entre mascotes.
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
        // sem Docker na máquina não há o que vigiar: dorme, não alarma
        if s.missing {
            return Mood::Sleep;
        }
        if !s.ok || s.containers.iter().any(|c| self.is_problem(c)) {
            return Mood::Alarm;
        }
        let busy_cpu = self.busy_cpu();
        if s.stats.values().any(|st| st.cpu >= busy_cpu) {
            return Mood::Busy;
        }
        if s.containers.iter().any(|c| c.state == "running") { Mood::Idle } else { Mood::Sleep }
    }

    /// A partir de quanto de CPU um container conta como "ocupado" (configuração do app).
    fn busy_cpu(&self) -> f32 {
        self.host.config.borrow().int(&self.manifest, "busy_cpu") as f32
    }

    // ---- tela ---------------------------------------------------------------

    /// Redesenha a mascote agora; `true` se a imagem ou a reação mudou.
    fn refresh_mascot(&mut self) -> bool {
        let sprite = sprites::get(&self.mascot_key());
        self.face.refresh(sprite, self.host.theme_for(&self.manifest), self.mood)
    }

    /// Redesenha a mascote e entrega imagem e reação à tela.
    fn push_mascot(&mut self, ui: &HubWindow) {
        self.refresh_mascot();
        let g = ui.global::<DockerApp>();
        g.set_mascot(self.face.image());
        g.set_reaction(self.face.reaction());
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
        g.set_missing(self.snap.missing);
        g.set_error_text(if self.snap.missing {
            "O comando docker não foi encontrado no PATH.".into()
        } else {
            SharedString::from(self.snap.error.as_str())
        });
        g.set_mood(self.mood.as_str().into());
        g.set_hide_stopped(self.host.config.borrow().bool(&self.manifest, "hide_stopped"));
        g.set_sort(self.host.config.borrow().string(&self.manifest, "sort").into());

        let speech = match self.speech.current() {
            Some(t) => t.to_string(),
            _ if errors > 0 => format!("{errors} container(s) com problema!"),
            _ if running == 0 => "Nenhum container rodando… zzz".into(),
            _ => format!("Tudo certo: {running} rodando."),
        };
        g.set_speech(speech.into());
        g.set_voice(Voice::id_of(&self.manifest.id, &self.manifest.name).into());

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
        let busy_cpu = self.busy_cpu();
        for g in &groups {
            if grouped {
                // com busca ativa, mostra tudo: senão o resultado fica escondido no grupo recolhido
                let collapsed = q.is_empty() && self.collapsed.contains(g.project);
                rows.push(header_row(g, collapsed));
                if collapsed {
                    continue;
                }
            }
            rows.extend(g.items.iter().map(|c| self.row(c, busy_cpu)));
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

    fn row(&self, c: &Container, busy_cpu: f32) -> ContainerRow {
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
            busy: running && st.cpu >= busy_cpu,
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

    /// O Docker é o provedor que mais atrasa: ao abrir a janela, lê tudo de novo para a
    /// tela não nascer velha.
    fn window_shown(&mut self, _ui: &HubWindow) {
        self.host.bus.poke(Topic::Docker);
    }

    fn enable(&mut self, ui: &HubWindow) {
        self.baseline = false; // sem histórico: não acusa eventos antigos
        ui.global::<DockerApp>().set_theme(render::colors(self.host.theme_for(&self.manifest)));
        self.push_mascot(ui);
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

    fn on_report(&mut self, report: &Report, ui: &HubWindow) {
        let working = self.working.take();
        match report {
            // a falha fica na fala (com o prazo normal), em vez de sumir na próxima leitura
            Report::Error(msg) => self.speech.say(msg.clone()),
            // deu certo: tira o "Parando X…", a menos que outra fala já tenha entrado
            Report::Done | Report::Output(_) => {
                if working.is_some_and(|w| self.speech.is(&w)) {
                    self.speech.clear();
                }
            }
        }
        self.push_header(ui);
    }

    fn tick(&mut self, _frame: u32, ui: &HubWindow) {
        // a fala vence no prazo, e o humor muda quando um "caiu agora" envelhece
        let mood = self.compute_mood();
        if self.speech.take_expired() || mood != self.mood {
            self.mood = mood;
            self.push_header(ui);
        }
        // o ritmo vem do relógio do `Animator`, não do quadro do hub
        if self.refresh_mascot() {
            self.push_mascot(ui);
        }
    }

    fn settings_changed(&mut self, ui: &HubWindow) {
        ui.global::<DockerApp>().set_theme(render::colors(self.host.theme_for(&self.manifest)));
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

    fn lines(&self) -> &'static str {
        include_str!("../../miniapps/docker/lines.toml")
    }

    fn summary(&self) -> String {
        if self.snap.missing {
            return "Docker: não instalado".into();
        }
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
