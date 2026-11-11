//! Notificações do desktop, no máximo uma por minuto para o mesmo evento.

use std::collections::HashMap;
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct Notifier {
    last: HashMap<String, Instant>,
}

impl Notifier {
    /// `key` identifica o evento (ex.: "crash:web") para o limite de frequência.
    pub fn send(&mut self, key: &str, title: &str, body: &str, urgent: bool) {
        let now = Instant::now();
        if self.last.get(key).is_some_and(|t| now.duration_since(*t) < Duration::from_secs(60)) {
            return;
        }
        self.last.insert(key.to_string(), now);
        let (title, body) = (title.to_string(), body.to_string());
        // o envio fala com o D-Bus e pode demorar: fora da thread da UI
        std::thread::spawn(move || {
            let mut n = notify_rust::Notification::new();
            n.appname(&crate::core::config::display_name()).summary(&title).body(&body).icon("utilities-system-monitor");
            n.urgency(if urgent { notify_rust::Urgency::Normal } else { notify_rust::Urgency::Low });
            if !urgent {
                n.timeout(8000);
            }
            if let Err(e) = n.show() {
                eprintln!("miniutils: notificação falhou: {e}");
            }
        });
    }
}
