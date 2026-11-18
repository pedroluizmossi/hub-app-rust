//! Mascot animations: drawn poses, frame by frame.
//!
//! Without an animation, a mascot is a single drawing (`Sprite::grid`) and what changes is
//! computed on top of it (blink, LED, glitch). An animation swaps the drawing for a sequence
//! of frames; the effects still apply on top of each frame. Each mascot's `Player`
//! (`anim.rs`) decides which frame to show.
//!
//! Each mascot can have a file `src/mascot/clips/<key>.toml` (register it in `SOURCES`):
//!
//! ```toml
//! [idle]                         # animation name
//! fps = 4                        # frames per second (or `durations`)
//! durations = [900, 120, 120]    # ms of each frame (instead of `fps`)
//! loop = true                    # repeats (default: true for a mood, false otherwise)
//! enter = "yawn"                 # plays once when entering this mood, before the loop
//! frames = [
//! """
//! ... 14 rows of 16 characters, with the same color roles as `sprites.rs` ...
//! """,
//! ]
//! ```
//!
//! When it plays:
//! - named after a reaction (`celebrate`, `surprised`): while the reaction lasts;
//! - named after a mood (`sleep`, `idle`, `happy`, `busy`, `alarm`): whenever the mascot is
//!   in it, after its `enter` transition;
//! - other names: when something asks for it (transitions, a game).
//!
//! To draw without recompiling: `MINIUTILS_CLIPS=src/mascot/clips cargo run` reads the
//! `.toml` files in that folder and reloads on save (see `watch`).
//! To convert a PNG: `cargo run -p sprite-import -- --help`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{Duration, SystemTime};

use super::sprites::{self, SPRITES, Sprite};

/// A frame: 14 rows, like `Sprite::grid`.
pub type Grid = [&'static str; 14];

/// Animation files built into the binary: (mascot key, content).
const SOURCES: &[(&str, &str)] = &[
    ("neko", include_str!("clips/neko.toml")),
    ("lumi", include_str!("clips/lumi.toml")),
];

/// Names of the animations that play by themselves for the mood (they repeat by default).
pub const MOODS: [&str; 5] = ["sleep", "idle", "happy", "busy", "alarm"];

#[derive(Debug)]
pub struct Clip {
    pub frames: Vec<Grid>,
    /// How long each frame stays on screen, in ms.
    pub durations: Vec<u32>,
    pub looping: bool,
    pub enter: Option<String>,
}

impl Clip {
    /// Length of one full lap, in ms.
    pub fn total_ms(&self) -> u32 {
        self.durations.iter().sum()
    }

    /// The shortest frame (the hub speeds its timer up so as not to skip frames).
    pub fn shortest_ms(&self) -> u32 {
        self.durations.iter().copied().min().unwrap_or(1000)
    }

    /// Index of the frame after playing for `ms`. Without looping, it stops on the last one.
    pub fn index_at(&self, ms: u64) -> usize {
        let total = self.total_ms().max(1) as u64;
        let mut t = if self.looping { ms % total } else { ms.min(total - 1) };
        for (i, d) in self.durations.iter().enumerate() {
            if t < *d as u64 {
                return i;
            }
            t -= *d as u64;
        }
        self.frames.len() - 1
    }

    pub fn frame_at(&self, ms: u64) -> &Grid {
        &self.frames[self.index_at(ms)]
    }

    /// Finished playing (only meaningful without `loop`).
    pub fn done_at(&self, ms: u64) -> bool {
        !self.looping && ms >= self.total_ms() as u64
    }
}

/// A mascot's animations, by name.
pub type Clips = HashMap<String, Clip>;

type Library = HashMap<String, Arc<Clips>>;

fn library() -> &'static RwLock<Arc<Library>> {
    static LIB: OnceLock<RwLock<Arc<Library>>> = OnceLock::new();
    LIB.get_or_init(|| {
        let lib = match dev_dir() {
            Some(dir) => {
                watch(dir.clone());
                load_dir(&dir)
            }
            None => load_embedded(),
        };
        RwLock::new(Arc::new(lib))
    })
}

/// Animations of the sprites from files (`file.rs`), by key. `set_extra` replaces them.
static EXTRA: RwLock<Option<Arc<Library>>> = RwLock::new(None);

/// Animations of mascot `key`, if it has any.
pub fn of(key: &str) -> Option<Arc<Clips>> {
    if let Some(c) = library().read().ok()?.get(key).cloned() {
        return Some(c);
    }
    EXTRA.read().ok()?.as_ref()?.get(key).cloned()
}

/// Replaces the animations of the sprites from files.
pub fn set_extra(lib: HashMap<String, Arc<Clips>>) {
    if let Ok(mut extra) = EXTRA.write() {
        *extra = Some(Arc::new(lib));
    }
}

fn load_embedded() -> Library {
    let mut lib = Library::new();
    for (key, src) in SOURCES {
        match parse(sprites::get(key), src) {
            Ok(clips) => {
                lib.insert(key.to_string(), Arc::new(clips));
            }
            Err(e) => log::warn!("invalid animations for {key}: {e}"),
        }
    }
    lib
}

// ---- development: read from the folder and reload on save ---------------------------

/// Animation folder for development (`MINIUTILS_CLIPS`), if set.
fn dev_dir() -> Option<PathBuf> {
    std::env::var_os("MINIUTILS_CLIPS").filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// Reads every `<key>.toml` in the folder (key = a mascot of `sprites.rs`). A file with an
/// error is left out, with the error in the log.
fn load_dir(dir: &Path) -> Library {
    let mut lib = Library::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        log::warn!("MINIUTILS_CLIPS={} is not a readable folder", dir.display());
        return lib;
    };
    for path in entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "toml")) {
        let Some(key) = path.file_stem().and_then(|s| s.to_str()) else { continue };
        let Some(sprite) = SPRITES.iter().find(|s| s.key == key) else {
            log::warn!("{}: no mascot \"{key}\" in sprites.rs", path.display());
            continue;
        };
        let parsed = std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|src| parse(sprite, &src));
        match parsed {
            Ok(clips) => {
                lib.insert(key.to_string(), Arc::new(clips));
            }
            Err(e) => log::warn!("{}: {e}", path.display()),
        }
    }
    lib
}

/// Checks the folder every half second and reloads when a `.toml` changes. Only exists with
/// `MINIUTILS_CLIPS` set (for whoever is drawing), and each reload leaves the old frames in
/// memory (they are `'static`): not meant for normal use.
fn watch(dir: PathBuf) {
    let stamp = |dir: &Path| -> Vec<(PathBuf, SystemTime)> {
        let mut v: Vec<_> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| Some((e.path(), e.metadata().ok()?.modified().ok()?)))
            .filter(|(p, _)| p.extension().is_some_and(|e| e == "toml"))
            .collect();
        v.sort();
        v
    };
    let spawned = std::thread::Builder::new().name("miniutils-clips".into()).spawn(move || {
        let mut last = stamp(&dir);
        log::info!("animations from {} (reloaded on save)", dir.display());
        loop {
            std::thread::sleep(Duration::from_millis(500));
            let now = stamp(&dir);
            if now == last {
                continue;
            }
            last = now;
            let lib = load_dir(&dir);
            log::info!("animations reloaded ({} mascot(s))", lib.len());
            if let Ok(mut l) = library().write() {
                *l = Arc::new(lib);
            }
        }
    });
    if let Err(e) = spawned {
        log::warn!("no thread to reload animations ({e})");
    }
}

// ---- format -------------------------------------------------------------------------

/// Valid color roles in a regular mascot (see the top of `sprites.rs`).
pub(crate) const ROLES: &[u8] = b".KPSHMCEWBTFDA";

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawClip {
    fps: Option<u32>,
    durations: Option<Vec<u32>>,
    #[serde(rename = "loop")]
    looping: Option<bool>,
    enter: Option<String>,
    frames: Vec<String>,
}

/// Reads an animation file and checks each frame against `sprite`'s base drawing. The
/// errors are for whoever is drawing (they go to the log), so they are not translated.
pub fn parse(sprite: &Sprite, src: &str) -> Result<Clips, String> {
    let raw: HashMap<String, RawClip> = toml::from_str(src).map_err(|e| e.to_string())?;
    let mut clips = HashMap::new();
    for (name, c) in &raw {
        let err = |e: String| format!("[{name}]: {e}");
        if c.frames.is_empty() {
            return Err(err("no frames".into()));
        }
        let durations = match (&c.durations, c.fps) {
            (Some(_), Some(_)) => return Err(err("use `fps` or `durations`, not both".into())),
            (Some(d), None) => {
                if d.len() != c.frames.len() {
                    return Err(err(format!("{} durations for {} frames", d.len(), c.frames.len())));
                }
                if let Some(bad) = d.iter().find(|&&ms| !(16..=60_000).contains(&ms)) {
                    return Err(err(format!("duration {bad} ms outside 16..60000")));
                }
                d.clone()
            }
            (None, fps) => {
                let fps = fps.unwrap_or(4);
                if !(1..=30).contains(&fps) {
                    return Err(err(format!("fps {fps} outside 1..30")));
                }
                vec![1000 / fps; c.frames.len()]
            }
        };
        let mood = MOODS.contains(&name.as_str());
        if !mood && c.enter.is_some() {
            return Err(err("enter only applies to a mood animation".into()));
        }
        // the transition it mentions must exist in the same file and play once
        if let Some(other) = &c.enter {
            match raw.get(other) {
                None => return Err(err(format!("mentions \"{other}\", which is not in this file"))),
                Some(o) if o.looping == Some(true) || MOODS.contains(&other.as_str()) => {
                    return Err(err(format!("\"{other}\" loops: a transition plays once")));
                }
                Some(_) => {}
            }
        }
        let mut frames = Vec::new();
        for (i, text) in c.frames.iter().enumerate() {
            frames.push(grid(sprite, text).map_err(|e| err(format!("frame {}: {e}", i + 1)))?);
        }
        clips.insert(
            name.clone(),
            Clip {
                frames,
                durations,
                looping: c.looping.unwrap_or(mood),
                enter: c.enter.clone(),
            },
        );
    }
    Ok(clips)
}

/// Turns a frame's text into a `Grid`, checking size and colors.
fn grid(sprite: &Sprite, text: &str) -> Result<Grid, String> {
    let rows: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if rows.len() != sprite.grid.len() {
        return Err(format!("{} rows, expected {}", rows.len(), sprite.grid.len()));
    }
    let mut out: Grid = [""; 14];
    for (y, row) in rows.iter().enumerate() {
        let want = sprite.grid[y].len();
        if row.len() != want {
            return Err(format!("row {} has {} columns, expected {want}", y + 1, row.len()));
        }
        if let Some(bad) = row.bytes().find(|&c| !valid_cell(sprite, c)) {
            return Err(format!("row {}: color '{}' that this mascot doesn't have", y + 1, bad as char));
        }
        // static data: loaded once and used until the program ends
        out[y] = Box::leak(row.to_string().into_boxed_str());
    }
    Ok(out)
}

fn valid_cell(sprite: &Sprite, c: u8) -> bool {
    match &sprite.pixel {
        // "pixel" mascot: the letters of its palette, the eye `e` and transparent
        Some(px) => c == b'.' || c == b'e' || px.color(c).is_some(),
        None => ROLES.contains(&c),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn frame(rows: &[&str]) -> String {
        format!("\"\"\"\n{}\n\"\"\"", rows.join("\n"))
    }

    #[test]
    fn embedded_animation_files_are_valid() {
        for (key, src) in SOURCES {
            let sprite = sprites::get(key);
            assert_eq!(sprite.key, *key, "mascot {key} is not in sprites.rs");
            parse(sprite, src).unwrap_or_else(|e| panic!("{key}: {e}"));
        }
    }

    #[test]
    fn rejects_bad_frame() {
        let neko = sprites::get("neko");
        let short = "[x]\nframes = [\"\"\"\nKKKK\n\"\"\"]";
        assert!(parse(neko, short).unwrap_err().contains("rows"));
        let row = "Z".repeat(16);
        let color = format!("[x]\nframes = [{}]", frame(&[row.as_str(); 14]));
        assert!(parse(neko, &color).unwrap_err().contains("color 'Z'"));
    }

    #[test]
    fn accepts_an_indented_frame() {
        let neko = sprites::get("neko");
        let rows: Vec<String> = neko.grid.iter().map(|r| format!("      {r}")).collect();
        let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
        let src = format!("[x]\nframes = [{}]", frame(&rows));
        let clips = parse(neko, &src).unwrap();
        assert_eq!(clips["x"].frames[0], neko.grid);
    }

    #[test]
    fn rejects_bad_reference_and_duration() {
        let neko = sprites::get("neko");
        let base = frame(&neko.grid);
        let missing = format!("[idle]\nenter = \"yawn\"\nframes = [{base}]");
        assert!(parse(neko, &missing).unwrap_err().contains("\"yawn\""));
        let durations = format!("[idle]\ndurations = [100]\nframes = [{base}, {base}]");
        assert!(parse(neko, &durations).unwrap_err().contains("1 durations for 2 frames"));
        let not_mood = format!("[x]\nenter = \"y\"\nframes = [{base}]\n[y]\nframes = [{base}]");
        assert!(parse(neko, &not_mood).unwrap_err().contains("only applies to a mood animation"));
    }

    #[test]
    fn duration_per_frame() {
        let neko = sprites::get("neko");
        let base = frame(&neko.grid);
        let src = format!(
            "[idle]\ndurations = [800, 100, 100]\nframes = [{base}, {base}, {base}]\n[x]\nfps = 10\nframes = [{base}, {base}]"
        );
        let clips = parse(neko, &src).unwrap();
        let idle = &clips["idle"];
        assert!(idle.looping);
        assert_eq!(idle.total_ms(), 1000);
        assert_eq!(idle.shortest_ms(), 100);
        assert_eq!(idle.index_at(0), 0);
        assert_eq!(idle.index_at(799), 0);
        assert_eq!(idle.index_at(800), 1);
        assert_eq!(idle.index_at(950), 2);
        assert_eq!(idle.index_at(1000), 0);
        let x = &clips["x"];
        assert!(!x.looping);
        assert_eq!(x.index_at(5000), 1);
        assert!(!x.done_at(199));
        assert!(x.done_at(200));
    }
}
