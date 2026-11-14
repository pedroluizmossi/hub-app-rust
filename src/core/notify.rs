//! Desktop notifications, at most one per minute for the same event.

use std::collections::HashMap;
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct Notifier {
    last: HashMap<String, Instant>,
}

impl Notifier {
    /// `key` identifies the event (e.g. "crash:web") for the rate limit.
    pub fn send(&mut self, key: &str, title: &str, body: &str, urgent: bool) {
        let now = Instant::now();
        if self.last.get(key).is_some_and(|t| now.duration_since(*t) < Duration::from_secs(60)) {
            return;
        }
        self.last.insert(key.to_string(), now);
        let (title, body) = (title.to_string(), body.to_string());
        // sending talks to the system (D-Bus on Linux) and may be slow: off the UI thread
        std::thread::spawn(move || {
            let mut n = notify_rust::Notification::new();
            n.appname(&crate::core::config::display_name()).summary(&title).body(&body).icon("utilities-system-monitor");
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
}
