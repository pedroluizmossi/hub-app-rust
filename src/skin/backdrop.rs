//! Fundos desenhados atrás da grade do painel. Feitos aqui, numa imagem do tamanho da
//! área, porque o renderizador por software do Slint não tem padrões repetidos nem
//! gradientes radiais baratos: uma imagem pronta custa só uma cópia por quadro.
//!
//! Um fundo é uma lista de camadas (`Layer`) pintadas em ordem, com cores do visual pelo
//! nome (`accent`, `bg`…) ou fixas (`#RRGGBB`). Os embutidos estão em `BUILTIN`; os da
//! pessoa vêm de `fundos/<chave>.toml` (ver `user.rs`) com as mesmas camadas.
//!
//! As camadas `rain`, `twinkle` e `scan` não vão para a imagem: viram elementos leves
//! (`Ambient`) que a tela anima por cima dela (ver `ui/ambient.slint`), repintando só o
//! pedacinho que mexe. Redesenhar a imagem a cada quadro custaria de 20 a 100 ms.
//!
//! As medidas são em pixels lógicos (como no design) e `scale` converte para físicos.

use std::sync::RwLock;

use serde::Deserialize;
use slint::{Rgb8Pixel, SharedPixelBuffer};

use super::Skin;
use super::color::mix;

/// Cor de uma camada: uma cor do visual pelo nome, ou fixa.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Paint {
    Token(&'static str),
    Hex(u32),
}

impl Paint {
    fn resolve(self, s: &Skin) -> u32 {
        match self {
            Paint::Token(name) => s.color(name).unwrap_or(s.accent),
            Paint::Hex(c) => c,
        }
    }
}

impl<'de> Deserialize<'de> for Paint {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        if let Some(c) = super::color::parse_hex(&text) {
            return Ok(Paint::Hex(c));
        }
        super::COLOR_FIELDS.iter().find(|f| **f == text).map(|f| Paint::Token(f)).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "cor \"{text}\": use #RRGGBB ou uma cor do visual ({})",
                super::COLOR_FIELDS.join(", ")
            ))
        })
    }
}

fn one() -> f32 {
    1.0
}
fn half() -> f32 {
    0.5
}
fn rain_speed() -> f32 {
    220.0
}
fn rain_length() -> f32 {
    12.0
}
fn twinkle_size() -> f32 {
    2.0
}
fn scan_height() -> f32 {
    70.0
}
fn scan_speed() -> f32 {
    30.0
}

/// Uma camada. Posições `x`/`y` são frações da área (0 = esquerda/topo, 1 = direita/base),
/// com `dx`/`dy` em pixels somados a elas; tamanhos em pixels.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum Layer {
    /// Grade de pontos de raio `radius`, passo `step`, começando em `offset`.
    Dots {
        step: f32,
        #[serde(default)]
        offset: f32,
        radius: f32,
        color: Paint,
        #[serde(default = "one")]
        alpha: f32,
    },
    /// Brilho elíptico centrado em (x, y), raios `rx`/`ry`, sumindo até `fade` do raio.
    Glow {
        x: f32,
        y: f32,
        #[serde(default)]
        dx: f32,
        #[serde(default)]
        dy: f32,
        rx: f32,
        ry: f32,
        #[serde(default = "one")]
        fade: f32,
        color: Paint,
        #[serde(default = "one")]
        alpha: f32,
    },
    /// Brilho que alcança os quatro cantos (vinheta), centrado em (x, y).
    Vignette {
        #[serde(default = "half")]
        x: f32,
        #[serde(default = "half")]
        y: f32,
        #[serde(default = "one")]
        fade: f32,
        color: Paint,
        alpha: f32,
    },
    /// Linhas finas repetidas a cada `step`, no ângulo `deg` (0 = horizontais).
    Stripes { deg: f32, step: f32, color: Paint, alpha: f32 },
    /// Retângulo com o canto em (x, y).
    Rect {
        x: f32,
        y: f32,
        #[serde(default)]
        dx: f32,
        #[serde(default)]
        dy: f32,
        w: f32,
        h: f32,
        color: Paint,
        #[serde(default = "one")]
        alpha: f32,
    },
    /// A cidade da "Noite chuvosa" (lua, prédios, janelas que apagam). Só embutido.
    #[serde(skip)]
    City,
    /// Animada: `count` gotas (numa área de 1000×600; escala com a área) caindo a `speed` px/s.
    Rain {
        count: u32,
        color: Paint,
        alpha: f32,
        #[serde(default = "rain_speed")]
        speed: f32,
        #[serde(default = "rain_length")]
        length: f32,
    },
    /// Animada: `count` pontos de lado `size` que acendem de vez em quando; com `grid`,
    /// presos à grade (a mesma de uma camada `dots`).
    Twinkle {
        count: u32,
        color: Paint,
        alpha: f32,
        #[serde(default = "twinkle_size")]
        size: f32,
        #[serde(default)]
        grid: f32,
    },
    /// Animada: uma faixa de altura `height` descendo a `speed` px/s (varredura de tubo).
    Scan {
        color: Paint,
        alpha: f32,
        #[serde(default = "scan_height")]
        height: f32,
        #[serde(default = "scan_speed")]
        speed: f32,
    },
}

impl Layer {
    /// Confere os limites (um passo minúsculo travaria o desenho; mil gotas pesariam).
    pub fn check(&self) -> Result<(), String> {
        let alpha = |a: f32| if (0.0..=1.0).contains(&a) { Ok(()) } else { Err(format!("alpha {a} fora de 0 a 1")) };
        let min = |name: &str, v: f32, m: f32| if v >= m { Ok(()) } else { Err(format!("{name} {v} é menor que {m}")) };
        let count = |c: u32| if c <= 200 { Ok(()) } else { Err(format!("count {c} passa de 200")) };
        match *self {
            Layer::Dots { step, radius, alpha: a, .. } => {
                min("step", step, 4.0)?;
                min("radius", radius, 0.2)?;
                alpha(a)
            }
            Layer::Glow { rx, ry, fade, alpha: a, .. } => {
                min("rx", rx, 1.0)?;
                min("ry", ry, 1.0)?;
                min("fade", fade, 0.05)?;
                alpha(a)
            }
            Layer::Vignette { fade, alpha: a, .. } => {
                min("fade", fade, 0.05)?;
                alpha(a)
            }
            Layer::Stripes { step, alpha: a, .. } => {
                min("step", step, 2.0)?;
                alpha(a)
            }
            Layer::Rect { w, h, alpha: a, .. } => {
                min("w", w, 0.0)?;
                min("h", h, 0.0)?;
                alpha(a)
            }
            Layer::City => Ok(()),
            Layer::Rain { count: c, alpha: a, speed, length, .. } => {
                count(c)?;
                min("speed", speed, 1.0)?;
                min("length", length, 1.0)?;
                alpha(a)
            }
            Layer::Twinkle { count: c, alpha: a, size, .. } => {
                count(c)?;
                min("size", size, 1.0)?;
                alpha(a)
            }
            Layer::Scan { alpha: a, height, speed, .. } => {
                min("height", height, 1.0)?;
                min("speed", speed, 1.0)?;
                alpha(a)
            }
        }
    }
}

/// Um fundo: nome e camadas.
#[derive(Debug, Clone, PartialEq)]
pub struct Fundo {
    pub key: &'static str,
    pub label: &'static str,
    pub layers: &'static [Layer],
}

const ACCENT: Paint = Paint::Token("accent");

pub static BUILTIN: &[Fundo] = &[
    Fundo {
        key: "neon",
        label: "Grade neon",
        layers: &[
            Layer::Dots { step: 20.0, offset: 10.0, radius: 1.0, color: ACCENT, alpha: 0.09 },
            Layer::Glow { x: 0.0, y: 0.0, dx: 0.0, dy: 0.0, rx: 420.0, ry: 260.0, fade: 0.7, color: Paint::Token("accent2"), alpha: 0.10 },
            Layer::Glow { x: 1.0, y: 1.0, dx: 0.0, dy: 0.0, rx: 460.0, ry: 300.0, fade: 0.7, color: ACCENT, alpha: 0.09 },
            Layer::Twinkle { count: 24, color: ACCENT, alpha: 0.8, size: 2.0, grid: 20.0 },
        ],
    },
    Fundo {
        key: "terminal",
        label: "Tubo (varredura)",
        layers: &[
            // vinheta: mais claro no meio, apagando até 75% do raio até o canto
            Layer::Vignette { x: 0.5, y: 0.45, fade: 0.75, color: ACCENT, alpha: 0.06 },
            // linhas de varredura: 1 px escuro a cada 3
            Layer::Stripes { deg: 0.0, step: 3.0, color: Paint::Hex(0x000000), alpha: 0.28 },
            Layer::Scan { color: ACCENT, alpha: 0.05, height: 90.0, speed: 30.0 },
        ],
    },
    Fundo {
        key: "papel",
        label: "Papel pontilhado",
        layers: &[Layer::Dots { step: 24.0, offset: 12.0, radius: 1.5, color: Paint::Token("surface_edge"), alpha: 0.7 }],
    },
    Fundo {
        key: "cidade",
        label: "Cidade chuvosa",
        layers: &[
            Layer::City,
            Layer::Rain { count: 40, color: Paint::Token("sub"), alpha: 0.35, speed: 220.0, length: 12.0 },
        ],
    },
];

/// Fundos de arquivo (depois dos embutidos); vazam a cada recarga, como temas e visuais.
static EXTRA: RwLock<Vec<&'static Fundo>> = RwLock::new(Vec::new());

pub fn set_extra(list: Vec<Fundo>) {
    let leaked = list.into_iter().map(|f| &*Box::leak(Box::new(f))).collect();
    if let Ok(mut extra) = EXTRA.write() {
        *extra = leaked;
    }
}

pub fn find(key: &str) -> Option<&'static Fundo> {
    if let Some(f) = BUILTIN.iter().find(|f| f.key == key) {
        return Some(f);
    }
    EXTRA.read().ok()?.iter().copied().find(|f| f.key == key)
}

pub fn get(key: &str) -> &'static Fundo {
    find(key).unwrap_or(&BUILTIN[0])
}

fn rgb(c: u32) -> [f32; 3] {
    [((c >> 16) & 0xff) as f32 / 255.0, ((c >> 8) & 0xff) as f32 / 255.0, (c & 0xff) as f32 / 255.0]
}

/// Tela RGB em f32 (0..1), com operações de pintura com transparência.
struct Canvas {
    w: usize,
    h: usize,
    s: f32,
    px: Vec<[f32; 3]>,
}

impl Canvas {
    fn new(w: usize, h: usize, s: f32, bg: u32) -> Self {
        Canvas { w, h, s, px: vec![rgb(bg); w * h] }
    }

    fn blend(&mut self, x: usize, y: usize, c: [f32; 3], a: f32) {
        if a <= 0.0 {
            return;
        }
        let a = a.min(1.0);
        let p = &mut self.px[y * self.w + x];
        for i in 0..3 {
            p[i] += (c[i] - p[i]) * a;
        }
    }

    /// Retângulo em pixels lógicos (bordas arredondadas para o pixel físico).
    fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, c: u32, a: f32) {
        let c = rgb(c);
        let (x0, y0) = ((x * self.s).round().max(0.0) as usize, (y * self.s).round().max(0.0) as usize);
        let x1 = (((x + w) * self.s).round() as usize).min(self.w);
        let y1 = (((y + h) * self.s).round() as usize).min(self.h);
        for yy in y0..y1 {
            for xx in x0..x1 {
                self.blend(xx, yy, c, a);
            }
        }
    }

    /// Brilho elíptico: cor `c` com alfa `a` no centro (cx, cy), sumindo até `fade` do raio.
    #[allow(clippy::too_many_arguments)] // centro, raios, fade e cor: agrupar não ajuda a ler
    fn glow(&mut self, cx: f32, cy: f32, rx: f32, ry: f32, fade: f32, c: u32, a: f32) {
        let c = rgb(c);
        let (cx, cy, rx, ry) = (cx * self.s, cy * self.s, rx * self.s * fade, ry * self.s * fade);
        let y0 = (cy - ry).max(0.0) as usize;
        let y1 = ((cy + ry).ceil() as usize).min(self.h);
        let x0 = (cx - rx).max(0.0) as usize;
        let x1 = ((cx + rx).ceil() as usize).min(self.w);
        for y in y0..y1 {
            for x in x0..x1 {
                let (dx, dy) = ((x as f32 + 0.5 - cx) / rx, (y as f32 + 0.5 - cy) / ry);
                let d = (dx * dx + dy * dy).sqrt();
                if d < 1.0 {
                    self.blend(x, y, c, a * (1.0 - d));
                }
            }
        }
    }

    /// Pontos de raio `r` numa grade de passo `step`, começando em `off`.
    fn dots(&mut self, step: f32, off: f32, r: f32, c: u32, a: f32) {
        let c = rgb(c);
        let (step, off, r) = (step * self.s, off * self.s, (r * self.s).max(0.6));
        let mut cy = off;
        while cy < self.h as f32 + r {
            let mut cx = off;
            while cx < self.w as f32 + r {
                let y0 = (cy - r).floor().max(0.0) as usize;
                let y1 = ((cy + r).ceil() as usize).min(self.h);
                let x0 = (cx - r).floor().max(0.0) as usize;
                let x1 = ((cx + r).ceil() as usize).min(self.w);
                for y in y0..y1 {
                    for x in x0..x1 {
                        let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
                        // borda suave de 1 pixel físico
                        self.blend(x, y, c, a * (r + 0.5 - d).clamp(0.0, 1.0));
                    }
                }
                cx += step;
            }
            cy += step;
        }
    }

    /// Linhas finas repetidas, como `repeating-linear-gradient(<deg>, cor 0 1px, transparente 1px <passo>)`.
    fn stripes(&mut self, deg: f32, step: f32, c: u32, a: f32) {
        let c = rgb(c);
        let (sin, cos) = deg.to_radians().sin_cos();
        let s = self.s;
        for y in 0..self.h {
            for x in 0..self.w {
                // posição ao longo da direção do gradiente (0° = para cima, 90° = para a direita)
                let t = (x as f32 * sin - y as f32 * cos) / s;
                if t.rem_euclid(step) < 1.0 {
                    self.blend(x, y, c, a);
                }
            }
        }
    }

    fn into_buffer(self) -> SharedPixelBuffer<Rgb8Pixel> {
        let mut buf = SharedPixelBuffer::<Rgb8Pixel>::new(self.w as u32, self.h as u32);
        for (d, p) in buf.make_mut_slice().iter_mut().zip(&self.px) {
            let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
            *d = Rgb8Pixel { r: q(p[0]), g: q(p[1]), b: q(p[2]) };
        }
        buf
    }
}

/// Desenha `fundo` com as cores de `skin` para uma área de `w`×`h` pixels lógicos na
/// escala `scale`. As camadas animadas ficam de fora (ver `ambient`).
pub fn render(fundo: &Fundo, skin: &Skin, w: f32, h: f32, scale: f32) -> SharedPixelBuffer<Rgb8Pixel> {
    let pw = ((w * scale).round() as usize).max(1);
    let ph = ((h * scale).round() as usize).max(1);
    let mut c = Canvas::new(pw, ph, scale, skin.bg);
    for layer in fundo.layers {
        match *layer {
            Layer::Dots { step, offset, radius, color, alpha } => c.dots(step, offset, radius, color.resolve(skin), alpha),
            Layer::Glow { x, y, dx, dy, rx, ry, fade, color, alpha } => {
                c.glow(x * w + dx, y * h + dy, rx, ry, fade, color.resolve(skin), alpha)
            }
            Layer::Vignette { x, y, fade, color, alpha } => {
                let (cx, cy) = (x * w, y * h);
                let (rx, ry) = (cx.max(w - cx) * 1.414, cy.max(h - cy) * 1.414);
                c.glow(cx, cy, rx, ry, fade, color.resolve(skin), alpha);
            }
            Layer::Stripes { deg, step, color, alpha } => c.stripes(deg, step, color.resolve(skin), alpha),
            Layer::Rect { x, y, dx, dy, w: rw, h: rh, color, alpha } => {
                c.rect(x * w + dx, y * h + dy, rw, rh, color.resolve(skin), alpha)
            }
            Layer::City => city(&mut c, skin, w, h),
            Layer::Rain { .. } | Layer::Twinkle { .. } | Layer::Scan { .. } => {}
        }
    }
    c.into_buffer()
}

/// Um elemento animado por cima do fundo, em pixels lógicos. A tela calcula a posição a
/// cada quadro a partir do relógio (`Anim.time`, em ms): ver `ui/ambient.slint`.
#[derive(Debug, Clone, PartialEq)]
pub struct Ambient {
    /// `"fall"` (desce e volta ao topo), `"blink"` (aparece `on` ms a cada `period` ms) ou
    /// `"scan"` (faixa que desce).
    pub kind: &'static str,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub color: u32,
    pub alpha: f32,
    /// Atraso, em ms, para os elementos não andarem juntos.
    pub phase: i32,
    pub period: i32,
    pub on: i32,
    /// Pixels por segundo (`fall` e `scan`).
    pub speed: f32,
}

/// Gerador pseudoaleatório fixo (xorshift): as mesmas gotas e estrelas a cada redesenho.
struct Rng(u32);

impl Rng {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        (self.0 % 10_000) as f32 / 10_000.0
    }
    fn range(&mut self, a: f32, b: f32) -> f32 {
        a + (b - a) * self.next()
    }
}

/// Inclinação da chuva (a mesma das linhas da cidade): x anda 0,18 px por px de queda.
pub const RAIN_SLANT: f32 = 0.18;

/// Os elementos animados de `fundo` para uma área de `w`×`h` pixels lógicos.
pub fn ambient(fundo: &Fundo, skin: &Skin, w: f32, h: f32) -> Vec<Ambient> {
    let area = (w * h / (1000.0 * 600.0)).clamp(0.2, 4.0);
    let scaled = |n: u32| ((n as f32 * area).round() as u32).min(200);
    let mut out = Vec::new();
    for (i, layer) in fundo.layers.iter().enumerate() {
        let mut rng = Rng(0x9e37_79b9 ^ (i as u32 + 1).wrapping_mul(0x85eb_ca6b));
        match *layer {
            Layer::Rain { count, color, alpha, speed, length } => {
                for _ in 0..scaled(count) {
                    out.push(Ambient {
                        kind: "fall",
                        // começa até RAIN_SLANT·h à direita, para cobrir a área inteira ao inclinar
                        x: rng.range(0.0, w + RAIN_SLANT * h),
                        y: rng.range(0.0, h + length),
                        w: 1.0,
                        h: length,
                        color: color.resolve(skin),
                        alpha,
                        phase: 0,
                        period: 0,
                        on: 0,
                        speed: speed * rng.range(0.8, 1.2),
                    });
                }
            }
            Layer::Twinkle { count, color, alpha, size, grid } => {
                for _ in 0..scaled(count) {
                    let (mut x, mut y) = (rng.range(0.0, w), rng.range(0.0, h));
                    if grid >= 4.0 {
                        x = (x / grid).floor() * grid + grid / 2.0;
                        y = (y / grid).floor() * grid + grid / 2.0;
                    }
                    let period = rng.range(2500.0, 7000.0) as i32;
                    out.push(Ambient {
                        kind: "blink",
                        x: x - size / 2.0,
                        y: y - size / 2.0,
                        w: size,
                        h: size,
                        color: color.resolve(skin),
                        alpha,
                        phase: rng.range(0.0, period as f32) as i32,
                        period,
                        on: rng.range(400.0, 1000.0) as i32,
                        speed: 0.0,
                    });
                }
            }
            Layer::Scan { color, alpha, height, speed } => out.push(Ambient {
                kind: "scan",
                x: 0.0,
                y: 0.0,
                w,
                h: height,
                color: color.resolve(skin),
                alpha,
                phase: 0,
                period: 0,
                on: 0,
                speed,
            }),
            Layer::City => city_lights(&mut out, skin, w, h, &mut rng),
            _ => {}
        }
    }
    out
}

/// Prédios do design (quadro 980×170), esticados na largura e presos embaixo.
const BUILDINGS: &[(f32, f32, f32, f32)] = &[
    (0.0, 90.0, 60.0, 80.0), (70.0, 60.0, 90.0, 110.0), (170.0, 100.0, 50.0, 70.0), (230.0, 40.0, 70.0, 130.0),
    (310.0, 80.0, 110.0, 90.0), (430.0, 55.0, 60.0, 115.0), (500.0, 95.0, 80.0, 75.0), (590.0, 30.0, 70.0, 140.0),
    (670.0, 75.0, 100.0, 95.0), (780.0, 50.0, 60.0, 120.0), (850.0, 85.0, 130.0, 85.0),
];
const WINDOWS: &[(f32, f32)] = &[
    (84.0, 76.0), (104.0, 96.0), (130.0, 76.0), (246.0, 56.0), (270.0, 86.0), (246.0, 116.0), (330.0, 96.0),
    (380.0, 116.0), (446.0, 72.0), (470.0, 102.0), (606.0, 46.0), (630.0, 76.0), (606.0, 106.0), (690.0, 92.0),
    (740.0, 112.0), (796.0, 66.0), (820.0, 96.0), (880.0, 102.0), (930.0, 122.0),
];

/// Cores da cidade, tiradas do visual (na "Noite chuvosa" dão as do design).
struct CityColors {
    building: u32,
    window: u32,
    sign: u32,
    moon: u32,
    rain: u32,
    glow: u32,
}

impl CityColors {
    fn of(s: &Skin) -> Self {
        CityColors {
            building: mix(s.edge, s.bg, 0.25),
            window: s.accent,
            sign: s.accent2,
            moon: mix(s.ink, s.accent, 0.6),
            rain: s.sub,
            glow: s.accent,
        }
    }
}

fn city(c: &mut Canvas, skin: &Skin, w: f32, h: f32) {
    let k = CityColors::of(skin);
    // lua com halo, no alto à direita
    let (mx, my) = (w - 72.0 - 44.0, 34.0);
    if mx > 0.0 {
        c.glow(mx + 22.0, my + 22.0, 62.0, 62.0, 1.0, k.moon, 0.22);
        c.rect(mx, my, 44.0, 44.0, k.moon, 1.0);
    }
    // prédios
    let (sx, base) = (w / 980.0, h - 170.0);
    for &(x, y, bw, bh) in BUILDINGS {
        c.rect(x * sx, base + y, bw * sx, bh, k.building, 1.0);
    }
    for &(x, y) in WINDOWS {
        c.rect(x * sx, base + y, 6.0, 6.0, k.window, 1.0);
    }
    c.rect(520.0 * sx, base + 110.0, 40.0 * sx, 4.0, k.sign, 1.0);
    // chuva parada (textura); as gotas que caem são a camada `rain`
    c.stripes(100.0, 17.0, k.rain, 0.08);
    c.stripes(100.0, 29.0, k.rain, 0.05);
    // brilho quente embaixo à esquerda
    c.glow(0.0, h, 380.0, 260.0, 0.7, k.glow, 0.16);
}

/// Algumas janelas apagam de vez em quando: um quadrado da cor do prédio por cima.
fn city_lights(out: &mut Vec<Ambient>, skin: &Skin, w: f32, h: f32, rng: &mut Rng) {
    let k = CityColors::of(skin);
    let (sx, base) = (w / 980.0, h - 170.0);
    for &(x, y) in WINDOWS.iter().step_by(3) {
        let period = rng.range(9000.0, 17000.0) as i32;
        out.push(Ambient {
            kind: "blink",
            x: x * sx,
            y: base + y,
            w: 6.0,
            h: 6.0,
            color: k.building,
            alpha: 1.0,
            phase: rng.range(0.0, period as f32) as i32,
            period,
            on: rng.range(2500.0, 6000.0) as i32,
            speed: 0.0,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tamanho_segue_a_escala() {
        let neon = crate::skin::get("neon");
        let b = render(get("papel"), neon, 100.0, 50.0, 2.0);
        assert_eq!((b.width(), b.height()), (200, 100));
        let b = render(get("neon"), neon, 0.0, 0.0, 1.0);
        assert_eq!((b.width(), b.height()), (1, 1));
    }

    #[test]
    fn todos_desenham_algo_alem_do_fundo() {
        for s in crate::skin::BUILTIN {
            let b = render(get(s.backdrop), s, 300.0, 200.0, 1.0);
            let first = b.as_slice()[0];
            assert!(b.as_slice().iter().any(|p| *p != first), "{} saiu liso", s.key);
        }
    }

    #[test]
    fn cores_vem_do_visual() {
        // a mesma grade com outro destaque muda de cor
        let neon = crate::skin::get("neon");
        let rosa = crate::skin::effective(neon, "magenta");
        assert_ne!(render(get("neon"), neon, 120.0, 80.0, 1.0).as_slice(), render(get("neon"), &rosa, 120.0, 80.0, 1.0).as_slice());
    }

    #[test]
    fn animados_seguem_a_area_e_ficam_dentro() {
        let noite = crate::skin::get("noite");
        let small = ambient(get("cidade"), noite, 500.0, 300.0);
        let big = ambient(get("cidade"), noite, 1500.0, 900.0);
        let falls = |v: &[Ambient]| v.iter().filter(|a| a.kind == "fall").count();
        assert!(falls(&big) > falls(&small) && falls(&small) > 0);
        assert!(big.iter().any(|a| a.kind == "blink"), "janelas que apagam");
        assert!(ambient(get("papel"), noite, 500.0, 300.0).is_empty());
        for a in ambient(get("neon"), noite, 800.0, 500.0) {
            assert!(a.x >= 0.0 && a.x < 800.0 && a.y >= 0.0 && a.y < 500.0 && a.period > a.on);
        }
    }

    #[test]
    fn limites_das_camadas() {
        let ok = Layer::Dots { step: 20.0, offset: 0.0, radius: 1.0, color: ACCENT, alpha: 0.1 };
        assert!(ok.check().is_ok());
        assert!(Layer::Dots { step: 1.0, offset: 0.0, radius: 1.0, color: ACCENT, alpha: 0.1 }.check().is_err());
        assert!(Layer::Rain { count: 500, color: ACCENT, alpha: 0.3, speed: 100.0, length: 10.0 }.check().is_err());
        assert!(Layer::Stripes { deg: 0.0, step: 3.0, color: ACCENT, alpha: 1.5 }.check().is_err());
        for f in BUILTIN {
            for l in f.layers {
                l.check().unwrap_or_else(|e| panic!("{}: {e}", f.key));
            }
        }
    }
}
