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
//! [ui]
//! view = "view.slint"     # componente exportado por último é a tela
//! script = "main.rhai"    # fn start(), fn tick(), fn action(name, arg)
//! every = 10              # tick a cada N segundos (opcional; 1 a 3600)
//! ```
//! Instalados ficam em `~/.local/share/miniutils/plugins/<id>/` (ver `store.rs`).

use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::manifest::{Manifest, SettingDef, SettingKind};
use crate::mascot::{sprites, themes};

/// Arquivos que um plugin pode ter (nada de binário, script ou pasta).
pub const ALLOWED_FILES: &[&str] =
    &["manifest.toml", "view.slint", "main.rhai", "README.md", "LICENSE", "LICENSE.md", "LICENSE.txt"];

/// Um plugin pronto para o host rodar.
#[derive(Debug, Clone)]
pub struct Plugin {
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
        if !manifest.uses.is_empty() {
            return Err("plugins não assinam provedores (uses) nesta versão".into());
        }
        if let Some(p) = manifest.permissions.exec.iter().find(|p| p.contains('/') || p.is_empty()) {
            return Err(format!("[permissions] exec: use o nome do programa, não um caminho: {p}"));
        }
        match (&extra.run, &extra.ui) {
            (None, None) => return Err("falta a seção [run] (plugin simples) ou [ui] (tela própria)".into()),
            (Some(_), Some(_)) => return Err("use [run] ou [ui], não os dois".into()),
            _ => {}
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
        Ok(Plugin { manifest, run: extra.run, ui: extra.ui, display: extra.display, moods: extra.mood })
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
        let parsed = std::fs::read_to_string(d.join("manifest.toml")).map_err(|e| e.to_string()).and_then(|t| Plugin::parse(&t));
        match parsed {
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
    }
}
