//! Peças comuns dos mini-apps: a mascote do cabeçalho, a fala com prazo e textos.
//!
//! Cada app guarda as suas e entrega o resultado à própria tela (as globais do Slint são
//! de tipos diferentes por app, então o "pôr na tela" fica no app, em duas linhas).

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use slint::{Image, SharedString};

use crate::mascot::anim::{Animator, Reaction};
use crate::mascot::sprites::Sprite;
use crate::mascot::themes::Theme;
use crate::mascot::{Mood, render};

/// A mascote do cabeçalho de um app: piscada, reações, cutucada e o último quadro
/// desenhado (a imagem só é trocada quando os pixels mudam).
pub struct HeaderMascot {
    cache: render::Cache,
    anim: Animator,
    reaction: Option<Reaction>,
}

impl HeaderMascot {
    /// `seed` distingue as mascotes (piscam em momentos diferentes).
    pub fn new(seed: u32) -> Self {
        HeaderMascot { cache: Default::default(), anim: Animator::new(seed), reaction: None }
    }

    /// Redesenha o quadro de agora; `true` se a imagem ou a reação mudaram.
    pub fn refresh(&mut self, sprite: &Sprite, theme: &Theme, mood: Mood) -> bool {
        let look = self.anim.look(mood, Instant::now());
        let reaction_changed = std::mem::replace(&mut self.reaction, look.reaction) != look.reaction;
        self.cache.update_look(sprite, theme, &look) || reaction_changed
    }

    /// Clique na mascote. Com "reduzir movimento" não reage. `true` se a reação começou
    /// (redesenhe).
    pub fn poke(&mut self, mood: Mood, reduce_motion: bool) -> bool {
        !reduce_motion && self.anim.poke(mood, Instant::now())
    }

    /// Dispara uma reação (comemorar, susto); `true` se começou.
    pub fn react(&mut self, r: Reaction) -> bool {
        self.anim.trigger(r, Instant::now())
    }

    pub fn image(&self) -> Image {
        self.cache.image()
    }

    /// Nome da reação para a tela ("" = nenhuma).
    pub fn reaction(&self) -> SharedString {
        self.reaction.map_or("", Reaction::as_str).into()
    }
}

/// O que a mascote está dizendo, com prazo: depois dele, o app volta à fala padrão.
pub struct Speech {
    said: Option<(String, Instant)>,
    ttl: Duration,
}

impl Speech {
    pub fn new(ttl: Duration) -> Self {
        Speech { said: None, ttl }
    }

    pub fn say(&mut self, text: String) {
        self.said = Some((text, Instant::now()));
    }

    /// A fala, se ainda está no prazo.
    pub fn current(&self) -> Option<&str> {
        self.said.as_ref().filter(|(_, at)| at.elapsed() < self.ttl).map(|(t, _)| t.as_str())
    }

    /// A fala é `text` (no prazo ou não): para trocar só a fala que o próprio app pôs.
    pub fn is(&self, text: &str) -> bool {
        self.said.as_ref().is_some_and(|(t, _)| t == text)
    }

    pub fn clear(&mut self) {
        self.said = None;
    }

    /// A fala acabou de vencer: some e devolve `true` uma vez (redesenhe o cabeçalho).
    /// Chame no `tick`, para a fala sumir no prazo mesmo sem leitura nova.
    pub fn take_expired(&mut self) -> bool {
        if self.said.is_some() && self.current().is_none() {
            self.said = None;
            return true;
        }
        false
    }
}

/// "1 porta", "3 portas".
pub fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// Segundos desde 1970 (0 se o relógio estiver antes disso).
pub fn unix_now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fala_vence_no_prazo() {
        let mut s = Speech::new(Duration::from_millis(30));
        assert_eq!(s.current(), None);
        s.say("oi".into());
        assert_eq!(s.current(), Some("oi"));
        assert!(s.is("oi"));
        std::thread::sleep(Duration::from_millis(40));
        assert_eq!(s.current(), None);
        assert!(s.is("oi"));
        assert!(s.take_expired());
        assert!(!s.take_expired());
        assert!(!s.is("oi"));
    }

    #[test]
    fn plural_conta() {
        assert_eq!(plural(1, "porta", "portas"), "1 porta");
        assert_eq!(plural(0, "porta", "portas"), "0 portas");
    }
}
