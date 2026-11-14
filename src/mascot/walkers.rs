//! Mascots that walk, of both kinds, in one list: what the walking subagents of the agents
//! app use (and any app that wants a mascot walking on a strip). The choreography is
//! `stroll.rs`; this gives each mascot its `Script` and its frames, already drawn:
//!
//! - **PNG sprite** (`pack.rs`): walks with a `walk` (or `run`) sheet; the frames are the
//!   pack's, recolored with its `[palette]` if it has one;
//! - **code mascot** (built-in or `sprites/<key>.toml`): walks with an `[anim.walk]`
//!   animation (`run`, `jump` and the stops in `extras` are optional); the frames are drawn
//!   in the app's theme, like the mascot anywhere else.
//!
//! The same rule for both: whoever has `walk` walks. Without `extras`, a stop is only `idle`
//! (a code mascot without `[anim.idle]` stops in its base drawing); any other action plays
//! at a stop only when listed in `extras`.

use std::collections::HashMap;
use std::sync::Arc;

use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

use super::anim::Look;
use super::clips::{Clip, Clips, Grid};
use super::render::{self, H, Mood, W};
use super::sprites::{self, Sprite};
use super::stroll::Script;
use super::themes::Theme;
use super::{file, pack};

/// A mascot that walks: its script and the frames (facing right, facing left) of each of
/// its animations.
pub struct Walker {
    pub script: Script,
    pub frames: Vec<(Vec<Image>, Vec<Image>)>,
}

/// Every mascot that walks, in the pickers' order, drawn in `theme`.
pub fn all(theme: &Theme) -> Vec<Walker> {
    sprites::all().into_iter().filter_map(|s| walker(s, theme)).collect()
}

/// The walker of `sprite` (`None` = it doesn't walk).
pub fn walker(sprite: &'static Sprite, theme: &Theme) -> Option<Walker> {
    match pack::art(sprite) {
        Some(art) => {
            let t = art.troupe()?;
            let imgs = |v: &[pack::Buf]| v.iter().map(|b| Image::from_rgba8(art.troupe_frame(b, theme))).collect();
            let frames = (0..t.script.names.len())
                .map(|c| {
                    let (right, left) = t.frames(c);
                    (imgs(right), imgs(left))
                })
                .collect();
            Some(Walker { script: t.script.clone(), frames })
        }
        None => drawn(sprite, &super::clips::of(sprite.key)?, &file::extras(sprite.key), theme),
    }
}

/// An animation of moving (`walk`, `run` and their variations like `walk+attack` or
/// `run_fast`), by whole word: `crunch` or `sidewalk` are not.
pub fn is_moving(name: &str) -> bool {
    name.split(|c: char| !c.is_ascii_alphanumeric()).any(|w| w.eq_ignore_ascii_case("walk") || w.eq_ignore_ascii_case("run"))
}

/// Does `sprite` walk? (Without drawing anything.)
pub fn walks(sprite: &Sprite) -> bool {
    match pack::art(sprite) {
        Some(art) => art.troupe().is_some(),
        None => super::clips::of(sprite.key).is_some_and(|c| c.contains_key("walk")),
    }
}

/// A code mascot walking: its `walk` animation, and `run`, `jump`, `idle` and the `extras`
/// it has. Without `[anim.idle]`, the stop is the base drawing.
fn drawn(sprite: &Sprite, clips: &Arc<Clips>, extras: &[String], theme: &Theme) -> Option<Walker> {
    clips.get("walk")?;
    let base = Clip { frames: vec![sprite.grid], durations: vec![1000], looping: true, enter: None };
    // the order of `names`: walk, run, jump, idle, other ways of walking, stops
    let mut names: Vec<String> = vec!["walk".to_string()];
    let push = |n: &str, names: &mut Vec<String>| {
        if !names.iter().any(|x| x == n) {
            names.push(n.to_string());
        }
    };
    for n in ["run", "jump"] {
        if clips.contains_key(n) {
            push(n, &mut names);
        }
    }
    push("idle", &mut names);
    let mut strolls: Vec<&String> = clips.keys().filter(|k| is_moving(k) && *k != "walk" && *k != "run").collect();
    strolls.sort();
    for k in strolls {
        push(k, &mut names);
    }
    for e in extras.iter().filter(|e| clips.contains_key(e.as_str())) {
        push(e, &mut names);
    }
    let clip = |n: &str| -> &Clip { clips.get(n).unwrap_or(&base) };
    let at = |n: &str| names.iter().position(|x| *x == n);
    let moving = is_moving;
    let script = Script {
        names: names.iter().map(|n| n.to_string()).collect(),
        frames: names.iter().map(|n| clip(n).frames.len()).collect(),
        // one pace per animation (the strip's choreography has no per-frame time)
        ms: names.iter().map(|n| clip(n).total_ms() / clip(n).frames.len().max(1) as u32).collect(),
        walk: 0,
        run: at("run"),
        jump: at("jump"),
        idle: at("idle"),
        strolls: names.iter().enumerate().filter(|(i, n)| *i > 0 && moving(n) && **n != "run").map(|(i, _)| i).collect(),
        extras: names.iter().enumerate().filter(|(_, n)| **n == "idle" || extras.iter().any(|e| e == *n)).map(|(i, _)| i).collect(),
    };
    let mut cache: HashMap<Grid, (Image, Image)> = HashMap::new();
    let mut draw = |g: &Grid| -> (Image, Image) {
        cache
            .entry(*g)
            .or_insert_with(|| {
                let mut look = Look::from_frame(Mood::Happy, 0);
                look.blink = false;
                look.pose = Some(*g);
                let px = render::render_look(sprite, theme, &look);
                (image(&px, false), image(&px, true))
            })
            .clone()
    };
    let frames = names
        .iter()
        .map(|n| {
            let (r, l): (Vec<Image>, Vec<Image>) = clip(n).frames.iter().map(&mut draw).unzip();
            (r, l)
        })
        .collect();
    Some(Walker { script, frames })
}

/// A 16×14 frame as an image, mirrored or not (walking to the left).
fn image(px: &[[u8; 4]; W * H], mirrored: bool) -> Image {
    let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(W as u32, H as u32);
    for (i, d) in buf.make_mut_slice().iter_mut().enumerate() {
        let (y, x) = (i / W, i % W);
        let s = px[y * W + if mirrored { W - 1 - x } else { x }];
        *d = Rgba8Pixel { r: s[0], g: s[1], b: s[2], a: s[3] };
    }
    Image::from_rgba8(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mascot::themes;

    /// A code mascot with `walk`, `jump`, `attack` and `boom`, listing only `attack`.
    fn walker_of(extras: &[&str]) -> Walker {
        let neko = sprites::get("neko");
        let frame = crate::mascot::clips::tests::frame(&neko.grid);
        let src = format!(
            "[walk]\nfps = 8\nframes = [{frame}, {frame}]\n[jump]\nframes = [{frame}]\n[attack]\nframes = [{frame}]\n[boom]\nframes = [{frame}]\n"
        );
        let clips = Arc::new(crate::mascot::clips::parse(neko, &src).unwrap());
        let extras: Vec<String> = extras.iter().map(|s| s.to_string()).collect();
        drawn(neko, &clips, &extras, themes::get("neon")).unwrap()
    }

    #[test]
    fn a_code_mascot_walks_with_its_walk_animation() {
        let w = walker_of(&["attack"]);
        let s = &w.script;
        let name = |i: usize| s.names[i].as_str();
        assert_eq!((name(s.walk), s.jump.map(name), s.run), ("walk", Some("jump"), None));
        // the base drawing is the idle stop; only what `extras` lists joins it
        assert_eq!(s.idle.map(name), Some("idle"));
        assert_eq!(s.extras.iter().map(|&i| name(i)).collect::<Vec<_>>(), ["idle", "attack"]);
        assert!(!s.names.iter().any(|n| n == "boom"), "a free action doesn't play by itself");
        assert_eq!((s.frames[s.walk], s.ms[s.walk]), (2, 125));
        // frames drawn, both ways
        assert_eq!(w.frames.len(), s.names.len());
        assert_eq!(w.frames[0].0.len(), 2);
        assert_eq!(w.frames[0].1[0].size().width, W as u32);
    }

    #[test]
    fn without_walk_it_doesnt_walk() {
        let neko = sprites::get("neko");
        let frame = crate::mascot::clips::tests::frame(&neko.grid);
        let clips = Arc::new(crate::mascot::clips::parse(neko, &format!("[idle]\nframes = [{frame}]\n")).unwrap());
        assert!(drawn(neko, &clips, &[], themes::get("neon")).is_none());
        assert!(walker_of(&[]).script.extras.len() == 1);
    }
}
