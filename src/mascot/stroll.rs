//! Subagents walking around as the imported mascots: each one walks a bit, stops and plays
//! one of the pack's animations (attack, push, throw, idle…), walks again, sometimes turns
//! around; it runs while calling tools and jumps when it finishes.
//!
//! Numbers only (which animation, which frame, which way, where on the strip): the app does
//! the drawing, with the frames of `pack::Troupe`. Time is in ms of any clock that only moves
//! forward; the position `x` goes from 0 (left end) to 1 (right end).

/// A mascot's animations for walking on the strip (indices into `frames`/`ms`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Script {
    pub names: Vec<String>,
    /// Frames of each animation.
    pub frames: Vec<usize>,
    /// How long each frame stays on screen.
    pub ms: Vec<u32>,
    pub walk: usize,
    /// Running: while calling tools.
    pub run: Option<usize>,
    /// Jumping: when finished.
    pub jump: Option<usize>,
    pub idle: Option<usize>,
    /// Other ways of walking besides `walk` (e.g. "walk+attack").
    pub strolls: Vec<usize>,
    /// Stops in place: attacks, pushing, throwing, idle…
    pub extras: Vec<usize>,
}

impl Script {
    /// One full lap of animation `c`, in ms.
    fn lap(&self, c: usize) -> u64 {
        (self.frames[c] as u64 * self.ms[c].max(1) as u64).max(1)
    }
}

/// What to draw of a subagent now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pose {
    /// Mascot (index in the list the app passed).
    pub hero: usize,
    pub clip: usize,
    pub frame: usize,
    /// Facing right.
    pub right: bool,
    pub x: f32,
}

/// Strip widths per second, walking and running.
const WALK_SPEED: f32 = 0.07;
const RUN_SPEED: f32 = 0.16;
/// Shortest stop (the animation repeats until it has passed).
const MIN_STAY_MS: u64 = 900;
/// Longest time step at once (a hidden window doesn't make anyone teleport).
const MAX_STEP_MS: u64 = 400;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Act {
    Move(usize),
    Stay(usize),
    /// Finished: jumps once and leaves (`None` = no jump in the pack, leaves at once).
    Leave(Option<usize>),
}

#[derive(Debug, Clone)]
struct Walker {
    id: String,
    hero: usize,
    x: f32,
    right: bool,
    act: Act,
    start: u64,
    end: u64,
    active: bool,
    /// Own pace (0.8 to 1.2× the speed): two running together don't walk glued together.
    pace: f32,
    /// Last stop, so the same one isn't repeated right after.
    last: Option<usize>,
    rng: u64,
}

impl Walker {
    /// A number from 0 to `n - 1` (xorshift: each subagent's sequence comes from its id).
    fn roll(&mut self, n: u64) -> u64 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        self.rng % n.max(1)
    }

    /// How it moves now: running if it is calling tools; otherwise usually walking,
    /// sometimes another way of walking from the pack.
    fn mover(&mut self, s: &Script) -> usize {
        if self.active && let Some(run) = s.run {
            return run;
        }
        if s.strolls.is_empty() || self.roll(4) > 0 {
            return s.walk;
        }
        s.strolls[self.roll(s.strolls.len() as u64) as usize]
    }

    /// A stop: "idle" a third of the time; otherwise another of the extras, not repeating
    /// the last one.
    fn extra(&mut self, s: &Script) -> usize {
        if let Some(idle) = s.idle.filter(|_| self.roll(3) == 0) {
            return idle;
        }
        let pool: Vec<usize> = s.extras.iter().copied().filter(|c| Some(*c) != self.last && Some(*c) != s.idle).collect();
        if pool.is_empty() {
            return s.extras[self.roll(s.extras.len() as u64) as usize];
        }
        pool[self.roll(pool.len() as u64) as usize]
    }

    /// What to do when what it was doing is over.
    fn next(&mut self, s: &Script, now: u64) {
        let stays = if self.active { 25 } else { 55 };
        let stop = matches!(self.act, Act::Move(_)) && !s.extras.is_empty() && self.roll(100) < stays;
        if stop {
            let c = self.extra(s);
            self.last = Some(c);
            let lap = s.lap(c);
            let laps = (1 + self.roll(3)).max(MIN_STAY_MS.div_ceil(lap));
            self.act = Act::Stay(c);
            self.end = now + lap * laps;
        } else {
            let turn = if matches!(self.act, Act::Stay(_)) { 40 } else { 30 };
            if self.roll(100) < turn {
                self.right = !self.right;
            }
            self.act = Act::Move(self.mover(s));
            self.end = now + 1500 + self.roll(2500);
        }
        self.start = now;
    }
}

/// The subagents walking on one strip (a session's).
#[derive(Debug, Clone, Default)]
pub struct Stage {
    walkers: Vec<Walker>,
    at: Option<u64>,
}

impl Stage {
    pub fn is_empty(&self) -> bool {
        self.walkers.is_empty()
    }

    /// Who is walking now: (id, mascot, calling tools). Newcomers start at a random spot on
    /// the strip; those who left jump once and vanish.
    pub fn cast(&mut self, now: u64, here: &[(String, usize, bool)], scripts: &[&Script]) {
        for w in &mut self.walkers {
            if matches!(w.act, Act::Leave(_)) || here.iter().any(|(id, ..)| *id == w.id) {
                continue;
            }
            let jump = scripts.get(w.hero).and_then(|s| s.jump);
            w.act = Act::Leave(jump);
            w.start = now;
            w.end = now + jump.map_or(0, |j| scripts[w.hero].lap(j));
        }
        self.walkers.retain(|w| !matches!(w.act, Act::Leave(_)) || w.end > now);
        for (id, hero, active) in here {
            let Some(s) = scripts.get(*hero) else { continue };
            match self.walkers.iter_mut().find(|w| w.id == *id && !matches!(w.act, Act::Leave(_))) {
                Some(w) => {
                    if w.active != *active {
                        w.active = *active;
                        // started or stopped running: switch the way of walking right away
                        if let Act::Move(c) = w.act
                            && (Some(c) == s.run || *active)
                        {
                            w.act = Act::Move(w.mover(s));
                        }
                    }
                }
                None => {
                    let seed = id.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3));
                    let mut w = Walker {
                        id: id.clone(),
                        hero: *hero,
                        x: 0.0,
                        right: true,
                        act: Act::Move(s.walk),
                        start: now,
                        end: now,
                        active: *active,
                        pace: 1.0,
                        last: None,
                        rng: seed | 1,
                    };
                    w.pace = (80 + w.roll(41)) as f32 / 100.0;
                    w.x = (10 + w.roll(81)) as f32 / 100.0;
                    w.right = w.roll(2) == 0;
                    w.act = Act::Move(w.mover(s));
                    w.end = now + 1500 + w.roll(2500);
                    self.walkers.push(w);
                }
            }
        }
    }

    /// Advances time to `now`: walkers move (and turn at the ends), whoever finished what
    /// it was doing goes to the next thing, whoever left and already jumped vanishes.
    pub fn step(&mut self, now: u64, scripts: &[&Script]) {
        let dt = self.at.map_or(0, |at| now.saturating_sub(at).min(MAX_STEP_MS)) as f32 / 1000.0;
        self.at = Some(now);
        for w in &mut self.walkers {
            let Some(s) = scripts.get(w.hero) else { continue };
            if let Act::Move(c) = w.act {
                let speed = if Some(c) == s.run { RUN_SPEED } else { WALK_SPEED } * w.pace;
                w.x += if w.right { speed } else { -speed } * dt;
                if w.x >= 1.0 {
                    (w.x, w.right) = (1.0, false);
                } else if w.x <= 0.0 {
                    (w.x, w.right) = (0.0, true);
                }
            }
            if now >= w.end && !matches!(w.act, Act::Leave(_)) {
                w.next(s, now);
            }
        }
        self.walkers.retain(|w| !matches!(w.act, Act::Leave(_)) || w.end > now);
    }

    /// What to draw now, in order of arrival. With "reduce motion" (`calm`), everyone stands
    /// still on the first frame, spread along the strip.
    pub fn poses(&self, now: u64, scripts: &[&Script], calm: bool) -> Vec<Pose> {
        let n = self.walkers.len();
        self.walkers
            .iter()
            .enumerate()
            .filter_map(|(k, w)| {
                let s = scripts.get(w.hero)?;
                if calm {
                    let clip = s.idle.unwrap_or(s.walk);
                    return Some(Pose { hero: w.hero, clip, frame: 0, right: true, x: (k as f32 + 0.5) / n as f32 });
                }
                let clip = match w.act {
                    Act::Move(c) | Act::Stay(c) | Act::Leave(Some(c)) => c,
                    Act::Leave(None) => s.walk,
                };
                let f = (now.saturating_sub(w.start) / s.ms[clip].max(1) as u64) as usize;
                // the leaving jump doesn't repeat: it stops on the last frame
                let frame = if matches!(w.act, Act::Leave(_)) { f.min(s.frames[clip] - 1) } else { f % s.frames[clip] };
                Some(Pose { hero: w.hero, clip, frame, right: w.right, x: w.x })
            })
            .collect()
    }

    /// The shortest frame among the animations on screen (the hub speeds its timer up to it).
    pub fn frame_ms(&self, scripts: &[&Script]) -> Option<u32> {
        self.walkers
            .iter()
            .filter_map(|w| {
                let s = scripts.get(w.hero)?;
                let c = match w.act {
                    Act::Move(c) | Act::Stay(c) | Act::Leave(Some(c)) => c,
                    Act::Leave(None) => return None,
                };
                Some(s.ms[c])
            })
            .min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// walk 0, run 1, jump 2, idle 3, attack 4, push 5, walk+attack 6.
    fn script() -> Script {
        let names = ["walk", "run", "jump", "idle", "attack", "push", "walk+attack"];
        Script {
            names: names.iter().map(|n| n.to_string()).collect(),
            frames: vec![6, 6, 8, 4, 4, 6, 6],
            ms: vec![125; 7],
            walk: 0,
            run: Some(1),
            jump: Some(2),
            idle: Some(3),
            strolls: vec![6],
            extras: vec![3, 4, 5],
        }
    }

    fn here(list: &[(&str, bool)]) -> Vec<(String, usize, bool)> {
        list.iter().map(|(id, active)| (id.to_string(), 0, *active)).collect()
    }

    #[test]
    fn walks_stops_and_varies_the_animations() {
        let s = script();
        let mut stage = Stage::default();
        stage.cast(0, &here(&[("a", false)]), &[&s]);
        let mut seen = std::collections::BTreeSet::new();
        let mut xs = Vec::new();
        for t in (0..120_000).step_by(125) {
            stage.step(t, &[&s]);
            let p = stage.poses(t, &[&s], false)[0];
            assert!((0.0..=1.0).contains(&p.x) && p.frame < s.frames[p.clip]);
            seen.insert(p.clip);
            xs.push(p.x);
        }
        // in two minutes: walked, stopped at every extra and at "idle", never ran nor jumped
        for c in [0, 3, 4, 5, 6] {
            assert!(seen.contains(&c), "{} never showed up: {seen:?}", s.names[c]);
        }
        assert!(!seen.contains(&1) && !seen.contains(&2), "{seen:?}");
        // and it moved around
        let (lo, hi) = xs.iter().fold((1f32, 0f32), |(lo, hi), x| (lo.min(*x), hi.max(*x)));
        assert!(hi - lo > 0.3, "{lo}..{hi}");
    }

    #[test]
    fn runs_while_calling_tools_and_jumps_when_done() {
        let s = script();
        let mut stage = Stage::default();
        stage.cast(0, &here(&[("a", true)]), &[&s]);
        stage.step(0, &[&s]);
        assert_eq!(stage.poses(0, &[&s], false)[0].clip, 1, "calling a tool: runs");
        // stopped calling: back to walking at once
        stage.cast(500, &here(&[("a", false)]), &[&s]);
        assert_ne!(stage.poses(500, &[&s], false)[0].clip, 1);
        // finished: jumps one lap (8 frames of 125 ms), stopping on the last frame, and vanishes
        stage.cast(1000, &here(&[]), &[&s]);
        let p = stage.poses(1000 + 7 * 125 + 50, &[&s], false);
        assert_eq!((p[0].clip, p[0].frame), (2, 7));
        stage.step(1999, &[&s]);
        assert!(!stage.is_empty());
        stage.step(2000, &[&s]);
        assert!(stage.is_empty());
    }

    #[test]
    fn each_at_its_own_pace_and_still_when_calm() {
        let s = script();
        let mut stage = Stage::default();
        stage.cast(0, &here(&[("a", false), ("b", false), ("c", false)]), &[&s]);
        let p = stage.poses(0, &[&s], false);
        assert_eq!(p.len(), 3);
        assert!(p[0].x != p[1].x || p[1].x != p[2].x, "{p:?}");
        let calm = stage.poses(0, &[&s], true);
        assert!(calm.iter().all(|p| p.frame == 0 && p.clip == 3));
        assert_eq!(calm.iter().map(|p| p.x).collect::<Vec<_>>(), [0.5 / 3.0, 1.5 / 3.0, 2.5 / 3.0]);
        // same id, same sequence: the screen doesn't change its mind when redrawing
        let mut again = Stage::default();
        again.cast(0, &here(&[("a", false), ("b", false), ("c", false)]), &[&s]);
        assert_eq!(again.poses(0, &[&s], false), p);
    }
}
