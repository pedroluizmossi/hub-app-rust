//! Desenha uma mascote num quadro RGBA 16×14 a partir de sprite + tema + humor + quadro.
//!
//! Mesmas regras de cor do `Mascot.qml` do CyberDock. A animação de piscar, LED e glitch
//! vem do número do quadro; flutuar/pular é feito no Slint (sem redesenhar).

use super::sprites::Sprite;
use super::themes::Theme;

pub const W: usize = 16;
pub const H: usize = 14;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
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

    /// O pior dos dois humores (sleep < idle < happy < busy < alarm).
    pub fn worst(self, other: Mood) -> Mood {
        self.max(other)
    }

    /// Humor de uma métrica 0..100: parada dorme, normal flutua, carregada pula, saturada alarma.
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

// ---- cores ------------------------------------------------------------------

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

/// Como `Qt.lighter`: multiplica o brilho; se estourar, tira saturação.
pub fn lighter(c: u32, f: f32) -> u32 {
    let (h, mut s, mut v) = to_hsv(c);
    v *= f;
    if v > 1.0 {
        s = (s - (v - 1.0)).max(0.0);
        v = 1.0;
    }
    from_hsv(h, s, v)
}

/// Como `Qt.darker`: divide o brilho.
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
    fur: u32,
    ear: u32,
}

impl Painter<'_> {
    fn eye(&self) -> Option<u32> {
        let f = self.frame;
        match self.mood {
            Mood::Sleep => None,
            Mood::Alarm => Some(if f % 4 < 2 { self.t.alarm } else { lighter(self.t.alarm, 1.6) }),
            Mood::Busy => Some(if f % 6 < 3 { self.t.warn } else { darker(self.t.warn, 1.35) }),
            Mood::Happy => (f % 10 < 7).then(|| lighter(self.t.ok, 1.25)),
            Mood::Idle => (f % 25 != 0).then(|| lighter(self.t.outline, 2.2)),
        }
    }

    fn led(&self) -> u32 {
        let on = self.frame % 4 < 2;
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
                // olho fechado: traço na última linha do olho, fundo no resto
                None if y == self.s.closed_row() => t.outline,
                None => return self.cell(self.s.eye_bg, y),
                Some(_) if ch == b'W' => WHITE,
                Some(e) => e,
            },
            _ => return None,
        })
    }
}

/// Desenha um quadro RGBA (16×14, linha a linha) da mascote.
pub fn render(sprite: &Sprite, theme: &Theme, mood: Mood, frame: u32) -> [[u8; 4]; W * H] {
    let p = Painter {
        s: sprite,
        t: theme,
        mood,
        frame,
        fur: sprite.fur.unwrap_or(theme.fur),
        ear: sprite.ear.unwrap_or(theme.ear),
    };
    // glitch do alarme: algumas linhas deslizam 1 px para o lado
    let glitch = mood == Mood::Alarm && frame % 6 < 2;
    let mut out = [[0u8; 4]; W * H];
    for (y, row) in sprite.grid.iter().enumerate() {
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

/// Quadro como imagem do Slint (o Slint amplia com `image-rendering: pixelated`).
pub fn image(sprite: &Sprite, theme: &Theme, mood: Mood, frame: u32) -> slint::Image {
    to_image(&render(sprite, theme, mood, frame))
}

fn to_image(px: &[[u8; 4]; W * H]) -> slint::Image {
    let mut buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(W as u32, H as u32);
    for (dst, src) in buf.make_mut_slice().iter_mut().zip(px.iter()) {
        *dst = slint::Rgba8Pixel { r: src[0], g: src[1], b: src[2], a: src[3] };
    }
    slint::Image::from_rgba8(buf)
}

/// Último quadro entregue à UI. O Slint redesenha a janela quando uma imagem muda, então
/// só se refaz a imagem quando os pixels mudam (mascote dormindo ou parada não redesenha).
#[derive(Default)]
pub struct Cache {
    px: Option<Box<[[u8; 4]; W * H]>>,
    img: slint::Image,
}

impl Cache {
    /// Atualiza para o quadro dado. Devolve `true` se mudou; aí `image()` tem a imagem nova.
    pub fn update(&mut self, sprite: &Sprite, theme: &Theme, mood: Mood, frame: u32) -> bool {
        let px = render(sprite, theme, mood, frame);
        if self.px.as_deref() == Some(&px) {
            return false;
        }
        self.img = to_image(&px);
        self.px = Some(Box::new(px));
        true
    }

    /// Imagem do último quadro atualizado (barata: a imagem é compartilhada).
    pub fn image(&self) -> slint::Image {
        self.img.clone()
    }
}

/// Quadro quadrado ampliado `scale`× em ARGB32 big-endian (formato do ícone de bandeja SNI).
pub fn argb_square(sprite: &Sprite, theme: &Theme, mood: Mood, frame: u32, scale: usize) -> (i32, Vec<u8>) {
    let px = render(sprite, theme, mood, frame);
    let side = W * scale;
    let top = (W - H) / 2 * scale; // centraliza as 14 linhas num quadrado de 16
    let mut data = vec![0u8; side * side * 4];
    for y in 0..H * scale {
        for x in 0..side {
            let [r, g, b, a] = px[(y / scale) * W + x / scale];
            let i = ((y + top) * side + x) * 4;
            data[i..i + 4].copy_from_slice(&[a, r, g, b]);
        }
    }
    (side as i32, data)
}

/// Cores do tema para a UI (cada app manda as suas).
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

/// Cor do tema como `slint::Color`.
pub fn color(c: u32) -> slint::Color {
    slint::Color::from_rgb_u8((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mascot::{sprites, themes};

    #[test]
    fn todas_as_mascotes_tem_16x14() {
        for s in sprites::SPRITES {
            for row in s.grid {
                assert_eq!(row.len(), W, "{} tem linha com largura errada", s.key);
            }
        }
    }

    #[test]
    fn olho_fecha_dormindo() {
        let s = sprites::get("neko");
        let t = themes::get("neon");
        let awake = render(s, t, Mood::Idle, 1);
        let asleep = render(s, t, Mood::Sleep, 1);
        assert_ne!(awake, asleep);
    }

    #[test]
    fn pior_humor() {
        assert_eq!(Mood::Idle.worst(Mood::Alarm), Mood::Alarm);
        assert_eq!(Mood::Sleep.worst(Mood::Idle), Mood::Idle);
    }

    #[test]
    fn lighter_darker() {
        assert_eq!(darker(0xffffff, 2.0), 0x808080);
        assert_eq!(lighter(0x808080, 2.0), 0xffffff);
    }
}
