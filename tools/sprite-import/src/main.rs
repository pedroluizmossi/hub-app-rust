//! Converte um PNG em quadros de animação para `src/mascot/clips/<mascote>.toml`.
//!
//! As mascotes não guardam cores, e sim papéis (`K` contorno, `P` cor principal, `E` olho…):
//! o tema do app decide a cor de cada papel. Por isso a conversão precisa de uma paleta que
//! diga qual cor do PNG vira qual papel. Sem paleta (ou com cor faltando), o programa lista
//! as cores do PNG num modelo pronto para preencher.
//!
//! Uso:
//!   cargo run -p sprite-import -- quadros.png > paleta.toml        # modelo com as cores
//!   cargo run -p sprite-import -- quadros.png --paleta neko.toml --nome sleep --fps 2
//!
//! O PNG é uma folha de quadros de 16×14 lado a lado (ou em grade, lidos linha por linha).
//! Quadros totalmente transparentes no fim da folha são ignorados.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Tamanho do quadro das mascotes (ver `src/mascot/render.rs`).
const W: usize = 16;
const H: usize = 14;

const HELP: &str = "\
sprite-import: PNG -> quadros de animação das mascotes (src/mascot/clips/<mascote>.toml)

uso: sprite-import <arquivo.png> [opções]

  --paleta <arq.toml>   cor -> papel, ex.: \"#1a1c2c\" = \"K\". Sem ela, sai um modelo com
                        as cores do PNG: salve com > paleta.toml e troque os ?
  --nome <nome>         nome da animação (padrão: o nome do PNG). Toca sozinha se for um
                        humor (sleep, idle, happy, busy, alarm) ou uma reação (celebrate, surprised)
  --fps <n>             quadros por segundo, 1 a 30 (padrão: 4)
  --loop / --sem-loop   repete ou toca uma vez (padrão: humor repete, o resto toca uma vez)

Papéis das mascotes comuns: . transparente, K contorno, P principal, S sombra, H brilho,
M detalhe claro, C visor, E olho, W brilho do olho, B blush, T boca, F pele, D hardware,
A LED. Mascotes \"pixel\" usam as letras da própria paleta (ver sprites.rs) e e para o olho.
O resultado sai na saída padrão: cole no .toml da mascote.";

struct Args {
    png: PathBuf,
    palette: Option<PathBuf>,
    name: String,
    fps: u32,
    looping: Option<bool>,
}

fn main() -> ExitCode {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.is_empty() || raw.iter().any(|a| matches!(a.as_str(), "-h" | "--help" | "--ajuda")) {
        println!("{HELP}");
        return ExitCode::SUCCESS;
    }
    let args = match parse_args(raw) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}\n\n{HELP}");
            return ExitCode::from(2);
        }
    };
    match run(&args) {
        Ok(out) => {
            print!("{out}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn parse_args(raw: Vec<String>) -> Result<Args, String> {
    let mut png = None;
    let (mut palette, mut name, mut fps, mut looping) = (None, None, 4, None);
    let mut it = raw.into_iter();
    while let Some(a) = it.next() {
        let mut value = |flag: &str| it.next().ok_or(format!("{flag} precisa de um valor"));
        match a.as_str() {
            "--paleta" => palette = Some(PathBuf::from(value("--paleta")?)),
            "--nome" => name = Some(value("--nome")?),
            "--fps" => fps = value("--fps")?.parse().map_err(|_| "--fps precisa de um número")?,
            "--loop" => looping = Some(true),
            "--sem-loop" => looping = Some(false),
            s if s.starts_with('-') => return Err(format!("opção desconhecida: {s}")),
            s => png = Some(PathBuf::from(s)),
        }
    }
    let png = png.ok_or("faltou o arquivo PNG")?;
    if !(1..=30).contains(&fps) {
        return Err(format!("--fps {fps} fora de 1..30"));
    }
    let name = name.unwrap_or_else(|| png.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default());
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Err(format!("nome de animação inválido: {name:?} (use letras, números, _ e -)"));
    }
    Ok(Args { png, palette, name, fps, looping })
}

fn run(args: &Args) -> Result<String, String> {
    let img = read_png(&args.png)?;
    let frames = split(&img)?;
    let colors = count_colors(&frames);
    let palette = match &args.palette {
        Some(p) => read_palette(p)?,
        // sem paleta: o modelo vai para a saída padrão (`> paleta.toml` e preencher)
        None => return Ok(template(&colors, &HashMap::new())),
    };
    let missing: Vec<_> = colors.keys().filter(|c| !palette.contains_key(*c)).collect();
    if !missing.is_empty() {
        return Err(format!(
            "{} cor(es) do PNG sem papel na paleta. Complete o arquivo:\n\n{}",
            missing.len(),
            template(&colors, &palette)
        ));
    }
    let grids: Vec<Vec<String>> = frames.iter().map(|f| to_grid(f, &palette)).collect();
    Ok(clip_toml(&args.name, args.fps, args.looping, &grids))
}

/// Imagem em RGBA8.
struct Image {
    w: usize,
    h: usize,
    px: Vec<[u8; 4]>,
}

fn read_png(path: &Path) -> Result<Image, String> {
    let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut decoder = png::Decoder::new(BufReader::new(file));
    // paleta indexada vira RGB(A), 16 bits vira 8, e a transparência (tRNS) vira alfa
    decoder.set_transformations(png::Transformations::normalize_to_color8() | png::Transformations::ALPHA);
    let mut reader = decoder.read_info().map_err(|e| format!("{}: {e}", path.display()))?;
    let mut buf = vec![0; reader.output_buffer_size().ok_or("PNG grande demais")?];
    let info = reader.next_frame(&mut buf).map_err(|e| format!("{}: {e}", path.display()))?;
    let (w, h) = (info.width as usize, info.height as usize);
    let data = &buf[..info.buffer_size()];
    let px = match info.color_type {
        png::ColorType::Rgba => data.as_chunks::<4>().0.iter().map(|c| [c[0], c[1], c[2], c[3]]).collect(),
        png::ColorType::Rgb => data.as_chunks::<3>().0.iter().map(|c| [c[0], c[1], c[2], 255]).collect(),
        png::ColorType::GrayscaleAlpha => data.as_chunks::<2>().0.iter().map(|c| [c[0], c[0], c[0], c[1]]).collect(),
        png::ColorType::Grayscale => data.iter().map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => return Err("PNG indexado não convertido (versão do png inesperada)".into()),
    };
    Ok(Image { w, h, px })
}

/// Um quadro de 16×14: cor `#rrggbb` de cada pixel, `None` = transparente.
type Frame = Vec<Option<String>>;

/// Corta a folha em quadros de 16×14, linha por linha; descarta os vazios do fim.
fn split(img: &Image) -> Result<Vec<Frame>, String> {
    if !img.w.is_multiple_of(W) || !img.h.is_multiple_of(H) {
        return Err(format!(
            "a imagem tem {}×{}: precisa ser feita de quadros de {W}×{H} (largura múltipla de {W}, altura de {H})",
            img.w, img.h
        ));
    }
    let mut frames = Vec::new();
    for fy in 0..img.h / H {
        for fx in 0..img.w / W {
            let mut f = Vec::with_capacity(W * H);
            for y in 0..H {
                for x in 0..W {
                    let p = img.px[(fy * H + y) * img.w + fx * W + x];
                    // meio-transparente conta como transparente: pixel art não tem meio-termo
                    f.push((p[3] >= 128).then(|| format!("#{:02x}{:02x}{:02x}", p[0], p[1], p[2])));
                }
            }
            frames.push(f);
        }
    }
    while frames.last().is_some_and(|f| f.iter().all(Option::is_none)) {
        frames.pop();
    }
    if frames.is_empty() {
        return Err("a imagem está toda transparente".into());
    }
    Ok(frames)
}

/// Quantos pixels de cada cor, somando todos os quadros.
fn count_colors(frames: &[Frame]) -> BTreeMap<String, usize> {
    let mut n = BTreeMap::new();
    for c in frames.iter().flatten().flatten() {
        *n.entry(c.clone()).or_insert(0) += 1;
    }
    n
}

/// Lê a paleta: uma tabela `"#rrggbb" = "K"` (um caractere por papel).
fn read_palette(path: &Path) -> Result<HashMap<String, char>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let table: toml::Table = text.parse().map_err(|e| format!("{}: {e}", path.display()))?;
    let mut out = HashMap::new();
    for (color, role) in table {
        let color = color.to_ascii_lowercase();
        if color.len() != 7 || !color.starts_with('#') || !color[1..].chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(format!("{}: cor inválida {color:?} (use \"#rrggbb\")", path.display()));
        }
        let role = role.as_str().unwrap_or_default();
        let mut chars = role.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) if c.is_ascii_graphic() && c != '"' => {
                out.insert(color, c);
            }
            _ => return Err(format!("{}: {color} precisa de um papel de 1 caractere, veio {role:?}", path.display())),
        }
    }
    Ok(out)
}

/// Modelo de paleta com todas as cores do PNG (as que já têm papel vêm preenchidas).
fn template(colors: &BTreeMap<String, usize>, known: &HashMap<String, char>) -> String {
    let mut out = String::from("# cor = papel (troque o ?). Papéis: K P S H M C E W B T F D A, ou as letras de uma mascote \"pixel\"\n");
    for (color, n) in colors {
        let role = known.get(color).copied().unwrap_or('?');
        out.push_str(&format!("\"{color}\" = \"{role}\"   # {n} pixel(s)\n"));
    }
    out
}

fn to_grid(frame: &Frame, palette: &HashMap<String, char>) -> Vec<String> {
    frame
        .chunks(W)
        .map(|row| row.iter().map(|c| c.as_ref().map_or('.', |c| palette[c])).collect())
        .collect()
}

/// A animação no formato de `src/mascot/clips/<mascote>.toml`.
fn clip_toml(name: &str, fps: u32, looping: Option<bool>, grids: &[Vec<String>]) -> String {
    let mut out = format!("[{name}]\nfps = {fps}\n");
    if let Some(l) = looping {
        out.push_str(&format!("loop = {l}\n"));
    }
    out.push_str("frames = [\n");
    for g in grids {
        out.push_str(&format!("\"\"\"\n{}\n\"\"\",\n", g.join("\n")));
    }
    out.push_str("]\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Folha com dois quadros: o 1º com um pixel preto no canto, o 2º com um vermelho.
    fn sheet() -> Image {
        let (w, h) = (W * 2, H);
        let mut px = vec![[0, 0, 0, 0]; w * h];
        px[0] = [0, 0, 0, 255];
        px[W + 1] = [255, 0, 0, 255];
        Image { w, h, px }
    }

    #[test]
    fn converte_folha_em_quadros() {
        let frames = split(&sheet()).unwrap();
        assert_eq!(frames.len(), 2);
        let palette = HashMap::from([("#000000".to_string(), 'K'), ("#ff0000".to_string(), 'P')]);
        let a = to_grid(&frames[0], &palette);
        let b = to_grid(&frames[1], &palette);
        assert_eq!(a.len(), H);
        assert_eq!(a[0], format!("K{}", ".".repeat(W - 1)));
        assert_eq!(b[0], format!(".P{}", ".".repeat(W - 2)));
        let toml = clip_toml("sleep", 2, None, &[a, b]);
        assert!(toml.starts_with("[sleep]\nfps = 2\nframes = [\n\"\"\"\nK..."));
        // o que sai é TOML válido, no formato que o app lê
        let parsed: toml::Table = toml.parse().unwrap();
        assert_eq!(parsed["sleep"]["frames"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn descarta_quadros_vazios_do_fim_e_recusa_tamanho_errado() {
        let mut img = sheet();
        img.px[W + 1] = [0, 0, 0, 0];
        assert_eq!(split(&img).unwrap().len(), 1);
        let torta = Image { w: 15, h: 14, px: vec![[0; 4]; 15 * 14] };
        assert!(split(&torta).unwrap_err().contains("16×14"));
    }

    #[test]
    fn lista_cores_sem_papel() {
        let colors = count_colors(&split(&sheet()).unwrap());
        let t = template(&colors, &HashMap::from([("#000000".to_string(), 'K')]));
        assert!(t.contains("\"#000000\" = \"K\"   # 1 pixel(s)"));
        assert!(t.contains("\"#ff0000\" = \"?\""));
    }

    #[test]
    fn le_png_de_verdade() {
        let dir = std::env::temp_dir().join(format!("sprite-import-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("quadro.png");
        let img = sheet();
        {
            let file = File::create(&path).unwrap();
            let mut enc = png::Encoder::new(std::io::BufWriter::new(file), img.w as u32, img.h as u32);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let data: Vec<u8> = img.px.iter().flatten().copied().collect();
            enc.write_header().unwrap().write_image_data(&data).unwrap();
        }
        let back = read_png(&path).unwrap();
        assert_eq!((back.w, back.h), (img.w, img.h));
        assert_eq!(back.px, img.px);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
