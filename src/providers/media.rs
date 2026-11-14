//! Media: who is playing, what, and the player commands.
//!
//! Collection reads the players every second and only returns a snapshot when something
//! changed. Commands (play/pause, track, seek) run on their own thread: the system may be
//! slow and the UI doesn't wait.
//!
//! The source differs per system, behind the same interface (`MediaProvider`, `send`,
//! `valid_service`): MPRIS over D-Bus on Linux (`linux.rs`), the system media controls on
//! Windows (`windows.rs`). The album cover comes from both (a URL on Linux, a stream on
//! Windows) and is pixelated here, on the provider thread.

#[cfg(target_os = "linux")]
#[path = "media/linux.rs"]
mod imp;
#[cfg(windows)]
#[path = "media/windows.rs"]
mod imp;

pub use imp::{MediaProvider, SOURCE, seek_to, send, valid_service};

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use crate::tr;

/// Side of the pixelated album cover, in pixels (the label of the CD Player disc).
pub const ART_SIZE: u32 = 16;
/// Cover files bigger than this are not read (a cover is tens or hundreds of KB).
pub const ART_MAX_BYTES: u64 = 8 << 20;
/// Largest cover accepted, in pixels per side (refuses absurd sizes before decoding).
const ART_MAX_SIDE: u32 = 4096;

/// The album cover, already pixelated: `ART_SIZE` × `ART_SIZE` RGB pixels, row by row.
/// Compared by `id` (a hash of the pixels), so comparing snapshots every second stays cheap.
#[derive(Clone)]
pub struct Art {
    pub id: u64,
    pub px: Arc<Vec<[u8; 3]>>,
}

impl PartialEq for Art {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl std::fmt::Debug for Art {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Art({:016x})", self.id)
    }
}

/// Cover image (PNG or JPEG bytes, as the player gives it) -> pixelated art: the center
/// square, shrunk to `ART_SIZE` by averaging. `None` if it isn't an image we read.
pub fn pixelate(bytes: &[u8]) -> Option<Art> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(ART_MAX_SIDE);
    limits.max_image_height = Some(ART_MAX_SIDE);
    reader.limits(limits);
    let img = reader.decode().ok()?.to_rgb8();
    let (w, h) = img.dimensions();
    let side = w.min(h);
    if side == 0 {
        return None;
    }
    let square = image::imageops::crop_imm(&img, (w - side) / 2, (h - side) / 2, side, side).to_image();
    let small = image::imageops::thumbnail(&square, ART_SIZE, ART_SIZE);
    let px: Vec<[u8; 3]> = small.pixels().map(|p| p.0).collect();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    px.hash(&mut hasher);
    Some(Art { id: hasher.finish(), px: Arc::new(px) })
}

/// Last cover read, by key (the URL on Linux, the track on Windows): reading and decoding
/// only happen when the track changes. A failed read is tried again up to `tries` times.
#[derive(Default)]
pub struct ArtCache {
    key: String,
    art: Option<Art>,
    tries: u8,
}

impl ArtCache {
    /// The cover for `key`; `load` reads its bytes (only called when needed, at most
    /// `tries` times per key). An empty key means "no cover".
    pub fn get(&mut self, key: &str, tries: u8, load: impl FnOnce() -> Option<Vec<u8>>) -> Option<Art> {
        if key != self.key {
            self.key = key.to_string();
            self.art = None;
            self.tries = 0;
        }
        if key.is_empty() {
            return None;
        }
        if self.art.is_none() && self.tries < tries {
            self.tries += 1;
            self.art = load().and_then(|bytes| pixelate(&bytes));
        }
        self.art.clone()
    }
}

/// Local path of a `file://` URL (the cover of browsers and local players), with the
/// `%XX` escapes undone. `None` for any other scheme.
#[cfg(any(target_os = "linux", test))]
pub fn file_url_path(url: &str) -> Option<std::path::PathBuf> {
    let rest = url.strip_prefix("file://")?;
    // `file://host/path`: only the local host (empty or "localhost")
    let path = rest.strip_prefix("localhost").unwrap_or(rest);
    if !path.starts_with('/') {
        return None;
    }
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        if bytes[i] == b'%'
            && let (Some(hi), Some(lo)) = (bytes.get(i + 1).and_then(|&b| hex(b)), bytes.get(i + 2).and_then(|&b| hex(b)))
        {
            out.push((hi * 16 + lo) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Some(std::path::PathBuf::from(String::from_utf8_lossy(&out).into_owned()))
}

/// What is playing, at one instant.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct MediaSnapshot {
    /// The system answered (false = D-Bus or media service down).
    pub ok: bool,
    pub error: String,
    /// Player identifier (MPRIS service on Linux, app id on Windows); empty = no player
    /// open.
    pub player: String,
    /// Friendly player name (`Identity`): "Firefox", "plasma-browser-integration"…
    pub app: String,
    /// Playing | Paused | Stopped
    pub state: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    /// Seconds.
    pub length: u32,
    pub position: u32,
    pub can_play: bool,
    pub can_pause: bool,
    pub can_previous: bool,
    pub can_next: bool,
    pub can_seek: bool,
    /// The player can bring its window to the front (MPRIS `CanRaise`; never on Windows,
    /// which has no way to do it from here).
    pub can_raise: bool,
    /// Album cover, pixelated (`None` = the player gives none or it didn't decode). Left
    /// out of the plugins' JSON.
    #[serde(skip)]
    pub art: Option<Art>,
}

impl MediaSnapshot {
    pub fn playing(&self) -> bool {
        self.state == "Playing"
    }

    /// The play/pause button does something now: a player is open and accepts the action
    /// the button would take (pause while playing, play otherwise).
    pub fn can_play_pause(&self) -> bool {
        self.has_player() && if self.playing() { self.can_pause } else { self.can_play }
    }

    pub fn paused(&self) -> bool {
        self.state == "Paused"
    }

    /// A player is open (even if stopped).
    pub fn has_player(&self) -> bool {
        !self.player.is_empty()
    }

    /// For the tray tooltip.
    pub fn summary(&self) -> String {
        if !self.ok {
            return tr!("CD Player: no media bus").into();
        }
        if !self.has_player() {
            return tr!("CD Player: no disc").into();
        }
        let name = if self.app.is_empty() { self.player.as_str() } else { self.app.as_str() };
        if self.title.is_empty() {
            return tr!("CD Player: {} open", name);
        }
        if self.artist.is_empty() {
            format!("CD Player: {}", self.title)
        } else {
            format!("CD Player: {} — {}", self.title, self.artist)
        }
    }
}

/// How good a player is to show (`usable`); higher wins, compared in order.
pub type Score = (u8, u8, u8, u8);

/// Preference between players, most important first: is playing, knows the length (gives
/// a bar and time), is the one already in use, has title/artist.
pub fn usable(s: &MediaSnapshot, current: &str) -> Score {
    (
        s.playing() as u8,
        (s.length > 0) as u8,
        (s.player == current) as u8,
        (!s.title.is_empty() || !s.artist.is_empty()) as u8,
    )
}

/// "3:07"; "1:02:03" past one hour.
pub fn mmss(total: u32) -> String {
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

/// Friendly name of a Windows player from the app identifier
/// (`Spotify.exe`, `SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify`, `MSEdge`…).
#[cfg(any(windows, test))]
pub fn friendly_app(id: &str) -> String {
    /// Identifiers that don't tell the program's name.
    const KNOWN: &[(&str, &str)] = &[("308046B0AF4A39CB", "Firefox"), ("MSEdge", "Edge")];
    if let Some((_, name)) = KNOWN.iter().find(|(k, _)| k.eq_ignore_ascii_case(id)) {
        return name.to_string();
    }
    // Store app: the name comes after the `!`
    let name = id.rsplit('!').next().unwrap_or(id);
    let name = if name.to_ascii_lowercase().ends_with(".exe") { &name[..name.len() - 4] } else { name };
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_player_name() {
        assert_eq!(friendly_app("Spotify.exe"), "Spotify");
        assert_eq!(friendly_app("chrome.EXE"), "Chrome");
        assert_eq!(friendly_app("SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify"), "Spotify");
        assert_eq!(friendly_app("308046B0AF4A39CB"), "Firefox");
        assert_eq!(friendly_app("MSEdge"), "Edge");
        assert_eq!(friendly_app(""), "");
    }

    /// A PNG `w`×`h`: left half red, right half blue.
    fn png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbImage::from_fn(w, h, |x, _| if x < w / 2 { image::Rgb([255, 0, 0]) } else { image::Rgb([0, 0, 255]) });
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    #[test]
    fn cover_is_cropped_to_the_center_and_shrunk() {
        let art = pixelate(&png(64, 32)).unwrap();
        assert_eq!(art.px.len(), (ART_SIZE * ART_SIZE) as usize);
        // center square of a 64×32: x 16..48, still red on the left and blue on the right
        let row = &art.px[..ART_SIZE as usize];
        assert_eq!(row[0], [255, 0, 0]);
        assert_eq!(row[ART_SIZE as usize - 1], [0, 0, 255]);
        // same picture, same id; not an image, no art
        assert_eq!(pixelate(&png(64, 32)).unwrap().id, art.id);
        assert!(pixelate(b"not an image").is_none());
    }

    #[test]
    fn cover_is_read_once_per_key_and_retried_a_few_times() {
        let mut cache = ArtCache::default();
        let mut reads = 0;
        assert!(cache.get("a", 4, || { reads += 1; Some(png(32, 32)) }).is_some());
        assert!(cache.get("a", 4, || { reads += 1; Some(png(32, 32)) }).is_some());
        assert_eq!(reads, 1);
        // a key whose cover fails: tried again, but not forever
        for _ in 0..10 {
            assert!(cache.get("b", 4, || { reads += 1; None }).is_none());
        }
        assert_eq!(reads, 1 + 4);
        // no key, no cover (and nothing read)
        assert!(cache.get("", 4, || { reads += 1; Some(png(32, 32)) }).is_none());
        assert_eq!(reads, 1 + 4);
    }

    #[test]
    fn file_url_becomes_a_path() {
        let p = |u: &str| file_url_path(u).map(|p| p.to_string_lossy().replace('\\', "/"));
        assert_eq!(p("file:///tmp/firefox-mpris/cover%20art_1.png").as_deref(), Some("/tmp/firefox-mpris/cover art_1.png"));
        assert_eq!(p("file://localhost/home/a/%C3%A1lbum.jpg").as_deref(), Some("/home/a/álbum.jpg"));
        // a lone `%` stays as it is
        assert_eq!(p("file:///a/100%.jpg").as_deref(), Some("/a/100%.jpg"));
        assert_eq!(p("https://i.scdn.co/image/x"), None);
        assert_eq!(p("file://otherhost/a.jpg"), None);
    }
}
