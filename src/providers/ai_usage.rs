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
    pub fn collect(&mut self, force: bool) -> UsageSnapshot {
        if self.cards.is_empty() { self.cards = UsageSnapshot::default().cards; }
        let poll = Duration::from_secs(POLL_SECS.load(Ordering::Relaxed).max(60));
        let enabled = ENABLED.load(Ordering::Relaxed);
        let elapsed = |i: usize, interval: Duration| enabled & (1 << i) != 0
            && self.retry_at[i].is_none_or(|t| Instant::now() >= t)
            && (force || self.last[i].is_none_or(|t| t.elapsed() >= interval));
        let should_read = [elapsed(0, poll), elapsed(1, poll), elapsed(2, poll.max(KIRO_MIN_POLL)), elapsed(3, poll)];
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
        for (idx, result) in [(0, codex), (1, claude), (2, kiro), (3, opencode)] {
            if let Some(result) = result {
                self.last[idx] = Some(Instant::now());
                match result {
                    Ok(mut card) => { card.updated_at = Some(now()); self.cards[idx] = card; }
                    Err(failure) => {
                        if failure.kind == FailureKind::RateLimited { self.retry_at[idx] = Some(Instant::now() + RATE_LIMIT_BACKOFF); }
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
        UsageSnapshot { cards: self.cards.clone() }
    }
}
