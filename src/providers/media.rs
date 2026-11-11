//! Mídia: quem está tocando, o quê, e os comandos do player.
//!
//! A coleta lê os players a cada segundo e só devolve um snapshot quando algo mudou. Os
//! comandos (play/pause, faixa, avançar) vão numa thread própria: o sistema pode demorar e
//! a UI não espera.
//!
//! A fonte muda por sistema, atrás da mesma interface (`MediaProvider`, `send`,
//! `valid_service`): MPRIS pelo D-Bus no Linux (`linux.rs`), os controles de mídia do
//! sistema no Windows (`windows.rs`).

#[cfg(target_os = "linux")]
#[path = "media/linux.rs"]
mod imp;
#[cfg(windows)]
#[path = "media/windows.rs"]
mod imp;

pub use imp::{MediaProvider, send, valid_service};

/// O que está tocando, num instante.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct MediaSnapshot {
    /// O sistema respondeu (false = D-Bus ou serviço de mídia fora do ar).
    pub ok: bool,
    pub error: String,
    /// Identificador do player (serviço MPRIS no Linux, id do app no Windows); vazio =
    /// nenhum player aberto.
    pub player: String,
    /// Nome amigável do player (`Identity`): "Firefox", "plasma-browser-integration"…
    pub app: String,
    /// Playing | Paused | Stopped
    pub state: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    /// Segundos.
    pub length: u32,
    pub position: u32,
    pub can_play: bool,
    pub can_pause: bool,
    pub can_previous: bool,
    pub can_next: bool,
    pub can_seek: bool,
}

impl MediaSnapshot {
    pub fn playing(&self) -> bool {
        self.state == "Playing"
    }

    pub fn paused(&self) -> bool {
        self.state == "Paused"
    }

    /// Tem um player aberto (mesmo parado).
    pub fn has_player(&self) -> bool {
        !self.player.is_empty()
    }

    /// Para o tooltip da bandeja.
    pub fn summary(&self) -> String {
        if !self.ok {
            return "CD Player: sem o barramento".into();
        }
        if !self.has_player() {
            return "CD Player: sem disco".into();
        }
        let nome = if self.app.is_empty() { self.player.as_str() } else { self.app.as_str() };
        if self.title.is_empty() {
            return format!("CD Player: {nome} aberto");
        }
        if self.artist.is_empty() {
            format!("CD Player: {}", self.title)
        } else {
            format!("CD Player: {} — {}", self.title, self.artist)
        }
    }
}

/// Preferência entre players, do mais importante ao menos: está tocando, sabe a duração
/// (dá barra e tempo), é o que já estava em uso, tem título/artista.
pub fn usable(s: &MediaSnapshot, current: &str) -> (u8, u8, u8, u8) {
    (
        s.playing() as u8,
        (s.length > 0) as u8,
        (s.player == current) as u8,
        (!s.title.is_empty() || !s.artist.is_empty()) as u8,
    )
}

/// "3:07"; "1:02:03" quando passa de uma hora.
pub fn mmss(total: u32) -> String {
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

/// Nome amigável de um player do Windows a partir do identificador do app
/// (`Spotify.exe`, `SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify`, `MSEdge`…).
#[cfg(any(windows, test))]
pub fn friendly_app(id: &str) -> String {
    /// Identificadores que não dizem o nome do programa.
    const KNOWN: &[(&str, &str)] = &[("308046B0AF4A39CB", "Firefox"), ("MSEdge", "Edge")];
    if let Some((_, name)) = KNOWN.iter().find(|(k, _)| k.eq_ignore_ascii_case(id)) {
        return name.to_string();
    }
    // app da loja: o nome vem depois do `!`
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
    fn nome_do_player_no_windows() {
        assert_eq!(friendly_app("Spotify.exe"), "Spotify");
        assert_eq!(friendly_app("chrome.EXE"), "Chrome");
        assert_eq!(friendly_app("SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify"), "Spotify");
        assert_eq!(friendly_app("308046B0AF4A39CB"), "Firefox");
        assert_eq!(friendly_app("MSEdge"), "Edge");
        assert_eq!(friendly_app(""), "");
    }
}
