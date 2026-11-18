//! Desktop notifications, at most one per minute for the same event.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Shortest time between two notifications of the same event.
const RATE: Duration = Duration::from_secs(60);

#[derive(Default)]
pub struct Notifier {
    last: HashMap<String, Instant>,
}

impl Notifier {
    /// `key` identifies the event (e.g. "crash:web") for the rate limit.
    pub fn send(&mut self, key: &str, title: &str, body: &str, urgent: bool) {
        if !self.should_send(key, Instant::now()) {
            return;
        }
        let (title, body) = (title.to_string(), body.to_string());
        // sending talks to the system (D-Bus on Linux) and may be slow: off the UI thread
        std::thread::spawn(move || {
            let mut n = notify_rust::Notification::new();
            n.appname(&crate::core::config::display_name()).summary(&title).body(&body).icon("utilities-system-monitor");
            // Windows ignores `appname`: the toast shows the name and icon of a registered
            // AppUserModelID (without one, notify-rust uses PowerShell's)
            #[cfg(windows)]
            if let Some(id) = toast::app_id() {
                n.app_id(id);
            }
            // urgency only exists in Linux notifications
            #[cfg(target_os = "linux")]
            n.urgency(if urgent { notify_rust::Urgency::Normal } else { notify_rust::Urgency::Low });
            if !urgent {
                n.timeout(8000);
            }
            if let Err(e) = n.show() {
                log::warn!("notification failed: {e}");
            }
        });
    }

    /// Rate limit: `false` if `key` was sent less than `RATE` ago. Keys older than that are
    /// forgotten, so per-session keys (`agents:<tool>:<session>`) don't pile up for days.
    fn should_send(&mut self, key: &str, now: Instant) -> bool {
        self.last.retain(|_, t| now.saturating_duration_since(*t) < RATE);
        if self.last.contains_key(key) {
            return false;
        }
        self.last.insert(key.to_string(), now);
        true
    }
}

/// The AppUserModelID of Windows toasts: registered per user under
/// `HKCU\Software\Classes\AppUserModelId` with the name and an icon (the mascot), so the
/// toasts say "MiniUtils" instead of "Windows PowerShell". The uninstaller removes the key.
#[cfg(windows)]
mod toast {
    use std::path::PathBuf;
    use std::sync::OnceLock;

    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, REG_SZ, RegSetKeyValueW};
    use windows::core::HSTRING;

    const APP_ID: &str = "MiniUtils.MiniUtils";
    const KEY: &str = "Software\\Classes\\AppUserModelId\\MiniUtils.MiniUtils";

    /// The id to send toasts with; registered on the first call (once per run, so a new
    /// display name or a moved data folder is picked up at the next start). `None` if the
    /// registration failed: the toast then goes out as PowerShell's, as before.
    pub fn app_id() -> Option<&'static str> {
        static REGISTERED: OnceLock<bool> = OnceLock::new();
        let ok = *REGISTERED.get_or_init(|| match register() {
            Ok(()) => true,
            Err(e) => {
                log::warn!("notifications: could not register the app id ({e}); they show as PowerShell");
                false
            }
        });
        ok.then_some(APP_ID)
    }

    fn register() -> Result<(), String> {
        set("DisplayName", &crate::core::config::display_name())?;
        // without an icon Windows shows a generic one: not a reason to give up the name
        match icon() {
            Ok(path) => set("IconUri", &path.to_string_lossy())?,
            Err(e) => log::debug!("notifications: no icon ({e})"),
        }
        Ok(())
    }

    fn set(name: &str, value: &str) -> Result<(), String> {
        let wide: Vec<u16> = value.encode_utf16().chain(Some(0)).collect();
        // SAFETY: the strings live until the call returns; the size is in bytes, with the 0.
        // Creates the key if it does not exist.
        let r = unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                &HSTRING::from(KEY),
                &HSTRING::from(name),
                REG_SZ.0,
                Some(wide.as_ptr().cast()),
                (wide.len() * 2) as u32,
            )
        };
        r.ok().map_err(|e| e.message())
    }

    /// The mascot as a PNG in `%LOCALAPPDATA%\miniutils` (the toast needs an image file).
    fn icon() -> Result<PathBuf, String> {
        use crate::mascot::{Mood, render, sprites, themes};
        let dir = dirs::data_local_dir().ok_or("no local data folder")?.join("miniutils");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = dir.join("notification-icon.png");
        let (side, rgba) = render::rgba_square(sprites::get("neko"), themes::get("neon"), Mood::Idle, 1, 4);
        image::save_buffer(&path, &rgba, side as u32, side as u32, image::ExtendedColorType::Rgba8).map_err(|e| e.to_string())?;
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limits_per_key_and_forgets_old_keys() {
        let mut n = Notifier::default();
        let t0 = Instant::now();
        assert!(n.should_send("a", t0));
        assert!(!n.should_send("a", t0 + Duration::from_secs(30)));
        assert!(n.should_send("b", t0 + Duration::from_secs(30)));
        // past the limit: "a" goes again, and nothing older than RATE is kept
        assert!(n.should_send("a", t0 + RATE));
        assert!(n.should_send("c", t0 + RATE * 3));
        assert_eq!(n.last.len(), 1);
    }
}
