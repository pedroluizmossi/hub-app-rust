//! The pixel-art CD: a 24×24 disc drawn in the app theme, with a spinning highlight.
//!
//! Like the mascots, it is drawn here, in pixel art like the rest of the hub. The
//! iridescence uses the theme colors. When the player gives an album cover, it goes on the
//! label, already pixelated (`providers::media::Art`, `ART_SIZE` across). The cover itself
//! does not turn: rotating 16 pixels with no smoothing flickers, so the spin shows in the
//! moving highlight.

use super::render::{darker, lighter};
use super::themes::Theme;
use crate::providers::media::{ART_SIZE, Art};

pub const SIZE: usize = 24;
/// Frames in one turn (22.5° each; ~2 s at 8 frames/s).
pub const STEPS: u8 = 16;

/// Outer radius, in pixels.
const R: f32 = 11.4;
/// Radius of the center hole.
const HOLE: f32 = 1.5;
/// Radius of the label with the album cover: the cover spans its diameter, one cover pixel
/// per disc pixel.
const LABEL_R: f32 = ART_SIZE as f32 / 2.0;

/// The disc in one frame. `spin` is the frame within the turn; `lit = false` dims the disc
/// when there is no player; `art` (the album cover) goes on the label.
pub fn pixels(theme: &Theme, spin: u8, lit: bool, art: Option<&Art>) -> [[u8; 4]; SIZE * SIZE] {
    let mut out = [[0u8; 4]; SIZE * SIZE];
    let c = (SIZE as f32 - 1.0) / 2.0;
    let rot = spin as f32 / STEPS as f32 * std::f32::consts::TAU;
    // the disc's aluminum: not too light (it vanishes in the light theme), not dark
    let body = mix(theme.sub, theme.text, 0.18);

    for y in 0..SIZE {
        for x in 0..SIZE {
            let (dx, dy) = (x as f32 - c, y as f32 - c);
            let r = (dx * dx + dy * dy).sqrt();
            if r > R {
                continue; // outside the disc: transparent
            }
            let a = dy.atan2(dx);
            if r <= HOLE {
                continue; // hole: transparent, shows the well's background
            }
            let mut color = if r <= HOLE + 0.9 {
                // rim of the hole: a little lighter than the muted tone, so it shows
                lighter(theme.muted, 1.25)
            } else if let Some(art) = art.filter(|_| r <= LABEL_R) {
                // label: the cover pixel under this disc pixel
                let side = ART_SIZE as usize;
                let at = |d: f32| ((d + LABEL_R).floor().max(0.0) as usize).min(side - 1);
                let [r8, g8, b8] = art.px[at(dy) * side + at(dx)];
                let mut c = ((r8 as u32) << 16) | ((g8 as u32) << 8) | b8 as u32;
                // the highlight passes over the label too, softer
                let spec = (a - rot).cos().max(0.0).powf(6.0);
                c = mix(c, 0xffffff, spec * 0.25);
                // edge of the label: a thin dark line where the data area starts
                if r > LABEL_R - 0.8 {
                    c = darker(c, 1.43);
                }
                c
            } else {
                // iridescence: two theme colors that turn with the disc
                let iri = mix(theme.visor, theme.fur, 0.5 + 0.5 * (2.0 * a - rot).cos());
                let mut c = mix(body, iri, 0.5);
                // tracks: thin data rings
                if ((r - HOLE) * 1.7) as i32 % 2 == 0 {
                    c = darker(c, 1.11);
                }
                // spinning highlight
                let spec = (a - rot).cos().max(0.0).powf(6.0);
                c = mix(c, 0xffffff, spec * 0.7);
                // outer rim
                if r > R - 0.9 {
                    c = mix(c, theme.text, 0.45);
                }
                c
            };
            if !lit {
                color = darker(color, 1.43);
            }
            let pixel = rgb(color);
            out[y * SIZE + x] = [pixel.0, pixel.1, pixel.2, 255];
        }
    }
    out
}

/// A frame as a Slint image (scaled up with `image-rendering: pixelated`).
pub fn image(theme: &Theme, spin: u8, lit: bool, art: Option<&Art>) -> slint::Image {
    let px = pixels(theme, spin, lit, art);
    let mut buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(SIZE as u32, SIZE as u32);
    for (dst, src) in buffer.make_mut_slice().iter_mut().zip(px.iter()) {
        *dst = slint::Rgba8Pixel { r: src[0], g: src[1], b: src[2], a: src[3] };
    }
    slint::Image::from_rgba8(buffer)
}

/// Last frame handed to the UI. Slint repaints when the image changes, so the disc is only
/// redrawn when the frame, the theme, the state or the cover change.
#[derive(Default)]
pub struct Cache {
    key: Option<(&'static str, u8, bool, Option<u64>)>,
    img: Option<slint::Image>,
}

impl Cache {
    /// Redraws if needed; `true` when the image changed.
    pub fn update(&mut self, theme: &Theme, spin: u8, lit: bool, art: Option<&Art>) -> bool {
        let key = (theme.key, spin, lit, art.map(|a| a.id));
        if self.key == Some(key) {
            return false;
        }
        self.key = Some(key);
        self.img = Some(image(theme, spin, lit, art));
        true
    }

    pub fn image(&self) -> slint::Image {
        self.img.clone().unwrap_or_default()
    }
}

fn rgb(c: u32) -> (u8, u8, u8) {
    (((c >> 16) & 0xff) as u8, ((c >> 8) & 0xff) as u8, (c & 0xff) as u8)
}

/// Blends `b` over `a`: 0 = only `a`, 1 = only `b`.
fn mix(a: u32, b: u32, t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let (ar, ag, ab) = rgb(a);
    let (br, bg, bb) = rgb(b);
    let f = |x: u8, y: u8| -> u32 { (x as f32 + (y as f32 - x as f32) * t).round() as u32 };
    (f(ar, br) << 16) | (f(ag, bg) << 8) | f(ab, bb)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mascot::themes;

    fn px(px: &[[u8; 4]; SIZE * SIZE], x: usize, y: usize) -> [u8; 4] {
        px[y * SIZE + x]
    }

    #[test]
    fn outside_is_transparent_and_center_is_the_hole() {
        let t = themes::get("neon");
        let p = pixels(t, 0, true, None);
        assert_eq!(px(&p, 0, 0)[3], 0);
        assert_eq!(px(&p, SIZE - 1, 0)[3], 0);
        // center: transparent hole (the disc's well shows through)
        assert_eq!(px(&p, SIZE / 2, SIZE / 2)[3], 0);
        // middle of the disc (between the hole and the rim): opaque
        assert_eq!(px(&p, SIZE / 2 + 7, SIZE / 2)[3], 255);
    }

    #[test]
    fn cover_goes_on_the_label_and_the_rim_stays() {
        let t = themes::get("neon");
        let px_count = (ART_SIZE * ART_SIZE) as usize;
        let art = Art { id: 1, px: std::sync::Arc::new(vec![[200, 10, 10]; px_count]) };
        let plain = pixels(t, 0, true, None);
        let with = pixels(t, 0, true, Some(&art));
        // left of the hole, inside the label and away from the highlight (which starts on
        // the right at spin 0): exactly the cover's color
        assert_eq!(px(&with, SIZE / 2 - 5, SIZE / 2), [200, 10, 10, 255]);
        // the hole and the outer data area are the same with or without a cover
        assert_eq!(px(&with, SIZE / 2, SIZE / 2), px(&plain, SIZE / 2, SIZE / 2));
        assert_eq!(px(&with, SIZE / 2 - 11, SIZE / 2), px(&plain, SIZE / 2 - 11, SIZE / 2));
        // another cover redraws the cached frame
        let mut c = Cache::default();
        assert!(c.update(t, 0, true, Some(&art)));
        assert!(!c.update(t, 0, true, Some(&art)));
        assert!(c.update(t, 0, true, Some(&Art { id: 2, ..art.clone() })));
    }

    #[test]
    fn spinning_and_theme_change_the_pixels() {
        let t = themes::get("neon");
        assert_ne!(pixels(t, 0, true, None), pixels(t, 4, true, None));
        assert_ne!(pixels(t, 0, true, None), pixels(t, 0, false, None));
        assert_ne!(pixels(t, 0, true, None), pixels(themes::get("aconchego"), 0, true, None));
        // same frame, same image
        assert_eq!(pixels(t, 3, true, None), pixels(t, 3, true, None));
    }

    #[test]
    fn unlit_disc_is_darker() {
        let max = |p: [u8; 4]| p[0].max(p[1]).max(p[2]);
        for key in ["neon", "aconchego"] {
            let t = themes::get(key);
            let (lit, unlit) = (pixels(t, 0, true, None), pixels(t, 0, false, None));
            // a data-area pixel, away from the highlight
            let (x, y) = (SIZE / 2 - 7, SIZE / 2);
            assert_eq!(px(&lit, x, y)[3], 255);
            assert!(max(px(&unlit, x, y)) < max(px(&lit, x, y)), "{key}: the unlit disc got lighter");
        }
    }

    #[test]
    fn cache_redraws_only_on_change() {
        let t = themes::get("neon");
        let mut c = Cache::default();
        assert!(c.update(t, 0, true, None));
        assert!(!c.update(t, 0, true, None));
        assert!(c.update(t, 1, true, None));
        assert!(c.update(t, 1, false, None));
        assert_eq!((c.image().size().width, c.image().size().height), (SIZE as u32, SIZE as u32));
    }
}
