//! The user's themes, skins and backdrops, read from files, without recompiling:
//!
//! ```text
//! <data>/miniutils/              Linux: ~/.local/share/miniutils   Windows: %APPDATA%\miniutils
//! ├── themes/<key>.toml          colors of an app theme (and of the mascot)
//! ├── skins/<key>.toml           look of the window
//! ├── backdrops/<key>.toml       panel backdrop, in layers
//! └── mascots/<key>/             imported mascot: PNG sheets (see `mascot/pack.rs`)
//! ```
//!
//! The key is the file name. A theme or skin can start from an existing one (`base`) and
//! change only what it wants. Commented examples in `examples/appearance/`; the format is in
//! DESIGN.md. `MINIUTILS_APPEARANCE` changes the folder (tests, `tools/dev-ui.sh`).
//!
//! A file with an error is left out, with the reason (Settings › Appearance and the log): a
//! broken file does not keep the hub from opening.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::backdrop::{self, Backdrop, Layer};
use super::color::parse_hex;
use super::{Buttons, FONTS, NUM_FIELDS, Skin, Tag, TitleStyle};
use crate::mascot::pack;
use crate::mascot::themes::{self, Theme};
use crate::tr;

pub const THEMES: &str = "themes";
pub const SKINS: &str = "skins";
pub const BACKDROPS: &str = "backdrops";

/// Folder that holds `themes/`, `skins/` and `backdrops/`.
pub fn dir() -> PathBuf {
    match std::env::var_os("MINIUTILS_APPEARANCE").filter(|v| !v.is_empty()) {
        Some(d) => PathBuf::from(d),
        None => dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("miniutils"),
    }
}

/// A file that did not get in, and why.
#[derive(Debug, Clone, PartialEq)]
pub struct Skipped {
    /// `themes/ocean.toml`
    pub file: String,
    pub reason: String,
}

/// What one read of the folder found.
#[derive(Debug, Default)]
pub struct Loaded {
    pub themes: Vec<Theme>,
    pub skins: Vec<Skin>,
    pub backdrops: Vec<Backdrop>,
    /// Imported mascots that got in (they themselves live in `mascot::pack`).
    pub mascots: usize,
    pub skipped: Vec<Skipped>,
}

impl Loaded {
    /// "Themes: 2 · skins: 1 · backdrops: 0 · mascots: 3"
    pub fn summary(&self) -> String {
        tr!("Themes: {} · skins: {} · backdrops: {} · mascots: {}", self.themes.len(), self.skins.len(), self.backdrops.len(), self.mascots)
    }
}

/// Reads the folder and swaps the program's file themes, skins, backdrops and mascots.
pub fn reload() -> Loaded {
    let root = dir();
    let mut loaded = load(&root);
    let (arts, skipped) = pack::load(&root.join(pack::DIR));
    loaded.mascots = arts.len();
    loaded.skipped.extend(skipped.into_iter().map(|s| Skipped { file: s.dir, reason: s.reason }));
    pack::set(arts);
    for s in &loaded.skipped {
        log::warn!("{} skipped: {}", root.join(&s.file).display(), s.reason);
    }
    backdrop::set_extra(loaded.backdrops.clone());
    themes::set_extra(loaded.themes.clone());
    super::set_extra(loaded.skins.clone());
    loaded
}

/// Reads `root` without touching the program: backdrops and themes first, because a skin
/// refers to both.
pub fn load(root: &Path) -> Loaded {
    let mut out = Loaded::default();
    let (dir, shown) = (root.join(BACKDROPS), BACKDROPS);
    for (key, path) in files(&dir) {
        let builtin = backdrop::BUILTIN.iter().any(|f| f.key == key);
        match read(&path, builtin).and_then(|t| parse_backdrop(&key, &t)) {
            Ok(f) => out.backdrops.push(f),
            Err(reason) => out.skipped.push(Skipped { file: format!("{shown}/{key}.toml"), reason }),
        }
    }
    let (dir, shown) = (root.join(THEMES), THEMES);
    for (key, path) in files(&dir) {
        let builtin = themes::BUILTIN.iter().any(|t| t.key == key);
        match read(&path, builtin).and_then(|t| parse_theme(&key, &t, &out.themes)) {
            Ok(t) => out.themes.push(t),
            Err(reason) => out.skipped.push(Skipped { file: format!("{shown}/{key}.toml"), reason }),
        }
    }
    let (dir, shown) = (root.join(SKINS), SKINS);
    for (key, path) in files(&dir) {
        let builtin = super::BUILTIN.iter().any(|s| s.key == key);
        match read(&path, builtin).and_then(|t| parse_skin(&key, &t, &out)) {
            Ok(s) => out.skins.push(s),
            Err(reason) => out.skipped.push(Skipped { file: format!("{shown}/{key}.toml"), reason }),
        }
    }
    out
}

/// The `.toml` files of `dir`, in order, with the key (file name). An invalid key is not
/// dropped here: the error comes when reading it (`read`).
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

/// Reads the TOML, checking the key (the file name) first.
fn read(path: &Path, builtin: bool) -> Result<toml::Table, String> {
    let key = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    if key.is_empty() || key.len() > 32 || !key.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_') {
        return Err(tr!("the file name (the key) may only have a-z, 0-9, - and _ (up to 32)").into());
    }
    if builtin {
        return Err(tr!("there is already a built-in one called \"{}\": choose another name", key));
    }
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    if text.len() > 64 * 1024 {
        return Err(tr!("file too large (64 KiB at most)").into());
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
        Some(_) => Err(tr!("{}: expected a text in quotes", key)),
    }
}

fn hex(key: &str, v: &toml::Value) -> Result<u32, String> {
    v.as_str().and_then(parse_hex).ok_or_else(|| tr!("{}: expected a color \"#RRGGBB\"", key))
}

/// Label: the file's own or, without one, the key.
fn label(t: &toml::Table, key: &str) -> Result<&'static str, String> {
    let l = string(t, "label")?.unwrap_or(key).trim();
    if l.is_empty() || l.chars().count() > 40 {
        return Err(tr!("label: 1 to 40 characters").into());
    }
    Ok(leak(l))
}

pub fn parse_theme(key: &str, t: &toml::Table, loaded: &[Theme]) -> Result<Theme, String> {
    let base = string(t, "base")?.unwrap_or("neon");
    let mut theme = *themes::BUILTIN
        .iter()
        .chain(loaded)
        .find(|x| x.key == base)
        .ok_or_else(|| tr!("base: there is no theme \"{}\"", base))?;
    theme.key = leak(key);
    theme.label = label(t, key)?;
    for (k, v) in t {
        match k.as_str() {
            "label" | "base" => {}
            _ => match theme.color_mut(k) {
                Some(slot) => *slot = hex(k, v)?,
                None => return Err(tr!("unknown field \"{}\" (colors: {})", k, themes::COLOR_FIELDS.join(", "))),
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
        .ok_or_else(|| tr!("base: there is no skin \"{}\"", base))?;
    s.key = leak(key);
    s.label = label(t, key)?;
    let pick = |k: &str, v: &toml::Value, options: &[&'static str]| -> Result<&'static str, String> {
        let text = v.as_str().unwrap_or_default();
        options.iter().copied().find(|o| *o == text).ok_or_else(|| tr!("{}: use one of {}", k, options.join(", ")))
    };
    for (k, v) in t {
        match k.as_str() {
            "label" | "base" => {}
            "app_theme" => {
                let theme = v.as_str().unwrap_or_default();
                let exists = themes::BUILTIN.iter().chain(&loaded.themes).any(|x| x.key == theme);
                if !exists {
                    return Err(tr!("app_theme: there is no theme \"{}\"", theme));
                }
                s.app_theme = leak(theme);
            }
            "backdrop" => {
                let f = v.as_str().unwrap_or_default();
                if !backdrop::BUILTIN.iter().chain(&loaded.backdrops).any(|x| x.key == f) {
                    return Err(tr!("backdrop: there is no backdrop \"{}\"", f));
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
                    .ok_or(tr!("title_weight: a number from 100 to 900"))? as i32
            }
            "title_cursor" => s.title_cursor = v.as_bool().ok_or(tr!("title_cursor: true or false"))?,
            "title_icon" | "title_icon_cut" => {
                let path = v.as_str().unwrap_or_default();
                // SVG path in the 8×8 frame: only commands and numbers
                if path.len() > 400 || !path.chars().all(|c| c.is_ascii_alphanumeric() || " .,-".contains(c)) {
                    return Err(tr!("{}: a short SVG path (letters, numbers, space, . , -)", k));
                }
                if k == "title_icon" { s.title_icon = leak(path) } else { s.title_icon_cut = leak(path) }
            }
            _ => {
                if let Some(&(_, lo, hi)) = NUM_FIELDS.iter().find(|f| f.0 == k) {
                    let n = v.as_float().or_else(|| v.as_integer().map(|i| i as f64)).ok_or(tr!("{}: a number", k))? as f32;
                    if !(lo..=hi).contains(&n) {
                        return Err(tr!("{}: from {} to {}", k, lo, hi));
                    }
                    if let Some(slot) = s.num_mut(k) {
                        *slot = n;
                    }
                } else if let Some(slot) = s.color_mut(k) {
                    *slot = hex(k, v)?;
                } else {
                    return Err(tr!("unknown field \"{}\" (see DESIGN.md › Skins from files)", k));
                }
            }
        }
    }
    Ok(s)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BackdropFile {
    #[serde(default)]
    label: Option<String>,
    #[serde(rename = "layer", default)]
    layers: Vec<Layer>,
}

pub fn parse_backdrop(key: &str, t: &toml::Table) -> Result<Backdrop, String> {
    let file: BackdropFile = toml::Value::Table(t.clone()).try_into().map_err(|e: toml::de::Error| e.message().to_string())?;
    if file.layers.is_empty() || file.layers.len() > 32 {
        return Err(tr!("1 to 32 layers ([[layer]])").into());
    }
    for (i, l) in file.layers.iter().enumerate() {
        l.check().map_err(|e| tr!("layer {}: {}", i + 1, e))?;
    }
    let mut table = toml::Table::new();
    if let Some(l) = &file.label {
        table.insert("label".into(), toml::Value::String(l.clone()));
    }
    Ok(Backdrop { key: leak(key), label: label(&table, key)?, layers: Box::leak(file.layers.into_boxed_slice()) })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(s: &str) -> toml::Table {
        s.parse().unwrap()
    }

    /// An empty temporary folder for one test.
    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("miniutils-appearance-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn examples_load_without_errors() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/appearance");
        let l = load(&root);
        assert!(l.skipped.is_empty(), "{:?}", l.skipped);
        assert_eq!((l.themes.len(), l.skins.len(), l.backdrops.len()), (1, 1, 1));
        // the example skin uses the example theme and backdrop
        let s = &l.skins[0];
        assert_eq!((s.app_theme, s.backdrop), (l.themes[0].key, l.backdrops[0].key));
        assert_eq!(l.summary(), "Themes: 1 · skins: 1 · backdrops: 1 · mascots: 0");
    }

    #[test]
    fn theme_inherits_from_base() {
        let t = parse_theme("ocean", &table("base = 'ghost'\nfur = '#ff7f50'"), &[]).unwrap();
        let ghost = themes::get("ghost");
        assert_eq!((t.key, t.label, t.fur, t.visor), ("ocean", "ocean", 0xff7f50, ghost.visor));
        assert!(parse_theme("x", &table("base = 'nothing'"), &[]).unwrap_err().contains("base"));
        assert!(parse_theme("x", &table("pelt = '#ffffff'"), &[]).unwrap_err().contains("unknown"));
        assert!(parse_theme("x", &table("fur = 'blue'"), &[]).unwrap_err().contains("#RRGGBB"));
    }

    #[test]
    fn skin_checks_every_field() {
        let none = Loaded::default();
        let ok = parse_skin(
            "mine",
            &table("base='terminal'\naccent='#ff0000'\nchamfer=6\ntitle='plain'\nfont_ui='Pixelify Sans'\npanel_top='none'"),
            &none,
        )
        .unwrap();
        assert_eq!((ok.accent, ok.chamfer, ok.title, ok.font_ui), (0xff0000, 6.0, TitleStyle::Plain, "Pixelify Sans"));
        assert_eq!(ok.backdrop, "terminal");
        let err = |s: &str| parse_skin("x", &table(s), &none).unwrap_err();
        assert!(err("chamfer = 99").contains("from 0 to 40"));
        assert!(err("font_ui = 'Comic Sans'").contains("use one of"));
        assert!(err("backdrop = 'moon'").contains("no backdrop"));
        assert!(err("app_theme = 'moon'").contains("no theme"));
        assert!(err("colour = '#ffffff'").contains("unknown"));
        assert!(err("title_icon = '<script>'").contains("SVG"));
    }

    #[test]
    fn layered_backdrop() {
        let f = parse_backdrop(
            "dots",
            &table("label='Dots'\n[[layer]]\nkind='dots'\nstep=16\nradius=2\ncolor='accent'\nalpha=0.1\n[[layer]]\nkind='rain'\ncount=20\ncolor='#ffffff'\nalpha=0.2"),
        )
        .unwrap();
        assert_eq!((f.label, f.layers.len()), ("Dots", 2));
        let err = |s: &str| parse_backdrop("x", &table(s)).unwrap_err();
        assert!(err("label='x'").contains("layers"));
        assert!(err("[[layer]]\nkind='dots'\nstep=1\nradius=1\ncolor='accent'").contains("layer 1"));
        assert!(err("[[layer]]\nkind='city'").contains("city"), "the city is built-in only");
        assert!(err("[[layer]]\nkind='dots'\nstep=10\nradius=1\ncolor='green'").contains("color"));
        assert!(err("[[layer]]\nkind='dots'\nstep=10\nradius=1\ncolor='accent'\nsize=2").contains("size"));
    }

    #[test]
    fn key_and_forbidden_names() {
        let dir = temp_dir("keys");
        std::fs::create_dir_all(dir.join(THEMES)).unwrap();
        std::fs::write(dir.join(THEMES).join("neon.toml"), "fur = '#000000'").unwrap();
        std::fs::write(dir.join(THEMES).join("Uppercase.toml"), "fur = '#000000'").unwrap();
        std::fs::write(dir.join(THEMES).join("broken.toml"), "fur = ").unwrap();
        std::fs::write(dir.join(THEMES).join("good.toml"), "label = 'Good'").unwrap();
        let l = load(&dir);
        assert_eq!(l.themes.len(), 1);
        let reason = |f: &str| l.skipped.iter().find(|s| s.file == format!("themes/{f}.toml")).unwrap().reason.clone();
        assert!(reason("neon").contains("already"));
        assert!(reason("Uppercase").contains("a-z"));
        assert!(!reason("broken").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
