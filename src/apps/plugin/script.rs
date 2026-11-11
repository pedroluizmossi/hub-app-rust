//! Script dos plugins com tela (`main.rhai`): roda numa thread só dele e conversa com a
//! tela por mensagens (`ScriptIn`/`ScriptOut`). A API está na doc de `apps/plugin`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use slint::{ModelRc, VecModel};
use slint_interpreter::Value;

use crate::apps::external::FromApp;
use crate::core::bus::Event;
use crate::core::chatter::FactKind;
use crate::core::exec::Exec;

/// Prazo de um `run()` do script.
const RUN_TIMEOUT: Duration = Duration::from_secs(30);

/// Valor que o script manda para a tela (sem `Rc`: atravessa threads).
#[derive(Debug, Clone)]
pub(super) enum PValue {
    Str(String),
    Num(f64),
    Bool(bool),
    List(Vec<PValue>),
    Map(Vec<(String, PValue)>),
    /// Imagem lida do disco (só o app externo manda).
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

    /// Do JSON de um app externo (`null` vira texto vazio).
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

    /// Para a tela (na thread da UI).
    pub(super) fn to_value(&self) -> Value {
        match self {
            PValue::Str(s) => Value::String(s.as_str().into()),
            PValue::Num(n) => Value::Number(*n),
            PValue::Bool(b) => Value::Bool(*b),
            PValue::List(l) => Value::Model(ModelRc::new(VecModel::from(l.iter().map(PValue::to_value).collect::<Vec<_>>()))),
            PValue::Map(m) => Value::Struct(m.iter().map(|(k, v)| (k.clone(), v.to_value())).collect()),
            PValue::Image(p) => Value::Image(slint::Image::load_from_path(p).unwrap_or_default()),
        }
    }
}

/// Do script para a UI.
#[derive(Debug, Clone)]
pub(super) enum ScriptOut {
    Set(String, PValue),
    Mood(String),
    Summary(String),
    Notify(String, String),
    /// Fato para a conversa entre mascotes: tipo, assunto e número.
    Fact(FactKind, String, Option<f32>),
    /// Erro na célula (texto vazio = o app externo diz que passou).
    Error(String),
    /// O processo do app externo saiu sem o host pedir.
    Exited(String),
}

/// Da UI (ou de uma tarefa assíncrona) para o script.
pub(super) enum ScriptIn {
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
pub(super) struct ScriptEnv {
    pub(super) exec: Exec,
    /// Domínios de `[permissions] net`.
    pub(super) net: Vec<String>,
    /// Arquivo dos dados (`store_get`/`store_set`); `None` = só na memória.
    pub(super) data: Option<PathBuf>,
    /// Canal de volta para as tarefas assíncronas (`None` = sem elas).
    pub(super) back: Option<Sender<ScriptIn>>,
}

/// Os dados que o script grava, carregados do arquivo e gravados de volta pelo host.
#[derive(Default)]
pub(super) struct Store {
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
pub(super) fn event_data(ev: &Event) -> serde_json::Value {
    let v = match ev {
        Event::Sys(s) => serde_json::to_value(s),
        Event::Gpu(s) => serde_json::to_value(s),
        Event::Docker(s) => serde_json::to_value(s),
        Event::Media(s) => serde_json::to_value(s),
        Event::AiUsage(s) => serde_json::to_value(s),
        Event::Ports(s) => serde_json::to_value(s),
        Event::Agents(s) => serde_json::to_value(s),
        Event::Procs(s) => serde_json::to_value(s),
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

/// Um fato para a conversa ("strain", "CPU", 93), do script ou do app externo.
fn fact_out(kind: &str, subject: &str, value: Option<f32>) -> Result<ScriptOut, String> {
    let k = FactKind::ALL
        .into_iter()
        .find(|k| k.key() == kind && *k != FactKind::Idle)
        .ok_or_else(|| format!("fact: tipo \"{kind}\" desconhecido (trouble, strain, forecast, recovered, poked)"))?;
    Ok(ScriptOut::Fact(k, subject.to_string(), value))
}

/// Mensagem do app externo → o que a UI aplica (o mesmo que o script manda).
pub(super) fn from_app(msg: FromApp) -> ScriptOut {
    match msg {
        FromApp::Set { prop, value } => ScriptOut::Set(prop, PValue::from_json(&value)),
        FromApp::Image { prop, path } => ScriptOut::Set(prop, PValue::Image(PathBuf::from(path))),
        FromApp::Mood { mood } => ScriptOut::Mood(mood),
        FromApp::Summary { text } => ScriptOut::Summary(text),
        FromApp::Notify { title, body } => ScriptOut::Notify(title, body),
        FromApp::Fact { kind, subject, value } => {
            fact_out(&kind, &subject, value.map(|v| v as f32)).unwrap_or_else(ScriptOut::Error)
        }
        FromApp::Error { message } => ScriptOut::Error(message),
    }
}

/// O interpretador com a API do host e os limites (um script em loop não trava nada:
/// estoura o limite de operações e vira erro na tela).
pub(super) fn engine(out: Arc<Mutex<Vec<ScriptOut>>>, env: ScriptEnv, settings: Arc<Mutex<rhai::Map>>, store: Arc<Mutex<Store>>) -> rhai::Engine {
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
    e.on_debug(|s, _, _| log::debug!("plugin: {s}"));
    e.register_fn("now", crate::apps::kit::unix_now);
    e.register_fn("setting", move |key: &str| -> rhai::Dynamic {
        settings.lock().ok().and_then(|m| m.get(key).cloned()).unwrap_or(rhai::Dynamic::UNIT)
    });

    // conversa entre mascotes: fact("strain", "CPU", 93)
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
pub(super) fn script_loop(source: String, env: ScriptEnv, settings: rhai::Map, rx: Receiver<ScriptIn>, deliver: impl Fn(Vec<ScriptOut>)) {
    let out = Arc::new(Mutex::new(Vec::new()));
    let shared_settings = Arc::new(Mutex::new(settings.clone()));
    let data = env.data.clone();
    let store = Arc::new(Mutex::new(Store::load(data.as_deref())));
    let engine = engine(out.clone(), env, shared_settings.clone(), store.clone());
    let flush = || {
        let saved = store.lock().map_err(|e| e.to_string()).and_then(|mut s| s.save(data.as_deref()));
        if let Err(e) = saved
            && let Ok(mut v) = out.lock() {
                v.push(ScriptOut::Error(e));
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
        if let Err(e) = r
            && let Ok(mut v) = out.lock() {
                v.push(ScriptOut::Error(format!("main.rhai: {e}")));
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;
    use std::time::Instant;

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
        
        saida.lock().unwrap().clone()
    }

    fn rodar(script: &str, entradas: Vec<ScriptIn>) -> Vec<ScriptOut> {
        rodar_com(script, env(&["echo"], None), entradas)
    }

    #[test]
    fn mensagens_do_app_externo_viram_as_do_script() {
        let set = from_app(FromApp::Set { prop: "n".into(), value: serde_json::json!({"a": [1, true]}) });
        assert!(matches!(&set, ScriptOut::Set(p, PValue::Map(m)) if p == "n" && matches!(&m[0].1, PValue::List(l) if l.len() == 2)));
        assert!(matches!(from_app(FromApp::Image { prop: "g".into(), path: "/x.png".into() }), ScriptOut::Set(_, PValue::Image(_))));
        assert!(matches!(
            from_app(FromApp::Fact { kind: "strain".into(), subject: "CPU".into(), value: Some(93.0) }),
            ScriptOut::Fact(FactKind::Strain, s, Some(_)) if s == "CPU"
        ));
        // tipo de fato desconhecido vira erro na célula, não pânico
        assert!(matches!(from_app(FromApp::Fact { kind: "x".into(), subject: String::new(), value: None }), ScriptOut::Error(e) if e.contains("desconhecido")));
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
}
