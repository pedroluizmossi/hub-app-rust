//! Short mascot reactions, on top of the mood.
//!
//! The `Mood` is the lasting state (asleep, normal, happy, busy, alarm). A reaction is a
//! short burst fired by an event ("a container came back") or by the user (easter egg);
//! when it ends, the mascot goes back to its mood. Each mascot has its own `Animator`.
//!
//! Everything is measured by the clock, not by frames: the window timer changes pace with
//! the mouse (see `FRAME_HOT` in `hub.rs`), and a reaction counted in frames would last twice
//! as long. For the same reason, a reaction fired while the mascot is off screen expires on
//! its own.

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::Mood;
use super::clips::{self, Clips, Grid};
use super::pack;
use super::sprites::Sprite;

/// "Reduce motion" (global setting): drawn animations stay in the base pose.
static CALM: AtomicBool = AtomicBool::new(false);

/// Turns "reduce motion" on or off for the drawn animations.
pub fn set_calm(on: bool) {
    CALM.store(on, Ordering::Relaxed);
}

/// "Reduce motion" is on.
pub fn calm() -> bool {
    CALM.load(Ordering::Relaxed)
}

thread_local! {
    /// Shortest frame (ms) requested by the drawn mascots since the hub last read it.
    static WANT_MS: Cell<u32> = const { Cell::new(0) };
}

pub(crate) fn request_frame_ms(ms: u32) {
    WANT_MS.with(|w| {
        if w.get() == 0 || ms < w.get() {
            w.set(ms);
        }
    });
}

/// The shortest frame some animation on screen asked for since the last call (the hub
/// speeds up its timer only while needed). `None` = nobody asked.
pub fn take_requested_frame_ms() -> Option<u32> {
    let ms = WANT_MS.with(|w| w.replace(0));
    (ms > 0).then_some(ms)
}

/// "Virtual" 125 ms frame: the drawing animates at the same pace with a fast or slow timer.
const FRAME: Duration = Duration::from_millis(125);
/// How long a blink lasts.
const BLINK: Duration = Duration::from_millis(160);
/// Clicks on the mascot for the easter egg, within this window.
const POKES: usize = 3;
const POKE_WINDOW: Duration = Duration::from_millis(1200);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reaction {
    /// Something went right (a container came back): jumps and shines.
    Celebrate,
    /// Easter egg: someone poked the mascot.
    Surprised,
}

impl Reaction {
    pub fn as_str(self) -> &'static str {
        match self {
            Reaction::Celebrate => "celebrate",
            Reaction::Surprised => "surprised",
        }
    }

    fn duration(self) -> Duration {
        match self {
            Reaction::Celebrate => Duration::from_millis(2500),
            Reaction::Surprised => Duration::from_millis(1400),
        }
    }

    /// Minimum time between two plays of the same reaction.
    fn cooldown(self) -> Duration {
        match self {
            Reaction::Celebrate => Duration::from_secs(3),
            Reaction::Surprised => Duration::from_secs(8),
        }
    }

    /// A higher-priority reaction is not interrupted by a lower one.
    fn priority(self) -> u8 {
        match self {
            Reaction::Celebrate => 2,
            Reaction::Surprised => 1,
        }
    }

    fn index(self) -> usize {
        self as usize
    }
}

/// What to draw now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Look {
    pub mood: Mood,
    pub reaction: Option<Reaction>,
    /// Eyes closed at this instant (blink).
    pub blink: bool,
    /// Virtual frame (LED, alarm eyes, glitch).
    pub frame: u32,
    /// Frame of a drawn animation (`clips.rs`); `None` = the base drawing.
    pub pose: Option<Grid>,
    /// This mascot's time in ms: picks the frame of imported mascots, which have their own
    /// pace (`pack.rs`).
    pub ms: u64,
}

impl Look {
    /// Without an `Animator`: blink and frame come from the global counter (tray, list icons).
    /// The blink falls on the last frame of each 25-frame cycle, so frame 0 (what a still
    /// portrait draws) has its eyes open.
    pub fn from_frame(mood: Mood, frame: u32) -> Self {
        Look { mood, reaction: None, blink: frame % 25 == 24, frame, pose: None, ms: frame as u64 * FRAME.as_millis() as u64 }
    }
}

/// Animation state of one mascot.
pub struct Animator {
    epoch: Instant,
    /// This mascot's offset: identical mascots don't blink together.
    phase: Duration,
    /// Interval between blinks (varies a little from one mascot to another).
    blink_every: Duration,
    active: Option<(Reaction, Instant)>,
    last: [Option<Instant>; 2],
    pokes: Vec<Instant>,
    player: Player,
}

impl Animator {
    /// `seed` tells the mascots apart (the index will do).
    pub fn new(seed: u32) -> Self {
        let s = seed.wrapping_mul(2_654_435_761); // spreads neighboring seeds
        Animator {
            epoch: Instant::now(),
            phase: Duration::from_millis((s % 3000) as u64),
            blink_every: Duration::from_millis(2800 + (s >> 8) as u64 % 1600),
            active: None,
            last: [None; 2],
            pokes: Vec::new(),
            player: Player::default(),
        }
    }

    /// Fires a reaction. Refuses if the same one played recently or if a more important one
    /// is playing. Returns `true` if it started.
    pub fn trigger(&mut self, r: Reaction, now: Instant) -> bool {
        if self.last[r.index()].is_some_and(|t| now.duration_since(t) < r.cooldown()) {
            return false;
        }
        if let Some(cur) = self.current(now)
            && cur.priority() > r.priority() {
                return false;
            }
        self.active = Some((r, now));
        self.last[r.index()] = Some(now);
        true
    }

    /// A click on the mascot. A few clicks in a row fire the easter egg, never during an
    /// alarm (it must not hide a problem). Returns `true` if the reaction started.
    pub fn poke(&mut self, mood: Mood, now: Instant) -> bool {
        self.pokes.retain(|t| now.duration_since(*t) < POKE_WINDOW);
        self.pokes.push(now);
        if self.pokes.len() < POKES || mood == Mood::Alarm {
            return false;
        }
        self.pokes.clear();
        self.trigger(Reaction::Surprised, now)
    }

    /// Reaction playing now, if any.
    pub fn current(&self, now: Instant) -> Option<Reaction> {
        let (r, t0) = self.active?;
        (now.duration_since(t0) < r.duration()).then_some(r)
    }

    /// What to draw at instant `now` for the given mood. An alarm interrupts the reaction.
    pub fn look(&mut self, mood: Mood, now: Instant) -> Look {
        if mood == Mood::Alarm {
            self.active = None;
        }
        if self.active.is_some() && self.current(now).is_none() {
            self.active = None;
        }
        let reaction = self.current(now);
        let t = now.duration_since(self.epoch) + self.phase;
        let frame = (t.as_millis() / FRAME.as_millis()) as u32;
        let in_cycle = t.as_millis() % self.blink_every.as_millis();
        let blink = reaction.is_none() && matches!(mood, Mood::Idle | Mood::Happy) && in_cycle < BLINK.as_millis();
        Look { mood, reaction, blink, frame, pose: None, ms: t.as_millis() as u64 }
    }

    /// Like `look`, with the frame of `sprite`'s drawn animation, if it has one (see
    /// `clips.rs`): the reaction's while it lasts; otherwise the mood's, after its `enter`
    /// transition. With "reduce motion", it stays in the base pose.
    pub fn look_with(&mut self, sprite: &Sprite, mood: Mood, now: Instant) -> Look {
        let mut look = self.look(mood, now);
        // imported: the frame comes from the time (`pack::Art::pick`); just ask for its pace
        if let Some(art) = pack::art(sprite) {
            if !calm() && let Some(ms) = art.frame_ms(&look) {
                request_frame_ms(ms);
            }
            return look;
        }
        if CALM.load(Ordering::Relaxed) {
            return look;
        }
        let Some(clips) = clips::of(sprite.key) else { return look };
        let reaction_clip = match (look.reaction, self.active) {
            (Some(r), Some((_, t0))) => clips.get(r.as_str()).map(|c| (c, now.duration_since(t0))),
            _ => None,
        };
        let pose = match reaction_clip {
            Some((clip, t)) => Some((*clip.frame_at(t.as_millis() as u64), clip)),
            None => self.player.pose(&clips, mood, now),
        };
        if let Some((grid, clip)) = pose {
            if clip.frames.len() > 1 {
                request_frame_ms(clip.shortest_ms());
            }
            look.pose = Some(grid);
        }
        look
    }
}

/// Which drawn animation plays now: the mood's `enter` transition, then its loop (see
/// `clips.rs`).
#[derive(Default)]
struct Player {
    /// Mood last time (to know when it changed).
    mood: Option<Mood>,
    /// Since when the mood's loop has been playing.
    loop_since: Option<Instant>,
    /// Transition playing once, and when it started.
    once: Option<(String, Instant)>,
}

impl Player {
    fn pose<'c>(&mut self, clips: &'c Clips, mood: Mood, now: Instant) -> Option<(Grid, &'c clips::Clip)> {
        if self.mood != Some(mood) {
            // mood changed: its transition plays, then the loop restarts
            self.once = clips.get(mood.as_str()).and_then(|c| c.enter.clone()).map(|e| (e, now));
            self.mood = Some(mood);
            self.loop_since = None;
        }
        if let Some((name, t0)) = &self.once {
            let t = now.duration_since(*t0).as_millis() as u64;
            match clips.get(name) {
                Some(c) if !c.done_at(t) => return Some((*c.frame_at(t), c)),
                // finished (or vanished in a reload): the loop restarts from the first frame
                _ => {
                    self.once = None;
                    self.loop_since = Some(now);
                }
            }
        }
        let clip = clips.get(mood.as_str())?;
        let since = *self.loop_since.get_or_insert(now);
        let t = now.duration_since(since).as_millis() as u64;
        Some((*clip.frame_at(t), clip))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// Test animations on the neko: each with its first row marked, to know which one played.
    fn test_clips() -> Clips {
        let neko = crate::mascot::sprites::get("neko");
        let q = |mark: &str| {
            let mut rows: Vec<&str> = neko.grid.to_vec();
            rows[0] = mark;
            crate::mascot::clips::tests::frame(&rows)
        };
        let (a, b, c) = (q("K..............."), q(".K.............."), q("..K............."));
        let src = format!(
            "[idle]\nfps = 1\nframes = [{a}]\n\
             [sleep]\nfps = 1\nenter = \"yawn\"\nframes = [{b}]\n\
             [yawn]\ndurations = [300]\nframes = [{c}]\n"
        );
        crate::mascot::clips::parse(neko, &src).unwrap()
    }

    fn mark(p: &mut Player, clips: &Clips, mood: Mood, t: Instant) -> &'static str {
        p.pose(clips, mood, t).map(|(g, _)| g[0]).unwrap_or("base")
    }

    #[test]
    fn transitions_on_mood_change() {
        let clips = test_clips();
        let mut p = Player::default();
        let t0 = Instant::now();
        assert_eq!(mark(&mut p, &clips, Mood::Idle, t0), "K...............");
        // idle -> sleep: enters sleep (yawn, 300 ms), then its loop
        assert_eq!(mark(&mut p, &clips, Mood::Sleep, t0 + ms(1000)), "..K.............");
        assert_eq!(mark(&mut p, &clips, Mood::Sleep, t0 + ms(1250)), "..K.............");
        assert_eq!(mark(&mut p, &clips, Mood::Sleep, t0 + ms(1350)), ".K..............");
        // mood without an animation: the base drawing
        assert_eq!(mark(&mut p, &clips, Mood::Busy, t0 + ms(2000)), "base");
    }

    #[test]
    fn reduce_motion_stays_in_base_pose() {
        let neko = crate::mascot::sprites::get("neko");
        let mut a = Animator::new(0);
        set_calm(true);
        let calm_pose = a.look_with(neko, Mood::Sleep, Instant::now()).pose;
        set_calm(false);
        assert_eq!(calm_pose, None);
    }

    #[test]
    fn reaction_expires_by_the_clock() {
        let mut a = Animator::new(0);
        let t0 = Instant::now();
        assert!(a.trigger(Reaction::Celebrate, t0));
        assert_eq!(a.look(Mood::Idle, t0 + ms(100)).reaction, Some(Reaction::Celebrate));
        assert_eq!(a.look(Mood::Idle, t0 + ms(2600)).reaction, None);
    }

    #[test]
    fn alarm_interrupts_the_reaction() {
        let mut a = Animator::new(0);
        let t0 = Instant::now();
        a.trigger(Reaction::Celebrate, t0);
        assert_eq!(a.look(Mood::Alarm, t0 + ms(100)).reaction, None);
        // and it doesn't come back when the alarm is over
        assert_eq!(a.look(Mood::Idle, t0 + ms(200)).reaction, None);
    }

    #[test]
    fn cooldown_and_priority() {
        let mut a = Animator::new(0);
        let t0 = Instant::now();
        assert!(a.trigger(Reaction::Celebrate, t0));
        assert!(!a.trigger(Reaction::Celebrate, t0 + ms(500)));
        // the easter egg doesn't interrupt the celebration
        assert!(!a.trigger(Reaction::Surprised, t0 + ms(500)));
        assert!(a.trigger(Reaction::Celebrate, t0 + ms(3100)));
    }

    #[test]
    fn three_pokes_fire_the_easter_egg_but_not_during_alarm() {
        let t0 = Instant::now();
        let mut a = Animator::new(0);
        assert!(!a.poke(Mood::Idle, t0));
        assert!(!a.poke(Mood::Idle, t0 + ms(200)));
        assert!(a.poke(Mood::Idle, t0 + ms(400)));

        let mut b = Animator::new(1);
        for i in 0..3 {
            assert!(!b.poke(Mood::Alarm, t0 + ms(i * 100)));
        }
        // spaced-out clicks don't count
        let mut c = Animator::new(2);
        for i in 0..3 {
            assert!(!c.poke(Mood::Idle, t0 + ms(i * 1000)));
        }
    }

    #[test]
    fn different_mascots_dont_blink_together() {
        let t0 = Instant::now();
        let mut a = Animator::new(0);
        let mut b = Animator::new(1);
        // samples 10 s every 20 ms: the blinks can't always coincide
        let (mut blinks_a, mut blinks_b, mut together) = (0, 0, 0);
        for i in 0..500 {
            let t = t0 + ms(i * 20);
            let (pa, pb) = (a.look(Mood::Idle, t).blink, b.look(Mood::Idle, t).blink);
            blinks_a += pa as u32;
            blinks_b += pb as u32;
            together += (pa && pb) as u32;
        }
        assert!(blinks_a > 0 && blinks_b > 0);
        assert!(together < blinks_a.min(blinks_b));
    }
}
