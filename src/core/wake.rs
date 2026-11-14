//! Wake-up call for the UI thread.
//!
//! Providers and background actions leave their result in a channel and ring the waker;
//! the hub wakes up, drains the channels and updates the screen and the tray. With nothing
//! arriving, the UI thread does nothing (no polling timer).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

type Ring = Box<dyn Fn() + Send + Sync>;

#[derive(Clone, Default)]
pub struct Waker(Arc<Inner>);

#[derive(Default)]
struct Inner {
    /// Already rang and the hub has not answered yet: further rings don't schedule again.
    pending: AtomicBool,
    ring: OnceLock<Ring>,
}

impl Waker {
    /// What ringing does (the hub schedules the update on the UI thread). Only the first
    /// call counts.
    pub fn install(&self, ring: impl Fn() + Send + Sync + 'static) {
        let _ = self.0.ring.set(Box::new(ring));
    }

    /// Signals that something arrived. Several rings before the hub answers become one.
    pub fn wake(&self) {
        if self.0.pending.swap(true, Ordering::AcqRel) {
            return;
        }
        // with nothing installed (during setup, tests), the first `take` answers
        if let Some(ring) = self.0.ring.get() {
            ring();
        }
    }

    /// The hub is answering now: the next rings schedule again. Call before draining the
    /// channels, so nothing that arrives in between goes unnoticed.
    pub fn take(&self) -> bool {
        self.0.pending.swap(false, Ordering::AcqRel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn several_rings_become_one_until_answered() {
        let w = Waker::default();
        let n = Arc::new(AtomicUsize::new(0));
        let c = n.clone();
        w.install(move || {
            c.fetch_add(1, Ordering::Relaxed);
        });
        w.wake();
        w.wake();
        assert_eq!(n.load(Ordering::Relaxed), 1);
        assert!(w.take());
        assert!(!w.take());
        w.wake();
        assert_eq!(n.load(Ordering::Relaxed), 2);
    }
}
