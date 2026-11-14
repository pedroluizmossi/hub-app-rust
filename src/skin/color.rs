//! Color math on 0xRRGGBB: mixing, contrast (WCAG) and parsing `#RRGGBB`.

/// `a` with weight `t` over `b`.
pub fn mix(a: u32, b: u32, t: f32) -> u32 {
    let ch = |s: u32| -> u32 {
        let (x, y) = (((a >> s) & 0xff) as f32, ((b >> s) & 0xff) as f32);
        ((x * t + y * (1.0 - t)).round() as u32) << s
    };
    ch(16) | ch(8) | ch(0)
}

/// Relative luminance (WCAG 2), from 0 (black) to 1 (white).
pub fn luminance(c: u32) -> f32 {
    let lin = |v: u32| {
        let v = v as f32 / 255.0;
        if v <= 0.040_45 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * lin((c >> 16) & 0xff) + 0.7152 * lin((c >> 8) & 0xff) + 0.0722 * lin(c & 0xff)
}

/// Contrast ratio between two colors (1 to 21).
pub fn contrast(a: u32, b: u32) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// Darkens (light background) or lightens (dark background) `c` until it has `min` contrast
/// over each color of `over`. Used for the accent color the user picks, which must be
/// readable as text on any skin.
pub fn ensure_contrast(c: u32, over: &[u32], min: f32) -> u32 {
    let light_bg = over.iter().map(|&b| luminance(b)).sum::<f32>() / over.len().max(1) as f32 > 0.4;
    let target = if light_bg { 0x000000 } else { 0xffffff };
    let ok = |x: u32| over.iter().all(|&b| contrast(x, b) >= min);
    (0..=20).map(|i| mix(target, c, i as f32 / 20.0)).find(|&x| ok(x)).unwrap_or(target)
}

/// Text over `bg`: the more readable of a near-black and white.
pub fn ink_on(bg: u32) -> u32 {
    const DARK: u32 = 0x0b0b10;
    if contrast(DARK, bg) >= contrast(0xffffff, bg) { DARK } else { 0xffffff }
}

/// `#RRGGBB` (or `RRGGBB`) to 0xRRGGBB.
pub fn parse_hex(s: &str) -> Option<u32> {
    let h = s.trim().strip_prefix('#').unwrap_or(s.trim());
    (h.len() == 6 && h.chars().all(|c| c.is_ascii_hexdigit())).then(|| u32::from_str_radix(h, 16).ok()).flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixing() {
        assert_eq!(mix(0xffffff, 0x000000, 1.0), 0xffffff);
        assert_eq!(mix(0xffffff, 0x000000, 0.0), 0x000000);
        assert_eq!(mix(0xff0000, 0x0000ff, 0.5), 0x800080);
    }

    #[test]
    fn contrast_and_adjustment() {
        assert!((contrast(0x000000, 0xffffff) - 21.0).abs() < 0.01);
        assert!((contrast(0x777777, 0x777777) - 1.0).abs() < 0.01);
        // cyan on light paper: darkens until readable
        let c = ensure_contrast(0x3df2ff, &[0xfbf1de], 3.0);
        assert!(contrast(c, 0xfbf1de) >= 3.0 && c != 0x3df2ff);
        // cyan on a dark background: already works, stays the same
        assert_eq!(ensure_contrast(0x3df2ff, &[0x0b0912], 3.0), 0x3df2ff);
        assert_eq!(ink_on(0xffffff), 0x0b0b10);
        assert_eq!(ink_on(0x000000), 0xffffff);
    }

    #[test]
    fn parses_hex() {
        assert_eq!(parse_hex("#3DF2FF"), Some(0x3df2ff));
        assert_eq!(parse_hex("0b0912"), Some(0x0b0912));
        assert_eq!(parse_hex("#fff"), None);
        assert_eq!(parse_hex("#gggggg"), None);
    }
}
