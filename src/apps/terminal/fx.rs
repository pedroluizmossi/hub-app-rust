//! The terminal window's effects, drawn once per size and skin (never per frame): the
//! CRT overlay (scanlines and vignette) and the light elements that the screen moves on
//! its own clock (rain behind the text, the scan band over it; see `ui/ambient.slint`).
//!
//! Like the panel's backdrop, everything that moves is a few small rectangles: the
//! software renderer repaints only where they pass.

use slint::{Rgba8Pixel, SharedPixelBuffer};

use crate::skin::Skin;
use crate::skin::backdrop::{self, Ambient, Backdrop, Layer, Paint};

/// Rain behind the text: thin, in the skin's accent, faint enough not to compete with it.
static RAIN: Backdrop = Backdrop {
    key: "terminal-rain",
    label: "",
    layers: &[Layer::Rain { count: 36, color: Paint::Token("accent"), alpha: 0.22, speed: 200.0, length: 14.0 }],
};

/// The band that goes down the screen, over the text (part of the CRT effect).
static SCAN: Backdrop = Backdrop {
    key: "terminal-scan",
    label: "",
    layers: &[Layer::Scan { color: Paint::Token("accent"), alpha: 0.06, height: 110.0, speed: 40.0 }],
};

/// Drops of rain for an area of `w`×`h` logical pixels.
pub fn rain(skin: &Skin, w: f32, h: f32) -> Vec<Ambient> {
    backdrop::ambient(&RAIN, skin, w, h)
}

/// The CRT scan band for an area of `w`×`h` logical pixels.
pub fn scan(skin: &Skin, w: f32, h: f32) -> Vec<Ambient> {
    backdrop::ambient(&SCAN, skin, w, h)
}

/// One dark line every this many logical pixels.
const SCANLINE_STEP: u32 = 3;
const SCANLINE_ALPHA: f32 = 0.16;
/// Darkest corner of the vignette.
const VIGNETTE_ALPHA: f32 = 0.42;
/// Distance from the center (0 = center, 1 = corner) where the vignette starts.
const VIGNETTE_START: f32 = 0.55;

/// The CRT overlay: black with transparency, `pw`×`ph` physical pixels at scale `scale`.
/// Scanlines follow logical pixels, so they keep their size on a HiDPI screen.
pub fn crt_overlay(pw: u32, ph: u32, scale: f32) -> SharedPixelBuffer<Rgba8Pixel> {
    let (pw, ph) = (pw.max(1), ph.max(1));
    let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(pw, ph);
    let (cx, cy) = (pw as f32 / 2.0, ph as f32 / 2.0);
    let scale = scale.max(0.5);
    let px = buf.make_mut_slice();
    for y in 0..ph {
        let line = ((y as f32 / scale) as u32) % SCANLINE_STEP == SCANLINE_STEP - 1;
        let dy = (y as f32 + 0.5 - cy) / cy;
        for x in 0..pw {
            let dx = (x as f32 + 0.5 - cx) / cx;
            // 0 in the center, 1 in the corners
            let r = ((dx * dx + dy * dy) / 2.0).sqrt();
            let t = ((r - VIGNETTE_START) / (1.0 - VIGNETTE_START)).clamp(0.0, 1.0);
            let vignette = t * t * (3.0 - 2.0 * t) * VIGNETTE_ALPHA;
            let lines = if line { SCANLINE_ALPHA } else { 0.0 };
            let a = 1.0 - (1.0 - vignette) * (1.0 - lines);
            px[(y * pw + x) as usize] = Rgba8Pixel { r: 0, g: 0, b: 0, a: (a * 255.0).round() as u8 };
        }
    }
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alpha(buf: &SharedPixelBuffer<Rgba8Pixel>, x: u32, y: u32) -> u8 {
        buf.as_slice()[(y * buf.width() + x) as usize].a
    }

    #[test]
    fn crt_darkens_lines_and_corners() {
        let buf = crt_overlay(300, 200, 1.0);
        assert_eq!((buf.width(), buf.height()), (300, 200));
        // center: clear on a normal row, a scanline on every 3rd
        assert_eq!(alpha(&buf, 150, 99), 0);
        assert!(alpha(&buf, 150, 98) > 30, "row 98 is a scanline");
        // the corners are the darkest
        assert!(alpha(&buf, 0, 0) > 90);
        assert!(alpha(&buf, 0, 0) > alpha(&buf, 75, 50));
        // at scale 2, a scanline is 2 physical pixels tall
        let hi = crt_overlay(300, 200, 2.0);
        assert!(alpha(&hi, 150, 100) > 30 && alpha(&hi, 150, 101) > 30);
        assert_eq!(alpha(&hi, 150, 102), 0);
    }

    #[test]
    fn rain_and_scan_follow_the_area() {
        let skin = crate::skin::BUILTIN[0].clone();
        let small = rain(&skin, 400.0, 300.0);
        let big = rain(&skin, 1600.0, 900.0);
        assert!(!small.is_empty() && big.len() > small.len());
        assert!(small.iter().all(|a| a.kind == "fall"));
        let s = scan(&skin, 800.0, 500.0);
        assert_eq!(s.len(), 1);
        assert_eq!((s[0].kind, s[0].w), ("scan", 800.0));
    }
}
