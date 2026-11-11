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
//! consultar os domínios de `[permissions] net`, receber as leituras dos provedores de
//! `uses`, guardar dados (num arquivo que o host grava), mandar notificação, contar fatos
//! para a conversa entre mascotes e ler as próprias opções.
//!
//! API do script (`main.rhai`):
//! ```text
//! fn start()                  ao ligar          this.settings = opções do app (mapa)
//! fn tick()                   a cada [ui] every
//! fn action(name, arg)        a tela chamou `callback action(string, string)`
//! fn on_event(topic, data)    leitura de um provedor de `uses` ("sys", "docker"…), em mapa
//! fn settings()               o usuário mudou uma opção (this.settings já é a nova)
//! ui("prop", valor)           muda uma propriedade da tela (texto, número, bool, lista, mapa)
//! mood("happy")               humor: sleep, idle, happy, busy, alarm
//! summary("texto")            linha do tooltip da bandeja
//! run(["ps", "-e"])           roda um programa da allowlist: #{ ok, out, err } (até 30 s)
//! run_async(["ps"], "fim")    o mesmo sem esperar: chama fn fim(result) depois
//! http_get(url)               GET https num domínio de net: #{ ok, status, body, err }
//! http_get_async(url, "fim")  o mesmo sem esperar (até 4 tarefas ao mesmo tempo)
//! parse_json(texto)  to_json(valor)
//! store_get("k")  store_set("k", valor)   dados que sobrevivem a desligar (() apaga)
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

use std::cell::RefCell;
use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Model, ModelRc, Timer, TimerMode, VecModel};
use slint_interpreter::{ComponentDefinition, ComponentInstance, Value};

use super::kit::HeaderMascot;
use crate::core::bus::Event;
use crate::core::chatter::{self, Fact, FactKind, Voice};
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
            eprintln!("plugin-dev: {id} carregado de {}", plugin.dir.display());
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
        std::fs::read(dir.join(&rel)).ok().hash(&mut h);
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
            eprintln!("plugin-dev: arquivos mudaram, recarregando");
            crate::hub::request_plugin_reload();
        }
    });
    timer
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
    check_images(dir, &dir.join("view.slint"), &source)?;
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

/// Caminho sem `.` nem `..` (sem tocar o disco: não segue links).
fn normalize(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// Um `import` da tela: só `ui/*.slint` do próprio plugin (arquivo comum, até 1 MiB).
fn local_import(dir: &Path, path: &Path) -> Result<String, String> {
    let p = normalize(path);
    let shown = path.display();
    let inside = p.parent() == Some(normalize(&dir.join("ui")).as_path()) && p.extension().is_some_and(|e| e == "slint");
    if !inside {
        return Err(format!("a tela de um plugin só importa std-widgets.slint, miniutils.slint e ui/*.slint ({shown})"));
    }
    let meta = std::fs::symlink_metadata(&p).map_err(|e| format!("{shown}: {e}"))?;
    if !meta.file_type().is_file() || meta.len() > plugins::MAX_FILE {
        return Err(format!("{shown}: precisa ser um arquivo comum de até 1 MiB"));
    }
    let text = std::fs::read_to_string(&p).map_err(|e| format!("{shown}: {e}"))?;
    check_images(dir, &p, &text)?;
    Ok(text)
}

/// Toda `@image-url("…")` de `file` aponta para `assets/` do plugin (nada de caminho
/// absoluto nem de fora da pasta).
fn check_images(dir: &Path, file: &Path, source: &str) -> Result<(), String> {
    let assets = normalize(&dir.join("assets"));
    let base = file.parent().unwrap_or(dir);
    let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    for (i, _) in source.match_indices("@image-url(") {
        let rest = source[i + "@image-url(".len()..].trim_start();
        let Some(rest) = rest.strip_prefix('"') else { continue };
        let Some(end) = rest.find('"') else { continue };
        let lit = &rest[..end];
        let target = normalize(&base.join(lit));
        if Path::new(lit).is_absolute() || target.parent() != Some(assets.as_path()) {
            return Err(format!("{name}: @image-url(\"{lit}\"): as imagens ficam em assets/ do plugin"));
        }
    }
    Ok(())
}

fn build_view(dir: &Path, source: String) -> Result<ComponentDefinition, String> {
    let mut compiler = slint_interpreter::Compiler::default();
    let base = dir.to_path_buf();
    compiler.set_file_loader(move |path: &Path| {
        let result = if path.file_name().is_some_and(|n| n == "miniutils.slint") {
            Ok(HOST_SLINT.to_string())
        } else {
            local_import(&base, path).map_err(|e| std::io::Error::new(std::io::ErrorKind::PermissionDenied, e))
        };
        Box::pin(async move { Some(result) })
    });
    let result = block_on(compiler.build_from_source(source, dir.join("view.slint")));
    let errors: Vec<String> = result
        .diagnostics()
        .filter(|d| d.level() == slint_interpreter::DiagnosticLevel::Error)
        .map(|d| {
            let (line, col) = d.line_column();
            // erro num arquivo de ui/: o caminho relativo à pasta do plugin
            let file = d.source_file().map_or("view.slint".into(), |p| p.strip_prefix(dir).unwrap_or(p).display().to_string());
            format!("{file}:{line}:{col}: {}", d.message())
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
    let env = ScriptEnv { exec: Exec::new("check", &[]), net: Vec::new(), data: None, back: None };
    engine(Default::default(), env, Default::default(), Default::default())
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
    /// Fato para a conversa entre mascotes: tipo, assunto e número.
    Fact(FactKind, String, Option<f32>),
    Error(String),
}

/// Da UI (ou de uma tarefa assíncrona) para o script.
enum ScriptIn {
    Tick,
    Action(String, String),
    Settings(rhai::Map),
    /// Leitura de um provedor que o plugin assina (`uses`): nome do tópico e dados.
    Event(String, rhai::Dynamic),
    /// Um `run_async`/`http_get_async` terminou: nome da função a chamar e o resultado.
    Done(String, rhai::Map),
    /// Desligou: a thread do script termina.
    Stop,
}

/// Prazo e tamanho máximo de um `http_get`.
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);
const HTTP_MAX: usize = 1 << 20;
/// Tarefas assíncronas (`run_async`, `http_get_async`) ao mesmo tempo, por plugin.
const MAX_ASYNC: usize = 4;
/// Tamanho máximo dos dados gravados (`store_set`), em JSON.
const STORE_MAX: usize = 1 << 20;

/// O que o script do plugin pode usar do host.
#[derive(Clone)]
struct ScriptEnv {
    exec: Exec,
    /// Domínios de `[permissions] net`.
    net: Vec<String>,
    /// Arquivo dos dados (`store_get`/`store_set`); `None` = só na memória.
    data: Option<PathBuf>,
    /// Canal de volta para as tarefas assíncronas (`None` = sem elas).
    back: Option<Sender<ScriptIn>>,
}

/// Os dados que o script grava, carregados do arquivo e gravados de volta pelo host.
#[derive(Default)]
struct Store {
    data: rhai::Map,
    dirty: bool,
}

impl Store {
    fn load(path: Option<&Path>) -> Store {
        let data = path
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .map(json_to_dyn)
            .and_then(|d| d.try_cast::<rhai::Map>())
            .unwrap_or_default();
        Store { data, dirty: false }
    }

    /// Grava se mudou (arquivo temporário e `rename`: nunca fica pela metade).
    fn save(&mut self, path: Option<&Path>) -> Result<(), String> {
        if !self.dirty {
            return Ok(());
        }
        self.dirty = false;
        let Some(path) = path else { return Ok(()) };
        let json = serde_json::to_vec(&dyn_to_json(&self.data.clone().into())).map_err(|e| e.to_string())?;
        if json.len() > STORE_MAX {
            return Err(format!("store_set: os dados passam de {} KiB; não foram gravados", STORE_MAX >> 10));
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json).and_then(|_| std::fs::rename(&tmp, path)).map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// JSON → valor do Rhai (números inteiros ficam inteiros).
fn json_to_dyn(v: serde_json::Value) -> rhai::Dynamic {
    use serde_json::Value as J;
    match v {
        J::Null => rhai::Dynamic::UNIT,
        J::Bool(b) => b.into(),
        J::Number(n) => match n.as_i64() {
            Some(i) => i.into(),
            None => n.as_f64().unwrap_or(0.0).into(),
        },
        J::String(s) => s.into(),
        J::Array(a) => rhai::Dynamic::from_array(a.into_iter().map(json_to_dyn).collect()),
        J::Object(o) => rhai::Dynamic::from_map(o.into_iter().map(|(k, v)| (k.into(), json_to_dyn(v))).collect()),
    }
}

/// Valor do Rhai → JSON (o que não tem equivalente vira texto).
fn dyn_to_json(d: &rhai::Dynamic) -> serde_json::Value {
    use serde_json::Value as J;
    if d.is_unit() {
        J::Null
    } else if let Some(b) = d.clone().try_cast::<bool>() {
        J::Bool(b)
    } else if let Some(i) = d.clone().try_cast::<i64>() {
        J::from(i)
    } else if let Some(f) = d.clone().try_cast::<f64>() {
        serde_json::Number::from_f64(f).map_or(J::Null, J::Number)
    } else if let Some(a) = d.clone().try_cast::<rhai::Array>() {
        J::Array(a.iter().map(dyn_to_json).collect())
    } else if let Some(m) = d.clone().try_cast::<rhai::Map>() {
        J::Object(m.iter().map(|(k, v)| (k.to_string(), dyn_to_json(v))).collect())
    } else {
        J::String(d.to_string())
    }
}

/// Os dados de uma leitura de provedor, como o script recebe em `on_event`.
fn event_data(ev: &Event) -> serde_json::Value {
    let v = match ev {
        Event::Sys(s) => serde_json::to_value(s),
        Event::Gpu(s) => serde_json::to_value(s),
        Event::Docker(s) => serde_json::to_value(s),
        Event::Media(s) => serde_json::to_value(s),
        Event::AiUsage(s) => serde_json::to_value(s),
        Event::Ports(s) => serde_json::to_value(s),
        Event::Agents(s) => serde_json::to_value(s),
    };
    v.unwrap_or(serde_json::Value::Null)
}

/// `url` é https, na porta padrão, sem usuário, e o domínio está em `allowed`.
fn check_url(url: &reqwest::Url, allowed: &[String]) -> Result<(), String> {
    if url.scheme() != "https" {
        return Err(format!("{url}: só https://"));
    }
    if !url.username().is_empty() || url.password().is_some() || url.port().is_some() {
        return Err(format!("{url}: sem usuário nem porta no endereço"));
    }
    let host = url.host_str().unwrap_or_default();
    if !allowed.iter().any(|h| h == host) {
        return Err(format!("{host} não está em [permissions] net"));
    }
    Ok(())
}

/// GET com prazo e limite de tamanho; redirecionamento só para domínio permitido.
fn http_get(url: &str, allowed: &[String]) -> Result<(u16, String), String> {
    use std::io::Read;
    let parsed = reqwest::Url::parse(url).map_err(|e| format!("{url}: {e}"))?;
    check_url(&parsed, allowed)?;
    let hosts = allowed.to_vec();
    let client = reqwest::blocking::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .user_agent(concat!("miniutils/", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::custom(move |a| {
            if a.previous().len() >= 5 {
                a.error("redirecionamentos demais")
            } else if let Err(e) = check_url(a.url(), &hosts) {
                a.error(format!("redirecionou para fora do permitido: {e}"))
            } else {
                a.follow()
            }
        }))
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client.get(parsed).send().map_err(|e| format!("{url}: {e}"))?;
    let status = resp.status().as_u16();
    let mut body = Vec::new();
    resp.take(HTTP_MAX as u64 + 1).read_to_end(&mut body).map_err(|e| format!("{url}: {e}"))?;
    if body.len() > HTTP_MAX {
        return Err(format!("{url}: a resposta passa de 1 MiB"));
    }
    Ok((status, String::from_utf8_lossy(&body).into_owned()))
}

/// Resultado de `run`: #{ ok, out, err }.
fn run_result(exec: &Exec, cmd: &rhai::Array) -> rhai::Map {
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
}

/// Resultado de `http_get`: #{ ok, status, body, err } (`ok` = respondeu com 2xx).
fn http_result(url: &str, allowed: &[String]) -> rhai::Map {
    let r = http_get(url, allowed);
    let mut m = rhai::Map::new();
    let (status, body) = r.as_ref().map(|(s, b)| (*s, b.clone())).unwrap_or_default();
    m.insert("ok".into(), (r.is_ok() && (200..300).contains(&status)).into());
    m.insert("status".into(), (status as i64).into());
    m.insert("body".into(), body.into());
    m.insert("err".into(), r.err().unwrap_or_default().into());
    m
}

/// Roda `job` numa thread e manda o resultado à função `cb` do script. `false` se não
/// deu (limite de tarefas, ou sem canal de volta).
fn spawn_async(env: &ScriptEnv, pending: &Arc<AtomicUsize>, cb: String, job: impl FnOnce() -> rhai::Map + Send + 'static) -> bool {
    let Some(back) = env.back.clone() else { return false };
    if pending.fetch_add(1, Ordering::SeqCst) >= MAX_ASYNC {
        pending.fetch_sub(1, Ordering::SeqCst);
        return false;
    }
    let left = pending.clone();
    let spawned = std::thread::Builder::new().name("plugin-async".into()).spawn(move || {
        let r = job();
        left.fetch_sub(1, Ordering::SeqCst);
        let _ = back.send(ScriptIn::Done(cb, r));
    });
    if spawned.is_err() {
        pending.fetch_sub(1, Ordering::SeqCst);
    }
    spawned.is_ok()
}

/// O interpretador com a API do host e os limites (um script em loop não trava nada:
/// estoura o limite de operações e vira erro na tela).
fn engine(out: Arc<Mutex<Vec<ScriptOut>>>, env: ScriptEnv, settings: Arc<Mutex<rhai::Map>>, store: Arc<Mutex<Store>>) -> rhai::Engine {
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

    // conversa entre mascotes: fact("strain", "CPU", 93)
    type Res<T> = Result<T, Box<rhai::EvalAltResult>>;
    let fact = move |kind: &str, subject: &str, value: Option<f32>| -> Res<()> {
        let k = FactKind::ALL
            .into_iter()
            .find(|k| k.key() == kind && *k != FactKind::Idle)
            .ok_or_else(|| format!("fact: tipo \"{kind}\" desconhecido (trouble, strain, forecast, recovered, poked)"))?;
        push(ScriptOut::Fact(k, subject.to_string(), value));
        Ok(())
    };
    let f = fact.clone();
    e.register_fn("fact", move |k: &str, s: &str| f(k, s, None));
    let f = fact.clone();
    e.register_fn("fact", move |k: &str, s: &str, v: i64| f(k, s, Some(v as f32)));
    e.register_fn("fact", move |k: &str, s: &str, v: f64| fact(k, s, Some(v as f32)));

    // dados guardados entre execuções
    let st = store.clone();
    e.register_fn("store_get", move |key: &str| -> rhai::Dynamic {
        st.lock().ok().and_then(|s| s.data.get(key).cloned()).unwrap_or(rhai::Dynamic::UNIT)
    });
    e.register_fn("store_set", move |key: &str, value: rhai::Dynamic| {
        if let Ok(mut s) = store.lock() {
            if value.is_unit() {
                s.data.remove(key);
            } else {
                s.data.insert(key.into(), value);
            }
            s.dirty = true;
        }
    });

    // JSON
    e.register_fn("parse_json", |text: &str| -> Res<rhai::Dynamic> {
        serde_json::from_str::<serde_json::Value>(text).map(json_to_dyn).map_err(|err| format!("parse_json: {err}").into())
    });
    e.register_fn("to_json", |value: rhai::Dynamic| dyn_to_json(&value).to_string());

    // programas da allowlist e domínios de [permissions] net
    let pending = Arc::new(AtomicUsize::new(0));
    let ex = env.exec.clone();
    e.register_fn("run", move |cmd: rhai::Array| run_result(&ex, &cmd));
    let net = env.net.clone();
    e.register_fn("http_get", move |url: &str| http_result(url, &net));
    let (en, pe) = (env.clone(), pending.clone());
    e.register_fn("run_async", move |cmd: rhai::Array, cb: &str| {
        let exec = en.exec.clone();
        spawn_async(&en, &pe, cb.to_string(), move || run_result(&exec, &cmd))
    });
    e.register_fn("http_get_async", move |url: &str, cb: &str| {
        let (url, net) = (url.to_string(), env.net.clone());
        spawn_async(&env, &pending, cb.to_string(), move || http_result(&url, &net))
    });
    e
}

/// O laço da thread do script: roda o topo do arquivo, `start()`, e depois atende os
/// pedidos até `Stop` (ou o canal fechar). Cada leva de mensagens vai para a UI por
/// `deliver`; os dados gravados com `store_set` vão para o disco depois de cada chamada.
fn script_loop(source: String, env: ScriptEnv, settings: rhai::Map, rx: Receiver<ScriptIn>, deliver: impl Fn(Vec<ScriptOut>)) {
    let out = Arc::new(Mutex::new(Vec::new()));
    let shared_settings = Arc::new(Mutex::new(settings.clone()));
    let data = env.data.clone();
    let store = Arc::new(Mutex::new(Store::load(data.as_deref())));
    let engine = engine(out.clone(), env, shared_settings.clone(), store.clone());
    let flush = || {
        let saved = store.lock().map_err(|e| e.to_string()).and_then(|mut s| s.save(data.as_deref()));
        if let Err(e) = saved {
            if let Ok(mut v) = out.lock() {
                v.push(ScriptOut::Error(e));
            }
        }
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
            ScriptIn::Stop => break,
            ScriptIn::Tick => call("tick", vec![], &mut this),
            ScriptIn::Action(n, a) => call("action", vec![n.into(), a.into()], &mut this),
            ScriptIn::Event(topic, data) => call("on_event", vec![topic.into(), data], &mut this),
            ScriptIn::Done(cb, result) if has(&cb, 1) => call(&cb, vec![result.into()], &mut this),
            ScriptIn::Done(cb, _) => Err(format!("falta fn {cb}(result) para receber o resultado").into()),
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

impl Full {
    fn send(&self, msg: ScriptIn) {
        if let Some(tx) = self.tx.borrow().as_ref() {
            let _ = tx.send(msg);
        }
    }

    /// Encerra a thread do script (as tarefas assíncronas dela também têm o canal, então
    /// fechar o nosso lado não basta).
    fn stop(&self) {
        if let Some(tx) = self.tx.borrow_mut().take() {
            let _ = tx.send(ScriptIn::Stop);
        }
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
}

impl PluginMini {
    #[allow(deprecated)] // ComponentFactory: experimental no Slint 1.18
    fn create(host: Rc<Host>, plugin: Plugin, dir: &Path, cards: Rc<VecModel<PluginCard>>, waker: Waker) -> Rc<RefCell<Self>> {
        let seed = 20 + cards.row_count() as u32;
        let mut error: Option<String> = None;
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
                    eprintln!("miniutils: plugin {}: {e}", plugin.manifest.id);
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
            full.send(ScriptIn::Tick);
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
        full.stop();
        let (tx, rx) = channel();
        let m = &self.plugin.manifest;
        let env = ScriptEnv {
            exec: self.host.exec_for(m),
            net: m.permissions.net.clone(),
            data: Some(plugins::data_file(&plugins::dir(), &m.id)),
            back: Some(tx.clone()),
        };
        *full.tx.borrow_mut() = Some(tx);
        let id = m.id.clone();
        let settings = self.settings_map();
        let source = full.source.clone();
        let waker = self.waker.clone();
        let spawned = std::thread::Builder::new().name(format!("plugin-{id}")).spawn(move || {
            script_loop(source, env, settings, rx, move |batch| {
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
                ScriptOut::Fact(kind, subject, value) => {
                    let voice = self.voice();
                    self.facts.push(Fact { kind, voice, subject, value });
                }
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
        self.mood_changed(before);
        self.push(ui);
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
            voice: Voice::id_of(&self.plugin.manifest.id, &self.plugin.manifest.name).into(),
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
        if let Some(full) = &self.full {
            full.stop();
        }
        self.facts.clear();
    }

    /// Leitura de um provedor de `uses`: vai ao `on_event` do script, como mapa.
    fn on_event(&mut self, ev: &Event, _ui: &HubWindow) {
        if let Some(full) = &self.full {
            full.send(ScriptIn::Event(ev.topic().name().into(), json_to_dyn(event_data(ev))));
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
        if self.refresh_mascot() || (self.plugin.run.is_some() && frame % 8 == 0) {
            self.push(ui);
        }
    }

    fn settings_changed(&mut self, ui: &HubWindow) {
        if let Some(full) = &self.full {
            full.send(ScriptIn::Settings(self.settings_map()));
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
            full.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(exec: &[&str], data: Option<PathBuf>) -> ScriptEnv {
        let exec: Vec<String> = exec.iter().map(|s| s.to_string()).collect();
        ScriptEnv { exec: Exec::new("t", &exec), net: vec!["api.exemplo.com".into()], data, back: None }
    }

    fn rodar_com(script: &str, env: ScriptEnv, entradas: Vec<ScriptIn>) -> Vec<ScriptOut> {
        let (tx, rx) = channel();
        for e in entradas {
            tx.send(e).unwrap();
        }
        drop(tx);
        let saida = Arc::new(Mutex::new(Vec::new()));
        let s = saida.clone();
        script_loop(script.into(), env, rhai::Map::new(), rx, move |b| s.lock().unwrap().extend(b));
        let v = saida.lock().unwrap().clone();
        v
    }

    fn rodar(script: &str, entradas: Vec<ScriptIn>) -> Vec<ScriptOut> {
        rodar_com(script, env(&["echo"], None), entradas)
    }

    #[test]
    fn script_recebe_provedores_guarda_dados_e_conta_fatos() {
        let arq = std::env::temp_dir().join(format!("miniutils-dados-{}/.data/a.b.json", std::process::id()));
        let _ = std::fs::remove_dir_all(arq.parent().unwrap().parent().unwrap());
        let s = r#"
            fn start() { let n = store_get("vezes"); if n == () { n = 0; } store_set("vezes", n + 1); ui("vezes", n + 1); }
            fn on_event(topic, data) {
                ui("cpu", data.cpu);
                ui("disco", data.disks[0].mount);
                if data.cpu > 90.0 { fact("strain", "CPU", data.cpu); }
                let j = parse_json(`{"a": [1, 2.5, "x"], "b": null}`);
                ui("json", to_json(j.a));
            }
        "#;
        let sys = crate::providers::sys::SysSnapshot {
            cpu: 93.0,
            disks: vec![crate::providers::sys::Disk { mount: "/".into(), used: 1, total: 2 }],
            ..Default::default()
        };
        let ev = ScriptIn::Event("sys".into(), json_to_dyn(event_data(&Event::Sys(sys))));
        let out = format!("{:?}", rodar_com(s, env(&[], Some(arq.clone())), vec![ev]));
        assert!(!out.contains("Error("), "{out}");
        assert!(out.contains(r#"Set("vezes", Num(1.0))"#), "{out}");
        assert!(out.contains(r#"Set("cpu", Num(93.0))"#), "{out}");
        assert!(out.contains(r#"Set("disco", Str("/"))"#), "{out}");
        assert!(out.contains(r#"Fact(Strain, "CPU", Some(93.0))"#), "{out}");
        assert!(out.contains(r#"Set("json", Str("[1,2.5,\"x\"]"))"#), "{out}");
        // os dados sobrevivem: a segunda vez lê o que a primeira gravou
        let out = format!("{:?}", rodar_com(s, env(&[], Some(arq.clone())), vec![]));
        assert!(out.contains(r#"Set("vezes", Num(2.0))"#), "{out}");
        let _ = std::fs::remove_dir_all(arq.parent().unwrap().parent().unwrap());
        // tipo de fato que não existe vira erro na tela
        assert!(format!("{:?}", rodar(r#"fn start() { fact("fofoca", "x"); }"#, vec![])).contains("desconhecido"));
    }

    #[test]
    #[cfg(unix)] // roda o `echo`, que no Windows não é um programa (é comando do cmd)
    fn run_async_responde_sem_travar_o_script() {
        let (tx, rx) = channel();
        let mut e = env(&["echo"], None);
        e.back = Some(tx.clone());
        let s = r#"
            fn start() { ui("pediu", run_async(["echo", "oi"], "fim")); ui("negado", run_async(["rm"], "fim")); }
            fn fim(r) { let o = r.out; o.trim(); ui("eco:" + r.ok, o); }
        "#;
        let saida = Arc::new(Mutex::new(Vec::new()));
        let sd = saida.clone();
        let t = std::thread::spawn(move || script_loop(s.into(), e, rhai::Map::new(), rx, move |b| sd.lock().unwrap().extend(b)));
        let fim = Instant::now() + Duration::from_secs(10);
        let tem = |txt: &str| format!("{:?}", saida.lock().unwrap()).contains(txt);
        while !(tem("eco:true") && tem("eco:false")) && Instant::now() < fim {
            std::thread::sleep(Duration::from_millis(20));
        }
        tx.send(ScriptIn::Stop).unwrap();
        t.join().unwrap();
        let out = format!("{:?}", saida.lock().unwrap());
        assert!(out.contains(r#"Set("pediu", Bool(true))"#), "{out}");
        assert!(out.contains(r#"Set("eco:true", Str("oi"))"#), "{out}");
        // o programa fora da allowlist roda "assíncrono" também, e falha
        assert!(out.contains(r#"Set("eco:false", Str(""))"#), "{out}");
    }

    #[test]
    fn http_so_https_e_nos_dominios_do_manifesto() {
        let net = vec!["api.exemplo.com".to_string()];
        let url = |u: &str| reqwest::Url::parse(u).unwrap();
        assert!(check_url(&url("https://api.exemplo.com/v1?x=1"), &net).is_ok());
        assert!(check_url(&url("http://api.exemplo.com/"), &net).unwrap_err().contains("https"));
        assert!(check_url(&url("https://outro.com/"), &net).unwrap_err().contains("net"));
        assert!(check_url(&url("https://exemplo.com/"), &net).is_err(), "subdomínio não vale pelo domínio");
        assert!(check_url(&url("https://u:s@api.exemplo.com/"), &net).is_err());
        assert!(check_url(&url("https://api.exemplo.com:8443/"), &net).is_err());
        // recusado antes de qualquer conexão
        let out = format!("{:?}", rodar(r#"fn start() { let r = http_get("https://evil.com/"); ui("r", r.ok); ui("e", r.err); }"#, vec![]));
        assert!(out.contains(r#"Set("r", Bool(false))"#) && out.contains("evil.com não está em [permissions] net"), "{out}");
    }

    #[test]
    fn tela_em_varios_arquivos_e_imagens_so_de_assets() {
        let dir = std::env::temp_dir().join(format!("miniutils-tela-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("ui")).unwrap();
        std::fs::create_dir_all(dir.join("assets")).unwrap();
        std::fs::write(
            dir.join("ui/barra.slint"),
            "export component Barra inherits Rectangle { in property <float> v; Image { source: @image-url(\"../assets/x.png\"); } }",
        )
        .unwrap();
        let view = "import { Barra } from \"ui/barra.slint\";\nexport component Main inherits Rectangle { in property <float> cpu; Barra { v: cpu; } }";
        let def = compile_view(&dir, view.into()).unwrap();
        assert!(def.properties().any(|(n, _)| n == "cpu"));
        // importar fora de ui/: recusa
        std::fs::write(dir.join("fora.slint"), "export component Barra inherits Rectangle {}").unwrap();
        let fora = view.replace("ui/barra.slint", "fora.slint");
        assert!(compile_view(&dir, fora).is_err());
        // imagem fora de assets/ (no ui/ ou na view): recusa
        std::fs::write(dir.join("ui/barra.slint"), "export component Barra inherits Rectangle { Image { source: @image-url(\"/etc/x.png\"); } }").unwrap();
        assert!(compile_view(&dir, view.into()).err().unwrap_or_default().contains("assets/"));
        let img = "export component Main inherits Rectangle { Image { source: @image-url(\"../segredo.png\"); } }";
        assert!(compile_view(&dir, img.into()).err().unwrap_or_default().contains("assets/"));
        let _ = std::fs::remove_dir_all(&dir);
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
    #[cfg(unix)] // roda o `echo`, que no Windows não é um programa (é comando do cmd)
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
        let e = ScriptEnv { exec: Exec::new("t", &p.manifest.permissions.exec), net: Vec::new(), data: None, back: None };
        script_loop(script, e, rhai::Map::new(), rx, move |b| s.lock().unwrap().extend(b));
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
