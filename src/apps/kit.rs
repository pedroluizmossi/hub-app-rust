//! Shared pieces of the mini-apps: the header mascot, speech with an expiry, and texts.
//!
//! Each app keeps its own and hands the result to its own screen (the Slint globals have a
//! different type per app, so "putting it on screen" stays in the app, in two lines).

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use slint::{Image, SharedString};

use crate::mascot::anim::{Animator, Reaction};
use crate::mascot::sprites::Sprite;
use crate::mascot::themes::Theme;
use crate::mascot::{Mood, render};

/// The mascot in an app's header: blinking, reactions, pokes and the last frame drawn
/// (the image is only swapped when the pixels change).
pub struct HeaderMascot {
    cache: render::Cache,
    anim: Animator,
    reaction: Option<Reaction>,
}

impl HeaderMascot {
    /// `seed` tells the mascots apart (they blink at different moments).
    pub fn new(seed: u32) -> Self {
        HeaderMascot { cache: Default::default(), anim: Animator::new(seed), reaction: None }
    }

    /// Redraws the current frame; `true` if the image or the reaction changed.
    pub fn refresh(&mut self, sprite: &Sprite, theme: &Theme, mood: Mood) -> bool {
        let look = self.anim.look_with(sprite, mood, Instant::now());
        let reaction_changed = std::mem::replace(&mut self.reaction, look.reaction) != look.reaction;
        self.cache.update_look(sprite, theme, &look) || reaction_changed
    }

    /// Click on the mascot. With "reduce motion" it doesn't react. `true` if the reaction
    /// started (redraw).
    pub fn poke(&mut self, mood: Mood, reduce_motion: bool) -> bool {
        !reduce_motion && self.anim.poke(mood, Instant::now())
    }

    /// Triggers a reaction (celebrate, startle); `true` if it started.
    pub fn react(&mut self, r: Reaction) -> bool {
        self.anim.trigger(r, Instant::now())
    }

    pub fn image(&self) -> Image {
        self.cache.image()
    }

    /// Name of the reaction for the screen ("" = none).
    pub fn reaction(&self) -> SharedString {
        self.reaction.map_or("", Reaction::as_str).into()
    }
}

/// What the mascot is saying, with an expiry: after it, the app goes back to its default line.
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

    /// The line, if it hasn't expired yet.
    pub fn current(&self) -> Option<&str> {
        self.said.as_ref().filter(|(_, at)| at.elapsed() < self.ttl).map(|(t, _)| t.as_str())
    }

    /// The line is `text` (expired or not): to replace only a line the app itself said.
    pub fn is(&self, text: &str) -> bool {
        self.said.as_ref().is_some_and(|(t, _)| t == text)
    }

    pub fn clear(&mut self) {
        self.said = None;
    }

    /// The line just expired: it goes away and this returns `true` once (redraw the header).
    /// Call it in `tick`, so the line disappears on time even without a new reading.
    pub fn take_expired(&mut self) -> bool {
        if self.said.is_some() && self.current().is_none() {
            self.said = None;
            return true;
        }
        false
    }
}

/// "1 port", "3 ports". `one` and `many` are already translated.
pub fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// Seconds since 1970 (0 if the clock is set before that).
pub fn unix_now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speech_expires_on_time() {
        let mut s = Speech::new(Duration::from_millis(30));
        assert_eq!(s.current(), None);
        s.say("hi".into());
        assert_eq!(s.current(), Some("hi"));
        assert!(s.is("hi"));
        std::thread::sleep(Duration::from_millis(40));
        assert_eq!(s.current(), None);
        assert!(s.is("hi"));
        assert!(s.take_expired());
        assert!(!s.take_expired());
        assert!(!s.is("hi"));
    }

    #[test]
    fn plural_counts() {
        assert_eq!(plural(1, "port", "ports"), "1 port");
        assert_eq!(plural(0, "port", "ports"), "0 ports");
    }
}
