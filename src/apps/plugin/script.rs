//! Script of plugins with a screen (`main.rhai`): runs on a thread of its own and talks to
//! the screen through messages (`ScriptIn`/`ScriptOut`). The API is in the docs of `apps/plugin`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use slint::{ModelRc, VecModel};
use slint_interpreter::Value;

use super::db::{Db, DbSpec};
use crate::apps::external::FromApp;
use crate::core::bus::Event;
use crate::core::chatter::FactKind;
use crate::core::exec::Exec;
use crate::tr;

/// Time limit of a script `run()`.
const RUN_TIMEOUT: Duration = Duration::from_secs(30);

/// Value the script sends to the screen (no `Rc`: it crosses threads).
#[derive(Debug, Clone)]
pub(super) enum PValue {
    Str(String),
    Num(f64),
    Bool(bool),
    List(Vec<PValue>),
    Map(Vec<(String, PValue)>),
    /// Image read from disk (only external apps send it).
    Image(PathBuf),
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

    /// From an external app's JSON (`null` becomes empty text).
    fn from_json(v: &serde_json::Value) -> PValue {
        use serde_json::Value as J;
        match v {
            J::Null => PValue::Str(String::new()),
            J::Bool(b) => PValue::Bool(*b),
            J::Number(n) => PValue::Num(n.as_f64().unwrap_or(0.0)),
            J::String(s) => PValue::Str(s.clone()),
            J::Array(a) => PValue::List(a.iter().map(PValue::from_json).collect()),
            J::Object(o) => PValue::Map(o.iter().map(|(k, v)| (k.clone(), PValue::from_json(v))).collect()),
        }
    }

    /// For the screen (on the UI thread).
    pub(super) fn to_value(&self) -> Value {
        match self {
            PValue::Str(s) => Value::String(s.as_str().into()),
            PValue::Num(n) => Value::Number(*n),
            PValue::Bool(b) => Value::Bool(*b),
            PValue::List(l) => Value::Model(ModelRc::new(VecModel::from(l.iter().map(PValue::to_value).collect::<Vec<_>>()))),
            PValue::Map(m) => Value::Struct(m.iter().map(|(k, v)| (k.clone(), v.to_value())).collect()),
            PValue::Image(p) => Value::Image(load_image(p)),
        }
    }
}

/// Reads an image the external app pointed to (empty, and a line in the log, if it can't).
pub(super) fn load_image(path: &Path) -> slint::Image {
    slint::Image::load_from_path(path).unwrap_or_else(|_| {
        log::warn!("image {}: couldn't load it", path.display());
        slint::Image::default()
    })
}

/// From the script to the UI.
#[derive(Debug, Clone)]
pub(super) enum ScriptOut {
    Set(String, PValue),
    Mood(String),
    Summary(String),
    Notify(String, String),
    /// Fact for the chat between mascots: kind, subject and number.
    Fact(FactKind, String, Option<f32>),
    /// Error in the cell (empty text = the external app says it's over).
    Error(String),
    /// The external app's process exited without the host asking.
    Exited(String),
    /// The data migration finished: the new version, or the error (the host restores the copy).
    Migrated(Result<u32, String>),
}

/// From the UI (or an async task) to the script.
pub(super) enum ScriptIn {
    Tick,
    Action(String, String),
    Settings(rhai::Map),
    /// Reading from a provider the plugin subscribes to (`uses`): topic name and data.
    Event(String, rhai::Dynamic),
    /// A `run_async`/`http_get_async` finished: name of the function to call and the result.
    Done(String, rhai::Map),
    /// Turned off: the script thread ends.
    Stop,
}

/// Time limit and maximum size of an `http_get`.
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);
const HTTP_MAX: usize = 1 << 20;
/// Async tasks (`run_async`, `http_get_async`) at the same time, per plugin.
const MAX_ASYNC: usize = 4;
/// Maximum size of the stored data (`store_set`), as JSON.
const STORE_MAX: usize = 1 << 20;

/// What the plugin's script may use from the host.
#[derive(Clone)]
pub(super) struct ScriptEnv {
    pub(super) exec: Exec,
    /// Domains of `[permissions] net`.
    pub(super) net: Vec<String>,
    /// Data file (`store_get`/`store_set`); `None` = in memory only.
    pub(super) data: Option<PathBuf>,
    /// The app declared `kv` (without it, `store_set` is an error).
    pub(super) kv: bool,
    /// The database, if the app declared `sqlite` (`db.rs`).
    pub(super) db: Option<DbSpec>,
    /// Migrate the data from one version to another before `start()` (`fn migrate(from, to)`).
    pub(super) migrate: Option<(u32, u32)>,
    /// Channel back for async tasks (`None` = none).
    pub(super) back: Option<Sender<ScriptIn>>,
    /// The plugin's own catalog (`i18n/<lang>.po`) for `tr()`; empty = English.
    pub(super) catalog: Arc<crate::core::i18n::Catalog>,
}

/// The data the script stores, loaded from the file and written back by the host.
#[derive(Default)]
pub(super) struct Store {
    data: rhai::Map,
    dirty: bool,
    /// The file exists but couldn't be read: never written over in this run.
    read_only: bool,
}

impl Store {
    /// Loads the data, and the problem to show on the card, if there was one. A file that
    /// isn't a JSON object goes aside as `<name>.broken` and the store starts empty; a file
    /// that can't be read (locked, no permission) is kept, and nothing is saved over it.
    fn load(path: Option<&Path>) -> (Store, Option<String>) {
        let mut store = Store::default();
        let Some(path) = path else { return (store, None) };
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (store, None),
            Err(e) => {
                log::warn!("{}: {e}; the stored data won't be saved in this run", path.display());
                store.read_only = true;
                return (store, Some(tr!("couldn't read the stored data ({}); changes won't be saved", e)));
            }
        };
        let parsed = serde_json::from_slice::<serde_json::Value>(&bytes)
            .map_err(|e| e.to_string())
            .and_then(|v| json_to_dyn(v).try_cast::<rhai::Map>().ok_or_else(|| "not a JSON object".to_string()));
        match parsed {
            Ok(data) => store.data = data,
            Err(e) => {
                let mut broken = path.as_os_str().to_owned();
                broken.push(".broken");
                let broken = PathBuf::from(broken);
                log::warn!("{}: {e}; moved to {}", path.display(), broken.display());
                if let Err(err) = std::fs::rename(path, &broken) {
                    log::warn!("{}: {err}; the stored data won't be saved in this run", broken.display());
                    store.read_only = true;
                }
                return (store, Some(tr!("the stored data was damaged and was set aside as {}", broken.display())));
            }
        }
        (store, None)
    }

    /// Writes if it changed (temporary file and `rename`: never left half-written).
    fn save(&mut self, path: Option<&Path>) -> Result<(), String> {
        if !self.dirty || self.read_only {
            return Ok(());
        }
        self.dirty = false;
        let Some(path) = path else { return Ok(()) };
        let json = serde_json::to_vec(&dyn_to_json(&self.data.clone().into())).map_err(|e| e.to_string())?;
        if json.len() > STORE_MAX {
            return Err(tr!("store_set: the data is over {} KiB; it wasn't saved", STORE_MAX >> 10));
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        crate::core::os::write_atomic(path, &json).map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// JSON → Rhai value (whole numbers stay integers).
pub(super) fn json_to_dyn(v: serde_json::Value) -> rhai::Dynamic {
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

/// Rhai value → JSON (whatever has no equivalent becomes text).
pub(super) fn dyn_to_json(d: &rhai::Dynamic) -> serde_json::Value {
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

/// The data of a provider reading, as the script gets it in `on_event`.
pub(super) fn event_data(ev: &Event) -> serde_json::Value {
    let v = match ev {
        Event::Sys(s) => serde_json::to_value(s),
        Event::Gpu(s) => serde_json::to_value(s),
        Event::Docker(s) => serde_json::to_value(s),
        Event::Kube(s) => serde_json::to_value(s),
        Event::Media(s) => serde_json::to_value(s),
        Event::AiUsage(s) => serde_json::to_value(s),
        Event::Ports(s) => serde_json::to_value(s),
        Event::Agents(s) => serde_json::to_value(s),
        Event::Procs(s) => serde_json::to_value(s),
    };
    v.unwrap_or(serde_json::Value::Null)
}

/// `url` is https, on the default port, without a user, and its domain is in `allowed`.
fn check_url(url: &reqwest::Url, allowed: &[String]) -> Result<(), String> {
    if url.scheme() != "https" {
        return Err(tr!("{}: https:// only", url));
    }
    if !url.username().is_empty() || url.password().is_some() || url.port().is_some() {
        return Err(tr!("{}: no user or port in the address", url));
    }
    let host = url.host_str().unwrap_or_default();
    if !allowed.iter().any(|h| h == host) {
        return Err(tr!("{} is not in [permissions] net", host));
    }
    Ok(())
}

/// A script's `http_get`: the domains of `[permissions] net` and one HTTP client for all
/// its requests (building one loads the system certificates), made on the first request.
struct Http {
    allowed: Vec<String>,
    client: std::sync::OnceLock<Result<reqwest::blocking::Client, String>>,
}

impl Http {
    fn new(allowed: Vec<String>) -> Self {
        Http { allowed, client: std::sync::OnceLock::new() }
    }

    /// The client: time limit, and redirects only to an allowed domain.
    fn client(&self) -> Result<&reqwest::blocking::Client, String> {
        let built = self.client.get_or_init(|| {
            let hosts = self.allowed.clone();
            reqwest::blocking::Client::builder()
                .timeout(HTTP_TIMEOUT)
                .user_agent(concat!("miniutils/", env!("CARGO_PKG_VERSION")))
                .redirect(reqwest::redirect::Policy::custom(move |a| {
                    if a.previous().len() >= 5 {
                        a.error(tr!("too many redirects"))
                    } else if let Err(e) = check_url(a.url(), &hosts) {
                        a.error(tr!("redirected outside what is allowed: {}", e))
                    } else {
                        a.follow()
                    }
                }))
                .build()
                .map_err(|e| e.to_string())
        });
        built.as_ref().map_err(Clone::clone)
    }
}

/// GET with a time and size limit; redirects only to an allowed domain.
fn http_get(http: &Http, url: &str) -> Result<(u16, String), String> {
    use std::io::Read;
    let parsed = reqwest::Url::parse(url).map_err(|e| format!("{url}: {e}"))?;
    check_url(&parsed, &http.allowed)?;
    let resp = http.client()?.get(parsed).send().map_err(|e| format!("{url}: {e}"))?;
    let status = resp.status().as_u16();
    let mut body = Vec::new();
    resp.take(HTTP_MAX as u64 + 1).read_to_end(&mut body).map_err(|e| format!("{url}: {e}"))?;
    if body.len() > HTTP_MAX {
        return Err(tr!("{}: the response is over 1 MiB", url));
    }
    Ok((status, String::from_utf8_lossy(&body).into_owned()))
}

/// Result of `run`: #{ ok, out, err }.
fn run_result(exec: &Exec, cmd: &rhai::Array) -> rhai::Map {
    let argv: Vec<String> = cmd.iter().map(|d| d.to_string()).collect();
    let result = match argv.split_first() {
        Some((prog, args)) => {
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            exec.run_blocking(prog, &args, RUN_TIMEOUT).map_err(|e| e.to_string())
        }
        None => Err(tr!("run([]): empty command").into()),
    };
    let mut m = rhai::Map::new();
    m.insert("ok".into(), result.is_ok().into());
    m.insert("out".into(), result.clone().unwrap_or_default().into());
    m.insert("err".into(), result.err().unwrap_or_default().into());
    m
}

/// Result of `http_get`: #{ ok, status, body, err } (`ok` = answered with 2xx).
fn http_result(http: &Http, url: &str) -> rhai::Map {
    let r = http_get(http, url);
    let mut m = rhai::Map::new();
    let (status, body) = r.as_ref().map(|(s, b)| (*s, b.clone())).unwrap_or_default();
    m.insert("ok".into(), (r.is_ok() && (200..300).contains(&status)).into());
    m.insert("status".into(), (status as i64).into());
    m.insert("body".into(), body.into());
    m.insert("err".into(), r.err().unwrap_or_default().into());
    m
}

/// Runs `job` on a thread and sends the result to the script's function `cb`. `false` if it
/// couldn't (task limit, or no channel back).
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

/// A fact for the chat ("strain", "CPU", 93), from the script or the external app.
fn fact_out(kind: &str, subject: &str, value: Option<f32>) -> Result<ScriptOut, String> {
    let k = FactKind::ALL
        .into_iter()
        .find(|k| k.key() == kind && *k != FactKind::Idle)
        .ok_or_else(|| tr!("fact: unknown kind \"{}\" (trouble, strain, forecast, recovered, poked)", kind))?;
    Ok(ScriptOut::Fact(k, subject.to_string(), value))
}

/// Message from the external app → what the UI applies (the same as the script sends). A
/// relative image path is relative to `dir`, the app's folder (where the process runs).
pub(super) fn from_app(msg: FromApp, dir: &Path) -> ScriptOut {
    match msg {
        FromApp::Set { prop, value } => ScriptOut::Set(prop, PValue::from_json(&value)),
        FromApp::Image { prop, path } => ScriptOut::Set(prop, PValue::Image(dir.join(path))),
        FromApp::Mood { mood } => ScriptOut::Mood(mood),
        FromApp::Summary { text } => ScriptOut::Summary(text),
        FromApp::Notify { title, body } => ScriptOut::Notify(title, body),
        FromApp::Fact { kind, subject, value } => {
            fact_out(&kind, &subject, value.map(|v| v as f32)).unwrap_or_else(ScriptOut::Error)
        }
        FromApp::Error { message } => ScriptOut::Error(message),
        FromApp::Migrated { version } => ScriptOut::Migrated(Ok(version)),
    }
}

/// The interpreter with the host API and the limits (a script in a loop hangs nothing: it
/// blows the operation limit and becomes an error on screen).
pub(super) fn engine(
    out: Arc<Mutex<Vec<ScriptOut>>>,
    env: ScriptEnv,
    settings: Arc<Mutex<rhai::Map>>,
    store: Arc<Mutex<Store>>,
    db: Arc<Mutex<Db>>,
) -> rhai::Engine {
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
    // texts the script builds, in the interface language: tr("Up for {}", [x]) with the
    // plugin's i18n/<lang>.po (like @tr in the view); lang() is "en" or "pt-BR"
    let cat = env.catalog.clone();
    e.register_fn("tr", move |s: &str| cat.get(s).unwrap_or(s).to_string());
    let cat = env.catalog.clone();
    e.register_fn("tr", move |s: &str, args: rhai::Array| {
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        let refs: Vec<&dyn std::fmt::Display> = args.iter().map(|a| a as &dyn std::fmt::Display).collect();
        crate::core::i18n::format(cat.get(s).unwrap_or(s), &refs)
    });
    e.register_fn("lang", || crate::core::i18n::current().tag().to_string());
    let p = push.clone();
    e.register_fn("notify", move |t: &str, b: &str| p(ScriptOut::Notify(t.to_string(), b.to_string())));
    let p = push.clone();
    e.on_print(move |s| p(ScriptOut::Summary(s.to_string())));
    e.on_debug(|s, _, _| log::debug!("plugin: {s}"));
    e.register_fn("now", crate::apps::kit::unix_now);
    e.register_fn("setting", move |key: &str| -> rhai::Dynamic {
        settings.lock().ok().and_then(|m| m.get(key).cloned()).unwrap_or(rhai::Dynamic::UNIT)
    });

    // chat between mascots: fact("strain", "CPU", 93)
    type Res<T> = Result<T, Box<rhai::EvalAltResult>>;
    let fact = move |kind: &str, subject: &str, value: Option<f32>| -> Res<()> {
        push(fact_out(kind, subject, value)?);
        Ok(())
    };
    let f = fact.clone();
    e.register_fn("fact", move |k: &str, s: &str| f(k, s, None));
    let f = fact.clone();
    e.register_fn("fact", move |k: &str, s: &str, v: i64| f(k, s, Some(v as f32)));
    e.register_fn("fact", move |k: &str, s: &str, v: f64| fact(k, s, Some(v as f32)));

    // data kept between runs (`kv` in [data]; without [data], the script has it)
    let st = store.clone();
    e.register_fn("store_get", move |key: &str| -> rhai::Dynamic {
        st.lock().ok().and_then(|s| s.data.get(key).cloned()).unwrap_or(rhai::Dynamic::UNIT)
    });
    let kv = env.kv;
    e.register_fn("store_set", move |key: &str, value: rhai::Dynamic| -> Res<()> {
        if !kv {
            return Err(tr!("store_set: this app didn't declare kv in the [data] of manifest.toml").into());
        }
        if let Ok(mut s) = store.lock() {
            if value.is_unit() {
                s.data.remove(key);
            } else {
                s.data.insert(key.into(), value);
            }
            s.dirty = true;
        }
        Ok(())
    });

    // SQLite database (`sqlite` in [data]; see db.rs)
    fn lock(db: &Mutex<Db>) -> Result<std::sync::MutexGuard<'_, Db>, Box<rhai::EvalAltResult>> {
        db.lock().map_err(|_| tr!("database: locked").into())
    }
    let d = db.clone();
    e.register_fn("db_batch", move |sql: &str| -> Res<()> { lock(&d)?.batch(sql).map_err(Into::into) });
    let d = db.clone();
    e.register_fn("db_exec", move |sql: &str| -> Res<i64> { lock(&d)?.exec(sql, &Vec::new()).map_err(Into::into) });
    let d = db.clone();
    e.register_fn("db_exec", move |sql: &str, p: rhai::Array| -> Res<i64> { lock(&d)?.exec(sql, &p).map_err(Into::into) });
    let d = db.clone();
    e.register_fn("db_query", move |sql: &str| -> Res<rhai::Array> { lock(&d)?.query(sql, &Vec::new()).map_err(Into::into) });
    e.register_fn("db_query", move |sql: &str, p: rhai::Array| -> Res<rhai::Array> { lock(&db)?.query(sql, &p).map_err(Into::into) });

    // JSON
    e.register_fn("parse_json", |text: &str| -> Res<rhai::Dynamic> {
        serde_json::from_str::<serde_json::Value>(text).map(json_to_dyn).map_err(|err| format!("parse_json: {err}").into())
    });
    e.register_fn("to_json", |value: rhai::Dynamic| dyn_to_json(&value).to_string());

    // allowlisted programs and domains of [permissions] net
    let pending = Arc::new(AtomicUsize::new(0));
    let ex = env.exec.clone();
    e.register_fn("run", move |cmd: rhai::Array| run_result(&ex, &cmd));
    let http = Arc::new(Http::new(env.net.clone()));
    let h = http.clone();
    e.register_fn("http_get", move |url: &str| http_result(&h, url));
    let (en, pe) = (env.clone(), pending.clone());
    e.register_fn("run_async", move |cmd: rhai::Array, cb: &str| {
        let exec = en.exec.clone();
        spawn_async(&en, &pe, cb.to_string(), move || run_result(&exec, &cmd))
    });
    e.register_fn("http_get_async", move |url: &str, cb: &str| {
        let (url, http) = (url.to_string(), http.clone());
        spawn_async(&env, &pending, cb.to_string(), move || http_result(&http, &url))
    });
    e
}

/// The loop of the script thread: runs the top of the file, the data migration (if the host
/// asked), `start()`, and then serves requests until `Stop` (or the channel closes). Each
/// batch of messages goes to the UI through `deliver`; data stored with `store_set` goes to
/// disk after each call, and a database transaction left open is rolled back.
pub(super) fn script_loop(source: String, env: ScriptEnv, settings: rhai::Map, rx: Receiver<ScriptIn>, deliver: impl Fn(Vec<ScriptOut>)) {
    let out = Arc::new(Mutex::new(Vec::new()));
    let shared_settings = Arc::new(Mutex::new(settings.clone()));
    let data = env.data.clone();
    let migrate = env.migrate;
    let (store, damaged) = Store::load(data.as_deref());
    let store = Arc::new(Mutex::new(store));
    let db = Arc::new(Mutex::new(Db::new(env.db.clone())));
    let engine = engine(out.clone(), env, shared_settings.clone(), store.clone(), db.clone());
    // an error is on the card: the next call that runs fine clears it (once)
    let failed = std::cell::Cell::new(false);
    let error = |e: String| {
        failed.set(true);
        if let Ok(mut v) = out.lock() {
            v.push(ScriptOut::Error(e));
        }
    };
    // not through `error`: a fine `start()` must not hide it at once
    if let (Some(e), Ok(mut v)) = (damaged, out.lock()) {
        v.push(ScriptOut::Error(e));
    }
    let flush = || {
        let saved = store.lock().map_err(|e| e.to_string()).and_then(|mut s| s.save(data.as_deref()));
        if let Err(e) = saved {
            error(e);
        }
        if let Some(warn) = db.lock().ok().and_then(|mut d| d.end_call()) {
            error(warn);
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
    // `Ok(true)`: a hook ran and finished fine; `Ok(false)`: there was no hook to run
    let report = |r: Result<bool, Box<rhai::EvalAltResult>>| match r {
        Err(e) => error(format!("main.rhai: {e}")),
        Ok(true) if failed.replace(false) => {
            if let Ok(mut v) = out.lock() {
                v.push(ScriptOut::Error(String::new()));
            }
        }
        Ok(_) => {}
    };
    report(engine.run_ast_with_scope(&mut scope, &ast).map(|_| false));
    let mut call = |name: &str, args: Vec<rhai::Dynamic>, this: &mut rhai::Dynamic| {
        if !has(name, args.len()) {
            return Ok(false);
        }
        let opts = rhai::CallFnOptions::new().eval_ast(false).rewind_scope(true).bind_this_ptr(this);
        engine.call_fn_with_options::<rhai::Dynamic>(opts, &mut scope, &ast, name, args).map(|_| true)
    };
    // data from an old version: fn migrate(from, to) before anything. If it fails: close the
    // database and save nothing (the host restores the copy from before) and the app doesn't start.
    if let Some((from, to)) = migrate {
        let r = call("migrate", vec![(from as rhai::INT).into(), (to as rhai::INT).into()], &mut this);
        let open_tx = db.lock().ok().and_then(|mut d| d.end_call());
        let broke = match (r, open_tx) {
            (Err(e), _) => Some(format!("main.rhai: {e}")),
            (Ok(_), Some(w)) => Some(w),
            (Ok(_), None) => None,
        };
        if let Some(e) = broke {
            if let Ok(mut d) = db.lock() {
                d.close();
            }
            deliver(vec![ScriptOut::Migrated(Err(e))]);
            return;
        }
        flush();
        deliver(vec![ScriptOut::Migrated(Ok(to))]);
    }
    report(call("start", vec![], &mut this));
    flush();
    while let Ok(msg) = rx.recv() {
        let r = match msg {
            ScriptIn::Stop => break,
            ScriptIn::Tick => call("tick", vec![], &mut this),
            ScriptIn::Action(n, a) => call("action", vec![n.into(), a.into()], &mut this),
            ScriptIn::Event(topic, data) => call("on_event", vec![topic.into(), data], &mut this),
            ScriptIn::Done(cb, result) if has(&cb, 1) => call(&cb, vec![result.into()], &mut this),
            ScriptIn::Done(cb, _) => Err(tr!("missing fn {}(result) to receive the result", cb).into()),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;
    #[cfg(unix)] // only the `run_async` test (which is Unix-only) uses it
    use std::time::Instant;

    fn env(exec: &[&str], data: Option<PathBuf>) -> ScriptEnv {
        let exec: Vec<String> = exec.iter().map(|s| s.to_string()).collect();
        ScriptEnv { exec: Exec::new("t", &exec), net: vec!["api.example.com".into()], data, kv: true, db: None, migrate: None, back: None, catalog: Default::default() }
    }

    fn run_with(script: &str, env: ScriptEnv, inputs: Vec<ScriptIn>) -> Vec<ScriptOut> {
        let (tx, rx) = channel();
        for e in inputs {
            tx.send(e).unwrap();
        }
        drop(tx);
        let output = Arc::new(Mutex::new(Vec::new()));
        let s = output.clone();
        script_loop(script.into(), env, rhai::Map::new(), rx, move |b| s.lock().unwrap().extend(b));

        output.lock().unwrap().clone()
    }

    fn run(script: &str, inputs: Vec<ScriptIn>) -> Vec<ScriptOut> {
        run_with(script, env(&["echo"], None), inputs)
    }

    #[test]
    fn error_clears_once_the_next_call_works() {
        let s = r#"
            fn start() { this.n = 0; }
            fn tick() { this.n += 1; if this.n == 1 { throw "boom"; } }
        "#;
        let errors: Vec<String> = run(s, vec![ScriptIn::Tick, ScriptIn::Tick, ScriptIn::Tick])
            .into_iter()
            .filter_map(|m| match m {
                ScriptOut::Error(e) => Some(e),
                _ => None,
            })
            .collect();
        // the failure, then one clear (not one per later call)
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert!(errors[0].contains("boom"), "{errors:?}");
        assert_eq!(errors[1], "");
    }

    #[test]
    fn damaged_store_is_set_aside_not_overwritten() {
        let dir = std::env::temp_dir().join(format!("miniutils-script-broken-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("store.json");
        std::fs::write(&file, "{not json").unwrap();
        let out = run_with(r#"fn start() { store_set("a", 1); }"#, env(&[], Some(file.clone())), vec![]);
        assert!(matches!(&out[0], ScriptOut::Error(e) if e.contains("store.json.broken")), "{out:?}");
        assert_eq!(std::fs::read_to_string(dir.join("store.json.broken")).unwrap(), "{not json");
        // the store started empty and saved the new value in a fresh file
        assert_eq!(std::fs::read_to_string(&file).unwrap(), r#"{"a":1}"#);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn external_app_messages_become_script_ones() {
        let dir = Path::new("/apps/x");
        let set = from_app(FromApp::Set { prop: "n".into(), value: serde_json::json!({"a": [1, true]}) }, dir);
        assert!(matches!(&set, ScriptOut::Set(p, PValue::Map(m)) if p == "n" && matches!(&m[0].1, PValue::List(l) if l.len() == 2)));
        // a relative image path is the app folder's; an absolute one stays
        let image = |path: &str| match from_app(FromApp::Image { prop: "g".into(), path: path.into() }, dir) {
            ScriptOut::Set(_, PValue::Image(p)) => p.to_string_lossy().replace('\\', "/"),
            other => panic!("{other:?}"),
        };
        assert_eq!(image("assets/chart.png"), "/apps/x/assets/chart.png");
        #[cfg(unix)]
        assert_eq!(image("/tmp/x.png"), "/tmp/x.png");
        assert!(matches!(
            from_app(FromApp::Fact { kind: "strain".into(), subject: "CPU".into(), value: Some(93.0) }, dir),
            ScriptOut::Fact(FactKind::Strain, s, Some(_)) if s == "CPU"
        ));
        // an unknown fact kind becomes an error in the cell, not a panic
        assert!(matches!(from_app(FromApp::Fact { kind: "x".into(), subject: String::new(), value: None }, dir), ScriptOut::Error(e) if e.contains("unknown")));
    }

    #[test]
    fn script_uses_the_database_and_migrates_before_starting() {
        let dir = std::env::temp_dir().join(format!("miniutils-script-db-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let with = |migrate: Option<(u32, u32)>| {
            let mut e = env(&[], None);
            e.db = Some(DbSpec { path: dir.join("data.sqlite"), max_bytes: 1 << 20 });
            e.migrate = migrate;
            e
        };
        let s = r#"
            fn migrate(from, to) {
                if from == 1 { db_batch("ALTER TABLE notes ADD COLUMN done INTEGER DEFAULT 0"); }
                if to == 9 { throw "can't go to 9"; }
            }
            fn start() {
                db_batch("CREATE TABLE IF NOT EXISTS notes(text TEXT)");
                db_exec("INSERT INTO notes(text) VALUES (?)", ["hi"]);
                let rows = db_query("SELECT count(*) AS n FROM notes");
                ui("n", rows[0].n);
            }
        "#;
        let out = format!("{:?}", run_with(s, with(None), vec![]));
        assert!(out.contains(r#"Set("n", Num(1.0))"#), "{out}");
        // version 1 → 2: the migration runs before start and the database carries on
        let out = format!("{:?}", run_with(s, with(Some((1, 2))), vec![]));
        assert!(out.contains("Migrated(Ok(2))") && out.contains(r#"Set("n", Num(2.0))"#), "{out}");
        // the migration fails: no start
        let out = format!("{:?}", run_with(s, with(Some((2, 9))), vec![]));
        assert!(out.contains("Migrated(Err(") && out.contains("can't go to") && !out.contains(r#"Set("n""#), "{out}");
        let _ = std::fs::remove_dir_all(&dir);
        // undeclared: database and kv become errors on screen
        let mut undeclared = env(&[], None);
        undeclared.kv = false;
        let out = format!("{:?}", run_with(r#"fn start() { store_set("a", 1); }"#, undeclared, vec![]));
        assert!(out.contains("didn't declare kv"), "{out}");
        let out = format!("{:?}", run(r#"fn start() { db_exec("SELECT 1"); }"#, vec![]));
        assert!(out.contains("[data]"), "{out}");
    }

    #[test]
    fn script_gets_providers_keeps_data_and_tells_facts() {
        let file = std::env::temp_dir().join(format!("miniutils-data-{}/.data/a.b.json", std::process::id()));
        let _ = std::fs::remove_dir_all(file.parent().unwrap().parent().unwrap());
        let s = r#"
            fn start() { let n = store_get("times"); if n == () { n = 0; } store_set("times", n + 1); ui("times", n + 1); }
            fn on_event(topic, data) {
                ui("cpu", data.cpu);
                ui("disk", data.disks[0].mount);
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
        let out = format!("{:?}", run_with(s, env(&[], Some(file.clone())), vec![ev]));
        assert!(!out.contains("Error("), "{out}");
        assert!(out.contains(r#"Set("times", Num(1.0))"#), "{out}");
        assert!(out.contains(r#"Set("cpu", Num(93.0))"#), "{out}");
        assert!(out.contains(r#"Set("disk", Str("/"))"#), "{out}");
        assert!(out.contains(r#"Fact(Strain, "CPU", Some(93.0))"#), "{out}");
        assert!(out.contains(r#"Set("json", Str("[1,2.5,\"x\"]"))"#), "{out}");
        // the data survives: the second time reads what the first one wrote
        let out = format!("{:?}", run_with(s, env(&[], Some(file.clone())), vec![]));
        assert!(out.contains(r#"Set("times", Num(2.0))"#), "{out}");
        let _ = std::fs::remove_dir_all(file.parent().unwrap().parent().unwrap());
        // a fact kind that doesn't exist becomes an error on screen
        assert!(format!("{:?}", run(r#"fn start() { fact("gossip", "x"); }"#, vec![])).contains("unknown"));
    }

    #[test]
    #[cfg(unix)] // runs `echo`, which on Windows isn't a program (it's a cmd command)
    fn run_async_answers_without_blocking_the_script() {
        let (tx, rx) = channel();
        let mut e = env(&["echo"], None);
        e.back = Some(tx.clone());
        let s = r#"
            fn start() { ui("asked", run_async(["echo", "hi"], "done")); ui("denied", run_async(["rm"], "done")); }
            fn done(r) { let o = r.out; o.trim(); ui("echo:" + r.ok, o); }
        "#;
        let output = Arc::new(Mutex::new(Vec::new()));
        let sd = output.clone();
        let t = std::thread::spawn(move || script_loop(s.into(), e, rhai::Map::new(), rx, move |b| sd.lock().unwrap().extend(b)));
        let until = Instant::now() + Duration::from_secs(10);
        let has = |txt: &str| format!("{:?}", output.lock().unwrap()).contains(txt);
        while !(has("echo:true") && has("echo:false")) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        tx.send(ScriptIn::Stop).unwrap();
        t.join().unwrap();
        let out = format!("{:?}", output.lock().unwrap());
        assert!(out.contains(r#"Set("asked", Bool(true))"#), "{out}");
        assert!(out.contains(r#"Set("echo:true", Str("hi"))"#), "{out}");
        // the program outside the allowlist also runs "asynchronously", and fails
        assert!(out.contains(r#"Set("echo:false", Str(""))"#), "{out}");
    }

    #[test]
    fn http_only_https_and_manifest_domains() {
        let net = vec!["api.example.com".to_string()];
        let url = |u: &str| reqwest::Url::parse(u).unwrap();
        assert!(check_url(&url("https://api.example.com/v1?x=1"), &net).is_ok());
        assert!(check_url(&url("http://api.example.com/"), &net).unwrap_err().contains("https"));
        assert!(check_url(&url("https://other.com/"), &net).unwrap_err().contains("net"));
        assert!(check_url(&url("https://example.com/"), &net).is_err(), "the parent domain doesn't count for the subdomain");
        assert!(check_url(&url("https://u:s@api.example.com/"), &net).is_err());
        assert!(check_url(&url("https://api.example.com:8443/"), &net).is_err());
        // refused before any connection
        let out = format!("{:?}", run(r#"fn start() { let r = http_get("https://evil.com/"); ui("r", r.ok); ui("e", r.err); }"#, vec![]));
        assert!(out.contains(r#"Set("r", Bool(false))"#) && out.contains("evil.com is not in [permissions] net"), "{out}");
    }

    #[test]
    fn script_keeps_state_and_talks_to_the_screen() {
        let s = r#"
            fn start() { this.n = 0; ui("title", "hi"); }
            fn tick() { this.n += 1; ui("n", this.n); if this.n == 2 { mood("happy"); } }
            fn action(name, arg) { ui("last", name + ":" + arg); }
        "#;
        let out = run(s, vec![ScriptIn::Tick, ScriptIn::Tick, ScriptIn::Action("clicked".into(), "x".into())]);
        let txt = format!("{out:?}");
        assert!(txt.contains(r#"Set("title", Str("hi"))"#), "{txt}");
        assert!(txt.contains(r#"Set("n", Num(2.0))"#), "{txt}");
        assert!(txt.contains(r#"Mood("happy")"#), "{txt}");
        assert!(txt.contains(r#"Set("last", Str("clicked:x"))"#), "{txt}");
    }

    #[test]
    #[cfg(unix)] // runs `echo`, which on Windows isn't a program (it's a cmd command)
    fn script_only_runs_what_the_manifest_allows_and_does_not_hang() {
        let s = r#"
            fn start() {
                let ok = run(["echo", "hello"]);
                let echo = ok.out;
                echo.trim();
                ui("echo", echo);
                let no = run(["rm", "-rf", "/tmp/nothing"]);
                ui("denied", no.ok);
                loop { }
            }
        "#;
        let out = run(s, vec![]);
        let txt = format!("{out:?}");
        assert!(txt.contains(r#"Set("echo", Str("hello"))"#), "{txt}");
        assert!(txt.contains(r#"Set("denied", Bool(false))"#), "{txt}");
        // the infinite loop blows the operation limit and becomes an error, it doesn't hang the test
        assert!(txt.contains("Error("), "{txt}");
    }
}
