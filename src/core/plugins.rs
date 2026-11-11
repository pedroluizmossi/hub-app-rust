//! Plugins da comunidade (prova de conceito): mini-apps instalados de um repositório.
//!
//! Há dois tipos de plugin, os dois sem código nativo:
//! - **simples**: o `manifest.toml` diz que comando rodar e como mostrar (abaixo); o host
//!   roda e mostra numa tela genérica;
//! - **com tela própria**: `[ui]` aponta um `view.slint` (interpretado em tempo de execução)
//!   e um `main.rhai` (lógica, num interpretador isolado). Ver `apps/plugin.rs`.
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
//! uses = ["sys"]          # provedores que o script recebe em fn on_event(topic, data)
//! [permissions]
//! exec = ["ps"]
//! net = ["api.exemplo.com"]   # domínios que http_get() pode consultar (só https)
//! [ui]
//! view = "view.slint"     # a tela: exporta `component Main`
//! script = "main.rhai"    # fn start(), fn tick(), fn action(name, arg)…
//! every = 10              # tick a cada N segundos (opcional; 1 a 3600)
//! ```
//!
//! Arquivos de um plugin (`files`): os de `TOP_FILES` na raiz, mais `ui/*.slint` (telas que o
//! `view.slint` importa) e `assets/*.png|jpg|svg` (imagens, por `@image-url`). Nada de
//! link, binário ou outra pasta. Os dados que o script grava (`store_set`) ficam fora da
//! pasta do plugin (`data_file`), para sobreviver a uma atualização.
//!
//! Instalados ficam em `~/.local/share/miniutils/plugins/<id>/` (ver `store.rs`). Com
//! `miniutils --plugin-dev <pasta>`, um plugin em desenvolvimento entra junto (`set_dev`).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::Deserialize;

use super::manifest::{Manifest, SettingDef, SettingKind};
use crate::mascot::{sprites, themes};

/// Arquivos da raiz de um plugin.
const TOP_FILES: &[&str] =
    &["manifest.toml", "view.slint", "main.rhai", "README.md", "LICENSE", "LICENSE.md", "LICENSE.txt"];
/// Subpastas e as extensões aceitas em cada uma.
const SUBDIRS: &[(&str, &[&str])] = &[("ui", &["slint"]), ("assets", &["png", "jpg", "jpeg", "svg"])];
/// Limites: por arquivo, no total e em número de arquivos.
pub const MAX_FILE: u64 = 1 << 20;
const MAX_TOTAL: u64 = 8 << 20;
const MAX_FILES: usize = 64;

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
        match (&extra.run, &extra.ui) {
            (None, None) => return Err("falta a seção [run] (plugin simples) ou [ui] (tela própria)".into()),
            (Some(_), Some(_)) => return Err("use [run] ou [ui], não os dois".into()),
            _ => {}
        }
        // provedores e rede chegam ao script: o plugin simples não tem quem os use
        if extra.ui.is_none() && !manifest.uses.is_empty() {
            return Err("uses só vale para plugin com [ui] (o script recebe em fn on_event)".into());
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
        if let Some(n) = extra.display.lines {
            if !(1..=6).contains(&n) {
                return Err(format!("[display] lines = {n}: use de 1 a 6"));
            }
        }
        for r in &extra.mood {
            if !["sleep", "idle", "happy", "busy", "alarm"].contains(&r.mood.as_str()) {
                return Err(format!("[[mood]] humor desconhecido: {}", r.mood));
            }
        }
        if !sprites::SPRITES.iter().any(|s| s.key == manifest.icon) {
            return Err(format!("icon = \"{}\": não há essa mascote", manifest.icon));
        }
        add_standard_settings(&mut manifest);
        Ok(Plugin { dir: PathBuf::new(), manifest, run: extra.run, ui: extra.ui, display: extra.display, moods: extra.mood })
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
        describe_grants(&self.manifest.permissions.exec, &self.manifest.permissions.net, &uses)
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

/// Texto das permissões: "executa: ps · rede: api.x.com · lê: sys" (ou "não executa
/// programas" se não pede nada).
pub fn describe_grants(exec: &[String], net: &[String], uses: &[String]) -> String {
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
    if parts.is_empty() { "não executa programas nem usa a rede".into() } else { parts.join(" · ") }
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
            let safe = !name.starts_with('.')
                && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
            if safe && exts.contains(&ext.as_str()) {
                take(Path::new(sub).join(&name))?;
            }
        }
    }
    if out.len() > MAX_FILES {
        return Err(format!("arquivos demais ({}; o limite é {MAX_FILES})", out.len()));
    }
    if total > MAX_TOTAL {
        return Err("os arquivos passam de 8 MiB juntos".into());
    }
    Ok(out)
}

/// Onde ficam os dados que o script grava (`store_set`): fora da pasta do plugin, que é
/// trocada inteira numa atualização.
pub fn data_file(root: &Path, id: &str) -> PathBuf {
    root.join(".data").join(format!("{id}.json"))
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
            Err(e) => eprintln!("plugin-dev: {}: {e}", dev.display()),
        }
    }
    list
}

/// Todo app tem tema e mascote; o plugin que não declarar ganha os padrões.
fn add_standard_settings(m: &mut Manifest) {
    if m.setting("theme").is_none() {
        let mut options = vec!["auto".to_string()];
        options.extend(themes::THEMES.iter().map(|t| t.key.to_string()));
        m.settings.push(SettingDef {
            key: "theme".into(),
            label: "Tema".into(),
            kind: SettingKind::Choice,
            default: toml::Value::String("auto".into()),
            min: None,
            max: None,
            step: None,
            options,
        });
    }
    if m.setting("mascot").is_none() {
        let mut options = vec![m.icon.clone()];
        options.extend(sprites::SPRITES.iter().map(|s| s.key.to_string()).filter(|k| *k != m.icon));
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
/// o motivo no terminal): um plugin quebrado não impede o hub de abrir.
pub fn load_installed(dir: &Path) -> Vec<Plugin> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut dirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    dirs.sort();
    let mut out: Vec<Plugin> = Vec::new();
    for d in dirs {
        // pastas temporárias de uma instalação em curso
        if d.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')) {
            continue;
        }
        match Plugin::load(&d) {
            Ok(p) if d.file_name().is_some_and(|n| n == p.manifest.id.as_str()) => {
                if out.iter().all(|o| o.manifest.id != p.manifest.id) {
                    out.push(p);
                }
            }
            Ok(p) => eprintln!("miniutils: plugin em {} tem o id {}, diferente da pasta", d.display(), p.manifest.id),
            Err(e) => eprintln!("miniutils: plugin em {} ignorado: {e}", d.display()),
        }
    }
    out
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
}
