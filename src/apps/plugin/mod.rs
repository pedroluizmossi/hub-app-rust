//! Mini-apps dos plugins da comunidade (ver `core/plugins.rs` e `store.rs`).
//!
//! O app externo (`[app]`, acesso total) usa a mesma tela e o mesmo estado dos plugins com
//! tela; no lugar do script, um processo próprio (`apps/external.rs`).
//!
//! Dois tipos sem código nativo:
//! - **simples** (`[run]`): o host roda o comando do manifesto (allowlist, sem shell) a cada
//!   `every` segundos e mostra a saída na tela genérica (`ui/plugin.slint`);
//! - **com tela própria** (`[ui]`): o `view.slint` do plugin é interpretado
//!   (`slint-interpreter`) e encaixado na célula por um `ComponentContainer`; a lógica é o
//!   `main.rhai`, que roda numa thread só dele e conversa com a tela por mensagens.
//!
//! O script nunca toca a UI nem o disco. O que ele pode, pela API abaixo: mudar
//! propriedades da tela, dizer o humor, rodar os programas da allowlist (com prazo),
//! consultar os domínios de `[permissions] net`, receber as leituras dos provedores de
//! `uses`, guardar dados (chave-valor, num arquivo que o host grava, e um banco SQLite que o
//! host abre, os dois na pasta de dados do app: `core/appdata.rs`), mandar notificação,
//! contar fatos para a conversa entre mascotes e ler as próprias opções.
//!
//! API do script (`main.rhai`):
//! ```text
//! fn start()                  ao ligar          this.settings = opções do app (mapa)
//! fn tick()                   a cada [ui] every
//! fn action(name, arg)        a tela chamou `callback action(string, string)`
//! fn on_event(topic, data)    leitura de um provedor de `uses` ("sys", "docker"…), em mapa;
//!                             "procs" traz os processos mais pesados (by_cpu, by_mem e
//!                             count), igual no Linux e no Windows: use-o em vez do `ps`
//! fn settings()               o usuário mudou uma opção (this.settings já é a nova)
//! ui("prop", valor)           muda uma propriedade da tela (texto, número, bool, lista, mapa)
//! mood("happy")               humor: sleep, idle, happy, busy, alarm
//! summary("texto")            linha do tooltip da bandeja
//! run(["git", "status"])     roda um programa da allowlist: #{ ok, out, err } (até 30 s)
//! run_async(["git"], "fim")   o mesmo sem esperar: chama fn fim(result) depois
//! http_get(url)               GET https num domínio de net: #{ ok, status, body, err }
//! http_get_async(url, "fim")  o mesmo sem esperar (até 4 tarefas ao mesmo tempo)
//! parse_json(texto)  to_json(valor)
//! store_get("k")  store_set("k", valor)   dados que sobrevivem a desligar (() apaga);
//!                             kv no [data] (sem [data], o script já tem)
//! db_batch(sql)  db_exec(sql, [params])  db_query(sql, [params])
//!                             banco SQLite (sqlite no [data]; detalhes em db.rs)
//! fn migrate(de, para)        o [data] version subiu: migre os dados antes de start()
//!                             (erro = o host devolve a cópia de antes e o app não começa)
//! fact("strain", "CPU", 93)   fato para a conversa: trouble, strain, forecast, recovered, poked
//! notify("título", "texto")
//! now()                       segundos desde 1970
//! ```
//! Estado entre chamadas fica em `this` (ex.: `this.count += 1`). O host já conta à conversa
//! as mudanças de humor (alarme, agitado, voltou ao normal) e as cutucadas na mascote.
//!
//! A tela (`view.slint`) exporta `component Main`; pode importar `std-widgets.slint`,
//! `miniutils.slint` (o `Theme`, abaixo) e as telas de `ui/` do próprio plugin; imagens só
//! de `assets/` (`@image-url("assets/logo.png")`). O host preenche, se a tela declarar:
//! `in property <Theme> theme` (cores do tema do app e fontes do visual: `theme.font-ui`,
//! `font-mono`, `font-title`), `in property <image> mascot`, `in property <string> mood`.
//! O fundo da tela é transparente e o texto já usa a fonte da interface do visual (ver
//! `compile_view`).

mod db;
mod script;
mod view;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::Path;
use std::rc::{Rc, Weak};
use std::sync::mpsc::{Sender, channel};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Model, ModelRc, Timer, TimerMode, VecModel};
use slint_interpreter::{ComponentDefinition, ComponentInstance, Value};

use super::external::{self, Incoming, ToApp};
use super::kit::HeaderMascot;
use crate::core::bus::Event;
use crate::core::chatter::{self, Fact, FactKind, Voice};
use crate::core::exec::Exec;
use crate::core::manifest::Manifest;
use crate::core::miniapp::{AppRef, Host, MiniApp, Report};
use crate::core::appdata::{self, Kind, Prep};
use crate::core::plugins::{self, Plugin, Ui};
use crate::core::registry::Registry;
use crate::core::wake::Waker;
use crate::mascot::anim::Reaction;
use crate::mascot::{Mood, sprites};
use crate::ui::{HubWindow, PluginApps, PluginCard};
use db::DbSpec;
use script::{ScriptEnv, ScriptIn, ScriptOut, engine, event_data, from_app, json_to_dyn, script_loop};
use view::{FONT_PROP, compile_view, theme_value};
pub use view::check_view;

// ---- conjunto de plugins carregados (thread da UI) -----------------------------------

struct Loaded {
    id: String,
    app: Rc<RefCell<PluginMini>>,
    /// Muda quando os arquivos do plugin mudam (atualização): aí o app é recriado.
    stamp: u64,
}

#[derive(Default)]
struct PluginSet {
    cards: Option<Rc<VecModel<PluginCard>>>,
    loaded: Vec<Loaded>,
    /// Modo dev: confere a pasta do plugin a cada segundo e recarrega quando muda.
    dev_watch: Option<Timer>,
}

thread_local! {
    static SET: RefCell<PluginSet> = RefCell::new(PluginSet::default());
}

fn find_app(id: &str) -> Option<Rc<RefCell<PluginMini>>> {
    SET.with(|s| s.borrow().loaded.iter().find(|l| l.id == id).map(|l| l.app.clone()))
}

/// Relê a pasta dos plugins e acerta o registro: acrescenta os novos, recria os que
/// mudaram e tira os removidos. `startup` = na montagem do hub (o registro liga os apps
/// depois); fora dela, quem estava ligado é religado na hora. Devolve se algo mudou.
pub fn sync(host: &Rc<Host>, ui: &HubWindow, registry: &Registry, startup: bool) -> bool {
    let cards = SET.with(|s| s.borrow().cards.clone()).unwrap_or_else(|| {
        let cards = Rc::new(VecModel::<PluginCard>::default());
        ui.global::<PluginApps>().set_cards(ModelRc::from(cards.clone()));
        bind(ui);
        SET.with(|s| {
            let mut s = s.borrow_mut();
            s.cards = Some(cards.clone());
            s.dev_watch = plugins::dev_dir().map(watch_dev);
        });
        cards
    });
    let root = plugins::dir();
    let installed = plugins::load_all(&root);
    let live_ui = (!startup).then_some(ui);
    let mut changed = false;

    // removidos (ou mudados: saem e entram de novo)
    let stamps: Vec<(String, u64)> = installed.iter().map(|p| (p.manifest.id.clone(), stamp(&p.dir))).collect();
    let gone: Vec<String> = SET.with(|s| {
        s.borrow()
            .loaded
            .iter()
            .filter(|l| !stamps.iter().any(|(id, st)| *id == l.id && *st == l.stamp))
            .map(|l| l.id.clone())
            .collect()
    });
    for id in gone {
        registry.remove(&id, live_ui);
        SET.with(|s| s.borrow_mut().loaded.retain(|l| l.id != id));
        if let Some(i) = (0..cards.row_count()).find(|&i| cards.row_data(i).is_some_and(|c| c.id == id)) {
            cards.remove(i);
        }
        changed = true;
    }
    // novos
    for (plugin, (id, st)) in installed.into_iter().zip(stamps) {
        if SET.with(|s| s.borrow().loaded.iter().any(|l| l.id == id)) {
            continue;
        }
        // o plugin em desenvolvimento já entra ligado, na primeira página
        if plugins::dev_dir() == Some(plugin.dir.as_path()) {
            log::info!("plugin-dev: {id} carregado de {}", plugin.dir.display());
            let mut cfg = host.config.borrow_mut();
            if cfg.page_of(&id).is_none() {
                cfg.move_to_page(&id, 0);
            }
            if !cfg.is_enabled(&id) {
                cfg.enabled.push(id.clone());
            }
            cfg.save();
        }
        cards.push(PluginCard { id: id.as_str().into(), ..Default::default() });
        let dir = plugin.dir.clone();
        let app = PluginMini::create(host.clone(), plugin, &dir, cards.clone(), host.bus.waker().clone());
        SET.with(|s| s.borrow_mut().loaded.push(Loaded { id: id.clone(), app: app.clone(), stamp: st }));
        registry.add(app as AppRef, live_ui);
        changed = true;
    }
    changed
}

/// Impressão dos arquivos do plugin (para notar uma atualização).
fn stamp(dir: &Path) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    std::fs::read(dir.join("manifest.toml")).ok().hash(&mut h);
    for rel in plugins::files(dir).unwrap_or_default() {
        rel.hash(&mut h);
        if plugins::is_bin(&rel) {
            // executável: tamanho e data bastam (e não lê megabytes a cada segundo)
            let meta = std::fs::metadata(dir.join(&rel)).ok();
            meta.map(|m| (m.len(), m.modified().ok())).hash(&mut h);
        } else {
            std::fs::read(dir.join(&rel)).ok().hash(&mut h);
        }
    }
    h.finish()
}

/// Modo dev: a cada segundo, se algum arquivo do plugin mudou, relê os plugins (os erros
/// da tela e do script saem no terminal).
fn watch_dev(dir: &'static Path) -> Timer {
    let timer = Timer::default();
    let last = std::cell::Cell::new(stamp(dir));
    timer.start(TimerMode::Repeated, Duration::from_secs(1), move || {
        let now = stamp(dir);
        if now != last.replace(now) {
            log::info!("plugin-dev: arquivos mudaram, recarregando");
            crate::hub::request_plugin_reload();
        }
    });
    timer
}

/// Para a lógica de um plugin (o processo do app externo, a thread do script) antes de a
/// loja trocar ou apagar a pasta dele ou os dados (no Windows, um executável rodando trava
/// a pasta; o script segura o banco). Ver `released` e `resume`.
pub fn suspend(id: &str) {
    if let Some(a) = find_app(id)
        && let Some(full) = &a.borrow().full
    {
        full.stop();
    }
}

/// A lógica parada por `suspend` já soltou os arquivos.
pub fn released(id: &str) -> bool {
    find_app(id).is_none_or(|a| a.borrow().data_released())
}

/// Depois da loja: religa um app ligado que ficou parado (a loja falhou, ou não trocou
/// nada). Se a loja trocou o app, o novo já está rodando e nada acontece.
pub fn resume(id: &str) {
    let Some(a) = find_app(id) else { return };
    let mut a = a.borrow_mut();
    let stopped = a.full.as_ref().is_some_and(|f| !f.running());
    if stopped && a.host.config.borrow().is_enabled(id) {
        a.start_logic();
        if let Some(ui) = crate::hub::ui_handle() {
            a.push(&ui);
        }
    }
}

/// Os callbacks de `PluginApps` são de todos os plugins: o id diz de qual.
fn bind(ui: &HubWindow) {
    let g = ui.global::<PluginApps>();
    let w = ui.as_weak();
    g.on_poked(move |id| {
        if let (Some(a), Some(ui)) = (find_app(&id), w.upgrade()) {
            let mut a = a.borrow_mut();
            let (mood, reduce) = (a.mood(), a.host.config.borrow().reduce_motion);
            let voice = a.voice();
            a.facts.push(Fact { kind: FactKind::Poked, voice, subject: String::new(), value: None });
            if a.face.poke(mood, reduce) {
                a.push(&ui);
            }
        }
    });
    let w = ui.as_weak();
    g.on_refresh(move |id| {
        if let (Some(a), Some(ui)) = (find_app(&id), w.upgrade()) {
            let mut a = a.borrow_mut();
            a.refresh_button();
            a.push(&ui);
        }
    });
}

/// Confere tela e script antes de instalar (a loja chama).
pub fn check_files(dir: &Path, _ui: &Ui) -> Result<(), String> {
    check_view(dir)?;
    let script = std::fs::read_to_string(dir.join("main.rhai")).map_err(|e| format!("main.rhai: {e}"))?;
    let env = ScriptEnv { exec: Exec::new("check", &[]), net: Vec::new(), data: None, kv: true, db: None, migrate: None, back: None };
    engine(Default::default(), env, Default::default(), Default::default(), Default::default())
        .compile(&script)
        .map_err(|e| format!("main.rhai: {e}"))?;
    Ok(())
}


// ---- o mini-app ---------------------------------------------------------------------

/// O que o host manda à lógica de um plugin com tela (script ou app externo).
enum Input {
    Tick,
    Action(String, String),
    /// As opções do app, num objeto JSON.
    Settings(serde_json::Value),
    /// Leitura de um provedor de `uses`: nome do tópico e dados.
    Event(String, serde_json::Value),
    /// A janela abriu (só o app externo recebe).
    Shown,
}

/// Para onde vão as entradas: a thread do script ou o processo do app externo.
enum Sink {
    Script(Sender<ScriptIn>),
    Process(external::Process),
}

impl Sink {
    fn send(&self, msg: Input) {
        match self {
            Sink::Script(tx) => {
                let m = match msg {
                    Input::Tick => ScriptIn::Tick,
                    Input::Action(n, a) => ScriptIn::Action(n, a),
                    Input::Settings(v) => ScriptIn::Settings(json_to_dyn(v).try_cast::<rhai::Map>().unwrap_or_default()),
                    Input::Event(topic, data) => ScriptIn::Event(topic, json_to_dyn(data)),
                    Input::Shown => return,
                };
                let _ = tx.send(m);
            }
            Sink::Process(p) => p.send(&match msg {
                Input::Tick => ToApp::Tick,
                Input::Action(name, arg) => ToApp::Action { name, arg },
                Input::Settings(settings) => ToApp::Settings { settings },
                Input::Event(topic, data) => ToApp::Event { topic, data },
                Input::Shown => ToApp::Shown,
            }),
        }
    }

    /// Para: a thread do script (as tarefas assíncronas dela também têm o canal, então
    /// fechar o nosso lado não basta) ou o processo (`stop`, e encerra se não sair).
    fn stop(self) {
        match self {
            Sink::Script(tx) => {
                let _ = tx.send(ScriptIn::Stop);
            }
            Sink::Process(mut p) => p.stop(),
        }
    }
}

/// O que só um plugin com tela própria (ou um app externo) tem.
struct Full {
    def: ComponentDefinition,
    /// O `main.rhai` (`None` = app externo).
    source: Option<String>,
    /// A instância que está na tela agora (o container recria ao trocar de página).
    instance: Rc<RefCell<Option<ComponentInstance>>>,
    /// Últimos valores postos na tela, para repor numa instância nova.
    values: Rc<RefCell<HashMap<String, Value>>>,
    /// A lógica rodando (`None` = desligado, ou o processo saiu).
    sink: Rc<RefCell<Option<Sink>>>,
    /// Muda a cada início: recados de um processo antigo são ignorados.
    generation: Rc<Cell<u64>>,
    /// A lógica da última vez ainda roda (thread do script ou processo): `false` quando
    /// soltou os arquivos de dados.
    alive: RefCell<Option<Arc<AtomicBool>>>,
    factory: slint::ComponentFactory,
}

impl Full {
    fn send(&self, msg: Input) {
        if let Some(sink) = self.sink.borrow().as_ref() {
            sink.send(msg);
        }
    }

    fn stop(&self) {
        if let Some(sink) = self.sink.borrow_mut().take() {
            sink.stop();
        }
    }

    fn running(&self) -> bool {
        self.sink.borrow().is_some()
    }
}

pub struct PluginMini {
    host: Rc<Host>,
    plugin: Plugin,
    cards: Rc<VecModel<PluginCard>>,
    waker: Waker,
    timer: Timer,
    full: Option<Full>,
    /// Plugin simples: última saída (ou o erro) e quando chegou.
    last: Option<(Result<String, String>, Instant)>,
    running: bool,
    /// Plugin com tela: erro da tela ou do script, humor e resumo que o script deu.
    error: Option<String>,
    script_mood: Mood,
    script_summary: String,
    mood: Mood,
    face: HeaderMascot,
    /// Fatos para a conversa entre mascotes, até o hub recolher.
    facts: Vec<Fact>,
    /// Migração dos dados em curso: de que versão para qual (`core/appdata.rs`).
    migrating: Option<(u32, u32)>,
}

impl PluginMini {
    #[allow(deprecated)] // ComponentFactory: experimental no Slint 1.18
    fn create(host: Rc<Host>, plugin: Plugin, dir: &Path, cards: Rc<VecModel<PluginCard>>, waker: Waker) -> Rc<RefCell<Self>> {
        let seed = 20 + cards.row_count() as u32;
        let mut error: Option<String> = None;
        let full = (plugin.ui.is_some() || plugin.app.is_some()).then_some(()).and_then(|_| {
            // app externo: sem script (a lógica é o executável)
            let source = match plugin.ui {
                Some(_) => std::fs::read_to_string(dir.join("main.rhai")).map(Some).map_err(|e| format!("main.rhai: {e}")),
                None => Ok(None),
            };
            let view = std::fs::read_to_string(dir.join("view.slint"))
                .map_err(|e| format!("view.slint: {e}"))
                .and_then(|v| compile_view(dir, v));
            match (view, source) {
                (Ok(def), Ok(source)) => Some(Full {
                    def,
                    source,
                    instance: Default::default(),
                    values: Default::default(),
                    sink: Default::default(),
                    generation: Default::default(),
                    alive: Default::default(),
                    factory: slint::ComponentFactory::default(),
                }),
                (Err(e), _) | (_, Err(e)) => {
                    log::warn!("plugin {}: {e}", plugin.manifest.id);
                    error = Some(e);
                    None
                }
            }
        });
        let app = Rc::new(RefCell::new(PluginMini {
            host,
            plugin,
            cards,
            waker,
            timer: Timer::default(),
            full,
            last: None,
            running: false,
            error,
            script_mood: Mood::Idle,
            script_summary: String::new(),
            mood: Mood::Sleep,
            face: HeaderMascot::new(seed),
            facts: Vec::new(),
            migrating: None,
        }));
        // fábrica da tela: o container chama quando a célula aparece
        {
            let mut a = app.borrow_mut();
            if let Some(full) = a.full.as_mut() {
                let (def, slot, values, sink) = (full.def.clone(), full.instance.clone(), full.values.clone(), full.sink.clone());
                full.factory = slint::ComponentFactory::new(move |ctx| {
                    let inst = def.create_embedded(ctx).ok()?;
                    for (k, v) in values.borrow().iter() {
                        let _ = inst.set_property(k, v.clone());
                    }
                    let sink = sink.clone();
                    let _ = inst.set_callback("action", move |args: &[Value]| {
                        let s = |i: usize| match args.get(i) {
                            Some(Value::String(s)) => s.to_string(),
                            Some(Value::Number(n)) => n.to_string(),
                            _ => String::new(),
                        };
                        if let Some(sink) = sink.borrow().as_ref() {
                            sink.send(Input::Action(s(0), s(1)));
                        }
                        Value::Void
                    });
                    *slot.borrow_mut() = Some(inst.clone_strong());
                    Some(inst)
                });
            }
        }
        // o relógio do plugin: comando (simples), `tick()` (com tela) ou "tick" (app externo)
        let every = {
            let p = &app.borrow().plugin;
            match (&p.run, &p.ui, &p.app) {
                (Some(run), _, _) => Some(run.every),
                (None, Some(ui), _) => ui.every,
                (None, None, Some(a)) => a.every,
                _ => None,
            }
        };
        if let Some(every) = every {
            let weak: Weak<RefCell<PluginMini>> = Rc::downgrade(&app);
            let a = app.borrow();
            a.timer.start(TimerMode::Repeated, Duration::from_secs(every), move || {
                if let Some(a) = weak.upgrade() {
                    a.borrow_mut().refresh_now();
                }
            });
            // desligado não roda nada: `enable` liga o timer
            a.timer.stop();
        }
        app
    }

    /// Roda já: o comando (simples) ou o `tick()` do script (com tela).
    fn refresh_now(&mut self) {
        if let Some(full) = &self.full {
            full.send(Input::Tick);
            return;
        }
        let Some(run) = &self.plugin.run else { return };
        if self.running {
            return;
        }
        self.running = true;
        let reporter = self.host.reporter(&self.plugin.manifest);
        let args: Vec<&str> = run.command[1..].iter().map(String::as_str).collect();
        self.host.exec_for(&self.plugin.manifest).run_async(&run.command[0], &args, move |r| match r {
            Ok(out) => reporter.output(out),
            Err(e) => reporter.error(e.to_string()),
        });
    }

    /// Botão de atualizar do cartão: um app externo que parou (ou deu erro) começa de
    /// novo; o resto roda já.
    fn refresh_button(&mut self) {
        let stopped = self.full.as_ref().is_some_and(|f| !f.running());
        if self.plugin.app.is_some() && (stopped || self.error.is_some()) {
            self.start_logic();
            self.mood_changed(self.mood);
        } else {
            self.refresh_now();
        }
    }

    /// Liga a lógica de um plugin com tela: a thread do script ou o processo do app externo.
    fn start_logic(&mut self) {
        let Some(full) = &self.full else { return };
        full.stop();
        let generation = full.generation.get() + 1;
        full.generation.set(generation);
        self.error = None;
        let source = full.source.clone();
        // a versão dos dados antes de tudo: nova = migrar (com cópia); mais nova que o app = parar
        let spec = self.plugin.data();
        let prep = appdata::prepare(&appdata::root(), &self.plugin.manifest.id, spec.version);
        self.migrating = match prep {
            Ok(Prep::Migrate { from, to }) => Some((from, to)),
            _ => None,
        };
        let result = match (prep, source) {
            (Err(e), _) => Err(e),
            (Ok(_), Some(source)) => self.start_script(source),
            (Ok(_), None) => self.start_process(generation),
        };
        if let Some(full) = &self.full {
            match result {
                Ok(sink) => *full.sink.borrow_mut() = Some(sink),
                Err(e) => {
                    log::warn!("plugin {}: {e}", self.plugin.manifest.id);
                    self.error = Some(e);
                }
            }
        }
    }

    /// A thread do script (plugin com tela).
    fn start_script(&self, source: String) -> Result<Sink, String> {
        let (tx, rx) = channel();
        let m = &self.plugin.manifest;
        let (spec, dir) = (self.plugin.data(), appdata::dir(&m.id));
        let env = ScriptEnv {
            exec: self.host.exec_for(m),
            net: m.permissions.net.clone(),
            data: Some(dir.join(appdata::KV_FILE)),
            kv: spec.has(Kind::Kv),
            db: spec.has(Kind::Sqlite).then(|| DbSpec { path: dir.join(appdata::DB_FILE), max_bytes: spec.max_bytes() }),
            migrate: self.migrating,
            back: Some(tx.clone()),
        };
        let alive = Arc::new(AtomicBool::new(true));
        if let Some(full) = &self.full {
            *full.alive.borrow_mut() = Some(alive.clone());
        }
        let id = m.id.clone();
        let settings = json_to_dyn(self.settings_json()).try_cast::<rhai::Map>().unwrap_or_default();
        let waker = self.waker.clone();
        std::thread::Builder::new()
            .name(format!("plugin-{id}"))
            .spawn(move || {
                script_loop(source, env, settings, rx, move |batch| {
                    let id = id.clone();
                    let _ = slint::invoke_from_event_loop(move || deliver(&id, None, batch));
                    // o hub atualiza humor e bandeja mesmo com a janela fechada
                    waker.wake();
                });
                // o banco e o store.json fecharam junto com o laço
                alive.store(false, Ordering::SeqCst);
            })
            .map_err(|e| format!("sem thread para o script: {e}"))?;
        Ok(Sink::Script(tx))
    }

    /// O processo do app externo. As mensagens dele se juntam numa leva e vão para a
    /// thread da UI de uma vez (um app que manda muitas linhas não enche o event loop).
    fn start_process(&self, generation: u64) -> Result<Sink, String> {
        let m = &self.plugin.manifest;
        let program = self
            .plugin
            .binary_path()
            .ok_or_else(|| format!("este app não tem versão para {}", plugins::this_os()))?;
        let data_dir = appdata::dir(&m.id);
        let launch = external::Launch { id: &m.id, program: &program, dir: &self.plugin.dir, data_dir: &data_dir };
        let pending: Arc<Mutex<Vec<ScriptOut>>> = Default::default();
        let (id, waker) = (m.id.clone(), self.waker.clone());
        let process = external::Process::spawn(&launch, move |incoming| {
            let out = match incoming {
                Incoming::Msg(msg) => from_app(msg),
                Incoming::Exited(e) => ScriptOut::Exited(e),
            };
            let first = match pending.lock() {
                Ok(mut v) => {
                    v.push(out);
                    v.len() == 1
                }
                Err(_) => false,
            };
            if first {
                let (id, pending) = (id.clone(), pending.clone());
                let _ = slint::invoke_from_event_loop(move || {
                    let batch = pending.lock().map(|mut v| std::mem::take(&mut *v)).unwrap_or_default();
                    deliver(&id, Some(generation), batch);
                });
                waker.wake();
            }
        })?;
        if let Some(full) = &self.full {
            *full.alive.borrow_mut() = Some(process.ended());
        }
        process.send(&ToApp::Start {
            api: external::PROTOCOL,
            id: m.id.clone(),
            os: plugins::this_os(),
            data_dir: data_dir.to_string_lossy().into_owned(),
            data_version: self.migrating.map_or(self.plugin.data().version, |(from, _)| from),
            settings: self.settings_json(),
        });
        Ok(Sink::Process(process))
    }

    /// As opções do app, para o script ou o app externo (objeto JSON).
    fn settings_json(&self) -> serde_json::Value {
        let cfg = self.host.config.borrow();
        let mut m = serde_json::Map::new();
        for s in &self.plugin.manifest.settings {
            let v = match cfg.value(&self.plugin.manifest, &s.key) {
                toml::Value::Boolean(b) => b.into(),
                toml::Value::Integer(i) => i.into(),
                toml::Value::Float(f) => f.into(),
                other => other.as_str().unwrap_or_default().to_string().into(),
            };
            m.insert(s.key.clone(), v);
        }
        serde_json::Value::Object(m)
    }

    /// Aplica o que o script mandou (thread da UI).
    fn apply(&mut self, batch: Vec<ScriptOut>, ui: &HubWindow) {
        let before = self.mood;
        for msg in batch {
            match msg {
                ScriptOut::Set(name, v) => self.set_view(&name, v.to_value()),
                ScriptOut::Mood(m) => self.script_mood = mood_from(&m),
                ScriptOut::Summary(s) => self.script_summary = s,
                ScriptOut::Fact(kind, subject, value) => {
                    let voice = self.voice();
                    self.facts.push(Fact { kind, voice, subject, value });
                }
                ScriptOut::Notify(t, b) => {
                    let key = format!("plugin:{}", self.plugin.manifest.id);
                    self.host.notifier.borrow_mut().send(&key, &t, &b, false);
                }
                ScriptOut::Error(e) if e.is_empty() => self.error = None,
                ScriptOut::Error(e) => {
                    log::warn!("plugin {}: {e}", self.plugin.manifest.id);
                    self.error = Some(e);
                }
                ScriptOut::Migrated(r) => self.migrated(r),
                ScriptOut::Exited(e) => {
                    log::warn!("app {}: {e}", self.plugin.manifest.id);
                    // saiu no meio da migração: a cópia de antes volta
                    let e = match self.migrating {
                        Some(_) => self.undo_migration(&e),
                        None => e,
                    };
                    self.error = Some(e);
                    if let Some(full) = &self.full {
                        full.stop();
                    }
                }
            }
        }
        self.mood_changed(before);
        self.push(ui);
    }

    /// A lógica disse que a migração terminou (ou o script disse que falhou).
    fn migrated(&mut self, r: Result<u32, String>) {
        let id = self.plugin.manifest.id.clone();
        let Some((from, to)) = self.migrating else {
            log::warn!("app {id}: \"migrated\" sem migração pendente; ignorado");
            return;
        };
        match r {
            Ok(v) if v == to => {
                self.migrating = None;
                match appdata::set_version(&appdata::dir(&id), to) {
                    Ok(()) => log::info!("app {id}: dados migrados da versão {from} para {to}"),
                    Err(e) => self.error = Some(format!("dados migrados, mas a versão não foi gravada: {e}")),
                }
            }
            Ok(v) => log::warn!("app {id}: migrou para a versão {v}, mas o manifesto diz {to}; a migração continua pendente"),
            Err(e) => {
                let msg = self.undo_migration(&e);
                self.error = Some(msg);
                if let Some(full) = &self.full {
                    full.stop();
                }
            }
        }
    }

    /// Devolve a cópia de antes da migração; o texto do erro para o cartão.
    fn undo_migration(&mut self, why: &str) -> String {
        let (id, (from, to)) = (self.plugin.manifest.id.clone(), self.migrating.take().unwrap_or_default());
        log::warn!("app {id}: a migração dos dados ({from} → {to}) falhou: {why}");
        match appdata::restore(&appdata::root(), &id) {
            Ok(()) => format!("a migração dos dados (versão {from} → {to}) falhou e os dados voltaram como estavam: {why}"),
            Err(e) => format!("a migração dos dados (versão {from} → {to}) falhou ({why}) e a cópia não voltou: {e}"),
        }
    }

    /// Recalcula o humor: saiu do alarme, comemora; mudou de verdade, vira fato para a
    /// conversa.
    fn mood_changed(&mut self, before: Mood) {
        self.mood = self.compute_mood();
        if before == Mood::Alarm && self.mood != Mood::Alarm {
            self.face.react(Reaction::Celebrate);
        }
        if let Some(f) = chatter::mood_fact(before, self.mood, self.voice(), "", None) {
            self.facts.push(f);
        }
    }

    /// A mascote do plugin na conversa entre mascotes.
    fn voice(&self) -> Voice {
        let m = &self.plugin.manifest;
        Voice {
            app: m.id.clone(),
            name: m.name.clone(),
            sprite: sprites::get(&self.mascot_key()),
            theme: self.host.theme_for(m),
            mood: self.mood,
        }
    }

    /// Põe um valor na tela do plugin e guarda para uma instância nova.
    fn set_view(&self, name: &str, v: Value) {
        let Some(full) = &self.full else { return };
        if let Some(inst) = full.instance.borrow().as_ref()
            && let Err(e) = inst.set_property(name, v.clone()) {
                log::warn!("plugin {}: propriedade {name}: {e}", self.plugin.manifest.id);
            }
        full.values.borrow_mut().insert(name.to_string(), v);
    }

    /// Propriedades que o host preenche, se a tela declarar.
    fn push_host_props(&self) {
        let Some(full) = &self.full else { return };
        let declared: Vec<String> = full.def.properties().map(|(n, _)| n).collect();
        let put = |name: &str, v: Value| {
            if declared.iter().any(|d| d == name) {
                self.set_view(name, v);
            }
        };
        let (skin, scale) = {
            let cfg = self.host.config.borrow();
            (crate::skin::get(&cfg.skin), cfg.text_scale as f32 / 100.0)
        };
        put("theme", theme_value(self.host.theme_for(&self.plugin.manifest), skin, scale));
        put(FONT_PROP, Value::String(skin.font_ui.into()));
        put("mascot", Value::Image(self.face.image()));
        put("mood", Value::String(self.mood.as_str().into()));
    }

    fn compute_mood(&self) -> Mood {
        if self.error.is_some() {
            return Mood::Alarm;
        }
        if self.plugin.run.is_none() {
            return self.script_mood;
        }
        match &self.last {
            None => Mood::Sleep,
            Some((Err(_), _)) => Mood::Alarm,
            Some((Ok(text), _)) if text.trim().is_empty() => Mood::Sleep,
            Some((Ok(text), _)) => {
                let low = text.to_lowercase();
                let rule = self.plugin.moods.iter().find(|r| low.contains(&r.contains.to_lowercase()));
                rule.map_or(Mood::Idle, |r| mood_from(&r.mood))
            }
        }
    }

    fn text(&self) -> String {
        if let Some(e) = &self.error {
            return e.lines().next().unwrap_or_default().to_string();
        }
        match &self.last {
            None => "…".into(),
            Some((Err(e), _)) => e.lines().next().unwrap_or_default().to_string(),
            Some((Ok(t), _)) => {
                let lines: Vec<&str> = t.lines().map(str::trim).filter(|l| !l.is_empty()).take(self.plugin.lines()).collect();
                if lines.is_empty() { "(sem saída)".into() } else { lines.join("\n") }
            }
        }
    }

    fn detail(&self) -> String {
        if let Some(e) = &self.error {
            return e.clone();
        }
        let Some(run) = &self.plugin.run else { return String::new() };
        let every = format!("a cada {} s", run.every);
        match &self.last {
            _ if self.running => format!("rodando {}…", run.command.join(" ")),
            None => every,
            Some((Err(_), at)) => format!("falhou há {} s · {every}", at.elapsed().as_secs()),
            Some((Ok(_), at)) => format!("atualizado há {} s · {every}", at.elapsed().as_secs()),
        }
    }

    fn refresh_mascot(&mut self) -> bool {
        let key = self.mascot_key();
        self.face.refresh(sprites::get(&key), self.host.theme_for(&self.plugin.manifest), self.mood)
    }

    /// Leva o estado ao cartão (e, com tela própria, as propriedades do host à tela).
    #[allow(deprecated)]
    fn push(&mut self, _ui: &HubWindow) {
        self.refresh_mascot();
        self.push_host_props();
        let card = PluginCard {
            id: self.plugin.manifest.id.as_str().into(),
            title: self.plugin.title().into(),
            text: self.text().into(),
            detail: self.detail().into(),
            failed: self.error.is_some() || matches!(self.last, Some((Err(_), _))),
            running: self.running,
            mascot: self.face.image(),
            mood: self.mood.as_str().into(),
            reaction: self.face.reaction(),
            voice: Voice::id_of(&self.plugin.manifest.id, &self.plugin.manifest.name).into(),
            has_view: self.full.is_some(),
            view: self.full.as_ref().map(|f| f.factory.clone()).unwrap_or_default(),
        };
        let id = &self.plugin.manifest.id;
        if let Some(i) = (0..self.cards.row_count()).find(|&i| self.cards.row_data(i).is_some_and(|c| c.id == id.as_str()))
            && self.cards.row_data(i).as_ref() != Some(&card) {
                self.cards.set_row_data(i, card);
            }
    }
}

/// Entrega o que o script (ou o processo da geração `generation`) mandou ao app dele, na
/// thread da UI. Recado de um processo que já foi trocado por outro é descartado.
fn deliver(id: &str, generation: Option<u64>, batch: Vec<ScriptOut>) {
    let Some(app) = find_app(id) else { return };
    let current = app.borrow().full.as_ref().map(|f| f.generation.get());
    if generation.is_some() && generation != current {
        return;
    }
    let ui = SET.with(|_| crate::hub::ui_handle());
    if let Some(ui) = ui {
        app.borrow_mut().apply(batch, &ui);
    }
}

fn mood_from(key: &str) -> Mood {
    match key {
        "sleep" => Mood::Sleep,
        "happy" => Mood::Happy,
        "busy" => Mood::Busy,
        "alarm" => Mood::Alarm,
        _ => Mood::Idle,
    }
}

impl MiniApp for PluginMini {
    fn manifest(&self) -> &Manifest {
        &self.plugin.manifest
    }

    fn enable(&mut self, ui: &HubWindow) {
        self.timer.restart();
        if self.full.is_some() {
            self.start_logic();
        } else {
            self.refresh_now();
        }
        self.mood = self.compute_mood();
        self.push(ui);
    }

    fn disable(&mut self, _ui: &HubWindow) {
        self.timer.stop();
        if let Some(full) = &self.full {
            full.stop();
        }
        self.facts.clear();
    }

    fn data_spec(&self) -> appdata::DataSpec {
        self.plugin.data()
    }

    fn release_data(&mut self, _ui: &HubWindow) {
        if let Some(full) = &self.full {
            full.stop();
        }
    }

    fn data_released(&self) -> bool {
        let Some(full) = &self.full else { return true };
        full.alive.borrow().as_ref().is_none_or(|a| !a.load(Ordering::SeqCst))
    }

    fn reload_data(&mut self, ui: &HubWindow) {
        if self.full.is_some() && self.host.config.borrow().is_enabled(&self.plugin.manifest.id) {
            self.start_logic();
            self.mood_changed(self.mood);
        }
        self.push(ui);
    }

    /// Leitura de um provedor de `uses`: vai ao `on_event` do script, como mapa (ou ao app
    /// externo, como mensagem "event").
    fn on_event(&mut self, ev: &Event, _ui: &HubWindow) {
        if let Some(full) = &self.full {
            full.send(Input::Event(ev.topic().name().into(), event_data(ev)));
        }
    }

    fn window_shown(&mut self, _ui: &HubWindow) {
        if let Some(full) = &self.full {
            full.send(Input::Shown);
        }
    }

    fn on_report(&mut self, report: &Report, ui: &HubWindow) {
        self.running = false;
        let before = self.mood;
        self.last = Some((
            match report {
                Report::Output(text) => Ok(text.clone()),
                Report::Done => Ok(String::new()),
                Report::Error(e) => Err(e.clone()),
            },
            Instant::now(),
        ));
        self.mood_changed(before);
        self.push(ui);
    }

    fn tick(&mut self, frame: u32, ui: &HubWindow) {
        // "atualizado há N s" anda sozinho; uma vez por segundo basta
        if self.refresh_mascot() || (self.plugin.run.is_some() && frame.is_multiple_of(8)) {
            self.push(ui);
        }
    }

    fn settings_changed(&mut self, ui: &HubWindow) {
        if let Some(full) = &self.full {
            full.send(Input::Settings(self.settings_json()));
        }
        self.push(ui);
    }

    fn take_facts(&mut self) -> Vec<Fact> {
        std::mem::take(&mut self.facts)
    }

    fn voices(&self) -> Vec<Voice> {
        vec![self.voice()]
    }

    fn mood(&self) -> Mood {
        self.mood
    }

    fn mascot_key(&self) -> String {
        self.host.config.borrow().string(&self.plugin.manifest, "mascot")
    }

    fn summary(&self) -> String {
        let line = if self.plugin.run.is_none() && self.error.is_none() {
            self.script_summary.lines().next().unwrap_or_default().to_string()
        } else {
            self.text().lines().next().unwrap_or_default().to_string()
        };
        format!("{}: {line}", self.plugin.manifest.name)
    }
}

impl Drop for PluginMini {
    fn drop(&mut self) {
        if let Some(full) = &self.full {
            full.stop();
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    /// O exemplo de banco: cria a tabela, guarda, marca e migra dados da versão 1.
    #[test]
    fn exemplo_com_banco_guarda_e_migra() {
        let dir = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/plugin-notas"));
        let p = Plugin::parse(&std::fs::read_to_string(dir.join("manifest.toml")).unwrap()).unwrap();
        check_files(dir, p.ui.as_ref().unwrap()).unwrap();
        let spec = p.data();
        assert!(spec.has(Kind::Sqlite) && !spec.has(Kind::Kv) && spec.version == 2);
        assert!(p.grants().contains("banco SQLite"));
        let dados = std::env::temp_dir().join(format!("miniutils-notas-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dados);
        let script = std::fs::read_to_string(dir.join("main.rhai")).unwrap();
        let rodar = |migrate: Option<(u32, u32)>, entradas: Vec<ScriptIn>| {
            let (tx, rx) = channel();
            for e in entradas {
                tx.send(e).unwrap();
            }
            drop(tx);
            let env = ScriptEnv {
                exec: Exec::new("t", &[]),
                net: Vec::new(),
                data: None,
                kv: false,
                db: Some(DbSpec { path: dados.join(appdata::DB_FILE), max_bytes: spec.max_bytes() }),
                migrate,
                back: None,
            };
            let saida = Arc::new(Mutex::new(Vec::new()));
            let s = saida.clone();
            script_loop(script.clone(), env, rhai::Map::new(), rx, move |b| s.lock().unwrap().extend(b));
            let txt = format!("{:?}", saida.lock().unwrap());
            assert!(!txt.contains("Error("), "{txt}");
            txt
        };
        let txt = rodar(None, vec![ScriptIn::Action("add".into(), "  comprar pão ".into()), ScriptIn::Action("toggle".into(), "1".into())]);
        assert!(txt.contains(r#"Str("comprar pão")"#), "{txt}");
        assert!(txt.contains("0 para fazer · 1 feita(s)"), "{txt}");
        // um banco da versão 1 (sem a coluna "feita") migra antes de começar
        let _ = std::fs::remove_dir_all(&dados);
        std::fs::create_dir_all(&dados).unwrap();
        rusqlite::Connection::open(dados.join(appdata::DB_FILE))
            .unwrap()
            .execute_batch("CREATE TABLE notas (id INTEGER PRIMARY KEY, texto TEXT NOT NULL, criada INTEGER NOT NULL); INSERT INTO notas VALUES (1, 'velha', 0);")
            .unwrap();
        let txt = rodar(Some((1, 2)), vec![]);
        assert!(txt.contains("Migrated(Ok(2))") && txt.contains("1 para fazer · 0 feita(s)"), "{txt}");
        let _ = std::fs::remove_dir_all(&dados);
    }

    #[test]
    fn exemplo_com_tela_e_script_funciona() {
        let dir = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/plugin-top"));
        let p = Plugin::parse(&std::fs::read_to_string(dir.join("manifest.toml")).unwrap()).unwrap();
        check_files(dir, p.ui.as_ref().unwrap()).unwrap();
        let def = compile_view(dir, std::fs::read_to_string(dir.join("view.slint")).unwrap()).unwrap();
        let props: Vec<String> = def.properties().map(|(n, _)| n).collect();
        assert!(props.iter().any(|n| n == "rows") && props.iter().any(|n| n == "theme"), "{props:?}");
        // o script recebe os processos do provedor `procs` (aqui, uma leitura de mentira)
        assert_eq!(p.manifest.uses, [crate::core::bus::Topic::Procs]);
        let proc = |pid, name: &str, cpu, mem| crate::providers::procs::Proc { pid, name: name.into(), cpu, mem, mem_bytes: 1 };
        let snap = crate::providers::procs::snapshot(vec![proc(1, "firefox", 30.0, 12.5), proc(2, "code", 2.0, 20.0)]);
        let (tx, rx) = channel();
        tx.send(ScriptIn::Event("procs".into(), json_to_dyn(event_data(&Event::Procs(snap))))).unwrap();
        tx.send(ScriptIn::Action("sort".into(), "mem".into())).unwrap();
        drop(tx);
        let saida = Arc::new(Mutex::new(Vec::new()));
        let s = saida.clone();
        let script = std::fs::read_to_string(dir.join("main.rhai")).unwrap();
        let e = ScriptEnv {
            exec: Exec::new("t", &p.manifest.permissions.exec),
            net: Vec::new(),
            data: None,
            kv: true,
            db: None,
            migrate: None,
            back: None,
        };
        script_loop(script, e, rhai::Map::new(), rx, move |b| s.lock().unwrap().extend(b));
        let txt = format!("{:?}", saida.lock().unwrap());
        assert!(!txt.contains("Error("), "{txt}");
        assert!(txt.contains(r#"Set("rows", List([Map("#), "{txt}");
        assert!(txt.contains(r#"Set("sort", Str("mem"))"#), "{txt}");
        assert!(txt.contains(r#"Str("30%")"#) || txt.contains(r#"Str("30.0%")"#), "{txt}");
        assert!(txt.contains("2 processos"), "{txt}");
        assert!(txt.contains(r#"Summary("code 20.0% de memória")"#) || txt.contains(r#"Summary("code 20% de memória")"#), "{txt}");
    }
}
