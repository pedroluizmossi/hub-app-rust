//! Converts a PNG into mascot animation frames for `src/mascot/clips/<mascot>.toml`.
//!
//! Mascots don't store colors but roles (`K` outline, `P` main color, `E` eye…): the app's
//! theme decides the color of each role. So the conversion needs a palette saying which PNG
//! color becomes which role. Without a palette (or with a color missing), the program lists
//! the PNG's colors in a template ready to fill in.
//!
//! Usage:
//!   cargo run -p sprite-import -- frames.png > palette.toml        # template with the colors
//!   cargo run -p sprite-import -- frames.png --palette neko.toml --name sleep --fps 2
//!
//! The PNG is a sheet of 16×14 frames side by side (or in a grid, read row by row).
//! Fully transparent frames at the end of the sheet are ignored.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Size of a mascot frame (see `src/mascot/render.rs`).
const W: usize = 16;
const H: usize = 14;

const HELP: &str = "\
sprite-import: PNG -> mascot animation frames (src/mascot/clips/<mascot>.toml)

usage: sprite-import <file.png> [options]

  --palette <file.toml> color -> role, e.g. \"#1a1c2c\" = \"K\". Without it, prints a template
                        with the PNG's colors: save it with > palette.toml and replace the ?
  --name <name>         animation name (default: the PNG's name). It plays by itself if it is
                        a mood (sleep, idle, happy, busy, alarm) or a reaction (celebrate, surprised)
  --fps <n>             frames per second, 1 to 30 (default: 4)
  --loop / --no-loop    repeats or plays once (default: a mood repeats, the rest plays once)

Roles of the common mascots: . transparent, K outline, P main, S shadow, H highlight,
M light detail, C visor, E eye, W eye highlight, B blush, T mouth, F skin, D hardware,
A LED. \"pixel\" mascots use the letters of their own palette (see sprites.rs) and e for the eye.
The result goes to standard output: paste it into the mascot's .toml.";

struct Args {
    png: PathBuf,
    palette: Option<PathBuf>,
    name: String,
    fps: u32,
    looping: Option<bool>,
}

fn main() -> ExitCode {
    let raw: Vec<String> = std::env::args().skip(1).collect();
    // `--ajuda`: the Portuguese name of older versions
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
        let mut value = |flag: &str| it.next().ok_or(format!("{flag} needs a value"));
        // the Portuguese names of older versions still work
        match a.as_str() {
            "--palette" | "--paleta" => palette = Some(PathBuf::from(value("--palette")?)),
            "--name" | "--nome" => name = Some(value("--name")?),
            "--fps" => fps = value("--fps")?.parse().map_err(|_| "--fps needs a number")?,
            "--loop" => looping = Some(true),
            "--no-loop" | "--sem-loop" => looping = Some(false),
            s if s.starts_with('-') => return Err(format!("unknown option: {s}")),
            s => png = Some(PathBuf::from(s)),
        }
    }
    let png = png.ok_or("the PNG file is missing")?;
    if !(1..=30).contains(&fps) {
        return Err(format!("--fps {fps} out of 1..30"));
    }
    let name = name.unwrap_or_else(|| png.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default());
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Err(format!("invalid animation name: {name:?} (use letters, digits, _ and -)"));
    }
    Ok(Args { png, palette, name, fps, looping })
}

fn run(args: &Args) -> Result<String, String> {
    let img = read_png(&args.png)?;
    let frames = split(&img)?;
    let colors = count_colors(&frames);
    let palette = match &args.palette {
        Some(p) => read_palette(p)?,
        // no palette: the template goes to standard output (`> palette.toml` and fill it in)
        None => return Ok(template(&colors, &HashMap::new())),
    };
    let missing: Vec<_> = colors.keys().filter(|c| !palette.contains_key(*c)).collect();
    if !missing.is_empty() {
        return Err(format!(
            "{} PNG color(s) with no role in the palette. Complete the file:\n\n{}",
            missing.len(),
            template(&colors, &palette)
        ));
    }
    let grids: Vec<Vec<String>> = frames.iter().map(|f| to_grid(f, &palette)).collect();
    Ok(clip_toml(&args.name, args.fps, args.looping, &grids))
}

/// An RGBA8 image.
struct Image {
    w: usize,
    h: usize,
    px: Vec<[u8; 4]>,
}

fn read_png(path: &Path) -> Result<Image, String> {
    let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut decoder = png::Decoder::new(BufReader::new(file));
    // an indexed palette becomes RGB(A), 16 bits become 8, and transparency (tRNS) becomes alpha
    decoder.set_transformations(png::Transformations::normalize_to_color8() | png::Transformations::ALPHA);
    let mut reader = decoder.read_info().map_err(|e| format!("{}: {e}", path.display()))?;
    let mut buf = vec![0; reader.output_buffer_size().ok_or("PNG too large")?];
    let info = reader.next_frame(&mut buf).map_err(|e| format!("{}: {e}", path.display()))?;
    let (w, h) = (info.width as usize, info.height as usize);
    let data = &buf[..info.buffer_size()];
    let px = match info.color_type {
        png::ColorType::Rgba => data.as_chunks::<4>().0.iter().map(|c| [c[0], c[1], c[2], c[3]]).collect(),
        png::ColorType::Rgb => data.as_chunks::<3>().0.iter().map(|c| [c[0], c[1], c[2], 255]).collect(),
        png::ColorType::GrayscaleAlpha => data.as_chunks::<2>().0.iter().map(|c| [c[0], c[0], c[0], c[1]]).collect(),
        png::ColorType::Grayscale => data.iter().map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => return Err("indexed PNG not converted (unexpected png version)".into()),
    };
    Ok(Image { w, h, px })
}

/// A 16×14 frame: the `#rrggbb` color of each pixel, `None` = transparent.
type Frame = Vec<Option<String>>;

/// Cuts the sheet into 16×14 frames, row by row; drops the empty ones at the end.
fn split(img: &Image) -> Result<Vec<Frame>, String> {
    if !img.w.is_multiple_of(W) || !img.h.is_multiple_of(H) {
        return Err(format!(
            "the image is {}×{}: it must be made of {W}×{H} frames (width a multiple of {W}, height of {H})",
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
                    // half-transparent counts as transparent: pixel art has no in-between
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
        return Err("the image is fully transparent".into());
    }
    Ok(frames)
}

/// How many pixels of each color, over all frames.
fn count_colors(frames: &[Frame]) -> BTreeMap<String, usize> {
    let mut n = BTreeMap::new();
    for c in frames.iter().flatten().flatten() {
        *n.entry(c.clone()).or_insert(0) += 1;
    }
    n
}

/// Reads the palette: a `"#rrggbb" = "K"` table (one character per role).
fn read_palette(path: &Path) -> Result<HashMap<String, char>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let table: toml::Table = text.parse().map_err(|e| format!("{}: {e}", path.display()))?;
    let mut out = HashMap::new();
    for (color, role) in table {
        let color = color.to_ascii_lowercase();
        if color.len() != 7 || !color.starts_with('#') || !color[1..].chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(format!("{}: invalid color {color:?} (use \"#rrggbb\")", path.display()));
        }
        let role = role.as_str().unwrap_or_default();
        let mut chars = role.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) if c.is_ascii_graphic() && c != '"' => {
                out.insert(color, c);
            }
            _ => return Err(format!("{}: {color} needs a 1-character role, got {role:?}", path.display())),
        }
    }
    Ok(out)
}

/// Palette template with every color of the PNG (the ones that already have a role come filled in).
fn template(colors: &BTreeMap<String, usize>, known: &HashMap<String, char>) -> String {
    let mut out = String::from("# color = role (replace the ?). Roles: K P S H M C E W B T F D A, or the letters of a \"pixel\" mascot\n");
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

/// The animation in the format of `src/mascot/clips/<mascot>.toml`.
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

    /// A sheet with two frames: the 1st with a black pixel in the corner, the 2nd with a red one.
    fn sheet() -> Image {
        let (w, h) = (W * 2, H);
        let mut px = vec![[0, 0, 0, 0]; w * h];
        px[0] = [0, 0, 0, 255];
        px[W + 1] = [255, 0, 0, 255];
        Image { w, h, px }
    }

    #[test]
    fn converts_sheet_into_frames() {
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
        // the output is valid TOML, in the format the app reads
        let parsed: toml::Table = toml.parse().unwrap();
        assert_eq!(parsed["sleep"]["frames"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn drops_empty_trailing_frames_and_refuses_wrong_size() {
        let mut img = sheet();
        img.px[W + 1] = [0, 0, 0, 0];
        assert_eq!(split(&img).unwrap().len(), 1);
        let crooked = Image { w: 15, h: 14, px: vec![[0; 4]; 15 * 14] };
        assert!(split(&crooked).unwrap_err().contains("16×14"));
    }

    #[test]
    fn lists_colors_without_a_role() {
        let colors = count_colors(&split(&sheet()).unwrap());
        let t = template(&colors, &HashMap::from([("#000000".to_string(), 'K')]));
        assert!(t.contains("\"#000000\" = \"K\"   # 1 pixel(s)"));
        assert!(t.contains("\"#ff0000\" = \"?\""));
    }

    #[test]
    fn old_portuguese_flags_still_work() {
        let a = parse_args(["x.png", "--nome", "idle", "--paleta", "p.toml", "--sem-loop"].map(String::from).to_vec()).unwrap();
        assert_eq!((a.name.as_str(), a.palette, a.looping), ("idle", Some(PathBuf::from("p.toml")), Some(false)));
    }

    #[test]
    fn reads_a_real_png() {
        let dir = std::env::temp_dir().join(format!("sprite-import-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("frame.png");
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
