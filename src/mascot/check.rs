//! What a mascot has, for whoever installs or draws one: `miniutils --check-mascot <path>`
//! and the card of the Mascots app use the same report. It reads with the hub's own readers
//! (`pack.rs` for a PNG sprite, `file.rs` for a code mascot), so what it says is what the hub
//! will do:
//!
//! - which animation shows each mood and reaction (a code mascot without one uses its
//!   drawing and the built-in effects);
//! - whether it walks (`walk`), runs, jumps and where it stops (`idle` and `extras`);
//! - whether it follows the theme (always for a code mascot; a PNG with `[palette]`), and the
//!   PNG colors that keep their own color;
//! - the free actions: animations that only play when an app asks for them.

use std::path::Path;

use super::clips::MOODS;
use super::pack::{self, Art};
use super::sprites::Sprite;
use super::file;
use crate::tr;

/// The two kinds of mascot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Drawn by letters, 16 × 14 (built-in or `sprites/<key>.toml`).
    Code,
    /// PNG sheets (`mascots/<key>/`).
    Sprite,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    pub kind: Kind,
    pub key: String,
    pub name: String,
    /// Frame size, in pixels.
    pub size: (usize, usize),
    /// Animation of each mood (`MOODS` order); "" = the drawing itself.
    pub moods: [String; 5],
    /// Animation of each reaction (celebrate, surprised); "" = the built-in effect.
    pub reactions: [String; 2],
    /// Walking: `None` = doesn't walk (no `walk`).
    pub walk: Option<Walk>,
    pub themed: bool,
    /// PNG colors that keep their own color (with `[palette]`), as `#rrggbb`.
    pub loose_colors: Vec<String>,
    /// Animations that only play when an app asks for them.
    pub free: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Walk {
    pub run: bool,
    pub jump: bool,
    /// Where it stops: `idle` and the `extras`.
    pub stops: Vec<String>,
}

/// Checks a mascot on disk: a folder of PNG sheets or a code mascot's `.toml`.
pub fn path(p: &Path) -> Result<Report, String> {
    if p.is_dir() {
        return Ok(of_art(&pack::read_folder(p)?));
    }
    if p.extension().is_some_and(|e| e == "toml") {
        let key = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        file::check_key(&key)?;
        let text = std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?;
        let f = file::parse(&key, &text)?;
        let clips = f.clips.as_ref();
        return Ok(code(&f.sprite, clips, &f.extras));
    }
    Err(tr!("{}: give a folder of PNG sheets or a mascot .toml", p.display()))
}

/// The report of a mascot already in the hub (any kind).
pub fn of_sprite(sprite: &Sprite) -> Report {
    match pack::art(sprite) {
        Some(art) => of_art(art),
        None => {
            code(sprite, super::clips::of(sprite.key).as_deref(), &file::extras(sprite.key))
        }
    }
}

fn of_art(art: &Art) -> Report {
    let (moods, reactions) = art.mapping();
    let walk = art.troupe().map(|t| {
        let s = &t.script;
        Walk { run: s.run.is_some(), jump: s.jump.is_some(), stops: s.extras.iter().map(|&i| s.names[i].clone()).collect() }
    });
    let mut known: Vec<String> = moods.to_vec();
    known.extend(reactions.iter().flatten().cloned());
    if let Some(t) = art.troupe() {
        known.extend(t.script.names.iter().cloned());
    }
    let free = art.declared_names().iter().filter(|n| !known.contains(n)).cloned().collect();
    let hex = |c: [u8; 3]| format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2]);
    Report {
        kind: Kind::Sprite,
        key: art.key.clone(),
        name: art.name.clone(),
        size: (art.w, art.h),
        moods,
        reactions: reactions.map(|r| r.unwrap_or_default()),
        walk,
        themed: art.themed(),
        loose_colors: if art.themed() { art.loose_colors(16).into_iter().map(hex).collect() } else { Vec::new() },
        free,
    }
}

/// A code mascot: `clips` are its animations (`[anim]`), `extras` its listed stops.
fn code(sprite: &Sprite, clips: Option<&super::clips::Clips>, extras: &[String]) -> Report {
    let mut names: Vec<String> = clips.map(|c| c.keys().cloned().collect()).unwrap_or_default();
    names.sort();
    let has = |n: &str| names.iter().any(|x| x == n);
    let moods = MOODS.map(|m| if has(m) { m.to_string() } else { String::new() });
    let reactions = ["celebrate", "surprised"].map(|r| if has(r) { r.to_string() } else { String::new() });
    let walk = has("walk").then(|| {
        let mut stops = vec!["idle".to_string()];
        stops.extend(extras.iter().cloned());
        Walk { run: has("run"), jump: has("jump"), stops }
    });
    // the ones nothing plays by itself (a mood's `enter` transition plays with its mood)
    // (from the clips being checked: a file not loaded yet has them only here)
    let enters: Vec<String> = clips.map(|c| c.values().filter_map(|c| c.enter.clone()).collect()).unwrap_or_default();
    let free = names
        .iter()
        .filter(|n| {
            !MOODS.contains(&n.as_str())
                && !["celebrate", "surprised", "walk", "run", "jump"].contains(&n.as_str())
                && !extras.contains(n)
                && !super::walkers::is_moving(n)
                && !enters.contains(n)
        })
        .cloned()
        .collect();
    Report {
        kind: Kind::Code,
        key: sprite.key.to_string(),
        name: sprite.name().to_string(),
        size: (super::render::W, super::render::H),
        moods,
        reactions,
        walk,
        themed: sprite.pixel.is_none(),
        loose_colors: Vec::new(),
        free,
    }
}

impl Report {
    /// The report as text, for the terminal and the Mascots app.
    pub fn text(&self) -> String {
        let kind = match self.kind {
            Kind::Code => tr!("code mascot"),
            Kind::Sprite => tr!("PNG sprite"),
        };
        let mut out = format!("{} ({}, {}×{}) · {}\n", self.name, kind, self.size.0, self.size.1, self.key);
        let shown = |a: &str, empty: &str| if a.is_empty() { empty.to_string() } else { a.to_string() };
        let moods: Vec<String> =
            MOODS.iter().zip(&self.moods).map(|(m, a)| format!("{m} → {}", shown(a, tr!("drawing")))).collect();
        out += &tr!("  moods: {}", moods.join(" · "));
        out.push('\n');
        let reactions: Vec<String> = ["celebrate", "surprised"]
            .iter()
            .zip(&self.reactions)
            .map(|(r, a)| format!("{r} → {}", shown(a, tr!("effect"))))
            .collect();
        out += &tr!("  reactions: {}", reactions.join(" · "));
        out.push('\n');
        out += &match &self.walk {
            None => tr!("  doesn't walk: no \"walk\" animation (the walking subagents leave it out)").to_string(),
            Some(w) => {
                let mut moves = vec!["walk"];
                if w.run {
                    moves.push("run");
                }
                if w.jump {
                    moves.push("jump");
                }
                tr!("  walks: {} · stops: {}", moves.join(", "), w.stops.join(", "))
            }
        };
        out.push('\n');
        out += &match (self.kind, self.themed) {
            (Kind::Code, true) => tr!("  follows the theme: yes").to_string(),
            (Kind::Code, false) => tr!("  follows the theme: only the eye ([palette] has its own colors)").to_string(),
            (Kind::Sprite, false) => tr!("  follows the theme: no (add [palette] to sprite.toml)").to_string(),
            (Kind::Sprite, true) if self.loose_colors.is_empty() => tr!("  follows the theme: yes ([palette])").to_string(),
            (Kind::Sprite, true) => tr!("  follows the theme: yes ([palette]); keep their color: {}", self.loose_colors.join(" ")),
        };
        out.push('\n');
        if !self.free.is_empty() {
            out += &tr!("  free actions (only when an app asks): {}", self.free.join(", "));
            out.push('\n');
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_the_example_code_mascot() {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/appearance/sprites/jellyfish.toml");
        let r = path(&p).unwrap();
        assert_eq!((r.kind, r.key.as_str(), r.size), (Kind::Code, "jellyfish", (16, 14)));
        assert_eq!(r.moods[1], "idle");
        assert_eq!(r.moods[0], "");
        assert!(r.walk.is_none() && r.themed && r.free.is_empty());
        let t = r.text();
        assert!(t.contains("idle → idle") && t.contains("doesn't walk"), "{t}");
    }

    #[test]
    fn a_code_mascot_that_walks_and_has_a_free_action() {
        let dir = std::env::temp_dir().join(format!("miniutils-check-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let neko = super::super::sprites::get("neko");
        let frame = super::super::clips::tests::frame(&neko.grid);
        let grid = neko.grid.join("\n");
        let src = format!("grid = \"\"\"\n{grid}\n\"\"\"\nextras = [\"wave\"]\n[anim.walk]\nframes = [{frame}]\n[anim.jump]\nframes = [{frame}]\n[anim.wave]\nframes = [{frame}]\n[anim.boom]\nframes = [{frame}]\n");
        std::fs::write(dir.join("cat.toml"), src).unwrap();
        let r = path(&dir.join("cat.toml")).unwrap();
        let w = r.walk.clone().unwrap();
        assert!(!w.run && w.jump);
        assert_eq!(w.stops, ["idle", "wave"]);
        assert_eq!(r.free, ["boom"]);
        assert!(r.text().contains("boom"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn neither_folder_nor_toml() {
        assert!(path(Path::new("nothing.txt")).is_err());
    }
}
