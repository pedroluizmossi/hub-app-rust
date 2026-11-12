//! Prévia das animações em GIF, fora do app: `miniutils --previa <mascote> [animação]`.
//!
//! Desenha cada quadro com o mesmo renderizador do app, em vários temas lado a lado e
//! ampliado, para conferir uma animação (ou mostrar a alguém) sem abrir a janela.

use std::fs::File;
use std::io::BufWriter;
use std::path::PathBuf;

use super::anim::{Look, Reaction};
use super::clips::{self, Clip};
use super::render::{self, H, W};
use super::sprites::SPRITES;
use super::themes::BUILTIN as THEMES;
use super::Mood;

/// Ampliação de cada pixel da mascote.
const SCALE: usize = 6;
/// Margem em volta de cada mascote (em pixels já ampliados).
const PAD: usize = 12;
/// Temas por linha na folha.
const COLS: usize = 4;
/// Uma animação que repete toca pelo menos isso na prévia.
const MIN_LOOP_MS: u32 = 2000;

pub const USAGE: &str = "\
uso: miniutils --previa <mascote> [animação] [--saida arquivo.gif]

Gera um GIF da animação (ou de todas, uma depois da outra) em todos os temas.
Sem --saida, grava <mascote>[-<animação>].gif na pasta atual.
Com MINIUTILS_CLIPS=<pasta>, lê as animações da pasta em vez das embutidas.";

/// Roda a prévia; devolve o caminho do GIF gravado.
pub fn run(args: &[String]) -> Result<PathBuf, String> {
    let mut pos = Vec::new();
    let mut out = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--saida" => out = Some(PathBuf::from(it.next().ok_or("--saida precisa de um arquivo")?)),
            s if s.starts_with('-') => return Err(format!("opção desconhecida: {s}\n\n{USAGE}")),
            s => pos.push(s.to_string()),
        }
    }
    let key = pos.first().ok_or(USAGE)?;
    let sprite = SPRITES.iter().find(|s| s.key == key).ok_or_else(|| format!("não há mascote \"{key}\" em sprites.rs"))?;
    let all = clips::of(key).ok_or_else(|| format!("a mascote \"{key}\" não tem animações"))?;
    let names: Vec<String> = match pos.get(1) {
        Some(name) if all.contains_key(name) => vec![name.clone()],
        Some(name) => {
            let mut have: Vec<&String> = all.keys().collect();
            have.sort();
            return Err(format!("\"{key}\" não tem a animação \"{name}\"; tem: {have:?}"));
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

    // a sequência: cada animação com o humor (ou reação) que o nome indica
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
        // o GIF conta em centésimos de segundo
        frame.delay = (ms / 10).max(2) as u16;
        enc.write_frame(&frame).map_err(|e| e.to_string())?;
    }
    Ok(path)
}

fn put(buf: &mut [u8], w: usize, x: usize, y: usize, p: [u8; 4]) {
    let i = (y * w + x) * 4;
    buf[i..i + 4].copy_from_slice(&p);
}

/// Humor e reação em que a animação toca no app, para os efeitos (olhos, LED, cores)
/// baterem: o do nome, ou o do humor que a cita como `enter`, `exit` ou variação.
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
    Look { mood, reaction, blink: false, frame: 0, pose: None }
}

/// Quadros e durações; uma animação que repete toca voltas até somar `MIN_LOOP_MS`.
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
    fn gera_gif_da_neko() {
        let dir = std::env::temp_dir().join(format!("miniutils-previa-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("neko.gif");
        let args = ["neko".to_string(), "--saida".into(), out.to_string_lossy().into_owned()];
        let path = run(&args).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.starts_with(b"GIF89a"));
        assert!(run(&["neko".into(), "nao-existe".into()]).unwrap_err().contains("tem:"));
        // a transição de entrada do sono aparece como no app: dormindo
        let neko = clips::of("neko").unwrap();
        assert_eq!(look_for("boceja", &neko).mood, Mood::Sleep);
        assert_eq!(look_for("celebrate", &neko).reaction, Some(Reaction::Celebrate));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
