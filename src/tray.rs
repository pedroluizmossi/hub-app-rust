//! Tray icon: the mascot in the hub's mood, with the menu (show, pin, media, quit).
//!
//! The same interface on both systems (`Tray::spawn`, `online`, `update`, `set_pinned`):
//! StatusNotifierItem over D-Bus on Linux (`linux.rs`), the notification area on Windows
//! (`windows.rs`).

#[cfg(target_os = "linux")]
#[path = "tray/linux.rs"]
mod imp;
#[cfg(windows)]
#[path = "tray/windows.rs"]
mod imp;

pub use imp::Tray;

use crate::core::miniapp::TrayMedia;

/// Longest "Artist — Title" line in the menu, in characters: a long one would make the menu
/// as wide as the screen.
const MEDIA_LABEL_MAX: usize = 60;

/// The media items of the menu: (label, verb for `hub::tray_media_command`, enabled).
pub(crate) fn media_items(m: &TrayMedia) -> [(&'static str, &'static str, bool); 3] {
    [
        (crate::tr!("Previous track"), "prev", m.can_previous),
        (if m.playing { crate::tr!("Pause") } else { crate::tr!("Play") }, "play-pause", m.can_play_pause),
        (crate::tr!("Next track"), "next", m.can_next),
    ]
}

/// What is playing, cut to `MEDIA_LABEL_MAX` characters with "…". Each system then escapes it
/// for its menu.
pub(crate) fn media_label(summary: &str) -> String {
    let summary = summary.trim();
    if summary.chars().count() <= MEDIA_LABEL_MAX {
        return summary.to_string();
    }
    let cut: String = summary.chars().take(MEDIA_LABEL_MAX - 1).collect();
    format!("{}…", cut.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn media(playing: bool) -> TrayMedia {
        TrayMedia {
            summary: String::new(),
            playing,
            can_previous: false,
            can_play_pause: true,
            can_next: true,
        }
    }

    #[test]
    fn media_label_is_cut_with_an_ellipsis() {
        assert_eq!(media_label("Artist — Title"), "Artist — Title");
        let long = "á".repeat(100);
        let label = media_label(&long);
        assert_eq!(label.chars().count(), MEDIA_LABEL_MAX);
        assert!(label.ends_with('…'));
        assert_eq!(media_label(&"x".repeat(MEDIA_LABEL_MAX)), "x".repeat(MEDIA_LABEL_MAX));
    }

    #[test]
    fn media_items_switch_play_and_pause() {
        let [prev, play, next] = media_items(&media(false));
        assert_eq!((prev.1, prev.2), ("prev", false));
        assert_eq!(play, (crate::tr!("Play"), "play-pause", true));
        assert_eq!((next.1, next.2), ("next", true));
        assert_eq!(media_items(&media(true))[1].0, crate::tr!("Pause"));
    }
}
