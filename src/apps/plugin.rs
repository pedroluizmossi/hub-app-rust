//! Mini-apps dos plugins da comunidade (ver `core/plugins.rs` e `store.rs`).
//!
//! Dois tipos, nenhum com código nativo:
//! - **simples** (`[run]`): o host roda o comando do manifesto (allowlist, sem shell) a cada
//!   `every` segundos e mostra a saída na tela genérica (`ui/plugin.slint`);
//! - **com tela própria** (`[ui]`): o `view.slint` do plugin é interpretado
//!   (`slint-interpreter`) e encaixado na célula por um `ComponentContainer`; a lógica é o
//!   `main.rhai`, que roda numa thread só dele e conversa com a tela por mensagens.
//!
//! O script nunca toca a UI nem o disco. O que ele pode, pela API abaixo: mudar
//! propriedades da tela, dizer o humor, rodar os programas da allowlist (com prazo),
//! mandar notificação e ler as próprias opções.
//!
//! API do script (`main.rhai`):
//! ```text
//! fn start()             ao ligar          this.settings = opções do app (mapa)
//! fn tick()              a cada [ui] every
//! fn action(name, arg)   a tela chamou `callback action(string, string)`
//! ui("prop", valor)      muda uma propriedade da tela (texto, número, bool, lista, mapa)
//! mood("happy")          humor: sleep, idle, happy, busy, alarm
//! summary("texto")       linha do tooltip da bandeja
//! run(["ps", "-e"])      roda um programa da allowlist: #{ ok, out, err }
//! notify("título", "texto")
//! now()                  segundos desde 1970
//! ```
//! Estado entre chamadas fica em `this` (ex.: `this.count += 1`).
//!
//! A tela (`view.slint`) exporta `component Main`; pode importar `std-widgets.slint` e
//! `miniutils.slint` (o `Theme`, abaixo), nada do disco. O host preenche, se a tela
//! declarar: `in property <Theme> theme` (cores do tema do app e fontes do visual:
//! `theme.font-ui`, `font-mono`, `font-title`), `in property <image> mascot`, `in property
//! <string> mood`. O fundo da tela é transparente e o texto já usa a fonte da interface do
//! visual (ver `compile_view`).

use std::cell::RefCell;
use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::rc::{Rc, Weak};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Model, ModelRc, Timer, TimerMode, VecModel};
use slint_interpreter::{ComponentDefinition, ComponentInstance, Value};

use super::kit::HeaderMascot;
use crate::core::bus::Event;
use crate::core::exec::Exec;
use crate::core::manifest::Manifest;
use crate::core::miniapp::{AppRef, Host, MiniApp, Report};
use crate::core::plugins::{self, Plugin, Ui};
use crate::core::registry::Registry;
use crate::core::wake::Waker;
use crate::mascot::anim::Reaction;
use crate::mascot::themes::Theme;
use crate::mascot::{Mood, render, sprites};
use crate::ui::{HubWindow, PluginApps, PluginCard};

/// O único arquivo que a tela de um plugin pode importar (além do `std-widgets.slint`).
const HOST_SLINT: &str = r#"
// Tipos que o MiniUtils oferece às telas dos plugins: import { Theme } from "miniutils.slint";
export struct Theme {
    fur: color, visor: color, outline: color, ear: color, blush: color,
    ok: color, warn: color, alarm: color, sleep: color,
    bg0: color, bg1: color, text: color, sub: color, muted: color,
    // fontes do visual da janela (o texto da tela já usa a font-ui por padrão)
    font-ui: string, font-mono: string, font-title: string,
}
"#;

/// Prazo de um `run()` do script.
const RUN_TIMEOUT: Duration = Duration::from_secs(30);

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
        SET.with(|s| s.borrow_mut().cards = Some(cards.clone()));
        cards
    });
    let root = plugins::dir();
    let installed = plugins::load_installed(&root);
    let live_ui = (!startup).then_some(ui);
    let mut changed = false;

    // removidos (ou mudados: saem e entram de novo)
    let stamps: Vec<(String, u64)> = installed.iter().map(|p| (p.manifest.id.clone(), stamp(&root.join(&p.manifest.id)))).collect();
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
        cards.push(PluginCard { id: id.as_str().into(), ..Default::default() });
        let app = PluginMini::create(host.clone(), plugin, &root.join(&id), cards.clone(), host.bus.waker().clone());
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
    for name in plugins::ALLOWED_FILES {
        std::fs::read(dir.join(name)).ok().hash(&mut h);
    }
    h.finish()
}

/// Os callbacks de `PluginApps` são de todos os plugins: o id diz de qual.
fn bind(ui: &HubWindow) {
    let g = ui.global::<PluginApps>();
    let w = ui.as_weak();
    g.on_poked(move |id| {
        if let (Some(a), Some(ui)) = (find_app(&id), w.upgrade()) {
            let mut a = a.borrow_mut();
            let (mood, reduce) = (a.mood(), a.host.config.borrow().reduce_motion);
            if a.face.poke(mood, reduce) {
                a.push(&ui);
            }
        }
    });
    let w = ui.as_weak();
    g.on_refresh(move |id| {
        if let (Some(a), Some(ui)) = (find_app(&id), w.upgrade()) {
            let mut a = a.borrow_mut();
            a.refresh_now();
            a.push(&ui);
        }
    });
}

// ---- tela interpretada --------------------------------------------------------------

/// Executor mínimo: a compilação só "espera" se o carregador de arquivo esperar, e o
/// nosso responde na hora.
fn block_on<F: Future>(f: F) -> F::Output {
    let mut f = std::pin::pin!(f);
    let mut cx = Context::from_waker(std::task::Waker::noop());
    loop {
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
    }
}

/// Compila a tela do plugin. Imports: só `std-widgets.slint` (embutido) e `miniutils.slint`.
///
/// O `Main` vira uma `Window`, que é o que o `ComponentContainer` encaixa:
/// - fundo: o container pinta atrás da tela o fundo da janela dela, e um `Main` que não é
///   `Window` ganha do Slint uma janela com o fundo do estilo (cinza), que ignora o tema.
///   Por isso a janela sai de fundo transparente (a célula, com as cores do tema, aparece);
/// - fonte: a janela usa a fonte da interface do visual (`default-font-family`, ligada a
///   uma propriedade `miniutils-font` que o host preenche).
///
/// O host tenta da versão mais completa para a original: se o plugin já define o próprio
/// fundo ou fonte, a propriedade repetida não compila e a próxima versão vale; se usa algo
/// que só um retângulo tem (borda), fica o original.
fn compile_view(dir: &Path, source: String) -> Result<ComponentDefinition, String> {
    let candidates = [(true, true), (true, false), (false, true), (false, false)];
    for (transparent, font) in candidates {
        if let Some(candidate) = as_window(&source, transparent, font) {
            if let Ok(def) = build_view(dir, candidate) {
                return Ok(def);
            }
        }
    }
    build_view(dir, source)
}

/// Propriedade que leva a fonte do visual à janela da tela do plugin.
const FONT_PROP: &str = "miniutils-font";

/// `source` com o `Main` herdando de `Window`, de fundo transparente e/ou com a fonte do
/// visual.
fn as_window(source: &str, transparent: bool, font: bool) -> Option<String> {
    let start = source.find("export component Main")?;
    let brace = start + source[start..].find('{')?;
    let header = &source[start..brace];
    let header = match header.find(" inherits ") {
        Some(i) => format!("{} inherits Window ", &header[..i]),
        None => format!("{} inherits Window ", header.trim_end()),
    };
    let mut extra = String::new();
    if transparent {
        extra.push_str("\n    background: transparent;");
    }
    if font {
        extra.push_str(&format!("\n    in property <string> {FONT_PROP};\n    default-font-family: {FONT_PROP};"));
    }
    Some(format!("{}{header}{{{extra}{}", &source[..start], &source[brace + 1..]))
}

fn build_view(dir: &Path, source: String) -> Result<ComponentDefinition, String> {
    let mut compiler = slint_interpreter::Compiler::default();
    compiler.set_file_loader(|path: &Path| {
        let allowed = path.file_name().is_some_and(|n| n == "miniutils.slint");
        let shown = path.display().to_string();
        Box::pin(async move {
            Some(if allowed {
                Ok(HOST_SLINT.to_string())
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!("a tela de um plugin só importa std-widgets.slint e miniutils.slint ({shown})"),
                ))
            })
        })
    });
    let result = block_on(compiler.build_from_source(source, dir.join("view.slint")));
    let errors: Vec<String> = result
        .diagnostics()
        .filter(|d| d.level() == slint_interpreter::DiagnosticLevel::Error)
        .map(|d| {
            let (line, col) = d.line_column();
            format!("view.slint:{line}:{col}: {}", d.message())
        })
        .collect();
    if !errors.is_empty() {
        return Err(errors.join("\n"));
    }
    result.component("Main").ok_or_else(|| "view.slint precisa exportar `component Main`".to_string())
}

/// Confere tela e script antes de instalar (a loja chama).
pub fn check_files(dir: &Path, _ui: &Ui) -> Result<(), String> {
    let view = std::fs::read_to_string(dir.join("view.slint")).map_err(|e| format!("view.slint: {e}"))?;
    compile_view(dir, view)?;
    let script = std::fs::read_to_string(dir.join("main.rhai")).map_err(|e| format!("main.rhai: {e}"))?;
    engine(Arc::new(Mutex::new(Vec::new())), Exec::new("check", &[]), Arc::new(Mutex::new(rhai::Map::new())))
        .compile(&script)
        .map_err(|e| format!("main.rhai: {e}"))?;
    Ok(())
}

/// O `Theme` da tela do plugin: cores do tema do app e fontes do visual da janela.
fn theme_value(t: &Theme, skin: &crate::skin::Skin) -> Value {
    let c = |v: u32| Value::Brush(slint::Brush::SolidColor(render::color(v)));
    let fields = [
        ("fur", t.fur),
        ("visor", t.visor),
        ("outline", t.outline),
        ("ear", t.ear),
        ("blush", t.blush),
        ("ok", t.ok),
        ("warn", t.warn),
        ("alarm", t.alarm),
        ("sleep", t.sleep),
        ("bg0", t.bg0),
        ("bg1", t.bg1),
        ("text", t.text),
        ("sub", t.sub),
        ("muted", t.muted),
    ];
    let fonts = [("font-ui", skin.font_ui), ("font-mono", skin.font_mono), ("font-title", skin.font_title)];
    Value::Struct(
        fields
            .into_iter()
            .map(|(k, v)| (k.to_string(), c(v)))
            .chain(fonts.into_iter().map(|(k, f)| (k.to_string(), Value::String(f.into()))))
            .collect(),
    )
}

// ---- script -------------------------------------------------------------------------

/// Valor que o script manda para a tela (sem `Rc`: atravessa threads).
#[derive(Debug, Clone)]
enum PValue {
    Str(String),
    Num(f64),
    Bool(bool),
    List(Vec<PValue>),
    Map(Vec<(String, PValue)>),
}

impl PValue {
    fn from_dynamic(d: &rhai::Dynamic) -> PValue {
        if let Some(b) = d.clone().try_cast::<bool>() {
            PValue::Bool(b)
        } else if let Some(i) = d.clone().try_cast::<i64>() {
            PValue::Num(i as f64)
        } else if let Some(f) = d.clone().try_cast::<f64>() {
            PValue::Num(f)
        } else if let Some(a) = d.clone().try_cast::<rhai::Array>() {
            PValue::List(a.iter().map(PValue::from_dynamic).collect())
        } else if let Some(m) = d.clone().try_cast::<rhai::Map>() {
            PValue::Map(m.iter().map(|(k, v)| (k.to_string(), PValue::from_dynamic(v))).collect())
        } else {
            PValue::Str(d.to_string())
        }
    }

    /// Para a tela (na thread da UI).
    fn to_value(&self) -> Value {
        match self {
            PValue::Str(s) => Value::String(s.as_str().into()),
            PValue::Num(n) => Value::Number(*n),
            PValue::Bool(b) => Value::Bool(*b),
            PValue::List(l) => Value::Model(ModelRc::new(VecModel::from(l.iter().map(PValue::to_value).collect::<Vec<_>>()))),
            PValue::Map(m) => Value::Struct(m.iter().map(|(k, v)| (k.clone(), v.to_value())).collect()),
        }
    }
}

/// Do script para a UI.
#[derive(Debug, Clone)]
enum ScriptOut {
    Set(String, PValue),
    Mood(String),
    Summary(String),
    Notify(String, String),
    Error(String),
}

/// Da UI para o script.
enum ScriptIn {
    Tick,
    Action(String, String),
    Settings(rhai::Map),
}

/// O interpretador com a API do host e os limites (um script em loop não trava nada:
/// estoura o limite de operações e vira erro na tela).
fn engine(out: Arc<Mutex<Vec<ScriptOut>>>, exec: Exec, settings: Arc<Mutex<rhai::Map>>) -> rhai::Engine {
    let mut e = rhai::Engine::new();
    e.set_max_operations(5_000_000)
        .set_max_call_levels(32)
        .set_max_expr_depths(64, 32)
        .set_max_string_size(1 << 20)
        .set_max_array_size(10_000)
        .set_max_map_size(10_000);
    e.disable_symbol("eval");
    let push = move |m: ScriptOut| {
        if let Ok(mut v) = out.lock() {
            v.push(m);
        }
    };
    let p = push.clone();
    e.register_fn("ui", move |name: &str, value: rhai::Dynamic| p(ScriptOut::Set(name.to_string(), PValue::from_dynamic(&value))));
    let p = push.clone();
    e.register_fn("mood", move |m: &str| p(ScriptOut::Mood(m.to_string())));
    let p = push.clone();
    e.register_fn("summary", move |s: &str| p(ScriptOut::Summary(s.to_string())));
    let p = push.clone();
    e.register_fn("notify", move |t: &str, b: &str| p(ScriptOut::Notify(t.to_string(), b.to_string())));
    let p = push.clone();
    e.on_print(move |s| p(ScriptOut::Summary(s.to_string())));
    e.on_debug(|s, _, _| eprintln!("plugin: {s}"));
    e.register_fn("now", || crate::apps::kit::unix_now());
    e.register_fn("setting", move |key: &str| -> rhai::Dynamic {
        settings.lock().ok().and_then(|m| m.get(key).cloned()).unwrap_or(rhai::Dynamic::UNIT)
    });
    e.register_fn("run", move |cmd: rhai::Array| -> rhai::Map {
        let argv: Vec<String> = cmd.iter().map(|d| d.to_string()).collect();
        let result = match argv.split_first() {
            Some((prog, args)) => {
                let args: Vec<&str> = args.iter().map(String::as_str).collect();
                exec.run_blocking(prog, &args, RUN_TIMEOUT).map_err(|e| e.to_string())
            }
            None => Err("run([]): comando vazio".into()),
        };
        let mut m = rhai::Map::new();
        m.insert("ok".into(), result.is_ok().into());
        m.insert("out".into(), result.clone().unwrap_or_default().into());
        m.insert("err".into(), result.err().unwrap_or_default().into());
        m
    });
    e
}

/// O laço da thread do script: roda o topo do arquivo, `start()`, e depois atende os
/// pedidos da UI até o canal fechar (app desligado). Cada leva de mensagens vai para a
/// UI por `deliver`.
fn script_loop(
    source: String,
    exec: Exec,
    settings: rhai::Map,
    rx: Receiver<ScriptIn>,
    deliver: impl Fn(Vec<ScriptOut>),
) {
    let out = Arc::new(Mutex::new(Vec::new()));
    let shared_settings = Arc::new(Mutex::new(settings.clone()));
    let engine = engine(out.clone(), exec, shared_settings.clone());
    let flush = || {
        let batch = out.lock().map(|mut v| std::mem::take(&mut *v)).unwrap_or_default();
        if !batch.is_empty() {
            deliver(batch);
        }
    };
    let ast = match engine.compile(&source) {
        Ok(ast) => ast,
        Err(e) => {
            deliver(vec![ScriptOut::Error(format!("main.rhai: {e}"))]);
            return;
        }
    };
    let hooks: Vec<(String, usize)> = ast.iter_functions().map(|f| (f.name.to_string(), f.params.len())).collect();
    let has = |name: &str, n: usize| hooks.iter().any(|(h, p)| h == name && *p == n);
    let mut scope = rhai::Scope::new();
    let mut this: rhai::Dynamic = {
        let mut m = rhai::Map::new();
        m.insert("settings".into(), settings.into());
        m.into()
    };
    let report = |r: Result<(), Box<rhai::EvalAltResult>>| {
        if let Err(e) = r {
            if let Ok(mut v) = out.lock() {
                v.push(ScriptOut::Error(format!("main.rhai: {e}")));
            }
        }
    };
    report(engine.run_ast_with_scope(&mut scope, &ast));
    let mut call = |name: &str, args: Vec<rhai::Dynamic>, this: &mut rhai::Dynamic| {
        if !has(name, args.len()) {
            return Ok(());
        }
        let opts = rhai::CallFnOptions::new().eval_ast(false).rewind_scope(true).bind_this_ptr(this);
        engine.call_fn_with_options::<rhai::Dynamic>(opts, &mut scope, &ast, name, args).map(|_| ())
    };
    report(call("start", vec![], &mut this));
    flush();
    while let Ok(msg) = rx.recv() {
        let r = match msg {
            ScriptIn::Tick => call("tick", vec![], &mut this),
            ScriptIn::Action(n, a) => call("action", vec![n.into(), a.into()], &mut this),
            ScriptIn::Settings(m) => {
                if let Ok(mut s) = shared_settings.lock() {
                    *s = m.clone();
                }
                if let Some(map) = this.write_lock::<rhai::Map>().as_deref_mut() {
                    map.insert("settings".into(), m.into());
                }
                call("settings", vec![], &mut this)
            }
        };
        report(r);
        flush();
    }
}

// ---- o mini-app ---------------------------------------------------------------------

/// O que só um plugin com tela própria tem.
struct Full {
    def: ComponentDefinition,
    source: String,
    /// A instância que está na tela agora (o container recria ao trocar de página).
    instance: Rc<RefCell<Option<ComponentInstance>>>,
    /// Últimos valores postos na tela, para repor numa instância nova.
    values: Rc<RefCell<HashMap<String, Value>>>,
    /// Canal para a thread do script (fechado = desligado).
    tx: Rc<RefCell<Option<Sender<ScriptIn>>>>,
    factory: slint::ComponentFactory,
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
}

impl PluginMini {
    #[allow(deprecated)] // ComponentFactory: experimental no Slint 1.18
    fn create(host: Rc<Host>, plugin: Plugin, dir: &Path, cards: Rc<VecModel<PluginCard>>, waker: Waker) -> Rc<RefCell<Self>> {
        let seed = 20 + cards.row_count() as u32;
        let mut error = None;
        let full = plugin.ui.as_ref().and_then(|_| {
            let source = std::fs::read_to_string(dir.join("main.rhai")).map_err(|e| format!("main.rhai: {e}"));
            let view = std::fs::read_to_string(dir.join("view.slint"))
                .map_err(|e| format!("view.slint: {e}"))
                .and_then(|v| compile_view(dir, v));
            match (view, source) {
                (Ok(def), Ok(source)) => Some(Full {
                    def,
                    source,
                    instance: Default::default(),
                    values: Default::default(),
                    tx: Default::default(),
                    factory: slint::ComponentFactory::default(),
                }),
                (Err(e), _) | (_, Err(e)) => {
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
        }));
        // fábrica da tela: o container chama quando a célula aparece
        {
            let mut a = app.borrow_mut();
            if let Some(full) = a.full.as_mut() {
                let (def, slot, values, tx) = (full.def.clone(), full.instance.clone(), full.values.clone(), full.tx.clone());
                full.factory = slint::ComponentFactory::new(move |ctx| {
                    let inst = def.create_embedded(ctx).ok()?;
                    for (k, v) in values.borrow().iter() {
                        let _ = inst.set_property(k, v.clone());
                    }
                    let tx = tx.clone();
                    let _ = inst.set_callback("action", move |args: &[Value]| {
                        let s = |i: usize| match args.get(i) {
                            Some(Value::String(s)) => s.to_string(),
                            Some(Value::Number(n)) => n.to_string(),
                            _ => String::new(),
                        };
                        if let Some(tx) = tx.borrow().as_ref() {
                            let _ = tx.send(ScriptIn::Action(s(0), s(1)));
                        }
                        Value::Void
                    });
                    *slot.borrow_mut() = Some(inst.clone_strong());
                    Some(inst)
                });
            }
        }
        // o relógio do plugin: comando (simples) ou `tick()` (com tela)
        let every = match (&app.borrow().plugin.run, &app.borrow().plugin.ui) {
            (Some(run), _) => Some(run.every),
            (None, Some(ui)) => ui.every,
            _ => None,
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
            if let Some(tx) = full.tx.borrow().as_ref() {
                let _ = tx.send(ScriptIn::Tick);
            }
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

    /// Liga a thread do script (plugin com tela).
    fn start_script(&mut self) {
        let Some(full) = &self.full else { return };
        let (tx, rx) = channel();
        *full.tx.borrow_mut() = Some(tx);
        let id = self.plugin.manifest.id.clone();
        let exec = self.host.exec_for(&self.plugin.manifest);
        let settings = self.settings_map();
        let source = full.source.clone();
        let waker = self.waker.clone();
        let spawned = std::thread::Builder::new().name(format!("plugin-{id}")).spawn(move || {
            script_loop(source, exec, settings, rx, move |batch| {
                let id = id.clone();
                let _ = slint::invoke_from_event_loop(move || deliver(&id, batch));
                // o hub atualiza humor e bandeja mesmo com a janela fechada
                waker.wake();
            })
        });
        if let Err(e) = spawned {
            self.error = Some(format!("sem thread para o script: {e}"));
        }
    }

    /// As opções do app, para o script.
    fn settings_map(&self) -> rhai::Map {
        let cfg = self.host.config.borrow();
        let mut m = rhai::Map::new();
        for s in &self.plugin.manifest.settings {
            let v = cfg.value(&self.plugin.manifest, &s.key);
            let d: rhai::Dynamic = match v {
                toml::Value::Boolean(b) => b.into(),
                toml::Value::Integer(i) => i.into(),
                toml::Value::Float(f) => f.into(),
                other => other.as_str().unwrap_or_default().to_string().into(),
            };
            m.insert(s.key.as_str().into(), d);
        }
        m
    }

    /// Aplica o que o script mandou (thread da UI).
    fn apply(&mut self, batch: Vec<ScriptOut>, ui: &HubWindow) {
        let before = self.mood;
        for msg in batch {
            match msg {
                ScriptOut::Set(name, v) => self.set_view(&name, v.to_value()),
                ScriptOut::Mood(m) => self.script_mood = mood_from(&m),
                ScriptOut::Summary(s) => self.script_summary = s,
                ScriptOut::Notify(t, b) => {
                    let key = format!("plugin:{}", self.plugin.manifest.id);
                    self.host.notifier.borrow_mut().send(&key, &t, &b, false);
                }
                ScriptOut::Error(e) => {
                    eprintln!("miniutils: plugin {}: {e}", self.plugin.manifest.id);
                    self.error = Some(e);
                }
            }
        }
        self.mood = self.compute_mood();
        if before == Mood::Alarm && self.mood != Mood::Alarm {
            self.face.react(Reaction::Celebrate);
        }
        self.push(ui);
    }

    /// Põe um valor na tela do plugin e guarda para uma instância nova.
    fn set_view(&self, name: &str, v: Value) {
        let Some(full) = &self.full else { return };
        if let Some(inst) = full.instance.borrow().as_ref() {
            if let Err(e) = inst.set_property(name, v.clone()) {
                eprintln!("miniutils: plugin {}: propriedade {name}: {e}", self.plugin.manifest.id);
            }
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
        let skin = crate::skin::get(&self.host.config.borrow().skin);
        put("theme", theme_value(self.host.theme_for(&self.plugin.manifest), skin));
        put(FONT_PROP, Value::String(skin.font_ui.into()));
        put("mascot", Value::Image(self.face.image()));
        put("mood", Value::String(self.mood.as_str().into()));
    }

    fn compute_mood(&self) -> Mood {
        if self.error.is_some() {
            return Mood::Alarm;
        }
        if self.plugin.ui.is_some() {
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
            has_view: self.full.is_some(),
            view: self.full.as_ref().map(|f| f.factory.clone()).unwrap_or_default(),
        };
        let id = &self.plugin.manifest.id;
        if let Some(i) = (0..self.cards.row_count()).find(|&i| self.cards.row_data(i).is_some_and(|c| c.id == id.as_str())) {
            if self.cards.row_data(i).as_ref() != Some(&card) {
                self.cards.set_row_data(i, card);
            }
        }
    }
}

/// Entrega o que o script mandou ao app dele (na thread da UI).
fn deliver(id: &str, batch: Vec<ScriptOut>) {
    let Some(app) = find_app(id) else { return };
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
            self.start_script();
        } else {
            self.refresh_now();
        }
        self.mood = self.compute_mood();
        self.push(ui);
    }

    fn disable(&mut self, _ui: &HubWindow) {
        self.timer.stop();
        // fechar o canal encerra a thread do script
        if let Some(full) = &self.full {
            full.tx.borrow_mut().take();
        }
    }

    fn on_event(&mut self, _ev: &Event, _ui: &HubWindow) {}

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
        self.mood = self.compute_mood();
        // saiu do alarme: comemora
        if before == Mood::Alarm && self.mood != Mood::Alarm {
            self.face.react(Reaction::Celebrate);
        }
        self.push(ui);
    }

    fn tick(&mut self, frame: u32, ui: &HubWindow) {
        // "atualizado há N s" anda sozinho; uma vez por segundo basta
        if self.refresh_mascot() || (self.plugin.run.is_some() && frame % 8 == 0) {
            self.push(ui);
        }
    }

    fn settings_changed(&mut self, ui: &HubWindow) {
        if let Some(full) = &self.full {
            if let Some(tx) = full.tx.borrow().as_ref() {
                let _ = tx.send(ScriptIn::Settings(self.settings_map()));
            }
        }
        self.push(ui);
    }

    fn mood(&self) -> Mood {
        self.mood
    }

    fn mascot_key(&self) -> String {
        self.host.config.borrow().string(&self.plugin.manifest, "mascot")
    }

    fn summary(&self) -> String {
        let line = if self.plugin.ui.is_some() && self.error.is_none() {
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
            full.tx.borrow_mut().take();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rodar(script: &str, entradas: Vec<ScriptIn>) -> Vec<ScriptOut> {
        let (tx, rx) = channel();
        for e in entradas {
            tx.send(e).unwrap();
        }
        drop(tx);
        let saida = Arc::new(Mutex::new(Vec::new()));
        let s = saida.clone();
        script_loop(script.into(), Exec::new("t", &["echo".into()]), rhai::Map::new(), rx, move |b| s.lock().unwrap().extend(b));
        let v = saida.lock().unwrap().clone();
        v
    }

    #[test]
    fn script_guarda_estado_e_fala_com_a_tela() {
        let s = r#"
            fn start() { this.n = 0; ui("titulo", "oi"); }
            fn tick() { this.n += 1; ui("n", this.n); if this.n == 2 { mood("happy"); } }
            fn action(name, arg) { ui("ultimo", name + ":" + arg); }
        "#;
        let out = rodar(s, vec![ScriptIn::Tick, ScriptIn::Tick, ScriptIn::Action("clicou".into(), "x".into())]);
        let txt = format!("{out:?}");
        assert!(txt.contains(r#"Set("titulo", Str("oi"))"#), "{txt}");
        assert!(txt.contains(r#"Set("n", Num(2.0))"#), "{txt}");
        assert!(txt.contains(r#"Mood("happy")"#), "{txt}");
        assert!(txt.contains(r#"Set("ultimo", Str("clicou:x"))"#), "{txt}");
    }

    #[test]
    fn script_so_roda_o_que_o_manifesto_permite_e_nao_trava() {
        let s = r#"
            fn start() {
                let ok = run(["echo", "ola"]);
                let eco = ok.out;
                eco.trim();
                ui("eco", eco);
                let nao = run(["rm", "-rf", "/tmp/nada"]);
                ui("negado", nao.ok);
                loop { }
            }
        "#;
        let out = rodar(s, vec![]);
        let txt = format!("{out:?}");
        assert!(txt.contains(r#"Set("eco", Str("ola"))"#), "{txt}");
        assert!(txt.contains(r#"Set("negado", Bool(false))"#), "{txt}");
        // o laço infinito estoura o limite de operações e vira erro, não trava o teste
        assert!(txt.contains("Error("), "{txt}");
    }

    #[test]
    fn exemplo_com_tela_e_script_funciona() {
        let dir = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/plugin-top"));
        let p = Plugin::parse(&std::fs::read_to_string(dir.join("manifest.toml")).unwrap()).unwrap();
        check_files(dir, p.ui.as_ref().unwrap()).unwrap();
        let def = compile_view(dir, std::fs::read_to_string(dir.join("view.slint")).unwrap()).unwrap();
        let props: Vec<String> = def.properties().map(|(n, _)| n).collect();
        assert!(props.iter().any(|n| n == "rows") && props.iter().any(|n| n == "theme"), "{props:?}");
        if !crate::core::exec::which("ps") {
            return;
        }
        let (tx, rx) = channel();
        tx.send(ScriptIn::Action("sort".into(), "mem".into())).unwrap();
        drop(tx);
        let saida = Arc::new(Mutex::new(Vec::new()));
        let s = saida.clone();
        let script = std::fs::read_to_string(dir.join("main.rhai")).unwrap();
        script_loop(script, Exec::new("t", &p.manifest.permissions.exec), rhai::Map::new(), rx, move |b| s.lock().unwrap().extend(b));
        let txt = format!("{:?}", saida.lock().unwrap());
        assert!(!txt.contains("Error("), "{txt}");
        assert!(txt.contains(r#"Set("rows", List([Map("#), "{txt}");
        assert!(txt.contains(r#"Set("sort", Str("mem"))"#), "{txt}");
    }

    #[test]
    fn tela_vira_janela_transparente_salvo_se_tiver_fundo_ou_borda() {
        let dir = Path::new("/tmp");
        let sem = "export component Main inherits Rectangle { in property <string> titulo; }";
        assert_eq!(
            as_window(sem, true, false).unwrap(),
            "export component Main inherits Window {\n    background: transparent; in property <string> titulo; }"
        );
        assert!(build_view(dir, as_window(sem, true, true).unwrap()).is_ok());
        let def = compile_view(dir, sem.into()).unwrap();
        assert!(def.properties().any(|(n, _)| n == FONT_PROP), "a versão completa (com a fonte) vale");
        // fundo próprio: a transparente repete a propriedade; vale a janela com o fundo dele
        let com = "export component Main inherits Rectangle { background: red; }";
        assert!(build_view(dir, as_window(com, true, true).unwrap()).is_err());
        assert!(build_view(dir, as_window(com, false, true).unwrap()).is_ok());
        // fonte própria: fica a janela transparente sem a fonte do visual
        let fonte = "export component Main inherits Rectangle { default-font-family: \"Serif\"; }";
        let def = compile_view(dir, fonte.into()).unwrap();
        assert!(!def.properties().any(|(n, _)| n == FONT_PROP));
        // borda só existe em retângulo: fica o original
        let borda = "export component Main inherits Rectangle { border-width: 1px; }";
        assert!(build_view(dir, as_window(borda, false, false).unwrap()).is_err());
        assert!(compile_view(dir, borda.into()).is_ok());
    }

    /// Desenha a tela do exemplo com o renderizador por software (o mesmo do app): o fundo
    /// tem de sair transparente, para a célula (com as cores do tema) aparecer atrás.
    #[test]
    fn tela_do_plugin_tem_fundo_transparente() {
        use slint::platform::software_renderer::{MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType};
        use slint::platform::{Platform, WindowAdapter};
        struct P(Rc<MinimalSoftwareWindow>);
        impl Platform for P {
            fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
                Ok(self.0.clone())
            }
        }
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        // só este teste cria janela; se outro já pôs uma plataforma, não dá para conferir
        if slint::platform::set_platform(Box::new(P(window.clone()))).is_err() {
            return;
        }
        let (w, h) = (300, 200);
        window.set_size(slint::PhysicalSize::new(w, h));
        let dir = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/plugin-top"));
        let def = compile_view(dir, std::fs::read_to_string(dir.join("view.slint")).unwrap()).unwrap();
        let inst = def.create().unwrap();
        inst.show().unwrap();
        let skin = crate::skin::get("aconchego");
        inst.set_property("theme", theme_value(crate::mascot::themes::get("aconchego"), skin)).unwrap();
        inst.set_property(FONT_PROP, Value::String(skin.font_ui.into())).unwrap();
        // as fontes chegam no Theme (o plugin pode usar theme.font-mono)
        let font = match inst.get_property("theme").unwrap() {
            Value::Struct(st) => st.get_field("font-mono").cloned(),
            _ => None,
        };
        assert_eq!(font, Some(Value::String(skin.font_mono.into())));
        let mut px = vec![PremultipliedRgbaColor::default(); (w * h) as usize];
        window.draw_if_needed(|r| {
            r.render(&mut px, w as usize);
        });
        let corner = px[((h - 1) * w + 1) as usize];
        assert_eq!(corner.alpha, 0, "fundo da tela do plugin não é transparente: {corner:?}");
        // e o texto do tema foi desenhado (algum pixel opaco)
        assert!(px.iter().any(|p| p.alpha == 255));
    }

    #[test]
    fn tela_so_importa_o_permitido() {
        let dir = Path::new("/tmp");
        let ok = "import { Theme } from \"miniutils.slint\";\nexport component Main inherits Rectangle { in property <Theme> theme; in property <string> titulo; callback action(string, string); }";
        assert!(compile_view(dir, ok.into()).is_ok());
        let fora = "import { X } from \"/etc/passwd\";\nexport component Main inherits Rectangle {}";
        assert!(compile_view(dir, fora.into()).is_err());
        let sem_main = "export component Outra inherits Rectangle {}";
        assert!(compile_view(dir, sem_main.into()).err().unwrap_or_default().contains("component Main"));
    }
}
