//! O CD em pixel art: um disco de 24×24 desenhado no tema do app, com o brilho girando.
//!
//! Igual às mascotes, o desenho é feito aqui (e não carregado do player): a capa do MPRIS
//! é um arquivo JPEG que exigiria o decodificador de imagens do Slint, e a pixel art
//! combina com o resto do hub. A iridescência usa as cores do tema.

use super::render::darker;
use super::themes::Theme;

pub const SIZE: usize = 24;
/// Quadros de uma volta (22,5° cada; ~2 s a 8 quadros/s).
pub const STEPS: u8 = 16;

/// Raio externo, em pixels.
const R: f32 = 11.4;
/// Raio do furo central.
const HOLE: f32 = 1.5;

/// O disco num quadro. `spin` é o quadro da volta; `lit` apaga o disco sem player.
pub fn pixels(theme: &Theme, spin: u8, lit: bool) -> [[u8; 4]; SIZE * SIZE] {
    let mut out = [[0u8; 4]; SIZE * SIZE];
    let c = (SIZE as f32 - 1.0) / 2.0;
    let rot = spin as f32 / STEPS as f32 * std::f32::consts::TAU;
    // alumínio do disco: nem claro demais (some no tema claro), nem escuro
    let body = mix(theme.sub, theme.text, 0.18);

    for y in 0..SIZE {
        for x in 0..SIZE {
            let (dx, dy) = (x as f32 - c, y as f32 - c);
            let r = (dx * dx + dy * dy).sqrt();
            if r > R {
                continue; // fora do disco: transparente
            }
            let a = dy.atan2(dx);
            if r <= HOLE {
                continue; // furo: transparente, mostra o fundo do poço
            }
            let mut color = if r <= HOLE + 0.9 {
                // aro do furo
                darker(theme.muted, 0.8)
            } else {
                // iridescência: duas cores do tema que giram com o disco
                let iri = mix(theme.visor, theme.fur, 0.5 + 0.5 * (2.0 * a - rot).cos());
                let mut c = mix(body, iri, 0.5);
                // trilhas: anéis finos de dados
                if ((r - HOLE) * 1.7) as i32 % 2 == 0 {
                    c = darker(c, 0.9);
                }
                // brilho que gira
                let spec = (a - rot).cos().max(0.0).powf(6.0);
                c = mix(c, 0xffffff, spec * 0.7);
                // aro externo
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

/// Quadro como imagem do Slint (ampliada com `image-rendering: pixelated`).
pub fn image(theme: &Theme, spin: u8, lit: bool) -> slint::Image {
    let px = pixels(theme, spin, lit);
    let mut buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(SIZE as u32, SIZE as u32);
    for (dst, src) in buffer.make_mut_slice().iter_mut().zip(px.iter()) {
        *dst = slint::Rgba8Pixel { r: src[0], g: src[1], b: src[2], a: src[3] };
    }
    slint::Image::from_rgba8(buffer)
}

/// Último quadro entregue à UI. O Slint redesenha quando a imagem muda, então o disco só
/// é redesenhado quando o quadro (ou o tema) muda.
#[derive(Default)]
pub struct Cache {
    key: Option<(&'static str, u8, bool)>,
    img: Option<slint::Image>,
}

impl Cache {
    /// Redesenha se preciso; `true` quando a imagem mudou.
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

/// Mistura `b` sobre `a`: 0 = só `a`, 1 = só `b`.
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
    fn fora_do_disco_e_transparente_e_o_centro_e_o_furo() {
        let t = themes::get("neon");
        let p = pixels(t, 0, true);
        assert_eq!(px(&p, 0, 0)[3], 0);
        assert_eq!(px(&p, SIZE - 1, 0)[3], 0);
        // centro: furo transparente (o poço do disco aparece)
        assert_eq!(px(&p, SIZE / 2, SIZE / 2)[3], 0);
        // meio do disco (entre o furo e o aro): opaco
        assert_eq!(px(&p, SIZE / 2 + 7, SIZE / 2)[3], 255);
    }

    #[test]
    fn girar_muda_os_pixels_e_o_tema_tambem() {
        let t = themes::get("neon");
        assert_ne!(pixels(t, 0, true), pixels(t, 4, true));
        assert_ne!(pixels(t, 0, true), pixels(t, 0, false));
        assert_ne!(pixels(t, 0, true), pixels(themes::get("aconchego"), 0, true));
        // mesmo quadro, mesma imagem
        assert_eq!(pixels(t, 3, true), pixels(t, 3, true));
    }

    #[test]
    fn cache_so_redesenha_quando_muda() {
        let t = themes::get("neon");
        let mut c = Cache::default();
        assert!(c.update(t, 0, true));
        assert!(!c.update(t, 0, true));
        assert!(c.update(t, 1, true));
        assert!(c.update(t, 1, false));
        assert_eq!((c.image().size().width, c.image().size().height), (SIZE as u32, SIZE as u32));
    }
}
