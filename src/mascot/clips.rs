//! Animações das mascotes: poses desenhadas, quadro a quadro.
//!
//! Sem animação, a mascote é um desenho só (`Sprite::grid`) e o que muda é calculado em
//! cima dele (piscada, LED, glitch). Uma animação troca o desenho por uma sequência de
//! quadros; os efeitos continuam valendo por cima de cada quadro. Quem decide qual quadro
//! mostrar é o `Player` de cada mascote (`anim.rs`).
//!
//! Cada mascote pode ter um arquivo `src/mascot/clips/<chave>.toml` (registre em `SOURCES`):
//!
//! ```toml
//! [idle]                         # nome da animação
//! fps = 4                        # quadros por segundo (ou `durations`)
//! durations = [900, 120, 120]    # ms de cada quadro (no lugar de `fps`)
//! loop = true                    # repete (padrão: true para humor, false para o resto)
//! enter = "boceja"               # toca uma vez ao entrar neste humor, antes do loop
//! exit = "acorda"                # toca uma vez ao sair dele
//! variants = ["coca", "olha"]    # de vez em quando, uma delas no lugar do loop
//! every = [8, 20]                # segundos entre variações (sorteado no intervalo)
//! frames = [
//! """
//! ... 14 linhas de 16 caracteres, com os mesmos papéis de cor de `sprites.rs` ...
//! """,
//! ]
//! ```
//!
//! Quando toca:
//! - com o nome de uma reação (`celebrate`, `surprised`): enquanto a reação durar;
//! - com o nome de um humor (`sleep`, `idle`, `happy`, `busy`, `alarm`): sempre que a
//!   mascote estiver nele, com `enter`/`exit`/`variants`;
//! - outros nomes: quando alguém pede (transições, variações, um jogo).
//!
//! Para desenhar sem recompilar: `MINIUTILS_CLIPS=src/mascot/clips cargo run` lê os
//! `.toml` dessa pasta e recarrega ao salvar (ver `watch`).
//! Para converter um PNG: `cargo run -p sprite-import -- --help`.
//! Para ver em GIF: `miniutils --previa <mascote> <animação>`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{Duration, SystemTime};

use super::sprites::{self, SPRITES, Sprite};

/// Um quadro: 14 linhas, como `Sprite::grid`.
pub type Grid = [&'static str; 14];

/// Arquivos de animação embutidos no binário: (chave da mascote, conteúdo).
const SOURCES: &[(&str, &str)] = &[
    ("neko", include_str!("clips/neko.toml")),
    ("lumi", include_str!("clips/lumi.toml")),
];

/// Nomes das animações que tocam sozinhas pelo humor (repetem por padrão).
pub const MOODS: [&str; 5] = ["sleep", "idle", "happy", "busy", "alarm"];

#[derive(Debug)]
pub struct Clip {
    pub frames: Vec<Grid>,
    /// Quanto cada quadro fica na tela, em ms.
    pub durations: Vec<u32>,
    pub looping: bool,
    pub enter: Option<String>,
    pub exit: Option<String>,
    pub variants: Vec<String>,
    /// Segundos entre variações: (mínimo, máximo).
    pub every: (u32, u32),
}

impl Clip {
    /// Duração de uma volta inteira, em ms.
    pub fn total_ms(&self) -> u32 {
        self.durations.iter().sum()
    }

    /// O quadro mais curto (o hub acelera o timer para não pular quadros).
    pub fn shortest_ms(&self) -> u32 {
        self.durations.iter().copied().min().unwrap_or(1000)
    }

    /// Índice do quadro depois de `ms` tocando. Sem repetir, para no último.
    pub fn index_at(&self, ms: u64) -> usize {
        let total = self.total_ms().max(1) as u64;
        let mut t = if self.looping { ms % total } else { ms.min(total - 1) };
        for (i, d) in self.durations.iter().enumerate() {
            if t < *d as u64 {
                return i;
            }
            t -= *d as u64;
        }
        self.frames.len() - 1
    }

    pub fn frame_at(&self, ms: u64) -> &Grid {
        &self.frames[self.index_at(ms)]
    }

    /// Terminou de tocar (só faz sentido sem `loop`).
    pub fn done_at(&self, ms: u64) -> bool {
        !self.looping && ms >= self.total_ms() as u64
    }
}

/// Animações de uma mascote, por nome.
pub type Clips = HashMap<String, Clip>;

type Library = HashMap<String, Arc<Clips>>;

fn library() -> &'static RwLock<Arc<Library>> {
    static LIB: OnceLock<RwLock<Arc<Library>>> = OnceLock::new();
    LIB.get_or_init(|| {
        let lib = match dev_dir() {
            Some(dir) => {
                watch(dir.clone());
                load_dir(&dir)
            }
            None => load_embedded(),
        };
        RwLock::new(Arc::new(lib))
    })
}

/// Animações da mascote `key`, se tiver.
pub fn of(key: &str) -> Option<Arc<Clips>> {
    library().read().ok()?.get(key).cloned()
}

fn load_embedded() -> Library {
    let mut lib = Library::new();
    for (key, src) in SOURCES {
        match parse(sprites::get(key), src) {
            Ok(clips) => {
                lib.insert(key.to_string(), Arc::new(clips));
            }
            Err(e) => log::warn!("animações de {key} inválidas: {e}"),
        }
    }
    lib
}

// ---- desenvolvimento: ler da pasta e recarregar ao salvar ---------------------------

/// Pasta das animações para desenvolvimento (`MINIUTILS_CLIPS`), se definida.
fn dev_dir() -> Option<PathBuf> {
    std::env::var_os("MINIUTILS_CLIPS").filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// Lê todo `<chave>.toml` da pasta (chave = uma mascote de `sprites.rs`). Arquivo com erro
/// fica de fora, com o erro no terminal.
fn load_dir(dir: &Path) -> Library {
    let mut lib = Library::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        log::warn!("MINIUTILS_CLIPS={} não é uma pasta legível", dir.display());
        return lib;
    };
    for path in entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "toml")) {
        let Some(key) = path.file_stem().and_then(|s| s.to_str()) else { continue };
        let Some(sprite) = SPRITES.iter().find(|s| s.key == key) else {
            log::warn!("{}: não há mascote \"{key}\" em sprites.rs", path.display());
            continue;
        };
        let parsed = std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|src| parse(sprite, &src));
        match parsed {
            Ok(clips) => {
                lib.insert(key.to_string(), Arc::new(clips));
            }
            Err(e) => log::warn!("{}: {e}", path.display()),
        }
    }
    lib
}

/// Olha a pasta a cada meio segundo e recarrega quando um `.toml` muda. Só existe com
/// `MINIUTILS_CLIPS` definida (uso de quem está desenhando), e cada recarga deixa os quadros
/// antigos na memória (são `'static`): não é para o uso normal.
fn watch(dir: PathBuf) {
    let stamp = |dir: &Path| -> Vec<(PathBuf, SystemTime)> {
        let mut v: Vec<_> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| Some((e.path(), e.metadata().ok()?.modified().ok()?)))
            .filter(|(p, _)| p.extension().is_some_and(|e| e == "toml"))
            .collect();
        v.sort();
        v
    };
    let spawned = std::thread::Builder::new().name("miniutils-clips".into()).spawn(move || {
        let mut last = stamp(&dir);
        log::info!("animações de {} (recarregam ao salvar)", dir.display());
        loop {
            std::thread::sleep(Duration::from_millis(500));
            let now = stamp(&dir);
            if now == last {
                continue;
            }
            last = now;
            let lib = load_dir(&dir);
            log::info!("animações recarregadas ({} mascote(s))", lib.len());
            if let Ok(mut l) = library().write() {
                *l = Arc::new(lib);
            }
        }
    });
    if let Err(e) = spawned {
        log::warn!("sem thread para recarregar animações ({e})");
    }
}

// ---- formato ------------------------------------------------------------------------

/// Papéis de cor válidos numa mascote comum (ver o topo de `sprites.rs`).
const ROLES: &[u8] = b".KPSHMCEWBTFDA";

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawClip {
    fps: Option<u32>,
    durations: Option<Vec<u32>>,
    #[serde(rename = "loop")]
    looping: Option<bool>,
    enter: Option<String>,
    exit: Option<String>,
    #[serde(default)]
    variants: Vec<String>,
    every: Option<[u32; 2]>,
    frames: Vec<String>,
}

/// Lê um arquivo de animações e confere cada quadro contra o desenho base de `sprite`.
pub fn parse(sprite: &Sprite, src: &str) -> Result<Clips, String> {
    let raw: HashMap<String, RawClip> = toml::from_str(src).map_err(|e| e.to_string())?;
    let mut clips = HashMap::new();
    for (name, c) in &raw {
        let err = |e: String| format!("[{name}]: {e}");
        if c.frames.is_empty() {
            return Err(err("nenhum quadro".into()));
        }
        let durations = match (&c.durations, c.fps) {
            (Some(_), Some(_)) => return Err(err("use `fps` ou `durations`, não os dois".into())),
            (Some(d), None) => {
                if d.len() != c.frames.len() {
                    return Err(err(format!("{} durações para {} quadros", d.len(), c.frames.len())));
                }
                if let Some(bad) = d.iter().find(|&&ms| !(16..=60_000).contains(&ms)) {
                    return Err(err(format!("duração {bad} ms fora de 16..60000")));
                }
                d.clone()
            }
            (None, fps) => {
                let fps = fps.unwrap_or(4);
                if !(1..=30).contains(&fps) {
                    return Err(err(format!("fps {fps} fora de 1..30")));
                }
                vec![1000 / fps; c.frames.len()]
            }
        };
        let mood = MOODS.contains(&name.as_str());
        if !mood && (c.enter.is_some() || c.exit.is_some() || !c.variants.is_empty()) {
            return Err(err("enter/exit/variants só valem numa animação de humor".into()));
        }
        // os nomes citados precisam existir no mesmo arquivo e tocar uma vez
        for other in c.enter.iter().chain(&c.exit).chain(&c.variants) {
            match raw.get(other) {
                None => return Err(err(format!("cita \"{other}\", que não existe neste arquivo"))),
                Some(o) if o.looping == Some(true) || MOODS.contains(&other.as_str()) => {
                    return Err(err(format!("\"{other}\" repete: transição e variação tocam uma vez")));
                }
                Some(_) => {}
            }
        }
        let every = c.every.unwrap_or([8, 20]);
        if every[0] == 0 || every[0] > every[1] {
            return Err(err(format!("every = {every:?}: use [mínimo, máximo] em segundos, mínimo ≥ 1")));
        }
        let mut frames = Vec::new();
        for (i, text) in c.frames.iter().enumerate() {
            frames.push(grid(sprite, text).map_err(|e| err(format!("quadro {}: {e}", i + 1)))?);
        }
        clips.insert(
            name.clone(),
            Clip {
                frames,
                durations,
                looping: c.looping.unwrap_or(mood),
                enter: c.enter.clone(),
                exit: c.exit.clone(),
                variants: c.variants.clone(),
                every: (every[0], every[1]),
            },
        );
    }
    Ok(clips)
}

/// Converte o texto de um quadro em `Grid`, conferindo tamanho e cores.
fn grid(sprite: &Sprite, text: &str) -> Result<Grid, String> {
    let rows: Vec<&str> = text.lines().map(str::trim_end).filter(|l| !l.is_empty()).collect();
    if rows.len() != sprite.grid.len() {
        return Err(format!("{} linhas, esperava {}", rows.len(), sprite.grid.len()));
    }
    let mut out: Grid = [""; 14];
    for (y, row) in rows.iter().enumerate() {
        let want = sprite.grid[y].len();
        if row.len() != want {
            return Err(format!("linha {} com {} colunas, esperava {want}", y + 1, row.len()));
        }
        if let Some(bad) = row.bytes().find(|&c| !valid_cell(sprite, c)) {
            return Err(format!("linha {}: cor '{}' que esta mascote não tem", y + 1, bad as char));
        }
        // dados estáticos: carregados uma vez e usados até o fim do programa
        out[y] = Box::leak(row.to_string().into_boxed_str());
    }
    Ok(out)
}

fn valid_cell(sprite: &Sprite, c: u8) -> bool {
    match &sprite.pixel {
        // mascote "pixel": as letras da paleta dela, o olho `e` e o transparente
        Some(px) => c == b'.' || c == b'e' || px.color(c).is_some(),
        None => ROLES.contains(&c),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn frame(rows: &[&str]) -> String {
        format!("\"\"\"\n{}\n\"\"\"", rows.join("\n"))
    }

    #[test]
    fn arquivos_de_animacao_validos() {
        for (key, src) in SOURCES {
            let sprite = sprites::get(key);
            assert_eq!(sprite.key, *key, "mascote {key} não existe em sprites.rs");
            parse(sprite, src).unwrap_or_else(|e| panic!("{key}: {e}"));
        }
    }

    #[test]
    fn recusa_quadro_errado() {
        let neko = sprites::get("neko");
        let curto = "[x]\nframes = [\"\"\"\nKKKK\n\"\"\"]";
        assert!(parse(neko, curto).unwrap_err().contains("linhas"));
        let linha = "Z".repeat(16);
        let cor = format!("[x]\nframes = [{}]", frame(&[linha.as_str(); 14]));
        assert!(parse(neko, &cor).unwrap_err().contains("cor 'Z'"));
    }

    #[test]
    fn recusa_referencia_e_duracao_erradas() {
        let neko = sprites::get("neko");
        let base = frame(&neko.grid);
        let sem_nome = format!("[idle]\nenter = \"boceja\"\nframes = [{base}]");
        assert!(parse(neko, &sem_nome).unwrap_err().contains("\"boceja\""));
        let duracoes = format!("[idle]\ndurations = [100]\nframes = [{base}, {base}]");
        assert!(parse(neko, &duracoes).unwrap_err().contains("1 durações para 2 quadros"));
        let fora = format!("[x]\nenter = \"y\"\nframes = [{base}]\n[y]\nframes = [{base}]");
        assert!(parse(neko, &fora).unwrap_err().contains("só valem numa animação de humor"));
    }

    #[test]
    fn duracao_por_quadro() {
        let neko = sprites::get("neko");
        let base = frame(&neko.grid);
        let src = format!(
            "[idle]\ndurations = [800, 100, 100]\nframes = [{base}, {base}, {base}]\n[x]\nfps = 10\nframes = [{base}, {base}]"
        );
        let clips = parse(neko, &src).unwrap();
        let idle = &clips["idle"];
        assert!(idle.looping);
        assert_eq!(idle.total_ms(), 1000);
        assert_eq!(idle.shortest_ms(), 100);
        assert_eq!(idle.index_at(0), 0);
        assert_eq!(idle.index_at(799), 0);
        assert_eq!(idle.index_at(800), 1);
        assert_eq!(idle.index_at(950), 2);
        assert_eq!(idle.index_at(1000), 0);
        let x = &clips["x"];
        assert!(!x.looping);
        assert_eq!(x.index_at(5000), 1);
        assert!(!x.done_at(199));
        assert!(x.done_at(200));
    }
}
