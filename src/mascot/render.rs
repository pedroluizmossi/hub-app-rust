//! Draws a mascot into a 16×14 RGBA frame from sprite + theme + mood + frame.
//!
//! Same color rules as CyberDock's `Mascot.qml`. Blinking, the LED and the glitch come from
//! the frame number; floating/jumping is done in Slint (without redrawing).

use super::pack;
use super::sprites::{Pixel, Sprite};
use super::themes::Theme;
use super::anim::{Look, Reaction};

pub const W: usize = 16;
pub const H: usize = 14;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub enum Mood {
    #[default]
    Sleep,
    Idle,
    Happy,
    Busy,
    Alarm,
}

impl Mood {
    pub fn as_str(self) -> &'static str {
        match self {
            Mood::Sleep => "sleep",
            Mood::Idle => "idle",
            Mood::Happy => "happy",
            Mood::Busy => "busy",
            Mood::Alarm => "alarm",
        }
    }

    /// The worse of two moods (sleep < idle < happy < busy < alarm).
    pub fn worst(self, other: Mood) -> Mood {
        self.max(other)
    }

    /// Mood for a 0..100 metric: idle sleeps, normal floats, loaded jumps, saturated alarms.
    pub fn from_level(value: f32, warn: f32, alarm: f32, available: bool) -> Mood {
        if !available || value < 3.0 {
            Mood::Sleep
        } else if value >= alarm {
            Mood::Alarm
        } else if value >= warn {
            Mood::Busy
        } else {
            Mood::Idle
        }
    }
}

// ---- colors -----------------------------------------------------------------

fn rgb(c: u32) -> (f32, f32, f32) {
    (
        ((c >> 16) & 0xff) as f32 / 255.0,
        ((c >> 8) & 0xff) as f32 / 255.0,
        (c & 0xff) as f32 / 255.0,
    )
}

fn pack(r: f32, g: f32, b: f32) -> u32 {
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    (q(r) << 16) | (q(g) << 8) | q(b)
}

fn to_hsv(c: u32) -> (f32, f32, f32) {
    let (r, g, b) = rgb(c);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let h = if d == 0.0 {
        0.0
    } else if max == r {
        60.0 * (((g - b) / d).rem_euclid(6.0))
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    let s = if max == 0.0 { 0.0 } else { d / max };
    (h, s, max)
}

fn from_hsv(h: f32, s: f32, v: f32) -> u32 {
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0).rem_euclid(2.0) - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match (h / 60.0) as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    pack(r + m, g + m, b + m)
}

/// Like `Qt.lighter`: multiplies the brightness; past the top, it takes away saturation.
pub fn lighter(c: u32, f: f32) -> u32 {
    let (h, mut s, mut v) = to_hsv(c);
    v *= f;
    if v > 1.0 {
        s = (s - (v - 1.0)).max(0.0);
        v = 1.0;
    }
    from_hsv(h, s, v)
}

/// Like `Qt.darker`: divides the brightness.
pub fn darker(c: u32, f: f32) -> u32 {
    let (h, s, v) = to_hsv(c);
    from_hsv(h, s, v / f)
}

const SKIN: u32 = 0xffe0d0;
const WHITE: u32 = 0xffffff;

struct Painter<'a> {
    s: &'a Sprite,
    t: &'a Theme,
    mood: Mood,
    frame: u32,
    blink: bool,
    reaction: Option<Reaction>,
    /// Row of the closed-eye stroke in this frame.
    closed_row: usize,
    fur: u32,
    ear: u32,
}

impl Painter<'_> {
    fn eye(&self) -> Option<u32> {
        let f = self.frame;
        match self.reaction {
            Some(Reaction::Celebrate) => return Some(lighter(self.t.ok, 1.4)),
            Some(Reaction::Surprised) => return Some(WHITE),
            None => {}
        }
        match self.mood {
            Mood::Sleep => None,
            Mood::Alarm => Some(if f % 4 < 2 { self.t.alarm } else { lighter(self.t.alarm, 1.6) }),
            Mood::Busy => Some(if f % 6 < 3 { self.t.warn } else { darker(self.t.warn, 1.35) }),
            Mood::Happy => (f % 10 < 7).then(|| lighter(self.t.ok, 1.25)),
            Mood::Idle => (!self.blink).then(|| lighter(self.t.outline, 2.2)),
        }
    }

    fn led(&self) -> u32 {
        let on = self.frame % 4 < 2;
        match self.reaction {
            Some(Reaction::Celebrate) => return if self.frame.is_multiple_of(2) { self.t.ok } else { lighter(self.t.visor, 1.5) },
            Some(Reaction::Surprised) => return lighter(self.t.visor, 1.6),
            None => {}
        }
        match self.mood {
            Mood::Sleep => darker(self.t.visor, 2.8),
            Mood::Alarm => if on { self.t.alarm } else { self.t.outline },
            Mood::Busy => if on { self.t.warn } else { darker(self.t.warn, 1.8) },
            Mood::Happy => if on { self.t.ok } else { self.t.visor },
            Mood::Idle => self.t.visor,
        }
    }

    fn cell(&self, ch: u8, y: usize) -> Option<u32> {
        let t = self.t;
        let dim = self.mood == Mood::Sleep;
        let alarm = self.mood == Mood::Alarm;
        Some(match ch {
            b'K' => t.outline,
            b'M' => self.ear,
            b'H' => if dim { self.fur } else { lighter(self.fur, 1.45) },
            b'S' => darker(self.fur, if dim { 1.6 } else { 1.35 }),
            b'B' => if alarm { self.fur } else { t.blush },
            b'P' => {
                if y >= self.s.shade_from {
                    darker(self.fur, 1.35)
                } else if dim {
                    darker(self.fur, 1.2)
                } else {
                    self.fur
                }
            }
            b'T' => if alarm { t.alarm } else { t.outline },
            b'C' => {
                if dim {
                    darker(t.visor, 2.8)
                } else if alarm {
                    darker(t.alarm, 1.6)
                } else {
                    t.visor
                }
            }
            b'F' => if dim { darker(SKIN, 1.15) } else { SKIN },
            b'D' => lighter(t.outline, 3.2),
            b'A' => self.led(),
            b'E' | b'W' => match self.eye() {
                // closed eye: a stroke on the eye's last row, background on the rest
                None if y == self.closed_row => t.outline,
                None => return self.cell(self.s.eye_bg, y),
                Some(_) if ch == b'W' => WHITE,
                Some(e) => e,
            },
            _ => return None,
        })
    }
}

/// A sprite with no drawing: gives the role colors to whoever has no `Sprite` of their own
/// (the PNG sprites with `[palette]`, see `role_color`).
static BLANK: Sprite = Sprite {
    key: "",
    label: "",
    eye_bg: b'.',
    shade_from: usize::MAX,
    fur: None,
    ear: None,
    pixel: None,
    grid: [""; 14],
};

/// Color of color role `role` (`K`, `P`, `E`, `A`…, see `sprites.rs`) in `theme` and
/// `mood`, without blinking: how a PNG sprite with `[palette]` follows the theme. A closed
/// eye (asleep) takes the outline color. `None` for a letter that isn't a role.
pub fn role_color(theme: &Theme, role: u8, mood: Mood) -> Option<u32> {
    let p = Painter {
        s: &BLANK,
        t: theme,
        mood,
        frame: 0,
        blink: false,
        reaction: None,
        closed_row: usize::MAX,
        fur: theme.fur,
        ear: theme.ear,
    };
    match role {
        b'E' | b'W' if mood == Mood::Sleep => Some(theme.outline),
        b'.' => None,
        _ => p.cell(role, 0),
    }
}

/// Color of a pixel of a "pixel" mascot (own palette): only the eye follows mood/theme.
fn pixel_cell(px: &Pixel, row: &[u8], x: usize, theme: &Theme, look: &Look) -> Option<u32> {
    let (mood, frame) = (look.mood, look.frame);
    let ch = row[x];
    let dim = |c: u32| if mood == Mood::Sleep { darker(c, 1.2) } else { c };
    if ch != b'e' {
        return px.color(ch).map(dim);
    }
    let open = match (look.reaction, mood) {
        (Some(Reaction::Celebrate), _) => Some(lighter(theme.ok, 1.4)),
        (Some(Reaction::Surprised), _) => Some(WHITE),
        (None, Mood::Sleep) => None,
        (None, Mood::Alarm) => Some(if frame % 4 < 2 { theme.alarm } else { lighter(theme.alarm, 1.5) }),
        (None, Mood::Busy) => Some(if frame % 6 < 3 { theme.warn } else { darker(theme.warn, 1.35) }),
        (None, Mood::Happy | Mood::Idle) => (!look.blink).then_some(px.eye),
    };
    // closed eye: takes the neighbor's color (darker), like an eyelid
    open.or_else(|| {
        let side = row.get(x.wrapping_sub(1)).or(row.get(x + 1)).and_then(|c| px.color(*c))?;
        Some(darker(side, 1.4))
    })
}

/// Draws an RGBA frame (16×14, row by row) of the mascot, blinking by the frame number.
pub fn render(sprite: &Sprite, theme: &Theme, mood: Mood, frame: u32) -> [[u8; 4]; W * H] {
    render_look(sprite, theme, &Look::from_frame(mood, frame))
}

/// Draws the mascot as the `Animator` said (its own blink and reaction).
pub fn render_look(sprite: &Sprite, theme: &Theme, look: &Look) -> [[u8; 4]; W * H] {
    let (mood, frame) = (look.mood, look.frame);
    // pose of the drawn animation, if any (see `clips.rs`); otherwise the base drawing
    let grid = look.pose.as_ref().unwrap_or(&sprite.grid);
    if let Some(px) = &sprite.pixel {
        let glitch = mood == Mood::Alarm && frame % 6 < 2;
        let mut out = [[0u8; 4]; W * H];
        for (y, row) in grid.iter().enumerate() {
            let row = row.as_bytes();
            let shift: isize = if glitch && (4..8).contains(&y) { if y % 2 == 0 { 1 } else { -1 } } else { 0 };
            for x in 0..row.len().min(W) {
                let Some(c) = pixel_cell(px, row, x, theme, look) else { continue };
                let dx = x as isize + shift;
                if (0..W as isize).contains(&dx) {
                    out[y * W + dx as usize] = [(c >> 16) as u8, (c >> 8) as u8, c as u8, 255];
                }
            }
        }
        return out;
    }
    let p = Painter {
        s: sprite,
        t: theme,
        mood,
        frame,
        blink: look.blink,
        reaction: look.reaction,
        closed_row: sprite.closed_row(grid),
        fur: sprite.fur.unwrap_or(theme.fur),
        ear: sprite.ear.unwrap_or(theme.ear),
    };
    // alarm glitch: a few rows slide 1 px sideways
    let glitch = mood == Mood::Alarm && frame % 6 < 2;
    let mut out = [[0u8; 4]; W * H];
    for (y, row) in grid.iter().enumerate() {
        let shift: isize = if glitch && (4..8).contains(&y) { if y % 2 == 0 { 1 } else { -1 } } else { 0 };
        for (x, ch) in row.bytes().enumerate() {
            let Some(c) = p.cell(ch, y) else { continue };
            let dx = x as isize + shift;
            if !(0..W as isize).contains(&dx) {
                continue;
            }
            out[y * W + dx as usize] = [(c >> 16) as u8, (c >> 8) as u8, c as u8, 255];
        }
    }
    out
}

/// A frame as a Slint image (Slint scales it up with `image-rendering: pixelated`). An
/// imported mascot (`pack.rs`) comes out at the size of its PNG.
pub fn image(sprite: &Sprite, theme: &Theme, mood: Mood, frame: u32) -> slint::Image {
    if let Some(art) = pack::art(sprite) {
        return art.image_in(art.pick(&Look::from_frame(mood, frame)), theme, mood);
    }
    to_image(&render(sprite, theme, mood, frame))
}

fn to_image(px: &[[u8; 4]; W * H]) -> slint::Image {
    let mut buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(W as u32, H as u32);
    for (dst, src) in buf.make_mut_slice().iter_mut().zip(px.iter()) {
        *dst = slint::Rgba8Pixel { r: src[0], g: src[1], b: src[2], a: src[3] };
    }
    slint::Image::from_rgba8(buf)
}

/// Last frame handed to the UI. Slint repaints the window when an image changes, so the
/// image is only rebuilt when the pixels change (a sleeping or still mascot doesn't repaint).
#[derive(Default)]
pub struct Cache {
    px: Option<Box<[[u8; 4]; W * H]>>,
    /// The imported mascot's frame from last time (instead of `px`).
    art: Option<(usize, pack::FrameKey, &'static str, Mood)>,
    img: slint::Image,
}

impl Cache {
    /// Updates to what the `Animator` said to draw. Returns `true` if it changed; then
    /// `image()` has the new image.
    pub fn update_look(&mut self, sprite: &Sprite, theme: &Theme, look: &Look) -> bool {
        if let Some(art) = pack::art(sprite) {
            // theme and mood count only for a PNG with `[palette]` (they recolor it)
            let (t, m) = if art.themed() { (theme.key, look.mood) } else { ("", Mood::Sleep) };
            let key = (art as *const pack::Art as usize, art.pick(look), t, m);
            if self.art == Some(key) {
                return false;
            }
            self.img = art.image_in(key.1, theme, look.mood);
            self.art = Some(key);
            self.px = None;
            return true;
        }
        self.art = None;
        let px = render_look(sprite, theme, look);
        if self.px.as_deref() == Some(&px) {
            return false;
        }
        self.img = to_image(&px);
        self.px = Some(Box::new(px));
        true
    }

    /// Image of the last updated frame (cheap: the image is shared).
    pub fn image(&self) -> slint::Image {
        self.img.clone()
    }
}

/// Square frame scaled `scale`× in RGBA: the side and the pixels. Base of the tray icons.
pub fn rgba_square(sprite: &Sprite, theme: &Theme, mood: Mood, frame: u32, scale: usize) -> (i32, Vec<u8>) {
    if let Some(art) = pack::art(sprite) {
        return art.square(art.pick(&Look::from_frame(mood, frame)), theme, mood, W * scale);
    }
    let px = render(sprite, theme, mood, frame);
    let side = W * scale;
    let top = (W - H) / 2 * scale; // centers the 14 rows in a 16-wide square
    let mut data = vec![0u8; side * side * 4];
    for y in 0..H * scale {
        for x in 0..side {
            let i = ((y + top) * side + x) * 4;
            data[i..i + 4].copy_from_slice(&px[(y / scale) * W + x / scale]);
        }
    }
    (side as i32, data)
}

/// Like `rgba_square`, in big-endian ARGB32 (the SNI tray icon format, on Linux).
#[cfg_attr(windows, allow(dead_code))]
pub fn argb_square(sprite: &Sprite, theme: &Theme, mood: Mood, frame: u32, scale: usize) -> (i32, Vec<u8>) {
    let (side, mut data) = rgba_square(sprite, theme, mood, frame, scale);
    for px in data.as_chunks_mut::<4>().0 {
        px.rotate_right(1); // [r, g, b, a] -> [a, r, g, b]
    }
    (side, data)
}

/// Theme colors for the UI (each app sends its own).
pub fn colors(t: &Theme) -> crate::ui::Colors {
    crate::ui::Colors {
        fur: color(t.fur),
        visor: color(t.visor),
        outline: color(t.outline),
        ok: color(t.ok),
        warn: color(t.warn),
        alarm: color(t.alarm),
        sleep: color(t.sleep),
        bg0: color(t.bg0),
        bg1: color(t.bg1),
        text: color(t.text),
        sub: color(t.sub),
        muted: color(t.muted),
    }
}

/// A theme color as `slint::Color`.
pub fn color(c: u32) -> slint::Color {
    slint::Color::from_rgb_u8((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mascot::{sprites, themes};

    #[test]
    fn every_mascot_is_16x14() {
        for s in sprites::SPRITES {
            for row in s.grid {
                assert_eq!(row.len(), W, "{} has a row of the wrong width", s.key);
            }
        }
    }

    #[test]
    fn pixel_mascots_have_a_color_for_every_letter() {
        for s in sprites::SPRITES.iter().filter(|s| s.pixel.is_some()) {
            let px = s.pixel.as_ref().unwrap();
            for row in s.grid {
                for ch in row.bytes().filter(|c| *c != b'.' && *c != b'e') {
                    assert!(px.color(ch).is_some(), "{}: no color for '{}'", s.key, ch as char);
                }
            }
            assert!(s.grid.iter().any(|r| r.contains('e')), "{} has no eye", s.key);
        }
        assert!(sprites::SPRITES.iter().any(|s| s.key == "lumi" && s.pixel.is_some()));
    }

    #[test]
    fn pixel_mascot_closes_its_eye_and_changes_on_alarm() {
        let s = sprites::get("capivara");
        let t = themes::get("neon");
        let awake = render(s, t, Mood::Idle, 1);
        assert_ne!(awake, render(s, t, Mood::Sleep, 1));
        assert_ne!(awake, render(s, t, Mood::Alarm, 3));
        assert!(awake.iter().any(|p| p[3] == 255));
    }

    #[test]
    fn eye_closes_when_asleep() {
        let s = sprites::get("neko");
        let t = themes::get("neon");
        let awake = render(s, t, Mood::Idle, 1);
        let asleep = render(s, t, Mood::Sleep, 1);
        assert_ne!(awake, asleep);
    }

    #[test]
    fn worst_mood() {
        assert_eq!(Mood::Idle.worst(Mood::Alarm), Mood::Alarm);
        assert_eq!(Mood::Sleep.worst(Mood::Idle), Mood::Idle);
    }

    #[test]
    fn lighter_darker() {
        assert_eq!(darker(0xffffff, 2.0), 0x808080);
        assert_eq!(lighter(0x808080, 2.0), 0xffffff);
    }
}
