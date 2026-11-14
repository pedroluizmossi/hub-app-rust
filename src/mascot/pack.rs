//! Imported mascots: PNG sheets of any size (16×16, 32×32, 64×64…), read from the user's
//! folder, without recompiling and without going into the repository.
//!
//! ```text
//! <data>/miniutils/mascots/<key>/          Linux: ~/.local/share/miniutils/mascots
//! ├── sprite.toml                          optional (without it, deduced from the names)
//! ├── Pink_Monster_Idle_4.png              one sheet per animation, frames side by side
//! └── Pink_Monster_Walk_6.png
//! ```
//!
//! ```toml
//! name = "Pink Monster"
//! fps = 8                     # default for every animation
//! facing = "right"            # where the drawing looks (walking the other way mirrors it)
//! trim = true                 # trims the transparent border (the same cut on everything)
//! # walking subagents: what they do at their stops (default: everything but death/hurt)
//! extras = ["idle", "attack1", "push", "throw"]
//! [anim]
//! idle = "Pink_Monster_Idle_4.png"                      # frames from the _4 in the name
//! run = { file = "Pink_Monster_Run_6.png", fps = 12 }   # or frames = 6
//! [moods]                     # mood → animation (without it: usual names, see MOOD_NAMES)
//! busy = "run"
//! [reactions]                 # celebrate, surprised
//! celebrate = "jump"
//! ```
//!
//! Without `sprite.toml`, each `<Name>_<Animation>_<N>.png` becomes the animation
//! `<animation>` with N frames (the prefix with the most animations is the mascot's; the
//! rest, like dust and rocks, is left out). Without the `_N`, frames are square (width =
//! sheet height). Animation names come from the user's files, so they are matched as they
//! are (`idle`, `walk`, `run`…: the usual names of free sprite packs).
//!
//! The imported mascot becomes a `Sprite` registered in `sprites` (key = folder name) and
//! shows up everywhere mascots are shown. The drawing is the PNG's: it doesn't follow the
//! theme nor blink; the mood shows through the animation chosen for it, and asleep it gets
//! slower and dimmed. Only PNG and TOML are read: nothing in the folder is executed.
//!
//! With a walking animation (`walk` or `run`), it also walks as the subagents of the agents
//! app (`Troupe`, choreography in `stroll`): walks, stops doing the pack's other
//! animations, runs (`run`) while calling tools and jumps (`jump`) when done.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use serde::Deserialize;
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

use super::Mood;
use super::anim::{self, Look, Reaction};
use super::sprites::{self, Sprite};
use super::stroll::Script;
use crate::tr;

/// Subfolder of the appearance folder (`skin::user::dir`).
pub const DIR: &str = "mascots";
const SPEC: &str = "sprite.toml";
/// Limits against odd files: frame, frames per animation, animations, file.
const MAX_SIDE: usize = 256;
const MAX_FRAMES: usize = 64;
const MAX_ANIMS: usize = 32;
const MAX_FILE: u64 = 4 << 20;
const DEFAULT_FPS: u32 = 8;
/// Names looked for for each mood (in `Mood` order), when `sprite.toml` doesn't say.
const MOOD_NAMES: [&[&str]; 5] = [
    &["sleep", "idle"],
    &["idle"],
    &["happy", "idle"],
    &["busy", "run", "walk", "idle"],
    &["alarm", "hurt", "idle"],
];
/// Walking animation of the subagents; without any of these, the mascot doesn't walk.
const WALK_NAMES: &[&str] = &["walk", "run"];
/// Jump for when a subagent finishes.
const JUMP_NAMES: &[&str] = &["jump", "celebrate"];
/// Left out of the subagents' stops (without `extras` in `sprite.toml`): dying and getting
/// hurt don't suit someone who is working.
const GRIM: &[&str] = &["death", "dead", "die", "hurt", "hit"];
/// Reactions (`Reaction`): celebrate, surprise.
const REACTION_NAMES: [&[&str]; 2] = [&["celebrate", "jump"], &["surprised", "hurt"]];

pub type Buf = SharedPixelBuffer<Rgba8Pixel>;
/// One RGBA frame, row by row.
type Pixels = Vec<[u8; 4]>;

/// An animation: the frames already cut, and dimmed (for sleeping).
pub struct Anim {
    pub name: String,
    frames: Vec<Buf>,
    dim: Vec<Buf>,
    /// How long each frame stays on screen.
    pub ms: u32,
}

/// An imported mascot.
pub struct Art {
    pub key: String,
    pub name: String,
    /// Frame size after trimming.
    pub w: usize,
    pub h: usize,
    anims: Vec<Anim>,
    /// Animation of each mood, in `Mood` order.
    moods: [usize; 5],
    reactions: [Option<usize>; 2],
    /// Walking as the subagents (`None` = no walking animation).
    troupe: Option<Troupe>,
}

/// A mascot's animations for walking as the subagents, with their own cut (a wide attack
/// doesn't shrink the header mascot), and their script (`stroll::Script`).
pub struct Troupe {
    clips: Vec<(Vec<Buf>, Vec<Buf>)>,
    faces_left: bool,
    pub script: Script,
}

impl Troupe {
    /// The frames of animation `clip` facing right and facing left.
    pub fn frames(&self, clip: usize) -> (&[Buf], &[Buf]) {
        let (plain, mirrored) = &self.clips[clip];
        if self.faces_left { (mirrored, plain) } else { (plain, mirrored) }
    }
}

/// The chosen frame: animation, frame, dimmed. Lets the caller redraw only on change.
pub type FrameKey = (usize, usize, bool);

impl Art {
    fn anim_index(&self, look: &Look) -> usize {
        let reaction = look.reaction.and_then(|r| match r {
            Reaction::Celebrate => self.reactions[0],
            Reaction::Surprised => self.reactions[1],
        });
        reaction.unwrap_or(self.moods[look.mood as usize])
    }

    /// Which frame to show now. Asleep, the animation runs at half speed and dimmed; with
    /// "reduce motion", it stays on the first frame.
    pub fn pick(&self, look: &Look) -> FrameKey {
        let i = self.anim_index(look);
        let a = &self.anims[i];
        let asleep = look.mood == Mood::Sleep && look.reaction.is_none();
        let ms = if asleep { a.ms * 2 } else { a.ms }.max(1) as u64;
        let f = if anim::calm() { 0 } else { (look.ms / ms) as usize % a.frames.len() };
        (i, f, asleep)
    }

    fn buf(&self, (a, f, dim): FrameKey) -> &Buf {
        let a = &self.anims[a];
        if dim { &a.dim[f] } else { &a.frames[f] }
    }

    pub fn image(&self, key: FrameKey) -> Image {
        Image::from_rgba8(self.buf(key).clone())
    }

    /// Shortest frame (ms) of the current animation, if it moves: the hub speeds up its timer.
    pub fn frame_ms(&self, look: &Look) -> Option<u32> {
        let a = &self.anims[self.anim_index(look)];
        (a.frames.len() > 1).then_some(a.ms)
    }

    /// Walking as the subagents (`None` = the mascot doesn't walk).
    pub fn troupe(&self) -> Option<&Troupe> {
        self.troupe.as_ref()
    }

    /// Frame centered in a square of side `side` or more (scaled up by a whole factor until
    /// close to `side`), in RGBA: the side and the pixels. Base of the tray icon.
    pub fn square(&self, key: FrameKey, side: usize) -> (i32, Vec<u8>) {
        let big = self.w.max(self.h);
        let k = (side / big).max(1);
        let side = big * k;
        let (ox, oy) = ((side - self.w * k) / 2, (side - self.h * k) / 2);
        let src = self.buf(key).as_bytes();
        let mut out = vec![0u8; side * side * 4];
        for y in 0..self.h * k {
            for x in 0..self.w * k {
                let s = ((y / k) * self.w + x / k) * 4;
                let d = ((y + oy) * side + x + ox) * 4;
                out[d..d + 4].copy_from_slice(&src[s..s + 4]);
            }
        }
        (side as i32, out)
    }
}

// ---- registry -------------------------------------------------------------------

/// Imported mascots and the `Sprite` of each. Each reload leaks the previous ones (the
/// screens hold `&'static Sprite`); reloading is rare and manual.
static ARTS: RwLock<Vec<(&'static Sprite, &'static Art)>> = RwLock::new(Vec::new());

/// The imported mascot behind `sprite` (by address: a key equal to an app mascot's, like
/// "claude", doesn't get mixed up).
pub fn art(sprite: &Sprite) -> Option<&'static Art> {
    let list = ARTS.read().ok()?;
    list.iter().find(|(s, _)| std::ptr::eq(*s, sprite)).map(|(_, a)| *a)
}

/// The imported mascots that can walk, in key order.
pub fn walkers() -> Vec<&'static Art> {
    ARTS.read().map(|l| l.iter().map(|(_, a)| *a).filter(|a| a.troupe.is_some()).collect()).unwrap_or_default()
}

/// Replaces the program's imported mascots.
pub fn set(arts: Vec<Art>) {
    let mut list = Vec::new();
    for art in arts {
        let art: &'static Art = Box::leak(Box::new(art));
        let sprite: &'static Sprite = Box::leak(Box::new(Sprite {
            key: Box::leak(art.key.clone().into_boxed_str()),
            label: Box::leak(art.name.clone().into_boxed_str()),
            eye_bg: b'.',
            shade_from: usize::MAX,
            fur: None,
            ear: None,
            pixel: None,
            grid: [EMPTY_ROW; 14],
        }));
        list.push((sprite, art));
    }
    sprites::set_extra(list.iter().map(|(s, _)| *s).collect());
    if let Ok(mut l) = ARTS.write() {
        *l = list;
    }
}

const EMPTY_ROW: &str = "................";

/// A folder that didn't make it in, and why.
#[derive(Debug, Clone, PartialEq)]
pub struct Skipped {
    /// `mascots/pink-monster`
    pub dir: String,
    /// Shown to the user (translated).
    pub reason: String,
}

/// Reads `root` (the `mascots/` folder) without
/// touching the program.
pub fn load(root: &Path) -> (Vec<Art>, Vec<Skipped>) {
    let (mut arts, mut skipped) = (Vec::new(), Vec::new());
    let Ok(entries) = std::fs::read_dir(root) else { return (arts, skipped) };
    let folder = root.file_name().and_then(|n| n.to_str()).unwrap_or(DIR).to_string();
    let mut dirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    dirs.sort();
    for dir in dirs {
        let key = dir.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string();
        let shown = format!("{folder}/{key}");
        match load_pack(&dir, &key) {
            Ok(a) => arts.push(a),
            Err(reason) => skipped.push(Skipped { dir: shown, reason }),
        }
    }
    (arts, skipped)
}

/// Valid key: lowercase letters, digits and hyphens, not repeating a built-in mascot.
fn check_key(key: &str) -> Result<(), String> {
    let ok = !key.is_empty()
        && key.len() <= 32
        && key.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if !ok {
        return Err(tr!("the folder name becomes the mascot's key: use only lowercase letters, digits and hyphens (up to 32)").into());
    }
    if sprites::SPRITES.iter().any(|s| s.key == key) {
        return Err(tr!("\"{}\" is already a built-in mascot; rename the folder", key));
    }
    Ok(())
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Spec {
    name: Option<String>,
    fps: Option<u32>,
    facing: Option<String>,
    trim: Option<bool>,
    #[serde(default)]
    anim: BTreeMap<String, AnimSpec>,
    #[serde(default)]
    moods: BTreeMap<String, String>,
    #[serde(default)]
    reactions: BTreeMap<String, String>,
    /// Stops of the walking subagents (names from `[anim]`).
    extras: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum AnimSpec {
    File(String),
    Full {
        file: String,
        frames: Option<usize>,
        fps: Option<u32>,
    },
}

impl AnimSpec {
    fn file(&self) -> &str {
        match self {
            AnimSpec::File(f) | AnimSpec::Full { file: f, .. } => f,
        }
    }
}

fn load_pack(dir: &Path, key: &str) -> Result<Art, String> {
    check_key(key)?;
    let spec_path = dir.join(SPEC);
    let spec: Spec = if spec_path.exists() {
        let text = read_limited(&spec_path)?;
        toml::from_str(&String::from_utf8_lossy(&text)).map_err(|e| format!("{SPEC}: {e}"))?
    } else {
        infer(dir)?
    };
    if spec.anim.is_empty() {
        return Err(tr!("no animations (no sprite.toml, and no PNG named like Name_Animation_N.png)").into());
    }
    if spec.anim.len() > MAX_ANIMS {
        return Err(tr!("too many animations ({}; up to {})", spec.anim.len(), MAX_ANIMS));
    }
    let fps = spec.fps.unwrap_or(DEFAULT_FPS);
    let faces_left = match spec.facing.as_deref() {
        None | Some("right") => false,
        Some("left") => true,
        Some(o) => return Err(tr!("facing = \"{}\": use \"right\" or \"left\"", o)),
    };

    // which animation each mood, reaction and the walk use (by name); only those are read:
    // attacks and climbs nobody uses don't widen the cut nor take memory
    let names: Vec<&str> = spec.anim.keys().map(String::as_str).collect();
    let find = |name: &str| names.iter().position(|n| *n == name);
    let pick = |cands: &[&str]| cands.iter().find_map(|n| find(n));
    let missing = |what: &str, name: &str| tr!("{} = \"{}\": there is no such animation", what, name);
    let mut moods = [0usize; 5];
    for (i, cands) in MOOD_NAMES.iter().enumerate() {
        moods[i] = pick(cands).unwrap_or(0);
    }
    for (mood, name) in &spec.moods {
        let i = super::clips::MOODS.iter().position(|m| m == mood).ok_or_else(|| tr!("[moods] unknown mood: {}", mood))?;
        moods[i] = find(name).ok_or_else(|| missing(&format!("[moods] {mood}"), name))?;
    }
    let mut reactions = [pick(REACTION_NAMES[0]), pick(REACTION_NAMES[1])];
    for (r, name) in &spec.reactions {
        let i = ["celebrate", "surprised"].iter().position(|x| x == r).ok_or_else(|| tr!("[reactions] unknown reaction: {}", r))?;
        reactions[i] = Some(find(name).ok_or_else(|| missing(&format!("[reactions] {r}"), name))?);
    }
    let mut used: Vec<usize> = moods.iter().copied().chain(reactions.iter().flatten().copied()).collect();
    used.sort();
    used.dedup();
    // the walking subagents: walk, run, jump, idle and the stops
    let walk = pick(WALK_NAMES);
    let mut cast: Vec<usize> = Vec::new();
    if let Some(walk) = walk {
        cast.push(walk);
        cast.extend(find("run"));
        cast.extend(pick(JUMP_NAMES));
        cast.extend(find("idle"));
        match &spec.extras {
            Some(list) => {
                for name in list {
                    cast.push(find(name).ok_or_else(|| missing("extras", name))?);
                }
            }
            None => cast.extend((0..names.len()).filter(|&i| !GRIM.iter().any(|g| names[i].starts_with(g)))),
        }
        cast.sort();
        cast.dedup();
    }
    let mut needed: Vec<usize> = used.iter().chain(&cast).copied().collect();
    needed.sort();
    needed.dedup();

    // sheets cut into frames, all the same size: index → (frames, ms)
    let mut raw: BTreeMap<usize, (Vec<Pixels>, u32)> = BTreeMap::new();
    let mut size: Option<(usize, usize)> = None;
    for &i in &needed {
        let (name, a) = (names[i], &spec.anim[names[i]]);
        let file = a.file();
        if file.contains('/') || file.contains('\\') || file.starts_with('.') {
            return Err(tr!("[anim] {}: \"{}\" must be a file in this folder", name, file));
        }
        let (w, h, px) = read_png(&dir.join(file))?;
        let n = match a {
            AnimSpec::Full { frames: Some(n), .. } => *n,
            _ => frames_in_name(file).unwrap_or(w.checked_div(h).unwrap_or(0)),
        };
        if n == 0 || n > MAX_FRAMES || !w.is_multiple_of(n) {
            return Err(tr!("{}: {}×{} doesn't split into {} equal frames (up to {})", file, w, h, n, MAX_FRAMES));
        }
        let fw = w / n;
        if fw > MAX_SIDE || h > MAX_SIDE {
            return Err(tr!("{}: {}×{} frame; the maximum is {}×{}", file, fw, h, MAX_SIDE, MAX_SIDE));
        }
        match size {
            None => size = Some((fw, h)),
            Some(s) if s != (fw, h) => {
                return Err(tr!("{}: {}×{} frame, but the other animations are {}×{}", file, fw, h, s.0, s.1));
            }
            _ => {}
        }
        let frames = (0..n).map(|i| (0..h).flat_map(|y| px[y * w + i * fw..y * w + (i + 1) * fw].to_vec()).collect()).collect();
        let afps = match a {
            AnimSpec::Full { fps: Some(f), .. } => *f,
            _ => fps,
        };
        if afps == 0 || afps > 30 {
            return Err(tr!("[anim] {}: fps {} (use 1 to 30)", name, afps));
        }
        raw.insert(i, (frames, 1000 / afps));
    }
    let (fw, fh) = size.unwrap_or_default();
    let trim = spec.trim.unwrap_or(true);
    // the same cut on every frame of a group: the mascot doesn't jump when switching animations
    let cut = |group: &[usize]| -> Result<(usize, usize, usize, usize), String> {
        if !trim {
            return Ok((0, 0, fw, fh));
        }
        bbox(group.iter().flat_map(|i| &raw[i].0), fw, fh).ok_or_else(|| tr!("the frames are all transparent").to_string())
    };
    let crop = |frames: &[Pixels], (x0, y0, x1, y1): (usize, usize, usize, usize)| -> Vec<Pixels> {
        frames.iter().map(|f| (y0..y1).flat_map(|y| f[y * fw + x0..y * fw + x1].to_vec()).collect()).collect()
    };
    let mirror = |f: &Pixels, w: usize, h: usize| -> Pixels {
        (0..h).flat_map(|y| f[y * w..(y + 1) * w].iter().rev().copied().collect::<Vec<_>>()).collect()
    };

    let core = cut(&used)?;
    let (w, h) = (core.2 - core.0, core.3 - core.1);
    let anims: Vec<Anim> = used
        .iter()
        .map(|i| {
            let (frames, ms) = &raw[i];
            let crop = crop(frames, core);
            let dim = |f: &Pixels| -> Pixels { f.iter().map(|p| [dim8(p[0]), dim8(p[1]), dim8(p[2]), p[3]]).collect() };
            Anim {
                name: names[*i].to_string(),
                frames: crop.iter().map(|f| to_buf(f, w, h)).collect(),
                dim: crop.iter().map(|f| to_buf(&dim(f), w, h)).collect(),
                ms: *ms,
            }
        })
        .collect();

    let troupe = match walk {
        None => None,
        Some(walk) => {
            let b = cut(&cast)?;
            let (tw, th) = (b.2 - b.0, b.3 - b.1);
            let clips = cast
                .iter()
                .map(|i| {
                    let crop = crop(&raw[i].0, b);
                    let plain = crop.iter().map(|f| to_buf(f, tw, th)).collect();
                    let mirrored = crop.iter().map(|f| to_buf(&mirror(f, tw, th), tw, th)).collect();
                    (plain, mirrored)
                })
                .collect();
            let at = |i: usize| cast.iter().position(|c| *c == i);
            let jump = pick(JUMP_NAMES);
            let run = find("run").filter(|r| *r != walk);
            let moving = |i: usize| names[i].contains("walk") || names[i].contains("run");
            let script = Script {
                names: cast.iter().map(|i| names[*i].to_string()).collect(),
                frames: cast.iter().map(|i| raw[i].0.len()).collect(),
                ms: cast.iter().map(|i| raw[i].1).collect(),
                walk: at(walk).unwrap_or(0),
                run: run.and_then(at),
                jump: jump.and_then(at),
                idle: find("idle").and_then(at),
                strolls: cast.iter().copied().filter(|&i| moving(i) && i != walk && Some(i) != run).filter_map(at).collect(),
                // the jump is kept for the end (unless sprite.toml puts it among the stops)
                extras: cast
                    .iter()
                    .copied()
                    .filter(|&i| !moving(i) && (Some(i) != jump || spec.extras.as_ref().is_some_and(|l| l.iter().any(|n| n == names[i]))))
                    .filter_map(at)
                    .collect(),
            };
            Some(Troupe { clips, faces_left, script })
        }
    };

    // indices into `used` (only the animations that were read)
    let at = |i: usize| used.iter().position(|u| *u == i).unwrap_or(0);
    let moods = moods.map(at);
    let reactions = reactions.map(|r| r.map(at));
    let name = spec.name.clone().unwrap_or_else(|| key.replace('-', " "));
    let list: Vec<&str> = anims.iter().map(|a| a.name.as_str()).collect();
    let walking = troupe.as_ref().map(|t| t.script.names.join(", ")).unwrap_or_else(|| "doesn't walk".into());
    log::debug!("mascot {key}: {w}×{h}, animations {}; subagents: {walking}", list.join(", "));
    Ok(Art { key: key.to_string(), name, w, h, anims, moods, reactions, troupe })
}

/// `Pink_Monster_Walk_6.png` → 6.
fn frames_in_name(file: &str) -> Option<usize> {
    let stem = file.strip_suffix(".png").or_else(|| file.strip_suffix(".PNG"))?;
    stem.rsplit_once('_')?.1.parse().ok()
}

/// `Pink_Monster_Walk_6` → ("Pink_Monster", "walk", 6).
fn parse_sheet_name(stem: &str) -> Option<(&str, String, usize)> {
    let (rest, n) = stem.rsplit_once('_')?;
    let n: usize = n.parse().ok()?;
    let (prefix, anim) = rest.rsplit_once('_')?;
    (!prefix.is_empty() && !anim.is_empty()).then(|| (prefix, anim.to_lowercase(), n))
}

/// Without `sprite.toml`: the `<Name>_<Animation>_<N>.png` sheets of the most common prefix.
fn infer(dir: &Path) -> Result<Spec, String> {
    let mut by_prefix: BTreeMap<String, BTreeMap<String, AnimSpec>> = BTreeMap::new();
    let entries = std::fs::read_dir(dir).map_err(|e| e.to_string())?;
    for path in entries.flatten().map(|e| e.path()) {
        let Some(file) = path.file_name().and_then(|n| n.to_str()) else { continue };
        let Some(stem) = file.strip_suffix(".png").or_else(|| file.strip_suffix(".PNG")) else { continue };
        if let Some((prefix, anim, _)) = parse_sheet_name(stem) {
            by_prefix.entry(prefix.to_string()).or_default().insert(anim, AnimSpec::File(file.to_string()));
        }
    }
    let Some((prefix, anim)) = by_prefix.into_iter().max_by_key(|(_, a)| a.len()) else {
        return Ok(Spec::default());
    };
    Ok(Spec { name: Some(prefix.replace('_', " ")), anim, ..Spec::default() })
}

/// The smallest rectangle with some visible pixel across all frames.
fn bbox<'a>(frames: impl Iterator<Item = &'a Pixels>, w: usize, h: usize) -> Option<(usize, usize, usize, usize)> {
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
    for f in frames {
        for y in 0..h {
            for x in 0..w {
                if f[y * w + x][3] > 0 {
                    (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1));
                }
            }
        }
    }
    (x1 > x0 && y1 > y0).then_some((x0, y0, x1, y1))
}

/// Dimmed channel (asleep): ~55% of the brightness.
fn dim8(c: u8) -> u8 {
    (c as u16 * 11 / 20) as u8
}

fn to_buf(px: &[[u8; 4]], w: usize, h: usize) -> Buf {
    let mut buf = Buf::new(w as u32, h as u32);
    for (d, s) in buf.make_mut_slice().iter_mut().zip(px) {
        *d = Rgba8Pixel { r: s[0], g: s[1], b: s[2], a: s[3] };
    }
    buf
}

fn read_limited(path: &Path) -> Result<Vec<u8>, String> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let len = std::fs::metadata(path).map_err(|e| format!("{name}: {e}"))?.len();
    if len > MAX_FILE {
        return Err(tr!("{}: {} KiB; the maximum is {} KiB", name, len >> 10, MAX_FILE >> 10));
    }
    std::fs::read(path).map_err(|e| format!("{name}: {e}"))
}

/// PNG as RGBA: width, height and pixels.
fn read_png(path: &Path) -> Result<(usize, usize, Pixels), String> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    // decodes the bytes whose size was checked (no second read of the file)
    let mut decoder = png::Decoder::new(std::io::Cursor::new(read_limited(path)?));
    // indexed palette becomes RGB(A), 16 bits become 8, and transparency (tRNS) becomes alpha
    decoder.set_transformations(png::Transformations::normalize_to_color8() | png::Transformations::ALPHA);
    let mut reader = decoder.read_info().map_err(|e| format!("{name}: {e}"))?;
    let (w, h) = (reader.info().width as usize, reader.info().height as usize);
    if w > MAX_SIDE * MAX_FRAMES || h > MAX_SIDE {
        return Err(tr!("{}: {}×{} is too large", name, w, h));
    }
    let mut buf = vec![0; reader.output_buffer_size().ok_or_else(|| tr!("{}: too large", name))?];
    let info = reader.next_frame(&mut buf).map_err(|e| format!("{name}: {e}"))?;
    let data = &buf[..info.buffer_size()];
    let px = match info.color_type {
        png::ColorType::Rgba => data.as_chunks::<4>().0.iter().map(|c| [c[0], c[1], c[2], c[3]]).collect(),
        png::ColorType::Rgb => data.as_chunks::<3>().0.iter().map(|c| [c[0], c[1], c[2], 255]).collect(),
        png::ColorType::GrayscaleAlpha => data.as_chunks::<2>().0.iter().map(|c| [c[0], c[0], c[0], c[1]]).collect(),
        png::ColorType::Grayscale => data.iter().map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => return Err(tr!("{}: indexed PNG not converted", name)),
    };
    Ok((w, h, px))
}

// ---- import -------------------------------------------------------------------

/// `miniutils --import-mascot <folder>`: finds the `<Name>_<Animation>_<N>.png` sheets in
/// the folder and its subfolders (a pack usually has one per character), copies each
/// character's into `mascots/<name>/` and writes a `sprite.toml` to tweak. Doesn't overwrite
/// a mascot that already exists. Returns one line per character (shown to the user).
pub fn import(src: &Path, root: &Path) -> Result<Vec<String>, String> {
    let mut dirs = vec![src.to_path_buf()];
    if let Ok(entries) = std::fs::read_dir(src) {
        let mut subs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
        subs.sort();
        dirs.extend(subs);
    }
    let mut out = Vec::new();
    for dir in dirs {
        let spec = infer(&dir)?;
        let Some(name) = spec.name.clone().filter(|_| spec.anim.len() >= 2) else { continue };
        let key = slug(&name);
        let dest = root.join(&key);
        if dest.exists() {
            out.push(tr!("{}: already exists in {} (delete the folder to import it again)", key, dest.display()));
            continue;
        }
        check_key(&key)?;
        std::fs::create_dir_all(&dest).map_err(|e| format!("{}: {e}", dest.display()))?;
        // a config file the user edits: its comments are in English, like the format docs
        let mut toml = format!(
            "# Mascot imported from {}\n# Format: top of src/mascot/pack.rs (or DESIGN.md › Imported mascots).\nname = \"{name}\"\nfps = {DEFAULT_FPS}\nfacing = \"right\"\n# walking subagents: what they do at their stops (without this, everything but death/hurt)\n# extras = [\"idle\", \"attack1\", \"push\"]\n\n[anim]\n",
            dir.display().to_string().replace('"', "'")
        );
        for (anim, a) in &spec.anim {
            let file = a.file();
            std::fs::copy(dir.join(file), dest.join(file)).map_err(|e| format!("{file}: {e}"))?;
            let k = if anim.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') { anim.clone() } else { format!("\"{anim}\"") };
            toml.push_str(&format!("{k} = \"{file}\"\n"));
        }
        toml.push_str("\n# mood → animation (sleep, idle, happy, busy, alarm); without this, the usual names apply\n# [moods]\n# busy = \"run\"\n");
        std::fs::write(dest.join(SPEC), toml).map_err(|e| format!("{SPEC}: {e}"))?;
        // check right away: what was copied must load
        match load_pack(&dest, &key) {
            Ok(a) => out.push(tr!("{}: {} ({} animations, {}×{} frame after trimming)", key, a.name, a.anims.len(), a.w, a.h)),
            Err(e) => out.push(tr!("{}: copied, but it doesn't load: {}", key, e)),
        }
    }
    if out.is_empty() {
        return Err(tr!("{}: no sheet named like Name_Animation_N.png (at least two animations)", src.display()));
    }
    Ok(out)
}

/// "Pink Monster" → "pink-monster".
fn slug(name: &str) -> String {
    let mut s = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            s.push(c);
        } else if !s.ends_with('-') && !s.is_empty() {
            s.push('-');
        }
    }
    s.trim_end_matches('-').chars().take(32).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes a `w`×`h` RGBA PNG with `px`.
    fn png(path: &Path, w: usize, h: usize, px: &[[u8; 4]]) {
        let file = std::fs::File::create(path).unwrap();
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(px.as_flattened()).unwrap();
    }

    /// Sheet with `n` frames of `side`×`side`, each with an opaque 2×3 square at (3+i, 4).
    fn sheet(path: &Path, n: usize, side: usize) {
        let w = n * side;
        let mut px = vec![[0u8; 4]; w * side];
        for i in 0..n {
            for y in 4..7 {
                for x in 3 + i..5 + i {
                    px[y * w + i * side + x] = [200, 10 * i as u8, 30, 255];
                }
            }
        }
        png(path, w, side, &px);
    }

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("miniutils-pack-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn deduces_from_names_and_trims_the_border() {
        let d = temp("deduce");
        let p = d.join("hero");
        std::fs::create_dir_all(&p).unwrap();
        sheet(&p.join("Hero_Idle_4.png"), 4, 32);
        sheet(&p.join("Hero_Walk_6.png"), 6, 32);
        sheet(&p.join("Dust_Dust_5.png"), 5, 16); // another prefix: left out
        let (arts, skipped) = load(&d);
        assert!(skipped.is_empty(), "{skipped:?}");
        let a = &arts[0];
        assert_eq!((a.key.as_str(), a.name.as_str(), a.anims.len()), ("hero", "Hero", 2));
        // the square moves from x=3 (frame 0) to x=8..10 (frame 5): the cut covers them all
        assert_eq!((a.w, a.h), (10 - 3, 3));
        let walk = a.anims.iter().find(|x| x.name == "walk").unwrap();
        assert_eq!((walk.name.as_str(), walk.frames.len(), walk.ms), ("walk", 6, 125));
        // mirrored (subagent walking left): the first opaque pixel of frame 0 goes to the
        // other end
        assert_eq!(walk.frames[0].as_slice()[0].a, 255);
        let t = a.troupe().unwrap();
        let (right, left) = t.frames(t.script.walk);
        assert_eq!(right[0].as_slice()[0].a, 255);
        assert_eq!(left[0].as_slice()[right[0].width() as usize - 1].a, 255);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn mood_picks_the_animation_and_sleep_dims() {
        let d = temp("mood");
        let p = d.join("h");
        std::fs::create_dir_all(&p).unwrap();
        for (f, n) in [("H_Idle_4.png", 4), ("H_Run_6.png", 6), ("H_Hurt_4.png", 4), ("H_Jump_8.png", 8)] {
            sheet(&p.join(f), n, 16);
        }
        let (arts, _) = load(&d);
        let a = &arts[0];
        let look = |mood, ms| Look { mood, reaction: None, blink: false, frame: 0, pose: None, ms };
        let name = |k: FrameKey| a.anims[k.0].name.clone();
        assert_eq!(name(a.pick(&look(Mood::Busy, 0))), "run");
        assert_eq!(name(a.pick(&look(Mood::Alarm, 0))), "hurt");
        assert_eq!(name(a.pick(&look(Mood::Idle, 0))), "idle");
        // 8 fps: at 250 ms, the third frame; asleep, at half speed and dimmed
        assert_eq!(a.pick(&look(Mood::Idle, 250)).1, 2);
        assert_eq!(a.pick(&look(Mood::Sleep, 250)), (a.moods[0], 1, true));
        let jump = Look { reaction: Some(Reaction::Celebrate), ..look(Mood::Idle, 0) };
        assert_eq!(name(a.pick(&jump)), "jump");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn subagents_use_the_whole_pack_except_dying() {
        let d = temp("troupe");
        let p = d.join("t");
        std::fs::create_dir_all(&p).unwrap();
        for (f, n) in [("T_Idle_4.png", 4), ("T_Walk_6.png", 6), ("T_Run_6.png", 6), ("T_Jump_8.png", 8), ("T_Death_8.png", 8), ("T_Walk+Attack_6.png", 6)] {
            sheet(&p.join(f), n, 16);
        }
        // an attack reaching the border: widens only the subagents' cut
        let mut px = vec![[0u8; 4]; 2 * 16 * 16];
        for f in 0..2 {
            px[5 * 32 + f * 16] = [255, 0, 0, 255];
            px[5 * 32 + f * 16 + 15] = [255, 0, 0, 255];
        }
        png(&p.join("T_Attack1_2.png"), 32, 16, &px);
        let (arts, skipped) = load(&d);
        assert!(skipped.is_empty(), "{skipped:?}");
        let a = &arts[0];
        let s = &a.troupe().unwrap().script;
        let name = |i: usize| s.names[i].as_str();
        assert!(!s.names.iter().any(|n| n == "death"), "{:?}", s.names);
        assert_eq!((name(s.walk), s.run.map(name), s.jump.map(name), s.idle.map(name)), ("walk", Some("run"), Some("jump"), Some("idle")));
        assert_eq!(s.strolls.iter().map(|&i| name(i)).collect::<Vec<_>>(), ["walk+attack"]);
        assert_eq!(s.extras.iter().map(|&i| name(i)).collect::<Vec<_>>(), ["attack1", "idle"]);
        // header: cut of the mood animations; subagents: the whole frame (16 wide)
        assert!(a.w < 16);
        let (right, _) = a.troupe().unwrap().frames(0);
        assert_eq!(right[0].width(), 16);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn sprite_toml_rules_and_errors_have_a_reason() {
        let d = temp("toml");
        let ok = d.join("ok");
        std::fs::create_dir_all(&ok).unwrap();
        sheet(&ok.join("walks.png"), 3, 24);
        std::fs::write(ok.join(SPEC), "name = \"Ok\"\nfacing = \"left\"\nextras = []\n[anim]\nwalk = { file = \"walks.png\", frames = 3, fps = 4 }\n[moods]\nbusy = \"walk\"\n").unwrap();
        let bad = d.join("bad");
        std::fs::create_dir_all(&bad).unwrap();
        sheet(&bad.join("x.png"), 3, 24);
        std::fs::write(bad.join(SPEC), "[anim]\nwalk = { file = \"x.png\", frames = 5 }\n").unwrap();
        let outside = d.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join(SPEC), "[anim]\nwalk = \"../ok/walks.png\"\n").unwrap();
        let neko = d.join("neko");
        std::fs::create_dir_all(&neko).unwrap();
        sheet(&neko.join("N_Idle_2.png"), 2, 16);
        let mood = d.join("mood");
        std::fs::create_dir_all(&mood).unwrap();
        sheet(&mood.join("m.png"), 2, 16);
        std::fs::write(mood.join(SPEC), "[anim]\nidle = \"m.png\"\n[moods]\nbusy = \"run\"\n").unwrap();

        let (arts, skipped) = load(&d);
        assert_eq!(arts.len(), 1);
        let a = &arts[0];
        assert_eq!(a.anims[0].ms, 250);
        // facing left: walking right uses the mirrored frames
        let t = a.troupe().unwrap();
        let (right, left) = t.frames(t.script.walk);
        assert_eq!((right.len(), left.len()), (3, 3));
        assert!(std::ptr::eq(right.as_ptr(), t.clips[t.script.walk].1.as_ptr()));
        let reasons: Vec<(&str, &str)> = skipped.iter().map(|s| (s.dir.as_str(), s.reason.as_str())).collect();
        assert!(reasons.iter().any(|(d, r)| d.ends_with("bad") && r.contains("5 equal frames")), "{reasons:?}");
        assert!(reasons.iter().any(|(d, r)| d.ends_with("outside") && r.contains("in this folder")), "{reasons:?}");
        assert!(reasons.iter().any(|(d, r)| d.ends_with("neko") && r.contains("built-in")), "{reasons:?}");
        assert!(reasons.iter().any(|(d, r)| d.ends_with("mood") && r.contains("[moods] busy = \"run\": there is no such animation")), "{reasons:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn imports_each_character_into_a_folder() {
        let src = temp("import-src");
        for who in ["Pink_Monster", "Dude_Monster"] {
            let p = src.join(format!("1 {who}"));
            std::fs::create_dir_all(&p).unwrap();
            sheet(&p.join(format!("{who}_Idle_4.png")), 4, 32);
            sheet(&p.join(format!("{who}_Walk_6.png")), 6, 32);
            sheet(&p.join("Rock1.png"), 1, 8);
        }
        let root = temp("import-dst");
        let out = import(&src, &root).unwrap();
        assert_eq!(out.len(), 2, "{out:?}");
        assert!(root.join("pink-monster/Pink_Monster_Walk_6.png").exists());
        assert!(!root.join("pink-monster/Rock1.png").exists());
        assert!(std::fs::read_to_string(root.join("dude-monster/sprite.toml")).unwrap().contains("name = \"Dude Monster\""));
        let (arts, skipped) = load(&root);
        assert_eq!((arts.len(), skipped.len()), (2, 0));
        // again: doesn't overwrite
        assert!(import(&src, &root).unwrap().iter().all(|l| l.contains("already exists")));
        let _ = std::fs::remove_dir_all(&src);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn tray_square_scales_by_a_whole_factor() {
        let d = temp("square");
        let p = d.join("q");
        std::fs::create_dir_all(&p).unwrap();
        sheet(&p.join("Q_Idle_2.png"), 2, 16);
        let (arts, _) = load(&d);
        let a = &arts[0];
        let (side, px) = a.square((0, 0, false), 32);
        // trimmed 3×3 frame → scaled 10× → 30
        assert_eq!(side, 30);
        assert_eq!(px.len(), 30 * 30 * 4);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn names() {
        assert_eq!(parse_sheet_name("Pink_Monster_Walk+Attack_6"), Some(("Pink_Monster", "walk+attack".into(), 6)));
        assert_eq!(parse_sheet_name("Rock1"), None);
        assert_eq!(frames_in_name("A_Run_6.png"), Some(6));
        assert_eq!(slug("Pink Monster"), "pink-monster");
        assert_eq!(slug("  Ação__X "), "a-o-x");
    }
}
