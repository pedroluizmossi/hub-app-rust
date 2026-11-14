//! Media: who is playing, what, and the player commands.
//!
//! Collection reads the players every second and only returns a snapshot when something
//! changed. Commands (play/pause, track, seek) run on their own thread: the system may be
//! slow and the UI doesn't wait.
//!
//! The source differs per system, behind the same interface (`MediaProvider`, `send`,
//! `valid_service`): MPRIS over D-Bus on Linux (`linux.rs`), the system media controls on
//! Windows (`windows.rs`).

#[cfg(target_os = "linux")]
#[path = "media/linux.rs"]
mod imp;
#[cfg(windows)]
#[path = "media/windows.rs"]
mod imp;

pub use imp::{MediaProvider, send, valid_service};

use crate::tr;

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
}

impl MediaSnapshot {
    pub fn playing(&self) -> bool {
        self.state == "Playing"
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

/// Preference between players, most important first: is playing, knows the length (gives
/// a bar and time), is the one already in use, has title/artist.
pub fn usable(s: &MediaSnapshot, current: &str) -> (u8, u8, u8, u8) {
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
}
