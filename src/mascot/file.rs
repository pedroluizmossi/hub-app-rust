//! Sprites from files: pixel mascots of 16 × 14, the same kind as the built-in ones
//! (`sprites.rs`), read from `sprites/<key>.toml` in the appearance folder
//! (`skin::user::dir`). Unlike an imported PNG (`pack.rs`), they follow the app theme
//! (color roles) or bring their own palette, blink, have the LED and may bring animations.
//! The store installs them too (`store/mascots.rs`).
//!
//! ```toml
//! label = "Night owl"        # name in the pickers (default: the key)
//! # 14 rows of 16 characters, with the color roles of `sprites.rs`:
//! # . transparent  K outline  P main  S shadow  H highlight  M light detail  C visor
//! # E eye  W eye glint  B blush  T mouth  F skin  D hardware  A LED (follows the mood)
//! grid = """
//! ................
//! ...
//! """
//! eye_bg = "P"               # what shows where the eye closes (default: P; "." with [palette])
//! shade_from = 11            # row from which P becomes shadow (optional)
//! fur = "#3a8f3a"            # fixed color of P, ignoring the theme (optional); ear = of M
//!
//! # or a "pixel" mascot, with its own colors: each lowercase letter is a color and `e` is
//! # the eye (it follows the mood). Then the grid uses these letters instead of the roles.
//! [palette]
//! eye = "#222222"            # the eye in a calm mood
//! k = "#1b1b2a"
//! p = "#ff9ec4"
//!
//! [anim.idle]                # animations, in the format of src/mascot/clips/*.toml
//! fps = 2
//! frames = [ ... ]
//! [anim.walk]                # with `walk` it walks as the subagents (`run`, `jump` too)
//! frames = [ ... ]
//!
//! # top-level, before the tables: where the walking subagents stop besides idle
//! extras = ["wave"]
//! ```
//!
//! An animation that isn't a mood, a reaction, a way of moving nor in `extras` is a free
//! action: it only plays when an app asks for it.
//!
//! The key is the file name. A file with an error is left out, with the reason (Settings ›
//! Appearance and the log); the errors inside `[anim]` come from `clips.rs`, in English.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;

use serde::Deserialize;

use super::clips::{self, Clips, ROLES};
use super::sprites::{self, Pixel, Sprite};
use crate::skin::color::parse_hex;
use crate::tr;

/// Folder of the sprite files, inside the appearance folder.
pub const DIR: &str = "sprites";
/// Biggest sprite file read (a sprite with a few animations has a few KB).
pub const MAX_BYTES: u64 = 64 * 1024;
const W: usize = 16;
const H: usize = 14;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    label: Option<String>,
    grid: String,
    eye_bg: Option<String>,
    shade_from: Option<usize>,
    fur: Option<String>,
    ear: Option<String>,
    palette: Option<BTreeMap<String, String>>,
    /// Stops of the walking subagents besides `idle` (names from `[anim]`).
    extras: Option<Vec<String>>,
    #[serde(default)]
    anim: toml::Table,
}

/// A sprite read from a file, not yet in the program (see `set`).
pub struct FileSprite {
    pub sprite: Sprite,
    pub clips: Option<Clips>,
    /// `extras`: animations the walking subagents also do at their stops.
    pub extras: Vec<String>,
}

/// A file that did not get in, and why.
#[derive(Debug, Clone, PartialEq)]
pub struct Skipped {
    /// `sprites/owl.toml`
    pub file: String,
    pub reason: String,
}

/// Valid key (the file name): lowercase letters, digits, `-` and `_`, up to 32, not a
/// built-in mascot.
pub fn check_key(key: &str) -> Result<(), String> {
    let ok = !key.is_empty()
        && key.len() <= 32
        && key.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
    if !ok {
        return Err(tr!("the file name (the key) may only have a-z, 0-9, - and _ (up to 32)").into());
    }
    if sprites::SPRITES.iter().any(|s| s.key == key) {
        return Err(tr!("there is already a built-in one called \"{}\": choose another name", key));
    }
    Ok(())
}

/// Reads one sprite file (`path`, key = file name), checking size, format and drawing.
pub fn read(path: &Path) -> Result<FileSprite, String> {
    let key = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    check_key(&key)?;
    let size = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if size > MAX_BYTES {
        return Err(tr!("file too large (64 KiB at most)").into());
    }
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    parse(&key, &text)
}

/// Turns a sprite file's text into a sprite (with its animations). The key must already
/// be valid (`check_key`).
pub fn parse(key: &str, text: &str) -> Result<FileSprite, String> {
    let raw: Raw = toml::from_str(text).map_err(|e| e.message().to_string())?;
    let label = raw.label.as_deref().unwrap_or(key).trim();
    if label.is_empty() || label.chars().count() > 40 {
        return Err(tr!("label: 1 to 40 characters").into());
    }
    let pixel = match &raw.palette {
        Some(p) => Some(palette(p)?),
        None => None,
    };
    if pixel.is_some() && (raw.fur.is_some() || raw.ear.is_some()) {
        return Err(tr!("fur and ear only apply to a mascot by color roles (without [palette])").into());
    }
    let color = |name: &str, v: &Option<String>| -> Result<Option<u32>, String> {
        match v {
            None => Ok(None),
            Some(s) => parse_hex(s).map(Some).ok_or_else(|| tr!("{}: expected a color \"#RRGGBB\"", name)),
        }
    };
    let (fur, ear) = (color("fur", &raw.fur)?, color("ear", &raw.ear)?);

    // the cells this mascot may use: the roles, or the palette letters plus `e` and `.`
    let valid = |c: u8| match &pixel {
        Some(px) => c == b'.' || c == b'e' || px.color(c).is_some(),
        None => ROLES.contains(&c),
    };
    let rows: Vec<&str> = raw.grid.lines().map(str::trim_end).filter(|l| !l.is_empty()).collect();
    if rows.len() != H {
        return Err(tr!("grid: {} rows, expected 14", rows.len()));
    }
    let mut grid: [&'static str; H] = [""; H];
    for (y, row) in rows.iter().enumerate() {
        if row.len() != W || !row.is_ascii() {
            return Err(tr!("grid: row {} has {} characters, expected 16", y + 1, row.chars().count()));
        }
        if let Some(bad) = row.bytes().find(|&c| !valid(c)) {
            return Err(if pixel.is_some() {
                tr!("grid: row {}: the letter '{}' has no color in [palette]", y + 1, bad as char)
            } else {
                tr!("grid: row {}: '{}' is not a color role (. K P S H M C E W B T F D A)", y + 1, bad as char)
            });
        }
        // static data: lives until the program ends (like the imported mascots)
        grid[y] = leak(row);
    }
    let eye = if pixel.is_some() { 'e' } else { 'E' };
    if !rows.iter().any(|r| r.contains(eye)) {
        return Err(tr!("grid: no eye ({} in this mascot)", eye));
    }
    let eye_bg = match raw.eye_bg.as_deref() {
        None => {
            if pixel.is_some() {
                b'.'
            } else {
                b'P'
            }
        }
        Some(s) if s.len() == 1 && valid(s.as_bytes()[0]) => s.as_bytes()[0],
        Some(_) => return Err(tr!("eye_bg: one color of this mascot (a role, or a letter of [palette])").into()),
    };
    let sprite = Sprite {
        key: leak(key),
        label: leak(label),
        eye_bg,
        shade_from: raw.shade_from.unwrap_or(usize::MAX),
        fur,
        ear,
        pixel,
        grid,
    };
    let clips = if raw.anim.is_empty() {
        None
    } else {
        // the same reader as the built-in animations (`clips.rs`), checked against this drawing
        let text = toml::to_string(&raw.anim).map_err(|e| e.to_string())?;
        Some(clips::parse(&sprite, &text).map_err(|e| tr!("anim: {}", e))?)
    };
    // the stops it lists must be animations of this file
    let extras = raw.extras.unwrap_or_default();
    if let Some(bad) = extras.iter().find(|e| !clips.as_ref().is_some_and(|c| c.contains_key(e.as_str()))) {
        return Err(tr!("extras: \"{}\" is not in [anim]", bad));
    }
    Ok(FileSprite { sprite, clips, extras })
}

/// `[palette]`: `eye` and one color per lowercase letter (not `e`, which is the eye).
fn palette(p: &BTreeMap<String, String>) -> Result<Pixel, String> {
    let mut eye = None;
    let mut pal = Vec::new();
    for (k, v) in p {
        let c = parse_hex(v).ok_or_else(|| tr!("{}: expected a color \"#RRGGBB\"", format!("palette.{k}")))?;
        if k == "eye" {
            eye = Some(c);
            continue;
        }
        let b = k.as_bytes();
        if b.len() != 1 || !b[0].is_ascii_lowercase() || b[0] == b'e' {
            return Err(tr!("[palette]: \"{}\" is not a lowercase letter (e is the eye)", k));
        }
        pal.push((b[0], c));
    }
    let eye = eye.ok_or(tr!("[palette]: eye = \"#RRGGBB\" is missing"))?;
    Ok(Pixel { eye, pal: Box::leak(pal.into_boxed_slice()) })
}

fn leak(s: &str) -> &'static str {
    Box::leak(s.to_string().into_boxed_str())
}

/// Reads `root` (the `sprites/` folder) without touching the program. `taken` are keys
/// already in use (the imported PNG mascots): a file with one of them stays out.
pub fn load(root: &Path, taken: &[String]) -> (Vec<FileSprite>, Vec<Skipped>) {
    let (mut out, mut skipped) = (Vec::new(), Vec::new());
    let Ok(entries) = std::fs::read_dir(root) else { return (out, skipped) };
    let mut files: Vec<_> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "toml"))
        .collect();
    files.sort();
    for path in files {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let key = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let result = if taken.contains(&key) {
            Err(tr!("the imported mascot {}/{} already uses this key", super::pack::DIR, key))
        } else {
            read(&path)
        };
        match result {
            Ok(s) => out.push(s),
            Err(reason) => skipped.push(Skipped { file: format!("{DIR}/{name}"), reason }),
        }
    }
    (out, skipped)
}

/// Replaces the program's sprites from files (and their animations). Each reload leaks the
/// previous ones (the screens hold `&'static Sprite`); reloading is rare and manual.
pub fn set(list: Vec<FileSprite>) {
    let mut sprites = Vec::new();
    let mut lib = HashMap::new();
    let mut extras = HashMap::new();
    for f in list {
        let sprite: &'static Sprite = Box::leak(Box::new(f.sprite));
        if let Some(clips) = f.clips {
            lib.insert(sprite.key.to_string(), Arc::new(clips));
        }
        if !f.extras.is_empty() {
            extras.insert(sprite.key.to_string(), f.extras);
        }
        sprites.push(sprite);
    }
    if let Ok(mut e) = EXTRAS.write() {
        *e = Some(extras);
    }
    clips::set_extra(lib);
    sprites::set_files(sprites);
}

/// `extras` of each sprite file, by key (see `set`).
static EXTRAS: std::sync::RwLock<Option<HashMap<String, Vec<String>>>> = std::sync::RwLock::new(None);

/// The stops the walking subagents do besides `idle` (`extras` of the file; empty for a
/// built-in mascot or one that lists none).
pub fn extras(key: &str) -> Vec<String> {
    EXTRAS.read().ok().and_then(|e| e.as_ref()?.get(key).cloned()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROLES_GRID: &str = r#"
grid = """
................
....KKKKKKKK....
...KPPPPPPPPK...
..KPPPPPPPPPPK..
..KPCCCCCCCCPK..
..KPCEWCCEWCPK..
..KPCEECCEECPK..
..KPCCCCCCCCPK..
..KPPPPTTPPPPK..
..KPPPPPPPPPPK..
...KPPPPPPPPK...
....KKKKKKKK....
................
................
"""
"#;

    #[test]
    fn reads_a_mascot_by_color_roles() {
        let f = parse("blob", &format!("label = \"Blob\"\nfur = \"#3a8f3a\"\n{ROLES_GRID}")).unwrap();
        assert_eq!(f.sprite.key, "blob");
        assert_eq!(f.sprite.label, "Blob");
        assert_eq!(f.sprite.fur, Some(0x3a8f3a));
        assert_eq!(f.sprite.eye_bg, b'P');
        assert!(f.sprite.pixel.is_none() && f.clips.is_none());
        assert_eq!(f.sprite.grid[5], "..KPCEWCCEWCPK..");
        // the drawing goes through the same renderer as the built-in ones
        let img = crate::mascot::render::image(&f.sprite, crate::mascot::themes::get("neon"), crate::mascot::Mood::Idle, 1);
        assert!(img.size().width > 0);
    }

    #[test]
    fn reads_a_pixel_mascot_with_animation() {
        let grid = ROLES_GRID.replace('K', "k").replace(['P', 'C', 'T'], "p").replace(['E', 'W'], "e");
        let rows: Vec<&str> = grid.lines().filter(|l| l.len() == 16).collect();
        let frame = crate::mascot::clips::tests::frame(&rows);
        let src = format!("{grid}\n[palette]\neye = \"#222222\"\nk = \"#111111\"\np = \"#ff9ec4\"\n\n[anim.idle]\nfps = 2\nframes = [{frame}, {frame}]\n");
        let f = parse("pinky", &src).unwrap();
        let px = f.sprite.pixel.as_ref().unwrap();
        assert_eq!((px.eye, px.color(b'p')), (0x222222, Some(0xff9ec4)));
        assert_eq!(f.sprite.eye_bg, b'.');
        assert_eq!(f.clips.unwrap()["idle"].frames.len(), 2);
    }

    #[test]
    fn rejects_bad_files() {
        let err = |src: &str| parse("x", src).err().expect("should be refused");
        assert!(err("grid = \"KKKK\"").contains("1 rows"));
        assert!(err(&ROLES_GRID.replacen("KKKKKKKK", "KKKKKKKZ", 1)).contains("'Z'"));
        assert!(err(&ROLES_GRID.replace(['E', 'W'], "C")).contains("no eye"));
        assert!(err(&format!("{ROLES_GRID}\n[palette]\neye = \"#000000\"\n")).contains("[palette]"));
        assert!(err(&format!("{ROLES_GRID}\n[palette]\nK = \"#000000\"\n")).contains("\"K\""));
        assert!(err(&format!("eye_bg = \"Z\"\n{ROLES_GRID}")).contains("eye_bg"));
        assert!(err(&format!("fur = \"green\"\n{ROLES_GRID}")).contains("fur"));
        assert!(err(&format!("nose = 1\n{ROLES_GRID}")).contains("nose"));
        // an animation that does not fit the drawing
        assert!(err(&format!("{ROLES_GRID}\n[anim.idle]\nframes = [\"\"\"\nKK\n\"\"\"]\n")).starts_with("anim:"));
        // keys
        assert!(check_key("neko").unwrap_err().contains("neko"));
        assert!(check_key("Owl").is_err());
        assert!(check_key("night-owl_2").is_ok());
    }

    #[test]
    fn loads_the_folder_and_skips_taken_keys() {
        let dir = std::env::temp_dir().join(format!("miniutils-sprites-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("blob.toml"), ROLES_GRID).unwrap();
        std::fs::write(dir.join("knight.toml"), ROLES_GRID).unwrap();
        std::fs::write(dir.join("broken.toml"), "grid = 1").unwrap();
        std::fs::write(dir.join("notes.txt"), "not a sprite").unwrap();
        let (ok, skipped) = load(&dir, &["knight".to_string()]);
        assert_eq!(ok.iter().map(|f| f.sprite.key).collect::<Vec<_>>(), ["blob"]);
        let files: Vec<&str> = skipped.iter().map(|s| s.file.as_str()).collect();
        assert_eq!(files, ["sprites/broken.toml", "sprites/knight.toml"]);
        assert!(skipped[1].reason.contains("mascots/knight"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
