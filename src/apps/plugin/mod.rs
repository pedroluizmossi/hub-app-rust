//! Mini-apps of community plugins (see `core/plugins.rs` and `store.rs`).
//!
//! The external app (`[app]`, full access) uses the same screen and the same state as
//! plugins with a screen; instead of the script, a process of its own (`apps/external.rs`).
//!
//! Two kinds without native code:
//! - **simple** (`[run]`): the host runs the manifest's command (allowlist, no shell) every
//!   `every` seconds and shows the output on the generic screen (`ui/plugin.slint`);
//! - **with its own screen** (`[ui]`): the plugin's `view.slint` is interpreted
//!   (`slint-interpreter`) and embedded in the cell by a `ComponentContainer`; the logic is
//!   `main.rhai`, which runs on a thread of its own and talks to the screen through messages.
//!
//! The script never touches the UI or the disk. What it can do, through the API below:
//! change properties of the screen, set the mood, run the allowlisted programs (with a time
//! limit), query the domains of `[permissions] net`, get the readings of the `uses`
//! providers, store data (key-value, in a file the host writes, and an SQLite database the
//! host opens, both in the app's data folder: `core/appdata.rs`), send notifications, tell
//! facts for the chat between mascots and read its own options.
//!
//! Script API (`main.rhai`):
//! ```text
//! fn start()                  on enable         this.settings = the app's options (map)
//! fn tick()                   every [ui] every
//! fn action(name, arg)        the screen called `callback action(string, string)`
//! fn on_event(topic, data)    reading from a `uses` provider ("sys", "docker"…), as a map;
//!                             "procs" brings the heaviest processes (by_cpu, by_mem and
//!                             count), the same on Linux and Windows: use it instead of `ps`
//! fn settings()               the user changed an option (this.settings is already the new one)
//! ui("prop", value)           changes a property of the screen (text, number, bool, list, map)
//! mood("happy")               mood: sleep, idle, happy, busy, alarm
//! summary("text")             line in the tray tooltip
//! run(["git", "status"])     runs an allowlisted program: #{ ok, out, err } (up to 30 s)
//! run_async(["git"], "done")  the same without waiting: calls fn done(result) afterwards
//! http_get(url)               https GET on a net domain: #{ ok, status, body, err }
//! http_get_async(url, "done") the same without waiting (up to 4 tasks at the same time)
//! parse_json(text)  to_json(value)
//! store_get("k")  store_set("k", value)   data that survives turning off (() deletes);
//!                             kv in [data] (without [data], the script already has it)
//! db_batch(sql)  db_exec(sql, [params])  db_query(sql, [params])
//!                             SQLite database (sqlite in [data]; details in db.rs)
//! fn migrate(from, to)        the [data] version went up: migrate the data before start()
//!                             (error = the host restores the copy from before and the app
//!                             doesn't start)
//! fact("strain", "CPU", 93)   fact for the chat: trouble, strain, forecast, recovered, poked
//! notify("title", "text")
//! now()                       seconds since 1970
//! ```
//! State between calls lives in `this` (e.g. `this.count += 1`). The host already tells the
//! chat about mood changes (alarm, busy, back to normal) and pokes on the mascot.
//!
//! The screen (`view.slint`) exports `component Main`; it can import `std-widgets.slint`,
//! `miniutils.slint` (the `Theme`, below) and the screens in the plugin's own `ui/`; images
//! only from `assets/` (`@image-url("assets/logo.png")`). The host fills in, if the screen
//! declares them: `in property <Theme> theme` (colors of the app's theme and fonts of the
//! look: `theme.font-ui`, `font-mono`, `font-title`), `in property <image> mascot`,
//! `in property <string> mood`. The screen's background is transparent and the text already
//! uses the look's interface font (see `compile_view`).

mod db;
mod script;
mod view;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
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
use crate::core::plugins::{self, Plugin};
use crate::core::registry::Registry;
use crate::core::wake::Waker;
use crate::mascot::anim::Reaction;
use crate::mascot::{Mood, sprites};
use crate::tr;
use crate::ui::{HubWindow, PluginApps, PluginCard};
use db::DbSpec;
use script::{ScriptEnv, ScriptIn, ScriptOut, engine, event_data, from_app, json_to_dyn, script_loop};
use view::{FONT_PROP, compile_view, theme_value};
pub use view::check_view;

// ---- set of loaded plugins (UI thread) -----------------------------------------------

struct Loaded {
    id: String,
    app: Rc<RefCell<PluginMini>>,
    /// Changes when the plugin's files change (an update): then the app is recreated.
    stamp: u64,
}

#[derive(Default)]
struct PluginSet {
    cards: Option<Rc<VecModel<PluginCard>>>,
    loaded: Vec<Loaded>,
    /// Dev mode: checks the plugin folders every second and reloads when one changes.
    dev_watch: Option<Timer>,
}

thread_local! {
    static SET: RefCell<PluginSet> = RefCell::new(PluginSet::default());
}

fn find_app(id: &str) -> Option<Rc<RefCell<PluginMini>>> {
    SET.with(|s| s.borrow().loaded.iter().find(|l| l.id == id).map(|l| l.app.clone()))
}

/// Reads the plugins folder again and updates the registry: adds the new ones, recreates
/// the changed ones and takes out the removed ones. `startup` = while the hub is being set
/// up (the registry enables the apps afterwards); otherwise, whatever was enabled is
/// enabled again right away. Returns whether anything changed.
pub fn sync(host: &Rc<Host>, ui: &HubWindow, registry: &Registry, startup: bool) -> bool {
    let cards = SET.with(|s| s.borrow().cards.clone()).unwrap_or_else(|| {
        let cards = Rc::new(VecModel::<PluginCard>::default());
        ui.global::<PluginApps>().set_cards(ModelRc::from(cards.clone()));
        bind(ui);
        SET.with(|s| {
            let mut s = s.borrow_mut();
            s.cards = Some(cards.clone());
            s.dev_watch = (!plugins::dev_dirs().is_empty()).then(|| watch_dev(plugins::dev_dirs()));
        });
        cards
    });
    let root = plugins::dir();
    let installed = plugins::load_all(&root);
    let live_ui = (!startup).then_some(ui);
    let mut changed = false;

    // removed (or changed: they leave and come back)
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
    // new ones
    for (plugin, (id, st)) in installed.into_iter().zip(stamps) {
        if SET.with(|s| s.borrow().loaded.iter().any(|l| l.id == id)) {
            continue;
        }
        // the plugin under development comes in already enabled, on the first page
        if plugins::is_dev(&plugin.dir) {
            log::info!("plugin-dev: {id} loaded from {}", plugin.dir.display());
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

/// Fingerprint of the plugin's files (to notice an update).
fn stamp(dir: &Path) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    std::fs::read(dir.join("manifest.toml")).ok().hash(&mut h);
    for rel in plugins::files(dir).unwrap_or_default() {
        rel.hash(&mut h);
        if plugins::is_bin(&rel) {
            // executable: size and date are enough (and it doesn't read megabytes every second)
            let meta = std::fs::metadata(dir.join(&rel)).ok();
            meta.map(|m| (m.len(), m.modified().ok())).hash(&mut h);
        } else {
            std::fs::read(dir.join(&rel)).ok().hash(&mut h);
        }
    }
    h.finish()
}

/// Dev mode: every second, if any of the plugin's files changed, reads the plugins again
/// (the screen's and the script's errors go to the terminal).
fn watch_dev(dirs: &'static [PathBuf]) -> Timer {
    let timer = Timer::default();
    let last = RefCell::new(dirs.iter().map(|d| stamp(d)).collect::<Vec<_>>());
    timer.start(TimerMode::Repeated, Duration::from_secs(1), move || {
        let now: Vec<u64> = dirs.iter().map(|d| stamp(d)).collect();
        let before = last.replace(now.clone());
        for (dir, _) in dirs.iter().zip(now.iter().zip(&before)).filter(|(_, (n, b))| n != b) {
            log::info!("plugin-dev: files changed in {}, reloading", dir.display());
        }
        if now != before {
            crate::hub::request_plugin_reload();
        }
    });
    timer
}

/// Stops a plugin's logic (the external app's process, the script thread) before the store
/// replaces or deletes its folder or its data (on Windows, a running executable locks the
/// folder; the script holds the database). See `released` and `resume`.
pub fn suspend(id: &str) {
    if let Some(a) = find_app(id)
        && let Some(full) = &a.borrow().full
    {
        full.stop();
    }
}

/// The logic stopped by `suspend` has already let go of the files.
pub fn released(id: &str) -> bool {
    find_app(id).is_none_or(|a| a.borrow().data_released())
}

/// After the store: restarts an enabled app that was left stopped (the store failed, or
/// replaced nothing). If the store replaced the app, the new one is already running and
/// nothing happens.
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

/// The `PluginApps` callbacks are shared by all plugins: the id says which one.
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

/// Checks the screen and the script before installing (the store calls this).
pub fn check_files(dir: &Path) -> Result<(), String> {
    check_view(dir)?;
    let script = std::fs::read_to_string(dir.join("main.rhai")).map_err(|e| format!("main.rhai: {e}"))?;
    let env = ScriptEnv { exec: Exec::new("check", &[]), net: Vec::new(), data: None, kv: true, db: None, migrate: None, back: None, catalog: Default::default() };
    engine(Default::default(), env, Default::default(), Default::default(), Default::default())
        .compile(&script)
        .map_err(|e| format!("main.rhai: {e}"))?;
    Ok(())
}


// ---- the mini-app -------------------------------------------------------------------

/// What the host sends to the logic of a plugin with a screen (script or external app).
enum Input {
    Tick,
    Action(String, String),
    /// The app's options, as a JSON object.
    Settings(serde_json::Value),
    /// Reading from a `uses` provider: topic name and data.
    Event(String, serde_json::Value),
    /// The window opened (only the external app gets it).
    Shown,
}

/// Where the inputs go: the script thread or the external app's process.
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

    /// Stops: the script thread (its async tasks also hold the channel, so closing our side
    /// isn't enough) or the process (`stop`, and kills it if it doesn't exit).
    fn stop(self) {
        match self {
            Sink::Script(tx) => {
                let _ = tx.send(ScriptIn::Stop);
            }
            Sink::Process(mut p) => p.stop(),
        }
    }
}

/// What only a plugin with its own screen (or an external app) has.
struct Full {
    def: ComponentDefinition,
    /// The `main.rhai` (`None` = external app).
    source: Option<String>,
    /// The instance on screen now (the container recreates it when the page changes).
    instance: Rc<RefCell<Option<ComponentInstance>>>,
    /// Last values put on the screen, to restore on a new instance.
    values: Rc<RefCell<HashMap<String, Value>>>,
    /// The running logic (`None` = off, or the process exited).
    sink: Rc<RefCell<Option<Sink>>>,
    /// Changes on every start: messages from an old process are ignored.
    generation: Rc<Cell<u64>>,
    /// The logic from last time is still running (script thread or process): `false` once
    /// it let go of the data files.
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
    /// Simple plugin: last output (or the error) and when it arrived.
    last: Option<(Result<String, String>, Instant)>,
    running: bool,
    /// Plugin with a screen: error of the screen or the script, mood and summary the script gave.
    error: Option<String>,
    script_mood: Mood,
    script_summary: String,
    mood: Mood,
    face: HeaderMascot,
    /// Facts for the chat between mascots, until the hub collects them.
    facts: Vec<Fact>,
    /// Data migration in progress: from which version to which (`core/appdata.rs`).
    migrating: Option<(u32, u32)>,
}

impl PluginMini {
    #[allow(deprecated)] // ComponentFactory: experimental in Slint 1.18
    fn create(host: Rc<Host>, plugin: Plugin, dir: &Path, cards: Rc<VecModel<PluginCard>>, waker: Waker) -> Rc<RefCell<Self>> {
        let seed = 20 + cards.row_count() as u32;
        let mut error: Option<String> = None;
        let full = (plugin.ui.is_some() || plugin.app.is_some()).then_some(()).and_then(|_| {
            // external app: no script (the logic is the executable)
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
        // screen factory: the container calls it when the cell shows up
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
        // the plugin's clock: command (simple), `tick()` (with a screen) or "tick" (external app)
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
            // when off nothing runs: `enable` starts the timer
            a.timer.stop();
        }
        app
    }

    /// Runs now: the command (simple) or the script's `tick()` (with a screen).
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

    /// The card's refresh button: an external app that stopped (or failed) starts again;
    /// the rest runs now.
    fn refresh_button(&mut self) {
        let stopped = self.full.as_ref().is_some_and(|f| !f.running());
        if self.plugin.app.is_some() && (stopped || self.error.is_some()) {
            self.start_logic();
            self.mood_changed(self.mood);
        } else {
            self.refresh_now();
        }
    }

    /// Starts the logic of a plugin with a screen: the script thread or the external app's process.
    fn start_logic(&mut self) {
        let Some(full) = &self.full else { return };
        full.stop();
        let generation = full.generation.get() + 1;
        full.generation.set(generation);
        self.error = None;
        let source = full.source.clone();
        // the data version before anything: new = migrate (with a copy); newer than the app = stop
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

    /// The script thread (plugin with a screen).
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
            catalog: Arc::new(crate::core::i18n::Catalog::of_app(&self.plugin.dir, crate::core::i18n::current())),
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
                    // the hub updates the mood and the tray even with the window closed
                    waker.wake();
                });
                // the database and store.json closed together with the loop
                alive.store(false, Ordering::SeqCst);
            })
            .map_err(|e| tr!("no thread for the script: {}", e))?;
        Ok(Sink::Script(tx))
    }

    /// The external app's process. Its messages are gathered into a batch and go to the UI
    /// thread at once (an app that sends many lines doesn't flood the event loop).
    fn start_process(&self, generation: u64) -> Result<Sink, String> {
        let m = &self.plugin.manifest;
        let program = self
            .plugin
            .binary_path()
            .ok_or_else(|| tr!("this app has no version for {}", plugins::this_os()))?;
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
            lang: crate::core::i18n::current().tag(),
            settings: self.settings_json(),
        });
        Ok(Sink::Process(process))
    }

    /// The app's options, for the script or the external app (JSON object).
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

    /// Applies what the script sent (UI thread).
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
                    // exited in the middle of the migration: the copy from before is restored
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

    /// The logic said the migration finished (or the script said it failed).
    fn migrated(&mut self, r: Result<u32, String>) {
        let id = self.plugin.manifest.id.clone();
        let Some((from, to)) = self.migrating else {
            log::warn!("app {id}: \"migrated\" without a pending migration; ignored");
            return;
        };
        match r {
            Ok(v) if v == to => {
                self.migrating = None;
                match appdata::set_version(&appdata::dir(&id), to) {
                    Ok(()) => log::info!("app {id}: data migrated from version {from} to {to}"),
                    Err(e) => self.error = Some(tr!("data migrated, but the version wasn't saved: {}", e)),
                }
            }
            Ok(v) => log::warn!("app {id}: migrated to version {v}, but the manifest says {to}; the migration is still pending"),
            Err(e) => {
                let msg = self.undo_migration(&e);
                self.error = Some(msg);
                if let Some(full) = &self.full {
                    full.stop();
                }
            }
        }
    }

    /// Restores the copy from before the migration; returns the error text for the card.
    fn undo_migration(&mut self, why: &str) -> String {
        let (id, (from, to)) = (self.plugin.manifest.id.clone(), self.migrating.take().unwrap_or_default());
        log::warn!("app {id}: the data migration ({from} → {to}) failed: {why}");
        match appdata::restore(&appdata::root(), &id) {
            Ok(()) => tr!("the data migration (version {} → {}) failed and the data was restored as it was: {}", from, to, why),
            Err(e) => tr!("the data migration (version {} → {}) failed ({}) and the copy wasn't restored: {}", from, to, why, e),
        }
    }

    /// Recomputes the mood: out of alarm, it celebrates; a real change becomes a fact for
    /// the chat.
    fn mood_changed(&mut self, before: Mood) {
        self.mood = self.compute_mood();
        if before == Mood::Alarm && self.mood != Mood::Alarm {
            self.face.react(Reaction::Celebrate);
        }
        if let Some(f) = chatter::mood_fact(before, self.mood, self.voice(), "", None) {
            self.facts.push(f);
        }
    }

    /// The plugin's mascot in the chat between mascots.
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

    /// Puts a value on the plugin's screen and keeps it for a new instance.
    fn set_view(&self, name: &str, v: Value) {
        let Some(full) = &self.full else { return };
        if let Some(inst) = full.instance.borrow().as_ref()
            && let Err(e) = inst.set_property(name, v.clone()) {
                log::warn!("plugin {}: property {name}: {e}", self.plugin.manifest.id);
            }
        full.values.borrow_mut().insert(name.to_string(), v);
    }

    /// Properties the host fills in, if the screen declares them.
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
                if lines.is_empty() { tr!("(no output)").into() } else { lines.join("\n") }
            }
        }
    }

    fn detail(&self) -> String {
        if let Some(e) = &self.error {
            return e.clone();
        }
        let Some(run) = &self.plugin.run else { return String::new() };
        let every = tr!("every {} s", run.every);
        match &self.last {
            _ if self.running => tr!("running {}…", run.command.join(" ")),
            None => every,
            Some((Err(_), at)) => tr!("failed {} s ago · {}", at.elapsed().as_secs(), every),
            Some((Ok(_), at)) => tr!("updated {} s ago · {}", at.elapsed().as_secs(), every),
        }
    }

    fn refresh_mascot(&mut self) -> bool {
        let key = self.mascot_key();
        self.face.refresh(sprites::get(&key), self.host.theme_for(&self.plugin.manifest), self.mood)
    }

    /// Takes the state to the card (and, with its own screen, the host's properties to the screen).
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

/// Delivers what the script (or the process of generation `generation`) sent to its app, on
/// the UI thread. A message from a process that has since been replaced is dropped.
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

    fn process_id(&self) -> Option<u32> {
        match self.full.as_ref()?.sink.borrow().as_ref()? {
            Sink::Process(p) => p.pid(),
            Sink::Script(_) => None,
        }
    }

    fn reload_data(&mut self, ui: &HubWindow) {
        if self.full.is_some() && self.host.config.borrow().is_enabled(&self.plugin.manifest.id) {
            self.start_logic();
            self.mood_changed(self.mood);
        }
        self.push(ui);
    }

    /// Reading from a `uses` provider: goes to the script's `on_event`, as a map (or to the
    /// external app, as an "event" message).
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
        // "updated N s ago" moves on its own; once a second is enough
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

    /// The database example: creates the table, stores, checks and migrates data from version 1.
    #[test]
    fn database_example_stores_and_migrates() {
        let dir = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/plugin-notes"));
        let p = Plugin::parse(&std::fs::read_to_string(dir.join("manifest.toml")).unwrap()).unwrap();
        check_files(dir).unwrap();
        let spec = p.data();
        assert!(spec.has(Kind::Sqlite) && !spec.has(Kind::Kv) && spec.version == 3);
        assert!(p.grants().contains("SQLite"));
        let data = std::env::temp_dir().join(format!("miniutils-notes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&data);
        let script = std::fs::read_to_string(dir.join("main.rhai")).unwrap();
        let run = |migrate: Option<(u32, u32)>, inputs: Vec<ScriptIn>| {
            let (tx, rx) = channel();
            for e in inputs {
                tx.send(e).unwrap();
            }
            drop(tx);
            let env = ScriptEnv {
                exec: Exec::new("t", &[]),
                net: Vec::new(),
                data: None,
                kv: false,
                db: Some(DbSpec { path: data.join(appdata::DB_FILE), max_bytes: spec.max_bytes() }),
                migrate,
                back: None,
                catalog: Default::default(),
            };
            let output = Arc::new(Mutex::new(Vec::new()));
            let s = output.clone();
            script_loop(script.clone(), env, rhai::Map::new(), rx, move |b| s.lock().unwrap().extend(b));
            let txt = format!("{:?}", output.lock().unwrap());
            assert!(!txt.contains("Error("), "{txt}");
            txt
        };
        let txt = run(None, vec![ScriptIn::Action("add".into(), "  buy bread ".into()), ScriptIn::Action("toggle".into(), "1".into())]);
        assert!(txt.contains(r#"Str("buy bread")"#), "{txt}");
        assert!(txt.contains(r#"Set("todo", Num(0.0))"#) && txt.contains(r#"Set("done", Num(1.0))"#), "{txt}");
        // a version 1 database (Portuguese names, without the "feita" column) migrates before starting
        let _ = std::fs::remove_dir_all(&data);
        std::fs::create_dir_all(&data).unwrap();
        rusqlite::Connection::open(data.join(appdata::DB_FILE))
            .unwrap()
            .execute_batch("CREATE TABLE notas (id INTEGER PRIMARY KEY, texto TEXT NOT NULL, criada INTEGER NOT NULL); INSERT INTO notas VALUES (1, 'old', 0);")
            .unwrap();
        let txt = run(Some((1, 3)), vec![]);
        assert!(txt.contains("Migrated(Ok(3))") && txt.contains(r#"Set("todo", Num(1.0))"#) && txt.contains(r#"Str("old")"#), "{txt}");
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn example_with_screen_and_script_works() {
        let dir = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/plugin-top"));
        let p = Plugin::parse(&std::fs::read_to_string(dir.join("manifest.toml")).unwrap()).unwrap();
        check_files(dir).unwrap();
        let def = compile_view(dir, std::fs::read_to_string(dir.join("view.slint")).unwrap()).unwrap();
        let props: Vec<String> = def.properties().map(|(n, _)| n).collect();
        assert!(props.iter().any(|n| n == "rows") && props.iter().any(|n| n == "theme"), "{props:?}");
        // the script gets the processes from the `procs` provider (here, a fake reading)
        assert_eq!(p.manifest.uses, [crate::core::bus::Topic::Procs]);
        let proc = |pid, name: &str, cpu, mem| crate::providers::procs::Proc { pid, name: name.into(), cpu, mem, mem_bytes: 1 };
        let snap = crate::providers::procs::snapshot(vec![proc(1, "firefox", 30.0, 12.5), proc(2, "code", 2.0, 20.0)]);
        let (tx, rx) = channel();
        tx.send(ScriptIn::Event("procs".into(), json_to_dyn(event_data(&Event::Procs(snap))))).unwrap();
        tx.send(ScriptIn::Action("sort".into(), "mem".into())).unwrap();
        drop(tx);
        let output = Arc::new(Mutex::new(Vec::new()));
        let s = output.clone();
        let script = std::fs::read_to_string(dir.join("main.rhai")).unwrap();
        let e = ScriptEnv {
            exec: Exec::new("t", &p.manifest.permissions.exec),
            net: Vec::new(),
            data: None,
            kv: true,
            db: None,
            migrate: None,
            back: None,
            catalog: Default::default(),
        };
        script_loop(script, e, rhai::Map::new(), rx, move |b| s.lock().unwrap().extend(b));
        let txt = format!("{:?}", output.lock().unwrap());
        assert!(!txt.contains("Error("), "{txt}");
        assert!(txt.contains(r#"Set("rows", List([Map("#), "{txt}");
        assert!(txt.contains(r#"Set("sort", Str("mem"))"#), "{txt}");
        assert!(txt.contains(r#"Str("30%")"#) || txt.contains(r#"Str("30.0%")"#), "{txt}");
        assert!(txt.contains(r#"Set("count", Num(2.0))"#), "{txt}");
        assert!(txt.contains(r#"Summary("code 20.0% memory")"#) || txt.contains(r#"Summary("code 20% memory")"#), "{txt}");
    }
}
