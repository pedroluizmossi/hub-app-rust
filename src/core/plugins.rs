//! Plugins da comunidade (prova de conceito): mini-apps instalados de um repositório.
//!
//! Há três tipos de plugin. Os dois primeiros não têm código nativo:
//! - **simples**: o `manifest.toml` diz que comando rodar e como mostrar (abaixo); o host
//!   roda e mostra numa tela genérica;
//! - **com tela própria**: `[ui]` aponta um `view.slint` (interpretado em tempo de execução)
//!   e um `main.rhai` (lógica, num interpretador isolado). Ver `apps/plugin/`.
//!
//! O terceiro, **app externo** (`[app]`), tem **acesso total**: a lógica é um executável
//! próprio (qualquer linguagem), que o host inicia ao ligar o app e com quem conversa por
//! JSON na entrada e na saída padrão (protocolo em `apps/external.rs`). Roda com as
//! permissões do usuário, por isso não declara `[permissions]`; a loja o anuncia como
//! acesso total e confere o hash do binário (ver `store.rs`).
//!
//! ```toml
//! uses = ["sys"]               # provedores repassados ao app (mensagem "event")
//! [app]
//! view = "view.slint"          # a tela, como num plugin com [ui]
//! linux = "bin/meu-app"        # o executável de cada sistema, em bin/ (um basta)
//! windows = "bin/meu-app.exe"
//! every = 10                   # mensagem "tick" a cada N segundos (opcional; 1 a 3600)
//! ```
//!
//! O `manifest.toml` é o mesmo formato dos mini-apps embutidos, com seções a mais:
//!
//! ```toml
//! id = "io.github.fulano.uptime"   # único; `cyber.*` é dos apps embutidos
//! name = "Tempo ligado"
//! icon = "coruja"
//! api = 1
//! [permissions]
//! exec = ["uptime"]                # só estes programas podem rodar (sem shell)
//! [run]
//! command = ["uptime", "-p"]       # o que rodar
//! every = 30                       # de quantos em quantos segundos (5 a 3600)
//! [display]
//! title = "Ligado há"
//! [[mood]]                         # humor pela saída: a 1ª regra que casar vale
//! contains = "week"
//! mood = "happy"
//! ```
//!
//! Sem regra que case, o humor é "idle"; saída vazia é "sleep"; o comando falhar é "alarm".
//!
//! Plugin com tela própria, no lugar de `[run]`:
//!
//! ```toml
//! uses = ["sys", "procs"] # provedores que o script recebe em fn on_event(topic, data)
//! [permissions]
//! exec = ["git"]
//! net = ["api.exemplo.com"]   # domínios que http_get() pode consultar (só https)
//! [ui]
//! view = "view.slint"     # a tela: exporta `component Main`
//! script = "main.rhai"    # fn start(), fn tick(), fn action(name, arg)…
//! every = 10              # tick a cada N segundos (opcional; 1 a 3600)
//! ```
//!
//! Arquivos de um plugin (`files`): os de `TOP_FILES` na raiz, mais `ui/*.slint` (telas que o
//! `view.slint` importa) e `assets/*.png|jpg|svg` (imagens, por `@image-url`). Nada de
//! link ou outra pasta; binário só em `bin/` de um app externo. Os dados ficam fora da pasta
//! do plugin, na pasta de dados do app (`core/appdata.rs`), para sobreviver a uma
//! atualização. O que o app guarda vai em `[data]`:
//!
//! ```toml
//! [data]
//! kinds = ["sqlite", "kv"]   # script: kv (store_set) e/ou sqlite (db_exec); app externo: files
//! version = 1                # formato dos dados; subir chama fn migrate(de, para)
//! max_mb = 10                # cota do banco
//! ```
//!
//! Sem `[data]`, o script tem `kv` e o app externo, `files`; o plugin simples não guarda nada.
//!
//! Instalados ficam em `dir()`: `~/.local/share/miniutils/plugins/<id>/` no Linux,
//! `%APPDATA%\miniutils\plugins\<id>\` no Windows. A loja instala ali (ver `store.rs`) e
//! quem quiser põe um app à mão (app local, sem `installed.json`); `scan` diz quais pastas
//! não viraram app e por quê. Com `miniutils --plugin-dev <pasta>`, um plugin em
//! desenvolvimento entra junto (`set_dev`).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::Deserialize;

use super::appdata::{DataSpec, Kind};
use super::manifest::{Manifest, SettingDef, SettingKind};
use crate::mascot::sprites;

/// Arquivos da raiz de um plugin.
const TOP_FILES: &[&str] =
    &["manifest.toml", "view.slint", "main.rhai", "README.md", "LICENSE", "LICENSE.md", "LICENSE.txt"];
/// Subpastas e as extensões aceitas em cada uma.
const SUBDIRS: &[(&str, &[&str])] = &[("ui", &["slint"]), ("assets", &["png", "jpg", "jpeg", "svg"])];
/// Limites: por arquivo, no total e em número de arquivos.
pub const MAX_FILE: u64 = 1 << 20;
const MAX_TOTAL: u64 = 8 << 20;
const MAX_FILES: usize = 64;
/// Executáveis de um app externo (`bin/`): por arquivo. Ficam fora do total acima.
pub const MAX_BIN: u64 = 256 << 20;
/// A pasta dos executáveis de um app externo.
const BIN_DIR: &str = "bin";

/// Um plugin pronto para o host rodar.
#[derive(Debug, Clone)]
pub struct Plugin {
    /// Pasta de onde foi lido (vazia quando veio só do texto do manifesto).
    pub dir: PathBuf,
    pub manifest: Manifest,
    /// Plugin simples: o comando que o host roda.
    pub run: Option<Run>,
    /// Plugin com tela própria e script.
    pub ui: Option<Ui>,
    /// App externo, com acesso total.
    pub app: Option<AppSpec>,
    pub display: Display,
    pub moods: Vec<MoodRule>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Run {
    pub command: Vec<String>,
    pub every: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ui {
    pub view: String,
    pub script: String,
    /// Intervalo do `tick()` do script, em segundos (sem = nunca).
    pub every: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppSpec {
    pub view: String,
    /// Executável no Linux (`bin/<nome>`, relativo à pasta do app).
    #[serde(default)]
    pub linux: Option<String>,
    /// Executável no Windows (`bin/<nome>.exe`).
    #[serde(default)]
    pub windows: Option<String>,
    /// Intervalo da mensagem `tick`, em segundos (sem = nunca).
    pub every: Option<u64>,
}

impl AppSpec {
    /// O executável deste sistema (relativo à pasta do app), se o app tiver um.
    pub fn binary(&self) -> Option<&str> {
        if cfg!(windows) { self.windows.as_deref() } else { self.linux.as_deref() }
    }

    /// Os executáveis declarados, com o sistema de cada um ("linux", "windows").
    pub fn binaries(&self) -> Vec<(&'static str, &str)> {
        [("linux", &self.linux), ("windows", &self.windows)]
            .into_iter()
            .filter_map(|(os, b)| b.as_deref().map(|b| (os, b)))
            .collect()
    }
}

/// O sistema em que este binário roda, como o manifesto e o índice o chamam.
pub fn this_os() -> &'static str {
    if cfg!(windows) { "windows" } else { "linux" }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Display {
    /// Título acima da saída (padrão: o nome do plugin).
    #[serde(default)]
    pub title: String,
    /// Quantas linhas da saída mostrar (1 a 6; padrão 3).
    #[serde(default)]
    pub lines: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MoodRule {
    pub contains: String,
    pub mood: String,
}

/// As seções que só os plugins têm.
#[derive(Deserialize)]
struct Extra {
    run: Option<Run>,
    ui: Option<Ui>,
    app: Option<AppSpec>,
    #[serde(default)]
    display: Display,
    #[serde(default)]
    mood: Vec<MoodRule>,
}

impl Plugin {
    /// Lê e valida o manifesto de um plugin.
    pub fn parse(text: &str) -> Result<Plugin, String> {
        let mut manifest = Manifest::parse(text).map_err(|e| e.to_string())?;
        let extra: Extra = toml::from_str(text).map_err(|e| e.to_string())?;
        validate_id(&manifest.id)?;
        if let Some(p) = manifest.permissions.exec.iter().find(|p| p.contains('/') || p.is_empty()) {
            return Err(format!("[permissions] exec: use o nome do programa, não um caminho: {p}"));
        }
        if let Some(h) = manifest.permissions.net.iter().find(|h| !valid_host(h)) {
            return Err(format!("[permissions] net: \"{h}\" não é um domínio (ex.: api.exemplo.com, sem https:// nem caminho)"));
        }
        match [extra.run.is_some(), extra.ui.is_some(), extra.app.is_some()].iter().filter(|s| **s).count() {
            0 => {
                return Err("falta a seção [run] (plugin simples), [ui] (tela própria) ou [app] (app com acesso total)".into());
            }
            1 => {}
            _ => return Err("use só uma das seções [run], [ui] ou [app]".into()),
        }
        if let Some(app) = &extra.app {
            // o executável roda com as permissões do usuário: uma allowlist não diria a verdade
            if !manifest.permissions.exec.is_empty() || !manifest.permissions.net.is_empty() {
                return Err("[app] tem acesso total: não declare [permissions] (a loja o anuncia como acesso total)".into());
            }
            if app.view != "view.slint" {
                return Err("[app] use view = \"view.slint\"".into());
            }
            if app.every.is_some_and(|e| !(1..=3600).contains(&e)) {
                return Err("[app] every: use de 1 a 3600 segundos".into());
            }
            if app.binaries().is_empty() {
                return Err("[app] falta o executável: linux = \"bin/<nome>\" e/ou windows = \"bin/<nome>.exe\"".into());
            }
            if let Some((os, b)) = app.binaries().into_iter().find(|(_, b)| !valid_bin(b)) {
                return Err(format!("[app] {os} = \"{b}\": o executável fica em bin/ (ex.: bin/meu-app)"));
            }
        }
        // provedores e rede chegam ao script (ou ao app): o plugin simples não tem quem os use
        if extra.run.is_some() && !manifest.uses.is_empty() {
            return Err("uses só vale para plugin com [ui] ou [app] (o script recebe em fn on_event)".into());
        }
        if extra.ui.is_none() && !manifest.permissions.net.is_empty() {
            return Err("[permissions] net só vale para plugin com [ui] (o script usa http_get)".into());
        }
        if let Some(run) = &extra.run {
            let program = run.command.first().ok_or("[run] command está vazio")?;
            if program.contains('/') {
                return Err(format!("[run] use o nome do programa, não um caminho: {program}"));
            }
            if !manifest.permissions.exec.contains(program) {
                return Err(format!("[run] roda \"{program}\", que não está em [permissions] exec"));
            }
            if !(5..=3600).contains(&run.every) {
                return Err(format!("[run] every = {}: use de 5 a 3600 segundos", run.every));
            }
        }
        if let Some(ui) = &extra.ui {
            if ui.view != "view.slint" || ui.script != "main.rhai" {
                return Err("[ui] use view = \"view.slint\" e script = \"main.rhai\"".into());
            }
            if ui.every.is_some_and(|e| !(1..=3600).contains(&e)) {
                return Err("[ui] every: use de 1 a 3600 segundos".into());
            }
        }
        if let Some(d) = &manifest.data {
            let bad = |k: &Kind| match (&extra.run, &extra.ui) {
                (Some(_), _) => true,
                (_, Some(_)) => *k == Kind::Files,
                _ => *k != Kind::Files,
            };
            if let Some(k) = d.kinds.iter().find(|k| bad(k)) {
                return Err(match (&extra.run, &extra.ui) {
                    (Some(_), _) => "[data]: o plugin simples não guarda dados (use [ui] ou [app])".to_string(),
                    (_, Some(_)) => format!("[data] kinds: \"{}\" não vale para script (use \"kv\" e/ou \"sqlite\")", k.key()),
                    _ => format!("[data] kinds: \"{}\" não vale para app externo (ele grava o que quiser: use \"files\")", k.key()),
                });
            }
        }
        if let Some(n) = extra.display.lines
            && !(1..=6).contains(&n) {
                return Err(format!("[display] lines = {n}: use de 1 a 6"));
            }
        for r in &extra.mood {
            if !["sleep", "idle", "happy", "busy", "alarm"].contains(&r.mood.as_str()) {
                return Err(format!("[[mood]] humor desconhecido: {}", r.mood));
            }
        }
        if sprites::find(&manifest.icon).is_none() {
            return Err(format!("icon = \"{}\": não há essa mascote", manifest.icon));
        }
        add_standard_settings(&mut manifest);
        Ok(Plugin {
            dir: PathBuf::new(),
            manifest,
            run: extra.run,
            ui: extra.ui,
            app: extra.app,
            display: extra.display,
            moods: extra.mood,
        })
    }

    /// Lê o plugin da pasta `dir`.
    pub fn load(dir: &Path) -> Result<Plugin, String> {
        let text = std::fs::read_to_string(dir.join("manifest.toml")).map_err(|e| format!("manifest.toml: {e}"))?;
        let mut p = Plugin::parse(&text)?;
        p.dir = dir.to_path_buf();
        Ok(p)
    }

    /// O que o plugin pode fazer, para mostrar antes de instalar ("executa: ps · rede:
    /// api.x.com · lê: sys").
    pub fn grants(&self) -> String {
        let uses: Vec<String> = self.manifest.uses.iter().map(|t| t.name().to_string()).collect();
        if self.app.is_some() {
            return describe_full_access(&uses);
        }
        describe_grants(&self.manifest.permissions.exec, &self.manifest.permissions.net, &uses, self.data().has(Kind::Sqlite))
    }

    /// O que o app guarda: o `[data]` do manifesto ou, sem ele, o padrão do tipo (`kv` no
    /// script, `files` no app externo, nada no plugin simples).
    pub fn data(&self) -> DataSpec {
        if let Some(d) = &self.manifest.data {
            return d.clone();
        }
        DataSpec::with(match (&self.ui, &self.app) {
            (Some(_), _) => &[Kind::Kv],
            (_, Some(_)) => &[Kind::Files],
            _ => &[],
        })
    }

    /// App externo: roda um executável próprio com as permissões do usuário.
    pub fn full_access(&self) -> bool {
        self.app.is_some()
    }

    /// O executável deste sistema (caminho completo), num app externo.
    pub fn binary_path(&self) -> Option<PathBuf> {
        self.app.as_ref()?.binary().map(|b| self.dir.join(b))
    }

    pub fn title(&self) -> &str {
        if self.display.title.is_empty() { &self.manifest.name } else { &self.display.title }
    }

    pub fn lines(&self) -> usize {
        self.display.lines.unwrap_or(3)
    }
}

/// Id de plugin: minúsculas, números, `.`, `_` e `-`, com pelo menos um ponto (ex.:
/// `io.github.fulano.uptime`). `cyber.*` é reservado aos apps embutidos.
pub fn validate_id(id: &str) -> Result<(), String> {
    let ok_chars = id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'));
    let parts_ok = id.split('.').count() >= 2 && id.split('.').all(|p| !p.is_empty());
    if !ok_chars || !parts_ok || id.len() > 80 {
        return Err(format!("id inválido: \"{id}\" (ex.: io.github.fulano.meu-app)"));
    }
    if id.starts_with("cyber.") {
        return Err(format!("id \"{id}\": o prefixo cyber. é dos apps embutidos"));
    }
    Ok(())
}

/// Domínio para `[permissions] net`: letras minúsculas, números, `-` e `.`, com um ponto
/// pelo menos (sem esquema, porta ou caminho).
pub fn valid_host(h: &str) -> bool {
    let chars = h.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.');
    let labels = h.split('.').all(|l| !l.is_empty() && !l.starts_with('-') && !l.ends_with('-'));
    chars && labels && h.contains('.') && h.len() <= 253
}

/// Texto das permissões: "executa: ps · rede: api.x.com · lê: sys · guarda: banco SQLite"
/// (ou "não executa programas" se não pede nada).
pub fn describe_grants(exec: &[String], net: &[String], uses: &[String], sqlite: bool) -> String {
    let mut parts = Vec::new();
    if !exec.is_empty() {
        parts.push(format!("executa: {}", exec.join(", ")));
    }
    if !net.is_empty() {
        parts.push(format!("rede: {}", net.join(", ")));
    }
    if !uses.is_empty() {
        parts.push(format!("lê: {}", uses.join(", ")));
    }
    if sqlite {
        parts.push(GRANT_SQLITE.into());
    }
    if parts.is_empty() { "não executa programas nem usa a rede".into() } else { parts.join(" · ") }
}

/// O banco de dados na lista de permissões (e nas permissões novas de uma atualização).
pub const GRANT_SQLITE: &str = "guarda: banco SQLite";

/// Texto das permissões de um app externo: "ACESSO TOTAL: … · lê: sys".
pub fn describe_full_access(uses: &[String]) -> String {
    let mut out = String::from("ACESSO TOTAL: roda um programa próprio com as permissões do seu usuário");
    if !uses.is_empty() {
        out.push_str(&format!(" · lê: {}", uses.join(", ")));
    }
    out
}

/// Executável de app externo: `bin/<nome>`, com nome simples (sem subpasta nem `..`).
fn valid_bin(path: &str) -> bool {
    path.strip_prefix("bin/").is_some_and(safe_name)
}

/// Nome de arquivo simples: letras, números, `-`, `_` e `.`, sem começar por ponto.
fn safe_name(name: &str) -> bool {
    !name.is_empty() && !name.starts_with('.') && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// O manifesto em `dir` declara um app externo (`[app]`)?
fn declares_app(dir: &Path) -> bool {
    std::fs::read_to_string(dir.join("manifest.toml"))
        .ok()
        .and_then(|t| toml::from_str::<Extra>(&t).ok())
        .is_some_and(|e| e.app.is_some())
}

/// Os arquivos do plugin em `dir` (caminhos relativos), conferidos: só arquivos comuns
/// (nada de link), dentro dos limites. Arquivos de outros tipos são ignorados.
pub fn files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut total = 0u64;
    let mut take = |rel: PathBuf| -> Result<(), String> {
        let meta = std::fs::symlink_metadata(dir.join(&rel)).map_err(|e| format!("{}: {e}", rel.display()))?;
        if !meta.file_type().is_file() {
            return Err(format!("{} não é um arquivo comum", rel.display()));
        }
        if meta.len() > MAX_FILE {
            return Err(format!("{} passa de 1 MiB", rel.display()));
        }
        total += meta.len();
        out.push(rel);
        Ok(())
    };
    for name in TOP_FILES {
        if std::fs::symlink_metadata(dir.join(name)).is_ok() {
            take(PathBuf::from(name))?;
        }
    }
    for (sub, exts) in SUBDIRS {
        let Ok(meta) = std::fs::symlink_metadata(dir.join(sub)) else { continue };
        if !meta.is_dir() {
            return Err(format!("{sub}/ não é uma pasta"));
        }
        let mut names: Vec<String> = std::fs::read_dir(dir.join(sub))
            .map_err(|e| format!("{sub}/: {e}"))?
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        for name in names {
            let ext = Path::new(&name).extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
            if safe_name(&name) && exts.contains(&ext.as_str()) {
                take(Path::new(sub).join(&name))?;
            }
        }
    }
    if declares_app(dir) {
        out.extend(bin_files(dir)?);
    }
    if out.len() > MAX_FILES {
        return Err(format!("arquivos demais ({}; o limite é {MAX_FILES})", out.len()));
    }
    if total > MAX_TOTAL {
        return Err("os arquivos passam de 8 MiB juntos".into());
    }
    Ok(out)
}

/// Os executáveis de um app externo (`bin/`), conferidos: arquivos comuns de nome simples,
/// até `MAX_BIN` cada. No modo dev vale um link (para o `target/` do projeto do app).
fn bin_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let bin = dir.join(BIN_DIR);
    let Ok(meta) = std::fs::symlink_metadata(&bin) else { return Ok(Vec::new()) };
    if !meta.is_dir() {
        return Err(format!("{BIN_DIR}/ não é uma pasta"));
    }
    let dev = dev_dir() == Some(dir);
    let mut names: Vec<String> = std::fs::read_dir(&bin)
        .map_err(|e| format!("{BIN_DIR}/: {e}"))?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| safe_name(n))
        .collect();
    names.sort();
    let mut out = Vec::new();
    for name in names {
        let rel = Path::new(BIN_DIR).join(&name);
        let path = dir.join(&rel);
        let meta = if dev { std::fs::metadata(&path) } else { std::fs::symlink_metadata(&path) }
            .map_err(|e| format!("{}: {e}", rel.display()))?;
        if !meta.file_type().is_file() {
            return Err(format!("{} não é um arquivo comum", rel.display()));
        }
        if meta.len() > MAX_BIN {
            return Err(format!("{} passa de {} MiB", rel.display(), MAX_BIN >> 20));
        }
        out.push(rel);
    }
    Ok(out)
}

/// Arquivo dentro de `bin/` (executável de app externo)?
pub fn is_bin(rel: &Path) -> bool {
    rel.starts_with(BIN_DIR)
}

/// Plugin em desenvolvimento (`miniutils --plugin-dev <pasta>`).
static DEV: OnceLock<PathBuf> = OnceLock::new();

pub fn set_dev(dir: PathBuf) {
    let _ = DEV.set(dir);
}

pub fn dev_dir() -> Option<&'static Path> {
    DEV.get().map(PathBuf::as_path)
}

/// Os instalados e, no modo dev, o plugin da pasta de desenvolvimento (que vale no lugar
/// de um instalado de mesmo id).
pub fn load_all(root: &Path) -> Vec<Plugin> {
    let mut list = load_installed(root);
    if let Some(dev) = dev_dir() {
        match Plugin::load(dev) {
            Ok(p) => {
                list.retain(|o| o.manifest.id != p.manifest.id);
                list.push(p);
            }
            Err(e) => log::warn!("plugin-dev: {}: {e}", dev.display()),
        }
    }
    list
}

/// Todo app tem tema e mascote; o plugin que não declarar ganha os padrões.
fn add_standard_settings(m: &mut Manifest) {
    if m.setting("theme").is_none() {
        // as opções (os temas) a tela preenche na hora: `SettingDef::live_options`
        m.settings.push(SettingDef {
            key: "theme".into(),
            label: "Tema".into(),
            kind: SettingKind::Choice,
            default: toml::Value::String("auto".into()),
            min: None,
            max: None,
            step: None,
            options: vec!["auto".into()],
        });
    }
    if m.setting("mascot").is_none() {
        let mut options = vec![m.icon.clone()];
        options.extend(sprites::all().iter().map(|s| s.key.to_string()).filter(|k| *k != m.icon));
        m.settings.push(SettingDef {
            key: "mascot".into(),
            label: "Mascote".into(),
            kind: SettingKind::Choice,
            default: toml::Value::String(m.icon.clone()),
            min: None,
            max: None,
            step: None,
            options,
        });
    }
}

/// Pasta dos plugins instalados: `MINIUTILS_PLUGINS`, ou `~/.local/share/miniutils/plugins`.
pub fn dir() -> PathBuf {
    match std::env::var_os("MINIUTILS_PLUGINS").filter(|v| !v.is_empty()) {
        Some(d) => PathBuf::from(d),
        None => dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("miniutils/plugins"),
    }
}

/// Plugins instalados em `dir`, válidos, em ordem de id. Os inválidos ficam de fora (com
/// o motivo no registro): um plugin quebrado não impede o hub de abrir.
pub fn load_installed(dir: &Path) -> Vec<Plugin> {
    let (list, skipped) = scan(dir);
    for s in skipped {
        log::warn!("plugin em {} ignorado: {}", dir.join(&s.folder).display(), s.reason);
    }
    list
}

/// Pasta de `dir` que não virou app, e por quê (mostrado em Configurações › Sistema e em
/// `--plugins`).
#[derive(Debug, Clone, PartialEq)]
pub struct Skipped {
    pub folder: String,
    pub reason: String,
}

/// Lê `dir`: os plugins válidos (em ordem de pasta) e as pastas recusadas. Não registra
/// nada; quem chama decide onde mostrar.
pub fn scan(dir: &Path) -> (Vec<Plugin>, Vec<Skipped>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return (Vec::new(), Vec::new()) };
    let mut dirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    dirs.sort();
    let (mut out, mut skipped) = (Vec::new(), Vec::new());
    for d in dirs {
        let folder = d.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        // pastas temporárias de uma instalação em curso, e `.data` (dados dos plugins)
        if folder.starts_with('.') {
            continue;
        }
        if !d.join("manifest.toml").is_file() {
            skipped.push(Skipped { folder, reason: "não tem manifest.toml".into() });
            continue;
        }
        match Plugin::load(&d) {
            // a pasta é o id, então não há dois plugins com o mesmo id
            Ok(p) if folder == p.manifest.id => out.push(p),
            Ok(p) => skipped.push(Skipped {
                reason: format!("o id do manifest.toml é \"{}\": a pasta tem de se chamar assim", p.manifest.id),
                folder,
            }),
            Err(e) => skipped.push(Skipped { folder, reason: e }),
        }
    }
    (out, skipped)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub const EXEMPLO: &str = include_str!("../../examples/plugin-uptime/manifest.toml");

    #[test]
    fn exemplo_e_valido() {
        let p = Plugin::parse(EXEMPLO).unwrap();
        assert_eq!(p.run.as_ref().unwrap().command[0], "uptime");
        assert!(p.manifest.setting("theme").is_some());
        assert!(p.manifest.setting("mascot").is_some());
    }

    #[test]
    fn recusa_plugin_perigoso_ou_torto() {
        let base = "name='x'\napi=1\n";
        let caso = |extra: &str| Plugin::parse(&format!("{base}{extra}")).unwrap_err();
        assert!(caso("id='cyber.x'\n[permissions]\nexec=['ls']\n[run]\ncommand=['ls']\nevery=10").contains("cyber."));
        assert!(caso("id='a.b'\n[run]\ncommand=['ls']\nevery=10").contains("não está em [permissions]"));
        assert!(caso("id='a.b'\n[permissions]\nexec=['/bin/ls']\n[run]\ncommand=['/bin/ls']\nevery=10").contains("caminho"));
        assert!(caso("id='a.b'\n[permissions]\nexec=['ls']\n[run]\ncommand=['ls']\nevery=1").contains("every"));
        assert!(caso("id='a.b'\n[permissions]\nexec=['ls']").contains("[run]"));
        assert!(caso("id='a.b'\n[ui]\nview='../x.slint'\nscript='main.rhai'").contains("view.slint"));
        assert!(caso("id='A b'\n[permissions]\nexec=['ls']\n[run]\ncommand=['ls']\nevery=10").contains("id inválido"));
        // provedores e rede só com tela e script
        assert!(caso("id='a.b'\nuses=['sys']\n[permissions]\nexec=['ls']\n[run]\ncommand=['ls']\nevery=10").contains("uses"));
        assert!(caso("id='a.b'\n[permissions]\nexec=['ls']\nnet=['a.com']\n[run]\ncommand=['ls']\nevery=10").contains("net"));
        let ui = "[ui]\nview='view.slint'\nscript='main.rhai'";
        assert!(caso(&format!("id='a.b'\n[permissions]\nnet=['https://a.com']\n{ui}")).contains("domínio"));
        assert!(caso(&format!("id='a.b'\n[permissions]\nnet=['a.com/x']\n{ui}")).contains("domínio"));
        let ok = Plugin::parse(&format!("{base}id='a.b'\nuses=['sys']\n[permissions]\nnet=['api.a.com']\n{ui}")).unwrap();
        assert_eq!(ok.grants(), "rede: api.a.com · lê: sys");
    }

    #[test]
    fn app_externo_valido_e_recusas() {
        let ok = Plugin::parse(include_str!("../../examples/app-hello/manifest.toml")).unwrap();
        let app = ok.app.as_ref().unwrap();
        assert!(ok.full_access());
        assert_eq!(app.binaries(), [("linux", "bin/app-hello"), ("windows", "bin/app-hello.exe")]);
        assert!(ok.grants().starts_with("ACESSO TOTAL"));
        assert!(ok.grants().ends_with("lê: sys"));
        let base = "name='x'\napi=1\nid='a.b'\n";
        let caso = |extra: &str| Plugin::parse(&format!("{base}{extra}")).unwrap_err();
        let app = "[app]\nview='view.slint'\nlinux='bin/x'";
        assert!(caso(&format!("[permissions]\nexec=['ls']\n{app}")).contains("acesso total"));
        assert!(caso(&format!("[permissions]\nnet=['a.com']\n{app}")).contains("acesso total"));
        assert!(caso("[app]\nview='view.slint'").contains("executável"));
        assert!(caso("[app]\nview='view.slint'\nlinux='/usr/bin/x'").contains("bin/"));
        assert!(caso("[app]\nview='view.slint'\nlinux='bin/../x'").contains("bin/"));
        assert!(caso("[app]\nview='view.slint'\nlinux='bin/sub/x'").contains("bin/"));
        assert!(caso(&format!("{app}\nevery=0")).contains("every"));
        assert!(caso(&format!("{app}\n[ui]\nview='view.slint'\nscript='main.rhai'")).contains("só uma"));
    }

    #[test]
    fn arquivos_do_plugin_so_os_permitidos() {
        let dir = std::env::temp_dir().join(format!("miniutils-arquivos-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("ui")).unwrap();
        std::fs::create_dir_all(dir.join("assets")).unwrap();
        std::fs::create_dir_all(dir.join("outra")).unwrap();
        for f in ["manifest.toml", "view.slint", "main.rhai", "ui/barra.slint", "ui/notas.txt", "assets/logo.png", "assets/.oculto.png", "outra/x.slint", "build.rs"] {
            std::fs::write(dir.join(f), "x").unwrap();
        }
        // o separador de pastas muda com o sistema: compara com `/`
        let got: Vec<String> = files(&dir).unwrap().iter().map(|p| p.display().to_string().replace('\\', "/")).collect();
        assert_eq!(got, ["manifest.toml", "view.slint", "main.rhai", "ui/barra.slint", "assets/logo.png"]);
        // link com nome permitido: recusa (não segue para fora da pasta). Só no Unix:
        // criar link no Windows pede privilégio
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc/hostname", dir.join("ui/fora.slint")).unwrap();
            assert!(files(&dir).unwrap_err().contains("ui/fora.slint"));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bin_so_entra_em_app_externo() {
        let dir = std::env::temp_dir().join(format!("miniutils-bin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join("bin/meu-app"), "x").unwrap();
        std::fs::write(dir.join("bin/.oculto"), "x").unwrap();
        let manifesto = |secao: &str| std::fs::write(dir.join("manifest.toml"), format!("id='a.b'\nname='x'\napi=1\n{secao}")).unwrap();
        let nomes = || -> Vec<String> { files(&dir).unwrap().iter().map(|p| p.display().to_string().replace('\\', "/")).collect() };
        // plugin com script: bin/ fica de fora
        manifesto("[ui]\nview='view.slint'\nscript='main.rhai'");
        assert_eq!(nomes(), ["manifest.toml"]);
        manifesto("[app]\nview='view.slint'\nlinux='bin/meu-app'");
        assert_eq!(nomes(), ["manifest.toml", "bin/meu-app"]);
        assert!(is_bin(Path::new("bin/meu-app")) && !is_bin(Path::new("binario.slint")));
        // fora do modo dev, link em bin/ é recusado
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/bin/sh", dir.join("bin/sh")).unwrap();
            assert!(files(&dir).unwrap_err().contains("bin/sh"));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_explica_as_pastas_recusadas() {
        let dir = std::env::temp_dir().join(format!("miniutils-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let pasta = |nome: &str, manifesto: Option<&str>| {
            std::fs::create_dir_all(dir.join(nome)).unwrap();
            if let Some(m) = manifesto {
                std::fs::write(dir.join(nome).join("manifest.toml"), m).unwrap();
            }
        };
        pasta("io.github.exemplo.uptime", Some(EXEMPLO));
        pasta("uptime", Some(EXEMPLO));
        pasta("vazia", None);
        pasta("torto", Some("id='a.b'\nname='x'\napi=1\n"));
        pasta(".data", None);
        let (ok, recusadas) = scan(&dir);
        assert_eq!(ok.len(), 1);
        assert_eq!(ok[0].manifest.id, "io.github.exemplo.uptime");
        let motivo = |p: &str| recusadas.iter().find(|s| s.folder == p).map(|s| s.reason.clone()).unwrap();
        assert!(motivo("uptime").contains("a pasta tem de se chamar"));
        assert_eq!(motivo("vazia"), "não tem manifest.toml");
        assert!(!motivo("torto").is_empty());
        assert_eq!(recusadas.len(), 3, "a .data fica de fora: {recusadas:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
