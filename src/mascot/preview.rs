//! GIF preview of the animations, outside the app: `miniutils --preview <mascot> [animation]`.
//!
//! Draws each frame with the app's own renderer, in several themes side by side and scaled
//! up, to check an animation (or show it to someone) without opening the window.

use std::fs::File;
use std::io::BufWriter;
use std::path::PathBuf;

use super::anim::{Look, Reaction};
use super::clips::{self, Clip};
use super::render::{self, H, W};
use super::sprites::SPRITES;
use super::themes::BUILTIN as THEMES;
use super::Mood;

/// How much each mascot pixel is scaled up.
const SCALE: usize = 6;
/// Margin around each mascot (in already scaled pixels).
const PAD: usize = 12;
/// Themes per row in the sheet.
const COLS: usize = 4;
/// A looping animation plays at least this long in the preview.
const MIN_LOOP_MS: u32 = 2000;

/// The `--output` flag, and its old Portuguese name, still accepted (not shown in the help).
const OUTPUT_FLAGS: [&str; 2] = ["--output", "--saida"];

/// Help for the subcommand, in the interface language.
pub fn usage() -> &'static str {
    crate::tr!("usage: miniutils --preview <mascot> [animation] [--output file.gif]\n\nMakes a GIF of the animation (or of all of them, one after another) in every theme.\nWithout --output, writes <mascot>[-<animation>].gif in the current folder.\nWith MINIUTILS_CLIPS=<folder>, reads the animations from that folder instead of the built-in ones.")
}

/// Runs the preview; returns the path of the GIF it wrote.
pub fn run(args: &[String]) -> Result<PathBuf, String> {
    let mut pos = Vec::new();
    let mut out = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            s if OUTPUT_FLAGS.contains(&s) => out = Some(PathBuf::from(it.next().ok_or(crate::tr!("--output needs a file name"))?)),
            s if s.starts_with('-') => return Err(crate::tr!("unknown option: {}\n\n{}", s, usage())),
            s => pos.push(s.to_string()),
        }
    }
    let key = pos.first().ok_or(usage())?;
    let sprite = SPRITES.iter().find(|s| s.key == key).ok_or_else(|| crate::tr!("there is no mascot \"{}\" in sprites.rs", key))?;
    let all = clips::of(key).ok_or_else(|| crate::tr!("the mascot \"{}\" has no animations", key))?;
    let names: Vec<String> = match pos.get(1) {
        Some(name) if all.contains_key(name) => vec![name.clone()],
        Some(name) => {
            let mut have: Vec<&String> = all.keys().collect();
            have.sort();
            return Err(crate::tr!("\"{}\" has no animation \"{}\"; it has: {}", key, name, format!("{have:?}")));
        }
        None => {
            let mut v: Vec<String> = all.keys().cloned().collect();
            v.sort();
            v
        }
    };
    let path = out.unwrap_or_else(|| PathBuf::from(match pos.get(1) {
        Some(n) => format!("{key}-{n}.gif"),
        None => format!("{key}.gif"),
    }));

    // the sequence: each animation with the mood (or reaction) its name points to
    let mut seq: Vec<(Look, u32)> = Vec::new();
    for name in &names {
        let clip = &all[name];
        let look = look_for(name, &all);
        for (grid, ms) in timeline(clip) {
            seq.push((Look { pose: Some(*grid), ..look }, ms));
        }
    }

    let rows = THEMES.len().div_ceil(COLS);
    let tile = (W * SCALE + 2 * PAD, H * SCALE + 2 * PAD);
    let (gw, gh) = (tile.0 * COLS.min(THEMES.len()), tile.1 * rows);
    let file = File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut enc = gif::Encoder::new(BufWriter::new(file), gw as u16, gh as u16, &[]).map_err(|e| e.to_string())?;
    enc.set_repeat(gif::Repeat::Infinite).map_err(|e| e.to_string())?;
    for (look, ms) in &seq {
        let mut rgba = vec![0u8; gw * gh * 4];
        for (i, theme) in THEMES.iter().enumerate() {
            let (ox, oy) = ((i % COLS) * tile.0, (i / COLS) * tile.1);
            let bg = theme.bg0;
            for y in 0..tile.1 {
                for x in 0..tile.0 {
                    put(&mut rgba, gw, ox + x, oy + y, [(bg >> 16) as u8, (bg >> 8) as u8, bg as u8, 255]);
                }
            }
            let px = render::render_look(sprite, theme, look);
            for y in 0..H {
                for x in 0..W {
                    let p = px[y * W + x];
                    if p[3] == 0 {
                        continue;
                    }
                    for dy in 0..SCALE {
                        for dx in 0..SCALE {
                            put(&mut rgba, gw, ox + PAD + x * SCALE + dx, oy + PAD + y * SCALE + dy, p);
                        }
                    }
                }
            }
        }
        let mut frame = gif::Frame::from_rgba_speed(gw as u16, gh as u16, &mut rgba, 10);
        // GIF counts in hundredths of a second
        frame.delay = (ms / 10).max(2) as u16;
        enc.write_frame(&frame).map_err(|e| e.to_string())?;
    }
    Ok(path)
}

fn put(buf: &mut [u8], w: usize, x: usize, y: usize, p: [u8; 4]) {
    let i = (y * w + x) * 4;
    buf[i..i + 4].copy_from_slice(&p);
}

/// Mood and reaction the animation plays in inside the app, so the effects (eyes, LED,
/// colors) match: the one in its name, or that of the mood that mentions it as `enter`,
/// `exit` or a variant.
fn look_for(name: &str, all: &clips::Clips) -> Look {
    let owner = all
        .iter()
        .find(|(_, c)| c.enter.as_deref() == Some(name) || c.exit.as_deref() == Some(name) || c.variants.iter().any(|v| v == name))
        .map(|(mood, _)| mood.as_str());
    let mood = match owner.unwrap_or(name) {
        "sleep" => Mood::Sleep,
        "happy" => Mood::Happy,
        "busy" => Mood::Busy,
        "alarm" => Mood::Alarm,
        _ => Mood::Idle,
    };
    let reaction = match name {
        "celebrate" => Some(Reaction::Celebrate),
        "surprised" => Some(Reaction::Surprised),
        _ => None,
    };
    Look { mood, reaction, blink: false, frame: 0, pose: None, ms: 0 }
}

/// Frames and durations; a looping animation plays laps until they add up to `MIN_LOOP_MS`.
fn timeline(clip: &Clip) -> Vec<(&clips::Grid, u32)> {
    let once: Vec<_> = clip.frames.iter().zip(clip.durations.iter().copied()).collect();
    if !clip.looping {
        return once;
    }
    let turns = MIN_LOOP_MS.div_ceil(clip.total_ms().max(1)).max(1);
    (0..turns).flat_map(|_| once.iter().copied()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn makes_a_gif_of_the_neko() {
        let dir = std::env::temp_dir().join(format!("miniutils-preview-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("neko.gif");
        let args = ["neko".to_string(), "--output".into(), out.to_string_lossy().into_owned()];
        let path = run(&args).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.starts_with(b"GIF89a"));
        assert!(run(&["neko".into(), "does-not-exist".into()]).unwrap_err().contains("it has:"));
        // the sleep enter transition shows as in the app: asleep
        let neko = clips::of("neko").unwrap();
        assert_eq!(look_for("yawn", &neko).mood, Mood::Sleep);
        assert_eq!(look_for("celebrate", &neko).reaction, Some(Reaction::Celebrate));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn old_output_flag_still_works() {
        let dir = std::env::temp_dir().join(format!("miniutils-preview-old-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("lumi.gif");
        let args = ["lumi".to_string(), "idle".into(), "--saida".into(), out.to_string_lossy().into_owned()];
        assert_eq!(run(&args).unwrap(), out);
        assert!(out.exists());
        // the old name is accepted but not advertised
        assert!(!usage().contains("--saida"));
        assert!(run(&["lumi".into(), "--output".into()]).unwrap_err().contains("--output"));
        assert!(run(&["lumi".into(), "--bogus".into()]).unwrap_err().contains("unknown option: --bogus"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
