//! Temas, visuais e fundos da pessoa, lidos de arquivo, sem recompilar:
//!
//! ```text
//! <dados>/miniutils/            Linux: ~/.local/share/miniutils   Windows: %APPDATA%\miniutils
//! ├── temas/<chave>.toml        cores de um tema de app (e da mascote)
//! ├── visuais/<chave>.toml      visual da janela
//! └── fundos/<chave>.toml       fundo do painel, em camadas
//! ```
//!
//! A chave é o nome do arquivo. Tema e visual podem partir de um existente (`base`) e
//! trocar só o que quiserem. Exemplos comentados em `examples/aparencia/`; o formato está
//! no DESIGN.md. `MINIUTILS_APARENCIA` troca a pasta (testes, `tools/dev-ui.sh`).
//!
//! Arquivo com erro fica de fora, com o motivo (Configurações › Aparência e o registro):
//! um arquivo quebrado não impede o hub de abrir.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::backdrop::{self, Fundo, Layer};
use super::color::parse_hex;
use super::{Buttons, FONTS, NUM_FIELDS, Skin, Tag, TitleStyle};
use crate::mascot::themes::{self, Theme};

pub const TEMAS: &str = "temas";
pub const VISUAIS: &str = "visuais";
pub const FUNDOS: &str = "fundos";

/// Pasta que contém `temas/`, `visuais/` e `fundos/`.
pub fn dir() -> PathBuf {
    match std::env::var_os("MINIUTILS_APARENCIA").filter(|v| !v.is_empty()) {
        Some(d) => PathBuf::from(d),
        None => dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("miniutils"),
    }
}

/// Arquivo que não entrou, e por quê.
#[derive(Debug, Clone, PartialEq)]
pub struct Skipped {
    /// `temas/oceano.toml`
    pub file: String,
    pub reason: String,
}

/// O que uma leitura da pasta achou.
#[derive(Debug, Default)]
pub struct Loaded {
    pub themes: Vec<Theme>,
    pub skins: Vec<Skin>,
    pub fundos: Vec<Fundo>,
    pub skipped: Vec<Skipped>,
}

impl Loaded {
    /// "2 temas · 1 visual · 0 fundos"
    pub fn summary(&self) -> String {
        let n = |k: usize, one: &str, many: &str| format!("{k} {}", if k == 1 { one } else { many });
        format!(
            "{} · {} · {}",
            n(self.themes.len(), "tema", "temas"),
            n(self.skins.len(), "visual", "visuais"),
            n(self.fundos.len(), "fundo", "fundos")
        )
    }
}

/// Lê a pasta e troca os temas, visuais e fundos de arquivo do programa.
pub fn reload() -> Loaded {
    let root = dir();
    let loaded = load(&root);
    for s in &loaded.skipped {
        log::warn!("{} ignorado: {}", root.join(&s.file).display(), s.reason);
    }
    backdrop::set_extra(loaded.fundos.clone());
    themes::set_extra(loaded.themes.clone());
    super::set_extra(loaded.skins.clone());
    loaded
}

/// Lê `root` sem mexer no programa: fundos e temas primeiro, porque um visual cita os dois.
pub fn load(root: &Path) -> Loaded {
    let mut out = Loaded::default();
    for (key, path) in files(&root.join(FUNDOS)) {
        let builtin = backdrop::BUILTIN.iter().any(|f| f.key == key);
        match read(&path, builtin).and_then(|t| parse_fundo(&key, &t)) {
            Ok(f) => out.fundos.push(f),
            Err(reason) => out.skipped.push(Skipped { file: format!("{FUNDOS}/{key}.toml"), reason }),
        }
    }
    for (key, path) in files(&root.join(TEMAS)) {
        let builtin = themes::BUILTIN.iter().any(|t| t.key == key);
        match read(&path, builtin).and_then(|t| parse_theme(&key, &t, &out.themes)) {
            Ok(t) => out.themes.push(t),
            Err(reason) => out.skipped.push(Skipped { file: format!("{TEMAS}/{key}.toml"), reason }),
        }
    }
    for (key, path) in files(&root.join(VISUAIS)) {
        let builtin = super::BUILTIN.iter().any(|s| s.key == key);
        match read(&path, builtin).and_then(|t| parse_skin(&key, &t, &out)) {
            Ok(s) => out.skins.push(s),
            Err(reason) => out.skipped.push(Skipped { file: format!("{VISUAIS}/{key}.toml"), reason }),
        }
    }
    out
}

/// Os `.toml` de `dir`, em ordem, com a chave (nome do arquivo). Chave inválida fica de
/// fora aqui mesmo, com o erro na leitura (`read`).
fn files(dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut v: Vec<(String, PathBuf)> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "toml"))
        .filter_map(|p| Some((p.file_stem()?.to_string_lossy().into_owned(), p)))
        .collect();
    v.sort();
    v
}

/// Lê o TOML, conferindo antes a chave (o nome do arquivo).
fn read(path: &Path, builtin: bool) -> Result<toml::Table, String> {
    let key = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    if key.is_empty() || key.len() > 32 || !key.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_') {
        return Err("o nome do arquivo (a chave) só pode ter a-z, 0-9, - e _ (até 32)".into());
    }
    if builtin {
        return Err(format!("já existe um embutido chamado \"{key}\": escolha outro nome"));
    }
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    if text.len() > 64 * 1024 {
        return Err("arquivo grande demais (máximo 64 KiB)".into());
    }
    text.parse::<toml::Table>().map_err(|e| e.message().to_string())
}

fn leak(s: &str) -> &'static str {
    Box::leak(s.to_string().into_boxed_str())
}

fn string<'a>(t: &'a toml::Table, key: &str) -> Result<Option<&'a str>, String> {
    match t.get(key) {
        None => Ok(None),
        Some(toml::Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(format!("{key}: esperava um texto entre aspas")),
    }
}

fn hex(key: &str, v: &toml::Value) -> Result<u32, String> {
    v.as_str().and_then(parse_hex).ok_or_else(|| format!("{key}: esperava uma cor \"#RRGGBB\""))
}

/// Rótulo: o do arquivo ou, sem ele, a chave.
fn label(t: &toml::Table, key: &str) -> Result<&'static str, String> {
    let l = string(t, "label")?.unwrap_or(key).trim();
    if l.is_empty() || l.chars().count() > 40 {
        return Err("label: de 1 a 40 caracteres".into());
    }
    Ok(leak(l))
}

pub fn parse_theme(key: &str, t: &toml::Table, loaded: &[Theme]) -> Result<Theme, String> {
    let base = string(t, "base")?.unwrap_or("neon");
    let mut theme = *themes::BUILTIN
        .iter()
        .chain(loaded)
        .find(|x| x.key == base)
        .ok_or_else(|| format!("base: não há tema \"{base}\""))?;
    theme.key = leak(key);
    theme.label = label(t, key)?;
    for (k, v) in t {
        match k.as_str() {
            "label" | "base" => {}
            _ => match theme.color_mut(k) {
                Some(slot) => *slot = hex(k, v)?,
                None => return Err(format!("campo desconhecido \"{k}\" (cores: {})", themes::COLOR_FIELDS.join(", "))),
            },
        }
    }
    Ok(theme)
}

pub fn parse_skin(key: &str, t: &toml::Table, loaded: &Loaded) -> Result<Skin, String> {
    let base = string(t, "base")?.unwrap_or("neon");
    let mut s = super::BUILTIN
        .iter()
        .chain(&loaded.skins)
        .find(|x| x.key == base)
        .cloned()
        .ok_or_else(|| format!("base: não há visual \"{base}\""))?;
    s.key = leak(key);
    s.label = label(t, key)?;
    let pick = |k: &str, v: &toml::Value, options: &[&'static str]| -> Result<&'static str, String> {
        let text = v.as_str().unwrap_or_default();
        options.iter().copied().find(|o| *o == text).ok_or_else(|| format!("{k}: use um de {}", options.join(", ")))
    };
    for (k, v) in t {
        match k.as_str() {
            "label" | "base" => {}
            "app_theme" => {
                let theme = v.as_str().unwrap_or_default();
                let exists = themes::BUILTIN.iter().chain(&loaded.themes).any(|x| x.key == theme);
                if !exists {
                    return Err(format!("app_theme: não há tema \"{theme}\""));
                }
                s.app_theme = leak(theme);
            }
            "backdrop" => {
                let f = v.as_str().unwrap_or_default();
                if !backdrop::BUILTIN.iter().chain(&loaded.fundos).any(|x| x.key == f) {
                    return Err(format!("backdrop: não há fundo \"{f}\""));
                }
                s.backdrop = leak(f);
            }
            "panel_top" | "edge_inner" => {
                let c = if v.as_str() == Some("none") { None } else { Some(hex(k, v)?) };
                if k == "panel_top" { s.panel_top = c } else { s.edge_inner = c }
            }
            "font_ui" => s.font_ui = pick(k, v, FONTS)?,
            "font_mono" => s.font_mono = pick(k, v, FONTS)?,
            "font_title" => s.font_title = pick(k, v, FONTS)?,
            "title" => {
                s.title = match pick(k, v, &["upper", "plain", "prompt"])? {
                    "upper" => TitleStyle::Upper,
                    "plain" => TitleStyle::Plain,
                    _ => TitleStyle::Prompt,
                }
            }
            "tag" => {
                s.tag = match pick(k, v, &["grid", "greeting", "none"])? {
                    "grid" => Tag::Grid,
                    "greeting" => Tag::Greeting,
                    _ => Tag::None,
                }
            }
            "buttons" => {
                s.buttons = match pick(k, v, &["icon", "bracket", "chunky"])? {
                    "icon" => Buttons::Icon,
                    "bracket" => Buttons::Bracket,
                    _ => Buttons::Chunky,
                }
            }
            "title_weight" => {
                s.title_weight = v
                    .as_integer()
                    .filter(|w| (100..=900).contains(w))
                    .ok_or("title_weight: um número de 100 a 900")? as i32
            }
            "title_cursor" => s.title_cursor = v.as_bool().ok_or("title_cursor: true ou false")?,
            "title_icon" | "title_icon_cut" => {
                let path = v.as_str().unwrap_or_default();
                // caminho SVG no quadro 8×8: só comandos e números
                if path.len() > 400 || !path.chars().all(|c| c.is_ascii_alphanumeric() || " .,-".contains(c)) {
                    return Err(format!("{k}: um caminho SVG curto (letras, números, espaço, . , -)"));
                }
                if k == "title_icon" { s.title_icon = leak(path) } else { s.title_icon_cut = leak(path) }
            }
            _ => {
                if let Some(&(_, lo, hi)) = NUM_FIELDS.iter().find(|f| f.0 == k) {
                    let n = v.as_float().or_else(|| v.as_integer().map(|i| i as f64)).ok_or(format!("{k}: um número"))? as f32;
                    if !(lo..=hi).contains(&n) {
                        return Err(format!("{k}: de {lo} a {hi}"));
                    }
                    if let Some(slot) = s.num_mut(k) {
                        *slot = n;
                    }
                } else if let Some(slot) = s.color_mut(k) {
                    *slot = hex(k, v)?;
                } else {
                    return Err(format!("campo desconhecido \"{k}\" (veja DESIGN.md › Visuais por arquivo)"));
                }
            }
        }
    }
    Ok(s)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FundoFile {
    #[serde(default)]
    label: Option<String>,
    #[serde(rename = "layer", default)]
    layers: Vec<Layer>,
}

pub fn parse_fundo(key: &str, t: &toml::Table) -> Result<Fundo, String> {
    let file: FundoFile = toml::Value::Table(t.clone()).try_into().map_err(|e: toml::de::Error| e.message().to_string())?;
    if file.layers.is_empty() || file.layers.len() > 32 {
        return Err("de 1 a 32 camadas ([[layer]])".into());
    }
    for (i, l) in file.layers.iter().enumerate() {
        l.check().map_err(|e| format!("camada {}: {e}", i + 1))?;
    }
    let mut table = toml::Table::new();
    if let Some(l) = &file.label {
        table.insert("label".into(), toml::Value::String(l.clone()));
    }
    Ok(Fundo { key: leak(key), label: label(&table, key)?, layers: Box::leak(file.layers.into_boxed_slice()) })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(s: &str) -> toml::Table {
        s.parse().unwrap()
    }

    #[test]
    fn exemplos_carregam_sem_erro() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/aparencia");
        let l = load(&root);
        assert!(l.skipped.is_empty(), "{:?}", l.skipped);
        assert_eq!((l.themes.len(), l.skins.len(), l.fundos.len()), (1, 1, 1));
        // o visual de exemplo usa o tema e o fundo de exemplo
        let s = &l.skins[0];
        assert_eq!((s.app_theme, s.backdrop), (l.themes[0].key, l.fundos[0].key));
        assert_eq!(l.summary(), "1 tema · 1 visual · 1 fundo");
    }

    #[test]
    fn tema_herda_da_base() {
        let t = parse_theme("oceano", &table("base = 'ghost'\nfur = '#ff7f50'"), &[]).unwrap();
        let ghost = themes::get("ghost");
        assert_eq!((t.key, t.label, t.fur, t.visor), ("oceano", "oceano", 0xff7f50, ghost.visor));
        assert!(parse_theme("x", &table("base = 'nada'"), &[]).unwrap_err().contains("base"));
        assert!(parse_theme("x", &table("pelo = '#ffffff'"), &[]).unwrap_err().contains("desconhecido"));
        assert!(parse_theme("x", &table("fur = 'azul'"), &[]).unwrap_err().contains("#RRGGBB"));
    }

    #[test]
    fn visual_confere_cada_campo() {
        let none = Loaded::default();
        let ok = parse_skin(
            "meu",
            &table("base='terminal'\naccent='#ff0000'\nchamfer=6\ntitle='plain'\nfont_ui='Pixelify Sans'\npanel_top='none'"),
            &none,
        )
        .unwrap();
        assert_eq!((ok.accent, ok.chamfer, ok.title, ok.font_ui), (0xff0000, 6.0, TitleStyle::Plain, "Pixelify Sans"));
        assert_eq!(ok.backdrop, "terminal");
        let erro = |s: &str| parse_skin("x", &table(s), &none).unwrap_err();
        assert!(erro("chamfer = 99").contains("de 0 a 40"));
        assert!(erro("font_ui = 'Comic Sans'").contains("use um de"));
        assert!(erro("backdrop = 'lua'").contains("não há fundo"));
        assert!(erro("app_theme = 'lua'").contains("não há tema"));
        assert!(erro("cor = '#ffffff'").contains("desconhecido"));
        assert!(erro("title_icon = '<script>'").contains("SVG"));
    }

    #[test]
    fn fundo_em_camadas() {
        let f = parse_fundo(
            "bolinhas",
            &table("label='Bolinhas'\n[[layer]]\nkind='dots'\nstep=16\nradius=2\ncolor='accent'\nalpha=0.1\n[[layer]]\nkind='rain'\ncount=20\ncolor='#ffffff'\nalpha=0.2"),
        )
        .unwrap();
        assert_eq!((f.label, f.layers.len()), ("Bolinhas", 2));
        let erro = |s: &str| parse_fundo("x", &table(s)).unwrap_err();
        assert!(erro("label='x'").contains("camadas"));
        assert!(erro("[[layer]]\nkind='dots'\nstep=1\nradius=1\ncolor='accent'").contains("camada 1"));
        assert!(erro("[[layer]]\nkind='city'").contains("city"), "a cidade é só embutida");
        assert!(erro("[[layer]]\nkind='dots'\nstep=10\nradius=1\ncolor='verde'").contains("cor"));
        assert!(erro("[[layer]]\nkind='dots'\nstep=10\nradius=1\ncolor='accent'\nraio=2").contains("raio"));
    }

    #[test]
    fn chave_e_nomes_proibidos() {
        let dir = std::env::temp_dir().join(format!("miniutils-aparencia-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(TEMAS)).unwrap();
        std::fs::write(dir.join(TEMAS).join("neon.toml"), "fur = '#000000'").unwrap();
        std::fs::write(dir.join(TEMAS).join("Maiusculo.toml"), "fur = '#000000'").unwrap();
        std::fs::write(dir.join(TEMAS).join("quebrado.toml"), "fur = ").unwrap();
        std::fs::write(dir.join(TEMAS).join("bom.toml"), "label = 'Bom'").unwrap();
        let l = load(&dir);
        assert_eq!(l.themes.len(), 1);
        let motivo = |f: &str| l.skipped.iter().find(|s| s.file == format!("temas/{f}.toml")).unwrap().reason.clone();
        assert!(motivo("neon").contains("já existe"));
        assert!(motivo("Maiusculo").contains("a-z"));
        assert!(!motivo("quebrado").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
