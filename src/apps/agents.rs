//! Mini-app Agentes de IA: as sessões do Claude Code, Codex e OpenCode e o estado de cada uma.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use slint::{ComponentHandle, Model, ModelRc, VecModel};

use super::ai_usage::ai_mascots;
use crate::core::bus::Event;
use crate::core::manifest::Manifest;
use crate::core::miniapp::{Host, MiniApp};
use crate::mascot::anim::{Animator, Reaction};
use crate::mascot::{Mood, render, sprites};
use crate::providers::agents::{Agent, AgentsSnapshot, State, Tool, clip};
use crate::ui::{AgentRow, AgentSub, AgentsApp, HubWindow};

const SPEECH_TTL: Duration = Duration::from_secs(30);
/// Subagentes desenhados andando na faixa de uma sessão (o contador mostra todos).
const MAX_WALKERS: usize = 6;
/// Subagentes listados no cartão de uma sessão.
const MAX_SUB_LINES: usize = 5;

pub struct AgentsMini {
    host: Rc<Host>,
    manifest: Manifest,
    snap: AgentsSnapshot,
    /// Estado de cada sessão na leitura anterior: é a mudança que vira fala e notificação.
    prev: HashMap<(Tool, String), State>,
    /// Primeira leitura depois de ligar: nada conta como mudança.
    baseline: bool,
    speech: Option<(String, Instant)>,
    mood: Mood,
    rows: Rc<VecModel<AgentRow>>,
    /// A sessão de cada linha da tela, na mesma ordem (os cliques vêm pelo índice).
    shown: Vec<Agent>,
    /// Sessão com as opções abertas (a linha clicada). Pela sessão, não pelo índice: a
    /// lista muda de ordem sozinha.
    selected: Option<(Tool, String)>,
    /// O que cada linha da tela mostra agora: só a linha que mudou é trocada (trocar a
    /// lista inteira recria todas as linhas e a tela pisca).
    cards: Vec<Card>,
    /// Mascote de cada ferramenta em cada estado (parada: desenhada uma vez por tema).
    faces: HashMap<(Tool, State), slint::Image>,
    mascot: render::Cache,
    anim: Animator,
    reaction: Option<Reaction>,
}

impl AgentsMini {
    pub fn create(host: Rc<Host>, ui: &HubWindow) -> Rc<RefCell<Self>> {
        let manifest =
            Manifest::parse(include_str!("../../miniapps/agents/manifest.toml")).expect("manifesto de Agentes inválido");
        let rows = Rc::new(VecModel::default());
        ui.global::<AgentsApp>().set_rows(ModelRc::from(rows.clone()));
        let app = Rc::new(RefCell::new(AgentsMini {
            host,
            manifest,
            snap: AgentsSnapshot::default(),
            prev: HashMap::new(),
            baseline: false,
            speech: None,
            mood: Mood::Sleep,
            rows,
            shown: Vec::new(),
            selected: None,
            cards: Vec::new(),
            faces: HashMap::new(),
            mascot: Default::default(),
            anim: Animator::new(13),
            reaction: None,
        }));
        let a = Rc::downgrade(&app);
        let w = ui.as_weak();
        ui.global::<AgentsApp>().on_mascot_poked(move || {
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
        let a = Rc::downgrade(&app);
        let w = ui.as_weak();
        ui.global::<AgentsApp>().on_toggle(move |row| {
            if let (Some(a), Some(ui)) = (a.upgrade(), w.upgrade()) {
                let mut a = a.borrow_mut();
                let key = a.shown.get(row.max(0) as usize).map(|s| (s.tool, s.id.clone()));
                a.selected = if a.selected == key { None } else { key };
                a.refresh(&ui);
            }
        });
        let a = Rc::downgrade(&app);
        let w = ui.as_weak();
        ui.global::<AgentsApp>().on_open_terminal(move |row| {
            if let (Some(a), Some(ui)) = (a.upgrade(), w.upgrade()) {
                a.borrow_mut().open_terminal(row.max(0) as usize, &ui);
            }
        });
        let a = Rc::downgrade(&app);
        let w = ui.as_weak();
        ui.global::<AgentsApp>().on_open_dir(move |row| {
            if let (Some(a), Some(ui)) = (a.upgrade(), w.upgrade()) {
                a.borrow_mut().open_dir(row.max(0) as usize, &ui);
            }
        });
        let a = Rc::downgrade(&app);
        let w = ui.as_weak();
        ui.global::<AgentsApp>().on_resume(move |row| {
            if let (Some(a), Some(ui)) = (a.upgrade(), w.upgrade()) {
                a.borrow_mut().resume(row.max(0) as usize, &ui);
            }
        });
        app
    }

    /// Abre a pasta da sessão no gerenciador de arquivos.
    fn open_dir(&mut self, row: usize, ui: &HubWindow) {
        let Some(a) = self.shown.get(row) else { return };
        if !crate::core::exec::open_dir(std::path::Path::new(&a.cwd)) {
            self.say(format!("Não achei a pasta {}.", a.cwd));
        }
        self.selected = None;
        self.refresh(ui);
    }

    /// Abre um terminal na pasta da sessão.
    fn open_terminal(&mut self, row: usize, ui: &HubWindow) {
        let Some(a) = self.shown.get(row) else { return };
        if let Err(e) = crate::core::exec::open_terminal_at(std::path::Path::new(&a.cwd)) {
            self.say(format!("Não deu para abrir o terminal: {e}"));
        }
        self.selected = None;
        self.refresh(ui);
    }

    /// Retoma uma sessão parada num terminal novo, na pasta dela.
    fn resume(&mut self, row: usize, ui: &HubWindow) {
        let Some(a) = self.shown.get(row).cloned() else { return };
        let Some(argv) = resume_argv(&a) else { return };
        let dir = std::path::Path::new(&a.cwd);
        let opened = self.host.exec_for(&self.manifest).open_terminal_in(a.tool.name(), Some(dir), &argv);
        self.say(match opened {
            Ok(()) => format!("Retomando no terminal: {}", a.title),
            Err(e) => format!("Não deu para abrir o {}: {e}", a.tool.name()),
        });
        self.selected = None;
        self.refresh(ui);
    }

    fn say(&mut self, text: String) {
        self.speech = Some((text, Instant::now()));
    }

    /// Compara com a leitura anterior: quem terminou e quem passou a esperar o usuário.
    fn ingest(&mut self, snap: AgentsSnapshot) {
        let cur: HashMap<(Tool, String), State> = snap.agents.iter().map(|a| ((a.tool, a.id.clone()), a.state)).collect();
        if self.baseline {
            let (notify_waiting, notify_done) = {
                let cfg = self.host.config.borrow();
                (cfg.bool(&self.manifest, "notify_waiting"), cfg.bool(&self.manifest, "notify_done"))
            };
            // subagente indo e vindo é barulho: só as sessões que o usuário abriu
            for a in snap.agents.iter().filter(|a| a.parent.is_none()) {
                let before = self.prev.get(&(a.tool, a.id.clone())).copied();
                let Some(change) = change(before, a) else { continue };
                let (title, body, notify, urgent) = match change {
                    Change::Waiting => {
                        self.anim.trigger(Reaction::Surprised, Instant::now());
                        ("Agente esperando você", format!("{} espera por você: {}", a.tool.name(), a.title), notify_waiting, true)
                    }
                    Change::Done => {
                        self.anim.trigger(Reaction::Celebrate, Instant::now());
                        ("Agente terminou", format!("{} terminou: {}", a.tool.name(), a.title), notify_done, false)
                    }
                    Change::Stopped => {
                        ("Agente parou", format!("{} parou ({}): {}", a.tool.name(), a.detail, a.title), notify_done, false)
                    }
                };
                if notify {
                    self.host.notifier.borrow_mut().send(&format!("agents:{}:{}", a.tool.name(), a.id), title, &body, urgent);
                }
                self.say(body);
            }
        }
        self.prev = cur;
        self.baseline = true;
        self.snap = snap;
    }

    /// As sessões que aparecem: as ativas sempre; as paradas, só dentro do prazo da opção.
    fn visible(&self, now: i64) -> Vec<&Agent> {
        let recent = self.host.config.borrow().int(&self.manifest, "recent").max(1) * 60_000;
        self.snap.agents.iter().filter(|a| a.state != State::Idle || now - a.since_ms <= recent).collect()
    }

    fn compute_mood(&self, list: &[&Agent]) -> Mood {
        if list.iter().any(|a| a.state == State::Waiting) {
            Mood::Busy
        } else if list.iter().any(|a| a.state == State::Working) {
            Mood::Happy
        } else if !list.is_empty() {
            Mood::Idle
        } else {
            Mood::Sleep
        }
    }

    fn face(&mut self, tool: Tool, state: State) -> slint::Image {
        if let Some(img) = self.faces.get(&(tool, state)) {
            return img.clone();
        }
        let name = if tool == Tool::OpenCode { "OpenCode Go" } else { tool.name() };
        let mood = match state {
            State::Working => Mood::Happy,
            State::Waiting => Mood::Busy,
            State::Idle => Mood::Sleep,
        };
        let theme = self.host.theme_for(&self.manifest);
        let img = ai_mascots::get(name).map(|s| render::image(s, theme, mood, 0)).unwrap_or_default();
        self.faces.insert((tool, state), img.clone());
        img
    }

    fn refresh(&mut self, ui: &HubWindow) {
        let now = now_ms();
        let home = dirs::home_dir().map(|h| h.to_string_lossy().into_owned()).unwrap_or_default();
        let all: Vec<Agent> = self.visible(now).into_iter().cloned().collect();
        self.mood = self.compute_mood(&all.iter().collect::<Vec<_>>());
        // uma linha por sessão; os subagentes ficam no cartão de quem os disparou
        let list = group(&all);
        // a sessão das opções abertas saiu da lista: fecha
        if self.selected.as_ref().is_some_and(|(t, id)| !list.iter().any(|(a, _)| a.tool == *t && a.id == *id)) {
            self.selected = None;
        }
        let selected = self.selected.clone();
        let cards: Vec<Card> = list
            .iter()
            .map(|(a, subs)| {
                let open = selected.as_ref().is_some_and(|(t, id)| a.tool == *t && a.id == *id);
                card(a, subs, &short_dir(&a.cwd, &home), now, open)
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
        self.cards = cards;
        let list: Vec<Agent> = list.into_iter().map(|(a, _)| a.clone()).collect();
        self.shown = list.clone();

        let count = |s: State| list.iter().filter(|a| a.state == s).count();
        let (working, waiting, idle) = (count(State::Working), count(State::Waiting), count(State::Idle));
        let g = ui.global::<AgentsApp>();
        g.set_working(working as i32);
        g.set_waiting(waiting as i32);
        g.set_idle(idle as i32);
        g.set_missing(self.baseline && self.snap.found.is_empty());
        let errors: Vec<String> = self.snap.errors.iter().map(|(t, e)| format!("{}: {e}", t.name())).collect();
        g.set_error_text(errors.join(" · ").into());
        g.set_mood(self.mood.as_str().into());
        let speech = match &self.speech {
            Some((t, at)) if at.elapsed() < SPEECH_TTL => t.clone(),
            _ if waiting > 0 => format!("{} por você!", plural(waiting, "agente esperando", "agentes esperando")),
            _ if working > 0 => format!("{}.", plural(working, "agente trabalhando", "agentes trabalhando")),
            _ if idle > 0 => "Tudo parado por aqui.".into(),
            _ => "Nenhum agente por perto… zzz".into(),
        };
        g.set_speech(speech.into());
    }

    /// O cartão no formato da tela (com as imagens das mascotes).
    fn row(&mut self, c: &Card) -> AgentRow {
        let subs: Vec<AgentSub> = c
            .subs
            .iter()
            .map(|s| AgentSub {
                kind: s.kind.as_str().into(),
                title: s.title.as_str().into(),
                state: state_key(s.state).into(),
                state_text: s.state_text.as_str().into(),
            })
            .collect();
        AgentRow {
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
        }
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
        let g = ui.global::<AgentsApp>();
        g.set_mascot(self.mascot.image());
        g.set_reaction(self.reaction.map_or("", Reaction::as_str).into());
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Change {
    /// Passou a esperar o usuário.
    Waiting,
    /// Estava trabalhando e terminou o turno.
    Done,
    /// Estava trabalhando e parou sem terminar (falhou, foi interrompido).
    Stopped,
}

/// O que mudou numa sessão desde a leitura anterior (`before` = estado que tinha).
fn change(before: Option<State>, a: &Agent) -> Option<Change> {
    match (before, a.state) {
        (Some(State::Waiting), State::Waiting) => None,
        // sessão que já aparece esperando também conta: é a hora de avisar
        (_, State::Waiting) => Some(Change::Waiting),
        (Some(State::Working), State::Idle) if a.detail.is_empty() => Some(Change::Done),
        (Some(State::Working), State::Idle) => Some(Change::Stopped),
        _ => None,
    }
}

/// Junta cada subagente à sessão que o disparou: (sessão, subagentes dela). Subagente
/// cuja sessão não está na lista (parada há muito tempo) fica com a própria linha.
/// Na ordem da tela: quem precisa de você, depois quem trabalha (ou tem subagente
/// trabalhando); em cada grupo, o mais recente.
fn group(all: &[Agent]) -> Vec<(&Agent, Vec<&Agent>)> {
    let is_top = |a: &Agent| match &a.parent {
        Some(p) => !all.iter().any(|o| o.tool == a.tool && o.id == *p),
        None => true,
    };
    let mut list: Vec<(&Agent, Vec<&Agent>)> = all
        .iter()
        .filter(|a| is_top(a))
        .map(|a| (a, all.iter().filter(|s| s.tool == a.tool && s.parent.as_deref() == Some(a.id.as_str())).collect()))
        .collect();
    list.sort_by_key(|(a, subs)| {
        let busy = subs.iter().any(|s| s.state == State::Working);
        (rank(a.state).min(if busy { rank(State::Working) } else { u8::MAX }), -a.since_ms)
    });
    list
}

/// Segunda linha: ferramenta, subagentes e o que faz agora (parada, a pasta).
fn info(a: &Agent, subs: &[&Agent], dir: &str) -> String {
    let mut parts = vec![a.tool.name().to_string()];
    if a.parent.is_some() {
        parts.push("subagente".into());
    }
    let working = subs.iter().filter(|s| s.state == State::Working).count();
    match (subs.len(), working) {
        (0, _) => {}
        (n, 0) => parts.push(plural(n, "subagente", "subagentes")),
        (n, w) if n == w => parts.push(plural(n, "subagente ativo", "subagentes ativos")),
        (n, w) => parts.push(format!("{w} de {n} subagentes ativos")),
    }
    if a.state != State::Idle && !a.action.is_empty() {
        parts.push(a.action.clone());
    } else if !dir.is_empty() {
        parts.push(dir.to_string());
    }
    parts.join(" · ")
}

/// Um subagente no cartão da sessão que o disparou.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SubLine {
    /// Tipo do subagente ("" = a ferramenta não diz, ou é igual ao título).
    kind: String,
    title: String,
    state: State,
    state_text: String,
}

/// Tudo o que uma linha da tela mostra, sem as imagens: dá para comparar com o que já
/// está lá. Os campos de detalhe vêm de `Details` e ficam vazios quando a ferramenta não
/// informa; a tela esconde o que estiver vazio.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Card {
    tool: Tool,
    state: State,
    title: String,
    info: String,
    state_text: String,
    sub: bool,
    can_resume: bool,
    open: bool,
    walkers: usize,
    /// Sem o prefixo do fabricante: "fable-5-2".
    model: String,
    effort: String,
    branch: String,
    /// Entrada, saída e cache, já abreviados.
    tokens: Option<[String; 3]>,
    /// Duração do último turno ("" = não sei).
    turn: String,
    /// O que mais se sabe do último turno: mensagens na conversa, por que parou.
    turn_note: String,
    prompt: String,
    subs: Vec<SubLine>,
    /// Subagentes além dos listados.
    more_subs: usize,
}

fn card(a: &Agent, subs: &[&Agent], dir: &str, now: i64, open: bool) -> Card {
    let d = &a.details;
    let mut note = Vec::new();
    if let Some((_, messages)) = d.last_turn.filter(|(_, m)| *m > 0) {
        note.push(format!("{messages} mensagens"));
    }
    // o motivo é da última resposta: no meio de um turno ainda não é "por que parou"
    if a.state == State::Idle && !d.stop_reason.is_empty() {
        note.push(stop_text(&d.stop_reason));
    }
    Card {
        tool: a.tool,
        state: a.state,
        title: a.title.clone(),
        info: info(a, subs, dir),
        state_text: state_text(a, now),
        sub: a.parent.is_some(),
        can_resume: resume_argv(a).is_some(),
        open,
        walkers: subs.iter().filter(|s| s.state == State::Working).count().min(MAX_WALKERS),
        model: d.model.strip_prefix("claude-").unwrap_or(&d.model).to_string(),
        effort: d.effort.clone(),
        branch: d.branch.clone(),
        tokens: d.tokens.map(|t| [compact(t.input), compact(t.output), compact(t.cache_read + t.cache_write)]),
        turn: d.last_turn.map(|(ms, _)| duration(ms)).unwrap_or_default(),
        turn_note: note.join(" · "),
        prompt: clip(&d.last_prompt, 140),
        subs: subs
            .iter()
            .take(MAX_SUB_LINES)
            .map(|s| SubLine {
                kind: if s.details.kind == s.title { String::new() } else { s.details.kind.clone() },
                title: s.title.clone(),
                state: s.state,
                state_text: state_text(s, now),
            })
            .collect(),
        more_subs: subs.len().saturating_sub(MAX_SUB_LINES),
    }
}

/// 950 → "950", 12 345 → "12 k", 1 234 567 → "1,2 M".
fn compact(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..10_000 => format!("{:.1} k", n as f64 / 1e3).replace('.', ","),
        10_000..1_000_000 => format!("{} k", n / 1_000),
        _ => format!("{:.1} M", n as f64 / 1e6).replace('.', ","),
    }
}

/// "8 s", "2 min 05 s".
fn duration(ms: i64) -> String {
    let s = (ms / 1000).max(0);
    if s < 60 { format!("{s} s") } else { format!("{} min {:02} s", s / 60, s % 60) }
}

/// Motivo da parada, em palavras (o nome cru se for um que não conheço).
fn stop_text(reason: &str) -> String {
    match reason {
        "end_turn" => "resposta completa".into(),
        "tool_use" => "parou numa ferramenta".into(),
        "max_tokens" => "bateu no limite de tokens".into(),
        "stop_sequence" => "parou numa sequência de parada".into(),
        "refusal" => "recusou o pedido".into(),
        "pause_turn" => "turno pausado".into(),
        other => format!("parou por {other}"),
    }
}

/// Comando que retoma a sessão num terminal. Só para sessão parada do Codex ou do
/// OpenCode: a do Claude está aberta no terminal dela, e retomar uma sessão no meio de
/// um turno poria dois processos escrevendo nela.
fn resume_argv(a: &Agent) -> Option<[&str; 3]> {
    // o id vai como argumento: nada que pareça uma opção ou um caminho
    let safe = !a.id.is_empty() && !a.id.starts_with('-') && a.id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if a.state != State::Idle || a.parent.is_some() || !safe {
        return None;
    }
    match a.tool {
        Tool::Codex => Some(["codex", "resume", a.id.as_str()]),
        Tool::OpenCode => Some(["opencode", "-s", a.id.as_str()]),
        Tool::Claude => None,
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

/// "1 agente", "3 agentes".
fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// "há 3 min", "há 2 h"; `None` com menos de um minuto ou sem hora conhecida.
fn ago(since_ms: i64, now: i64) -> Option<String> {
    let min = (now - since_ms) / 60_000;
    match min {
        _ if since_ms <= 0 || min < 1 => None,
        1..60 => Some(format!("há {min} min")),
        _ => Some(format!("há {} h", min / 60)),
    }
}

fn state_text(a: &Agent, now: i64) -> String {
    let ago = ago(a.since_ms, now);
    match a.state {
        State::Working if a.since_ms <= 0 => "trabalhando".into(),
        State::Working => ago.map_or("começou agora".into(), |t| format!("trabalhando {t}")),
        State::Waiting if a.detail.is_empty() => "esperando você".into(),
        State::Waiting => format!("esperando você: {}", a.detail),
        State::Idle => {
            let what = if a.detail.is_empty() { "parado" } else { a.detail.as_str() };
            match ago {
                Some(t) => format!("{what} {t}"),
                None if a.since_ms <= 0 => what.to_string(),
                None if a.detail.is_empty() => "terminou agora".into(),
                None => format!("{what} agora"),
            }
        }
    }
}

/// `/home/eu/Projetos/x` → `~/Projetos/x`.
fn short_dir(cwd: &str, home: &str) -> String {
    match cwd.strip_prefix(home) {
        Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => format!("~{rest}"),
        _ => cwd.to_string(),
    }
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
        self.snap = AgentsSnapshot::default();
        self.prev.clear();
        self.speech = None;
        self.mood = Mood::Sleep;
        self.shown.clear();
        self.selected = None;
        self.cards.clear();
        self.rows.set_vec(Vec::new());
    }

    fn on_event(&mut self, ev: &Event, ui: &HubWindow) {
        let Event::Agents(snap) = ev else { return };
        self.ingest(snap.clone());
        // também atualiza os "há N min", que mudam sem o estado mudar
        self.refresh(ui);
    }

    fn tick(&mut self, _frame: u32, ui: &HubWindow) {
        if self.refresh_mascot() {
            let g = ui.global::<AgentsApp>();
            g.set_mascot(self.mascot.image());
            g.set_reaction(self.reaction.map_or("", Reaction::as_str).into());
        }
    }

    fn settings_changed(&mut self, ui: &HubWindow) {
        ui.global::<AgentsApp>().set_theme(render::colors(self.host.theme_for(&self.manifest)));
        // tema novo: as mascotes das linhas são redesenhadas
        self.faces.clear();
        self.cards.clear();
        self.refresh(ui);
        self.push_mascot(ui);
    }

    fn mood(&self) -> Mood {
        self.mood
    }

    fn mascot_key(&self) -> String {
        self.host.config.borrow().string(&self.manifest, "mascot")
    }

    fn summary(&self) -> String {
        let list = self.visible(now_ms());
        let count = |s: State| list.iter().filter(|a| a.state == s).count();
        let (working, waiting) = (count(State::Working), count(State::Waiting));
        if waiting > 0 {
            format!("Agentes: {waiting} esperando você · {working} trabalhando")
        } else {
            format!("Agentes: {working} trabalhando")
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
        }
    }

    #[test]
    fn cartao_so_tem_o_que_a_ferramenta_informa() {
        use crate::providers::agents::{Details, Tokens};
        let now = 10 * 3_600_000;
        // ferramenta que não informa nada a mais: nenhum detalhe no cartão
        let bare = card(&agent(State::Idle, 1, ""), &[], "~/p", now, false);
        assert_eq!((bare.model.as_str(), bare.tokens.is_some(), bare.turn.as_str(), bare.prompt.as_str()), ("", false, "", ""));
        assert!(bare.subs.is_empty() && bare.turn_note.is_empty());

        let full = Agent {
            details: Details {
                last_prompt: "faça o\nwrapper".into(),
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
            title: "Ler a página".into(),
            details: Details { kind: "web-fetch".into(), ..Details::default() },
            ..agent(State::Working, now - 3 * 60_000, "")
        };
        let c = card(&full, &[&sub], "~/p", now, true);
        assert_eq!((c.model.as_str(), c.effort.as_str(), c.branch.as_str()), ("fable-5-2", "medium", "master"));
        assert_eq!(c.tokens, Some(["950".to_string(), "45 k".to_string(), "1,2 M".to_string()]));
        assert_eq!((c.turn.as_str(), c.turn_note.as_str()), ("2 min 05 s", "316 mensagens · resposta completa"));
        assert_eq!((c.prompt.as_str(), c.open, c.walkers), ("faça o wrapper", true, 1));
        let line = &c.subs[0];
        assert_eq!((line.kind.as_str(), line.title.as_str(), line.state_text.as_str()), ("web-fetch", "Ler a página", "trabalhando há 3 min"));

        // trabalhando: o motivo da última resposta ainda não é o da parada
        let busy = Agent { state: State::Working, ..full.clone() };
        assert_eq!(card(&busy, &[], "~/p", now, false).turn_note, "316 mensagens");
        // subagentes além do limite viram um número; tipo igual ao título não repete
        let same = Agent { title: "Explore".into(), details: Details { kind: "Explore".into(), ..Details::default() }, ..sub.clone() };
        let many = vec![&same; MAX_SUB_LINES + 2];
        let c = card(&full, &many, "~/p", now, false);
        assert_eq!((c.subs.len(), c.more_subs, c.subs[0].kind.as_str()), (MAX_SUB_LINES, 2, ""));
        assert_eq!(c.walkers, MAX_WALKERS.min(MAX_SUB_LINES + 2));

        assert_eq!((compact(1_500).as_str(), duration(8_400).as_str()), ("1,5 k", "8 s"));
        assert_eq!(stop_text("novo_motivo"), "parou por novo_motivo");
    }

    #[test]
    fn subagentes_ficam_na_linha_de_quem_disparou() {
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
            named("orfao", Some("sumiu"), State::Idle, 5),
            Agent { tool: Tool::Codex, ..named("c1", Some("a"), State::Working, 1) },
        ];
        let list = group(&all);
        let ids: Vec<(&str, usize)> = list.iter().map(|(a, subs)| (a.id.as_str(), subs.len())).collect();
        // "a" sobe por ter subagente trabalhando; o filho de outra ferramenta não é dele
        assert_eq!(ids, [("a", 2), ("c1", 0), ("b", 0), ("orfao", 0)]);

        let (a, subs) = &list[0];
        assert_eq!(info(a, subs, "~/p"), "Claude · 1 de 2 subagentes ativos · ~/p");
        assert_eq!(info(&all[3], &[], "~/p"), "Claude · ~/p");
        assert_eq!(info(&all[4], &[], ""), "Claude · subagente");
        let busy = Agent { action: "Edit: bus.rs".into(), ..named("w", None, State::Working, 1) };
        assert_eq!(info(&busy, &[&all[1]], "~/p"), "Claude · 1 subagente ativo · Edit: bus.rs");
        // ação antiga de sessão parada não aparece
        let stale = Agent { action: "Edit: bus.rs".into(), ..named("w", None, State::Idle, 1) };
        assert_eq!(info(&stale, &[], "~/p"), "Claude · ~/p");
    }

    #[test]
    fn so_retoma_sessao_parada_de_codex_e_opencode() {
        let of = |tool: Tool, state: State, id: &str| Agent { tool, id: id.into(), ..agent(state, 1, "") };
        assert_eq!(resume_argv(&of(Tool::Codex, State::Idle, "01a1-c8")), Some(["codex", "resume", "01a1-c8"]));
        assert_eq!(resume_argv(&of(Tool::OpenCode, State::Idle, "ses_f6e8")), Some(["opencode", "-s", "ses_f6e8"]));
        assert_eq!(resume_argv(&of(Tool::Claude, State::Idle, "abc")), None);
        assert_eq!(resume_argv(&of(Tool::Codex, State::Working, "abc")), None);
        assert_eq!(resume_argv(&of(Tool::Codex, State::Idle, "--help")), None);
        assert_eq!(resume_argv(&of(Tool::Codex, State::Idle, "a b")), None);
        let sub = Agent { parent: Some("p".into()), ..of(Tool::Codex, State::Idle, "abc") };
        assert_eq!(resume_argv(&sub), None);
    }

    #[test]
    fn texto_do_estado() {
        let now = 10 * 3_600_000;
        let min = 60_000;
        assert_eq!(state_text(&agent(State::Working, now - 3 * min, ""), now), "trabalhando há 3 min");
        assert_eq!(state_text(&agent(State::Working, now - 5_000, ""), now), "começou agora");
        assert_eq!(state_text(&agent(State::Waiting, now - 3 * min, "permission"), now), "esperando você: permission");
        assert_eq!(state_text(&agent(State::Waiting, now, ""), now), "esperando você");
        assert_eq!(state_text(&agent(State::Idle, now - 125 * min, ""), now), "parado há 2 h");
        assert_eq!(state_text(&agent(State::Idle, now - 5_000, ""), now), "terminou agora");
        assert_eq!(state_text(&agent(State::Idle, now - 9 * min, "falhou"), now), "falhou há 9 min");
        assert_eq!(state_text(&agent(State::Idle, 0, ""), now), "parado");
        assert_eq!(state_text(&agent(State::Working, 0, ""), now), "trabalhando");
    }

    #[test]
    fn so_a_mudanca_vira_aviso() {
        let idle = agent(State::Idle, 1, "");
        let waiting = agent(State::Waiting, 1, "");
        assert_eq!(change(Some(State::Working), &idle), Some(Change::Done));
        assert_eq!(change(Some(State::Working), &agent(State::Idle, 1, "falhou")), Some(Change::Stopped));
        assert_eq!(change(Some(State::Idle), &idle), None);
        // sessão nova já parada (ou que saiu da espera) não é "terminou"
        assert_eq!(change(None, &idle), None);
        assert_eq!(change(Some(State::Waiting), &idle), None);
        assert_eq!(change(Some(State::Working), &waiting), Some(Change::Waiting));
        assert_eq!(change(None, &waiting), Some(Change::Waiting));
        assert_eq!(change(Some(State::Waiting), &waiting), None);
        assert_eq!(change(Some(State::Idle), &agent(State::Working, 1, "")), None);
    }

    #[test]
    fn pasta_abreviada() {
        assert_eq!(short_dir("/home/eu/Projetos/x", "/home/eu"), "~/Projetos/x");
        assert_eq!(short_dir("/home/eu", "/home/eu"), "~");
        assert_eq!(short_dir("/home/eunice/x", "/home/eu"), "/home/eunice/x");
        assert_eq!(short_dir("/srv/x", ""), "/srv/x");
    }
}
