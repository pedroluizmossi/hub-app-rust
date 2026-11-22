//! Shaders of the Library: the built-in ones and the user's, in Ghostty's `custom-shader`
//! format (see `apps/terminal/shader.rs` for what they receive).
//!
//! ```text
//! <data>/miniutils/shaders/
//! ├── crt-soft.glsl              a shader: the key is the file name
//! ├── crt-soft.toml              optional, next to it: name, author, license, adjustments
//! └── pixel-scenes/              or a folder:
//!     ├── shader.glsl
//!     └── shader.toml
//! ```
//!
//! The adjustments are the shader's own number `#define`s (`#define BLOOM_STRENGTH 0.04`),
//! found by reading it: the line above (a comment) becomes the label, and a `[min, max]` in
//! those comments the range. The `.toml` can name, limit or hide them (`[params.NAME]`).
//! A changed value is written into the `#define` line when the shader is compiled; the file
//! itself is never touched.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use serde::Deserialize;

use super::Origin;
use crate::tr;

/// Subfolder of the appearance folder (`skin::user::dir`).
pub const DIR: &str = "shaders";
/// A shader file larger than this is refused (a long one is ~50 KiB).
const MAX_BYTES: u64 = 512 * 1024;

/// A shader of the Library.
#[derive(Debug, Clone, PartialEq)]
pub struct Shader {
    pub key: String,
    pub name: String,
    pub origin: Origin,
    /// The `.glsl` file (`None` for a built-in one).
    pub path: Option<PathBuf>,
    /// The `.toml` with its description, when there is one.
    pub meta_path: Option<PathBuf>,
    pub glsl: String,
    pub author: String,
    pub license: String,
    pub description: String,
    pub params: Vec<Param>,
}

/// One adjustment: a number `#define` of the shader.
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    pub label: String,
    pub default: f64,
    pub min: f64,
    pub max: f64,
    pub step: f64,
    /// Written without a decimal point (an `int` in GLSL).
    pub integer: bool,
}

/// A file of the folder that did not get in, and why.
#[derive(Debug, Clone, PartialEq)]
pub struct Skipped {
    pub key: String,
    pub path: PathBuf,
    pub reason: String,
}

/// The `.toml` next to a shader.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Meta {
    name: String,
    author: String,
    license: String,
    description: String,
    params: std::collections::BTreeMap<String, ParamMeta>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ParamMeta {
    label: Option<String>,
    min: Option<f64>,
    max: Option<f64>,
    step: Option<f64>,
    hidden: bool,
}

/// The built-in shaders: written for MiniUtils (MIT), like the rest of it. Their
/// adjustments get translated labels here (a user's shader shows its own comments).
pub fn builtin() -> Vec<Shader> {
    let make = |key: &str, name: &str, description: &str, glsl: &str, labels: &[(&str, &str)], hidden: &[&str]| {
        let mut meta = Meta::default();
        for (param, label) in labels {
            meta.params.insert((*param).into(), ParamMeta { label: Some((*label).into()), ..ParamMeta::default() });
        }
        for param in hidden {
            meta.params.insert((*param).into(), ParamMeta { hidden: true, ..ParamMeta::default() });
        }
        Shader {
            key: key.into(),
            name: name.into(),
            origin: Origin::Builtin,
            path: None,
            meta_path: None,
            glsl: glsl.into(),
            author: "MiniUtils".into(),
            license: "MIT".into(),
            description: description.into(),
            params: params_of(glsl, &meta),
        }
    };
    vec![
        make(
            "crt",
            tr!("CRT"),
            tr!("Curved glass, scanlines, a phosphor grille and dark corners."),
            include_str!("../../miniapps/terminal/shaders/crt.glsl"),
            &[
                ("CURVE_X", tr!("Glass flatness, across")),
                ("CURVE_Y", tr!("Glass flatness, down")),
                ("SCANLINE_PERIOD", tr!("Scanline spacing (px)")),
                ("SCANLINE_DEPTH", tr!("Scanline darkness")),
                ("MASK_DEPTH", tr!("Phosphor grille")),
                ("FRINGE", tr!("Color fringing (px)")),
                ("VIGNETTE", tr!("Corner darkening")),
                ("VIGNETTE_PLATEAU", tr!("Bright middle")),
            ],
            &[],
        ),
        make(
            "bloom",
            tr!("Bloom"),
            tr!("Bright text glows."),
            include_str!("../../miniapps/terminal/shaders/bloom.glsl"),
            &[("SPREAD", tr!("Glow size (px)")), ("THRESHOLD", tr!("Glow starts at brightness")), ("STRENGTH", tr!("Glow strength"))],
            &["SAMPLES"],
        ),
    ]
}

/// Folder of the user's shaders.
pub fn dir() -> PathBuf {
    crate::skin::user::dir().join(DIR)
}

/// What one read of the folder found (and when the folder looked like this).
struct Cache {
    stamp: u64,
    shaders: Vec<Shader>,
    skipped: Vec<Skipped>,
}

static CACHE: Mutex<Option<Cache>> = Mutex::new(None);

/// Built-in and user shaders, the built-in ones first. The folder is read again only
/// when something in it changed (`stamp`).
pub fn all() -> Vec<Shader> {
    with_cache(|c| builtin().into_iter().chain(c.shaders.iter().cloned()).collect())
}

/// The files that did not get in, with the reason.
pub fn skipped() -> Vec<Skipped> {
    with_cache(|c| c.skipped.clone())
}

pub fn find(key: &str) -> Option<Shader> {
    all().into_iter().find(|s| s.key == key)
}

/// A number that changes whenever a file of the folder is added, removed or saved.
pub fn current_stamp() -> u64 {
    stamp(&dir())
}

fn with_cache<T>(f: impl FnOnce(&Cache) -> T) -> T {
    let dir = dir();
    let now = stamp(&dir);
    let mut guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if guard.as_ref().is_none_or(|c| c.stamp != now) {
        let (shaders, skipped) = load(&dir);
        for s in &skipped {
            log::warn!("{} skipped: {}", s.path.display(), s.reason);
        }
        *guard = Some(Cache { stamp: now, shaders, skipped });
    }
    f(guard.as_ref().expect("filled above"))
}

/// Hash of every name, size and modification time in the folder (one level into its
/// subfolders): changes whenever a file is added, removed or saved.
pub fn stamp(dir: &Path) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let mut visit = |path: &Path| {
        let Ok(entries) = std::fs::read_dir(path) else { return Vec::new() };
        let mut subdirs = Vec::new();
        let mut list: Vec<_> = entries.flatten().collect();
        list.sort_by_key(|e| e.file_name());
        for e in list {
            let Ok(meta) = e.metadata() else { continue };
            e.file_name().hash(&mut h);
            meta.len().hash(&mut h);
            meta.modified().ok().and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok()).hash(&mut h);
            if meta.is_dir() {
                subdirs.push(e.path());
            }
        }
        subdirs
    };
    for sub in visit(dir) {
        visit(&sub);
    }
    h.finish()
}

/// Reads the user's folder: `<key>.glsl` (+ `<key>.toml`) and `<key>/shader.glsl`
/// (+ `shader.toml`).
pub fn load(dir: &Path) -> (Vec<Shader>, Vec<Skipped>) {
    let builtin_keys: Vec<String> = builtin().into_iter().map(|s| s.key).collect();
    let mut found: Vec<(String, PathBuf, Option<PathBuf>)> = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return (Vec::new(), Vec::new()) };
    for e in entries.flatten() {
        let path = e.path();
        if path.is_dir() {
            let glsl = path.join("shader.glsl");
            if glsl.is_file() {
                let key = e.file_name().to_string_lossy().into_owned();
                let meta = path.join("shader.toml");
                found.push((key, glsl, meta.is_file().then_some(meta)));
            }
        } else if path.extension().is_some_and(|x| x == "glsl") {
            let key = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let meta = path.with_extension("toml");
            found.push((key, path.clone(), meta.is_file().then_some(meta)));
        }
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    let mut shaders = Vec::new();
    let mut skipped = Vec::new();
    for (key, path, meta) in found {
        let read = check_key(&key)
            .and_then(|()| {
                if builtin_keys.contains(&key) {
                    Err(tr!("there is already a built-in one called \"{}\": choose another name", key))
                } else {
                    Ok(())
                }
            })
            .and_then(|()| read_shader(&key, &path, meta.as_deref()));
        match read {
            Ok(s) => shaders.push(s),
            Err(reason) => skipped.push(Skipped { key, path, reason }),
        }
    }
    (shaders, skipped)
}

/// A key the folder can hold: what themes and skins accept, too.
pub fn check_key(key: &str) -> Result<(), String> {
    let ok = !key.is_empty()
        && key.len() <= 32
        && key.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if ok { Ok(()) } else { Err(tr!("the file name (the key) may only have a-z, 0-9, - and _ (up to 32)").into()) }
}

fn read_shader(key: &str, path: &Path, meta_path: Option<&Path>) -> Result<Shader, String> {
    let size = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if size > MAX_BYTES {
        return Err(tr!("file too large ({} KiB at most)", MAX_BYTES / 1024));
    }
    let glsl = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    if !glsl.contains("mainImage") {
        return Err(tr!("no mainImage function: not a shader in Ghostty's format").into());
    }
    let meta: Meta = match meta_path {
        Some(p) => {
            let text = std::fs::read_to_string(p).map_err(|e| e.to_string())?;
            toml::from_str(&text).map_err(|e| format!("{}: {}", p.display(), e.message()))?
        }
        None => Meta::default(),
    };
    let params = params_of(&glsl, &meta);
    Ok(Shader {
        key: key.into(),
        name: if meta.name.trim().is_empty() { key.into() } else { meta.name.trim().into() },
        origin: Origin::User,
        path: Some(path.to_path_buf()),
        meta_path: meta_path.map(Path::to_path_buf),
        glsl,
        author: meta.author,
        license: meta.license,
        description: meta.description,
        params,
    })
}

/// The adjustments of a shader: its number `#define`s, less the ones used as switches
/// (`#ifdef NAME`), with what the `.toml` says about each.
fn params_of(glsl: &str, meta: &Meta) -> Vec<Param> {
    let lines: Vec<&str> = glsl.lines().collect();
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let Some((name, value, integer)) = number_define(line) else { continue };
        if is_switch(glsl, name) || out.iter().any(|p: &Param| p.name == name) {
            continue;
        }
        let pm = meta.params.get(name);
        if pm.is_some_and(|p| p.hidden) {
            continue;
        }
        let comments = comments_above(&lines, i);
        let (guess_min, guess_max) = range_in(&comments).unwrap_or_else(|| guess_range(value, integer));
        let min = pm.and_then(|p| p.min).unwrap_or(guess_min.min(value));
        let max = pm.and_then(|p| p.max).unwrap_or(guess_max.max(value));
        let step = pm.and_then(|p| p.step).unwrap_or(if integer { 1.0 } else { nice_step(max - min) });
        let label = pm
            .and_then(|p| p.label.clone())
            .or_else(|| comments.iter().find(|c| range_in(&[c.as_str()]).is_none() && !c.contains("\\in")).cloned())
            .unwrap_or_else(|| name.to_string());
        out.push(Param { name: name.to_string(), label, default: value, min, max, step, integer });
    }
    out
}

/// `#define NAME 0.25` → ("NAME", 0.25, false). Only a single number (not `vec2(…)` or
/// `1.0, 2.0`), possibly followed by a comment.
fn number_define(line: &str) -> Option<(&str, f64, bool)> {
    let rest = line.trim_start().strip_prefix("#define")?;
    let mut parts = rest.split_whitespace();
    let name = parts.next()?;
    if name.contains('(') || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    let value = parts.next()?;
    let tail: Vec<&str> = parts.collect();
    if !tail.is_empty() && !tail[0].starts_with("//") {
        return None;
    }
    let number = value.trim_end_matches(['f', 'F']);
    let parsed: f64 = number.parse().ok()?;
    Some((name, parsed, !number.contains(['.', 'e', 'E'])))
}

/// The name is tested with `#ifdef`/`#ifndef`/`defined(…)`: changing its value would do
/// nothing (or the shader switches on its presence).
fn is_switch(glsl: &str, name: &str) -> bool {
    glsl.lines().any(|l| {
        let l = l.trim_start();
        (l.starts_with("#ifdef") || l.starts_with("#ifndef") || l.starts_with("#if"))
            && (l.split_whitespace().nth(1) == Some(name) || l.contains(&format!("defined({name})")))
    })
}

/// The `//` comment lines right above line `i`, top first, without the slashes.
fn comments_above(lines: &[&str], i: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut j = i;
    while j > 0 {
        j -= 1;
        let Some(c) = lines[j].trim().strip_prefix("//") else { break };
        let c = c.trim();
        if !c.is_empty() {
            out.push(c.to_string());
        }
    }
    out.reverse();
    out
}

/// A `[min, max]` written in the comments (Ghostty shaders document ranges like that).
fn range_in(comments: &[impl AsRef<str>]) -> Option<(f64, f64)> {
    comments.iter().find_map(|c| {
        let c = c.as_ref();
        let inner = c.split_once('[')?.1.split_once(']')?.0;
        let (a, b) = inner.split_once(',')?;
        let (a, b): (f64, f64) = (a.trim().parse().ok()?, b.trim().parse().ok()?);
        (a < b).then_some((a, b))
    })
}

/// A range around the default when the shader gives none: from 0 (or the negative mirror)
/// to four times the value; [0, 1] for zero.
fn guess_range(value: f64, integer: bool) -> (f64, f64) {
    if value == 0.0 {
        return (0.0, if integer { 10.0 } else { 1.0 });
    }
    let span = value.abs() * 4.0;
    if value > 0.0 { (0.0, span) } else { (-span, span) }
}

/// About a hundred steps across the range, rounded to 1, 2 or 5 × a power of ten.
fn nice_step(range: f64) -> f64 {
    if range <= 0.0 {
        return 0.01;
    }
    let raw = range / 100.0;
    let pow = 10f64.powf(raw.log10().floor());
    let m = raw / pow;
    let nice = if m < 1.5 { 1.0 } else if m < 3.5 { 2.0 } else if m < 7.5 { 5.0 } else { 10.0 };
    nice * pow
}

/// The shader with the values the user changed written into its `#define` lines.
pub fn with_values(shader: &Shader, values: &toml::Table) -> String {
    if values.is_empty() {
        return shader.glsl.clone();
    }
    let mut out = String::with_capacity(shader.glsl.len());
    for line in shader.glsl.split_inclusive('\n') {
        let replaced = number_define(line).and_then(|(name, _, _)| {
            let param = shader.params.iter().find(|p| p.name == name)?;
            let v = values.get(name).and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))?;
            let v = v.clamp(param.min, param.max);
            let indent = &line[..line.len() - line.trim_start().len()];
            let comment = line.find("//").map(|i| format!(" {}", line[i..].trim_end())).unwrap_or_default();
            let newline = if line.ends_with('\n') { "\n" } else { "" };
            Some(format!("{indent}#define {name} {}{comment}{newline}", literal(v, param.integer)))
        });
        out.push_str(replaced.as_deref().unwrap_or(line));
    }
    out
}

/// GLSL text of a value: `3` for an int, `0.25` / `4.0` for a float (never a comma).
pub fn literal(v: f64, integer: bool) -> String {
    if integer {
        return format!("{}", v.round() as i64);
    }
    let s = format!("{v:.6}");
    let s = s.trim_end_matches('0');
    if s.ends_with('.') { format!("{s}0") } else { s.to_string() }
}

/// A free key for a copy of `key` in the folder: `key-copy`, `key-copy-2`…
pub fn free_key(key: &str, taken: &[String]) -> String {
    let base = format!("{}-copy", key.chars().take(24).collect::<String>());
    if !taken.contains(&base) {
        return base;
    }
    (2..).map(|n| format!("{base}-{n}")).find(|k| !taken.contains(k)).expect("an unbounded range ends with a free key")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
// How visible the bloom is
// [0, 1]
#define BLOOM_STRENGTH 0.04
#define SAMPLES 24 // count
#define CURVE 13.0, 11.0
#define TINT vec3(1.0)
#define CRTS_WARP 1
#ifdef CRTS_WARP
#endif
void mainImage(out vec4 c, in vec2 p) { c = vec4(BLOOM_STRENGTH); }
";

    #[test]
    fn finds_number_defines_with_label_and_range() {
        let p = params_of(SAMPLE, &Meta::default());
        let names: Vec<&str> = p.iter().map(|p| p.name.as_str()).collect();
        // vectors, lists and switches are not adjustments
        assert_eq!(names, ["BLOOM_STRENGTH", "SAMPLES"]);
        assert_eq!((p[0].label.as_str(), p[0].min, p[0].max, p[0].integer), ("How visible the bloom is", 0.0, 1.0, false));
        assert_eq!((p[1].label.as_str(), p[1].min, p[1].max, p[1].step, p[1].integer), ("SAMPLES", 0.0, 96.0, 1.0, true));
    }

    #[test]
    fn meta_names_limits_and_hides() {
        let meta: Meta = toml::from_str("[params.BLOOM_STRENGTH]\nlabel = 'Glow'\nmax = 0.5\n[params.SAMPLES]\nhidden = true\n").unwrap();
        let p = params_of(SAMPLE, &meta);
        assert_eq!(p.len(), 1);
        assert_eq!((p[0].label.as_str(), p[0].max), ("Glow", 0.5));
    }

    #[test]
    fn values_go_into_the_define_lines_only() {
        let s = Shader {
            key: "t".into(),
            name: "t".into(),
            origin: Origin::User,
            path: None,
            meta_path: None,
            glsl: SAMPLE.into(),
            author: String::new(),
            license: String::new(),
            description: String::new(),
            params: params_of(SAMPLE, &Meta::default()),
        };
        let values: toml::Table = toml::from_str("BLOOM_STRENGTH = 0.5\nSAMPLES = 7.6\nCURVE = 1.0\nBOGUS = 2").unwrap();
        let out = with_values(&s, &values);
        assert!(out.contains("#define BLOOM_STRENGTH 0.5\n"));
        assert!(out.contains("#define SAMPLES 8 // count\n"));
        // not an adjustment: left alone
        assert!(out.contains("#define CURVE 13.0, 11.0\n"));
        // out of range is clamped
        let out = with_values(&s, &toml::from_str("BLOOM_STRENGTH = 9.0").unwrap());
        assert!(out.contains("#define BLOOM_STRENGTH 1.0\n"));
    }

    #[test]
    fn literals_never_use_a_comma_and_keep_float_syntax() {
        assert_eq!(literal(4.0, false), "4.0");
        assert_eq!(literal(0.125, false), "0.125");
        assert_eq!(literal(-2.5, false), "-2.5");
        assert_eq!(literal(3.4, true), "3");
    }

    #[test]
    fn builtin_shaders_have_adjustments() {
        for s in builtin() {
            assert!(!s.params.is_empty(), "{} has no adjustments", s.key);
        }
    }

    #[test]
    fn folder_reads_files_and_folders_and_says_why_others_fail() {
        let dir = std::env::temp_dir().join(format!("miniutils-shaders-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("scene")).unwrap();
        std::fs::write(dir.join("soft.glsl"), SAMPLE).unwrap();
        std::fs::write(dir.join("soft.toml"), "name = 'Soft'\nlicense = 'CC0'\n").unwrap();
        std::fs::write(dir.join("scene/shader.glsl"), SAMPLE).unwrap();
        std::fs::write(dir.join("Bad Name.glsl"), SAMPLE).unwrap();
        std::fs::write(dir.join("crt.glsl"), SAMPLE).unwrap();
        std::fs::write(dir.join("empty.glsl"), "void main() {}").unwrap();
        let (shaders, skipped) = load(&dir);
        let keys: Vec<&str> = shaders.iter().map(|s| s.key.as_str()).collect();
        assert_eq!(keys, ["scene", "soft"]);
        assert_eq!((shaders[1].name.as_str(), shaders[1].license.as_str()), ("Soft", "CC0"));
        let mut bad: Vec<&str> = skipped.iter().map(|s| s.key.as_str()).collect();
        bad.sort();
        assert_eq!(bad, ["Bad Name", "crt", "empty"]);
        let before = stamp(&dir);
        std::fs::write(dir.join("soft.glsl"), format!("{SAMPLE}\n// edited")).unwrap();
        assert_ne!(before, stamp(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn copies_get_free_keys() {
        assert_eq!(free_key("crt", &[]), "crt-copy");
        assert_eq!(free_key("crt", &["crt-copy".into(), "crt-copy-2".into()]), "crt-copy-3");
    }
}
