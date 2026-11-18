//! AI subscription limits, read by querying the services directly.
//!
//! The queries never print credentials or response contents. Each source fails on its own;
//! the last valid reading is kept and marked as stale.
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[path = "ai_usage_direct.rs"]
mod direct;

use crate::tr;
use direct::FailureKind;

/// Interval between readings (s), from the app config ("Refresh every").
pub static POLL_SECS: AtomicU64 = AtomicU64::new(180);
/// Sources enabled in the config (bit `i` = source `i`, in `SOURCES` order). A disabled one is not queried.
pub static ENABLED: AtomicU8 = AtomicU8::new(0b1111);
/// Source names, in card order.
pub const SOURCES: [&str; 4] = ["Codex", "Claude", "Kiro", "OpenCode Go"];
/// Kiro is queried less often (never more often than this).
const KIRO_MIN_POLL: Duration = Duration::from_secs(900);
const RATE_LIMIT_BACKOFF: Duration = Duration::from_secs(300);

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub struct UsageWindow {
    pub label: String,
    pub used: f64,
    pub detail: String,
    pub resets_at: Option<i64>,
    pub reset_date: String,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub struct UsageCard {
    pub name: String,
    pub status: String,
    pub plan: String,
    pub windows: Vec<UsageWindow>,
    pub updated_at: Option<i64>,
    pub stale: bool,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct UsageSnapshot {
    pub cards: Vec<UsageCard>,
}

impl Default for UsageSnapshot {
    fn default() -> Self {
        Self { cards: SOURCES.into_iter()
            .map(|name| UsageCard { name: name.into(), status: tr!("Waiting for the first reading").into(), ..Default::default() }).collect() }
    }
}

#[derive(Default)]
pub struct UsageProvider {
    cards: Vec<UsageCard>,
    last: [Option<Instant>; 4],
    retry_at: [Option<Instant>; 4],
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

impl UsageProvider {
    /// Reads the sources that are due. `None` when nothing was read and the reading wasn't
    /// requested (`force`): the cards are the same as last time.
    pub fn collect(&mut self, force: bool) -> Option<UsageSnapshot> {
        if self.cards.is_empty() { self.cards = UsageSnapshot::default().cards; }
        let poll = Duration::from_secs(POLL_SECS.load(Ordering::Relaxed).max(60));
        let should_read = self.due(ENABLED.load(Ordering::Relaxed), poll, force, Instant::now());
        let (codex, claude, kiro, opencode) = std::thread::scope(|scope| {
            let c = should_read[0].then(|| scope.spawn(direct::codex));
            let a = should_read[1].then(|| scope.spawn(direct::claude));
            let k = should_read[2].then(|| scope.spawn(direct::kiro));
            let o = should_read[3].then(|| scope.spawn(direct::opencode));
            (c.map(|t| t.join().unwrap_or_else(|_| Err(tr!("Reading failed").into()))),
             a.map(|t| t.join().unwrap_or_else(|_| Err(tr!("Reading failed").into()))),
             k.map(|t| t.join().unwrap_or_else(|_| Err(tr!("Reading failed").into()))),
             o.map(|t| t.join().unwrap_or_else(|_| Err(tr!("Reading failed").into()))))
        });
        let mut read = false;
        for (idx, result) in [(0, codex), (1, claude), (2, kiro), (3, opencode)] {
            if let Some(result) = result {
                self.apply(idx, result, Instant::now(), now());
                read = true;
            }
        }
        (read || force).then(|| UsageSnapshot { cards: self.cards.clone() })
    }

    /// Which sources to read at `now`: enabled, not backing off after a rate limit, and
    /// past their interval (or `force`). Kiro never more often than `KIRO_MIN_POLL`.
    fn due(&self, enabled: u8, poll: Duration, force: bool, now: Instant) -> [bool; 4] {
        let due = |i: usize, interval: Duration| enabled & (1 << i) != 0
            && self.retry_at[i].is_none_or(|t| now >= t)
            && (force || self.last[i].is_none_or(|t| now.saturating_duration_since(t) >= interval));
        [due(0, poll), due(1, poll), due(2, poll.max(KIRO_MIN_POLL)), due(3, poll)]
    }

    /// Merges the result of reading source `idx` into its card. A failure keeps the last
    /// limits (marked stale), except a login failure, which clears them.
    fn apply(&mut self, idx: usize, result: Result<UsageCard, direct::Failure>, now: Instant, wall: i64) {
        self.last[idx] = Some(now);
        match result {
            Ok(mut card) => { card.updated_at = Some(wall); self.cards[idx] = card; }
            Err(failure) => {
                if failure.kind == FailureKind::RateLimited { self.retry_at[idx] = Some(now + RATE_LIMIT_BACKOFF); }
                let card = &mut self.cards[idx];
                // A login failure may mean the account changed: don't show the
                // previous session's limits as if they were the new account's.
                if failure.kind == FailureKind::Auth {
                    card.windows.clear();
                    card.updated_at = None;
                }
                card.status = failure.msg;
                card.stale = card.updated_at.is_some();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use direct::Failure;

    const POLL: Duration = Duration::from_secs(180);

    fn provider() -> UsageProvider {
        UsageProvider { cards: UsageSnapshot::default().cards, ..Default::default() }
    }

    fn reading() -> UsageCard {
        UsageCard { name: "Codex".into(), status: "Updated".into(),
            windows: vec![UsageWindow { label: "5 hours".into(), used: 40.0, ..Default::default() }], ..Default::default() }
    }

    fn failure(kind: FailureKind) -> Failure {
        Failure { msg: "failed".into(), kind }
    }

    #[test]
    fn success_sets_the_reading_time() {
        let mut p = provider();
        p.apply(0, Ok(reading()), Instant::now(), 1000);
        assert_eq!(p.cards[0].updated_at, Some(1000));
        assert!(!p.cards[0].stale);
    }

    #[test]
    fn other_failure_keeps_the_limits_as_stale() {
        let mut p = provider();
        let t = Instant::now();
        p.apply(0, Ok(reading()), t, 1000);
        p.apply(0, Err(failure(FailureKind::Other)), t, 1200);
        assert_eq!(p.cards[0].windows.len(), 1);
        assert_eq!(p.cards[0].updated_at, Some(1000));
        assert!(p.cards[0].stale);
        assert_eq!(p.cards[0].status, "failed");
    }

    #[test]
    fn auth_failure_clears_the_limits() {
        let mut p = provider();
        let t = Instant::now();
        p.apply(0, Ok(reading()), t, 1000);
        p.apply(0, Err(failure(FailureKind::Auth)), t, 1200);
        assert!(p.cards[0].windows.is_empty());
        assert_eq!(p.cards[0].updated_at, None);
        assert!(!p.cards[0].stale);
    }

    #[test]
    fn rate_limit_blocks_even_a_forced_read_until_the_backoff_ends() {
        let mut p = provider();
        let t = Instant::now();
        p.apply(1, Err(failure(FailureKind::RateLimited)), t, 1000);
        assert!(!p.due(0b1111, POLL, true, t + Duration::from_secs(10))[1]);
        assert!(p.due(0b1111, POLL, true, t + RATE_LIMIT_BACKOFF)[1]);
    }

    #[test]
    fn interval_and_disabled_sources() {
        let mut p = provider();
        let t = Instant::now();
        // never read: every enabled source is due
        assert_eq!(p.due(0b1011, POLL, false, t), [true, true, false, true]);
        for i in 0..4 { p.apply(i, Ok(reading()), t, 1000); }
        assert_eq!(p.due(0b1111, POLL, false, t + Duration::from_secs(60)), [false; 4]);
        // Kiro waits for its own, longer interval
        assert_eq!(p.due(0b1111, POLL, false, t + POLL), [true, true, false, true]);
        assert_eq!(p.due(0b1111, POLL, false, t + KIRO_MIN_POLL), [true; 4]);
        // forced: due regardless of the interval, but never a disabled one
        assert_eq!(p.due(0b0101, POLL, true, t), [true, false, true, false]);
    }
}
