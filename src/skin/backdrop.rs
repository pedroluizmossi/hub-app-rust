//! Fundos desenhados atrás da grade do painel. Feitos aqui, numa imagem do tamanho da
//! área, porque o renderizador por software do Slint não tem padrões repetidos nem
//! gradientes radiais baratos: uma imagem pronta custa só uma cópia por quadro.
//!
//! As medidas são em pixels lógicos (como no design) e `scale` converte para físicos.

use slint::{Rgb8Pixel, SharedPixelBuffer};

use super::Backdrop;

/// Tela RGB em f32 (0..1), com operações de pintura com transparência.
struct Canvas {
    w: usize,
    h: usize,
    s: f32,
    px: Vec<[f32; 3]>,
}

fn rgb(c: u32) -> [f32; 3] {
    [((c >> 16) & 0xff) as f32 / 255.0, ((c >> 8) & 0xff) as f32 / 255.0, (c & 0xff) as f32 / 255.0]
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

/// Desenha o fundo `kind` para uma área de `w`×`h` pixels lógicos na escala `scale`.
pub fn render(kind: Backdrop, bg: u32, w: f32, h: f32, scale: f32) -> SharedPixelBuffer<Rgb8Pixel> {
    let pw = ((w * scale).round() as usize).max(1);
    let ph = ((h * scale).round() as usize).max(1);
    let mut c = Canvas::new(pw, ph, scale, bg);
    match kind {
        Backdrop::NeonGrid => {
            c.dots(20.0, 10.0, 1.0, 0x3DF2FF, 0.09);
            c.glow(0.0, 0.0, 420.0, 260.0, 0.7, 0xFF3DB8, 0.10);
            c.glow(w, h, 460.0, 300.0, 0.7, 0x3DF2FF, 0.09);
        }
        Backdrop::Crt => {
            // vinheta: mais claro no meio, apagando até 75% do raio até o canto
            let (cx, cy) = (w * 0.5, h * 0.45);
            let (rx, ry) = (cx.max(w - cx) * 1.414, cy.max(h - cy) * 1.414);
            c.glow(cx, cy, rx, ry, 0.75, 0x08190C, 1.0);
            // linhas de varredura: 1 px escuro a cada 3
            c.stripes(0.0, 3.0, 0x000000, 0.28);
        }
        Backdrop::Paper => {
            c.dots(24.0, 12.0, 1.5, 0xE3CBA2, 1.0);
        }
        Backdrop::RainyCity => city(&mut c, w, h),
    }
    c.into_buffer()
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

fn city(c: &mut Canvas, w: f32, h: f32) {
    // lua com halo, no alto à direita
    let (mx, my) = (w - 72.0 - 44.0, 34.0);
    if mx > 0.0 {
        c.glow(mx + 22.0, my + 22.0, 62.0, 62.0, 1.0, 0xF3EBD0, 0.22);
        c.rect(mx, my, 44.0, 44.0, 0xF3EBD0, 1.0);
    }
    // prédios
    let (sx, base) = (w / 980.0, h - 170.0);
    for &(x, y, bw, bh) in BUILDINGS {
        c.rect(x * sx, base + y, bw * sx, bh, 0x151B3D, 1.0);
    }
    for &(x, y) in WINDOWS {
        c.rect(x * sx, base + y, 6.0, 6.0, 0xFFC46B, 1.0);
    }
    c.rect(520.0 * sx, base + 110.0, 40.0 * sx, 4.0, 0xFF6FB5, 1.0);
    // chuva
    c.stripes(100.0, 17.0, 0xA0B9FF, 0.08);
    c.stripes(100.0, 29.0, 0xA0B9FF, 0.05);
    // brilho quente embaixo à esquerda
    c.glow(0.0, h, 380.0, 260.0, 0.7, 0xFFB05A, 0.16);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tamanho_segue_a_escala() {
        let b = render(Backdrop::Paper, 0xF4E6CC, 100.0, 50.0, 2.0);
        assert_eq!((b.width(), b.height()), (200, 100));
        let b = render(Backdrop::NeonGrid, 0, 0.0, 0.0, 1.0);
        assert_eq!((b.width(), b.height()), (1, 1));
    }

    #[test]
    fn todos_desenham_algo_alem_do_fundo() {
        for kind in [Backdrop::NeonGrid, Backdrop::Crt, Backdrop::Paper, Backdrop::RainyCity] {
            let b = render(kind, 0x101010, 300.0, 200.0, 1.0);
            let first = b.as_slice()[0];
            assert!(b.as_slice().iter().any(|p| *p != first), "{kind:?} saiu liso");
        }
    }
}
