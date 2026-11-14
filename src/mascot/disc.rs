//! The pixel-art CD: a 24×24 disc drawn in the app theme, with a spinning highlight.
//!
//! Like the mascots, it is drawn here (not loaded from the player): the MPRIS cover is a
//! JPEG file that would need Slint's image decoder, and pixel art matches the rest of the
//! hub. The iridescence uses the theme colors.

use super::render::darker;
use super::themes::Theme;

pub const SIZE: usize = 24;
/// Frames in one turn (22.5° each; ~2 s at 8 frames/s).
pub const STEPS: u8 = 16;

/// Outer radius, in pixels.
const R: f32 = 11.4;
/// Radius of the center hole.
const HOLE: f32 = 1.5;

/// The disc in one frame. `spin` is the frame within the turn; `lit = false` dims the disc
/// when there is no player.
pub fn pixels(theme: &Theme, spin: u8, lit: bool) -> [[u8; 4]; SIZE * SIZE] {
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
                // rim of the hole
                darker(theme.muted, 0.8)
            } else {
                // iridescence: two theme colors that turn with the disc
                let iri = mix(theme.visor, theme.fur, 0.5 + 0.5 * (2.0 * a - rot).cos());
                let mut c = mix(body, iri, 0.5);
                // tracks: thin data rings
                if ((r - HOLE) * 1.7) as i32 % 2 == 0 {
                    c = darker(c, 0.9);
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
                color = darker(color, 0.7);
            }
            let pixel = rgb(color);
            out[y * SIZE + x] = [pixel.0, pixel.1, pixel.2, 255];
        }
    }
    out
}

/// A frame as a Slint image (scaled up with `image-rendering: pixelated`).
pub fn image(theme: &Theme, spin: u8, lit: bool) -> slint::Image {
    let px = pixels(theme, spin, lit);
    let mut buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(SIZE as u32, SIZE as u32);
    for (dst, src) in buffer.make_mut_slice().iter_mut().zip(px.iter()) {
        *dst = slint::Rgba8Pixel { r: src[0], g: src[1], b: src[2], a: src[3] };
    }
    slint::Image::from_rgba8(buffer)
}

/// Last frame handed to the UI. Slint repaints when the image changes, so the disc is only
/// redrawn when the frame (or the theme) changes.
#[derive(Default)]
pub struct Cache {
    key: Option<(&'static str, u8, bool)>,
    img: Option<slint::Image>,
}

impl Cache {
    /// Redraws if needed; `true` when the image changed.
    pub fn update(&mut self, theme: &Theme, spin: u8, lit: bool) -> bool {
        let key = (theme.key, spin, lit);
        if self.key == Some(key) {
            return false;
        }
        self.key = Some(key);
        self.img = Some(image(theme, spin, lit));
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
        let p = pixels(t, 0, true);
        assert_eq!(px(&p, 0, 0)[3], 0);
        assert_eq!(px(&p, SIZE - 1, 0)[3], 0);
        // center: transparent hole (the disc's well shows through)
        assert_eq!(px(&p, SIZE / 2, SIZE / 2)[3], 0);
        // middle of the disc (between the hole and the rim): opaque
        assert_eq!(px(&p, SIZE / 2 + 7, SIZE / 2)[3], 255);
    }

    #[test]
    fn spinning_and_theme_change_the_pixels() {
        let t = themes::get("neon");
        assert_ne!(pixels(t, 0, true), pixels(t, 4, true));
        assert_ne!(pixels(t, 0, true), pixels(t, 0, false));
        assert_ne!(pixels(t, 0, true), pixels(themes::get("aconchego"), 0, true));
        // same frame, same image
        assert_eq!(pixels(t, 3, true), pixels(t, 3, true));
    }

    #[test]
    fn cache_redraws_only_on_change() {
        let t = themes::get("neon");
        let mut c = Cache::default();
        assert!(c.update(t, 0, true));
        assert!(!c.update(t, 0, true));
        assert!(c.update(t, 1, true));
        assert!(c.update(t, 1, false));
        assert_eq!((c.image().size().width, c.image().size().height), (SIZE as u32, SIZE as u32));
    }
}
