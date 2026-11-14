//! Bus of the data providers.
//!
//! Each provider runs on its own thread and only collects while some active mini-app
//! subscribes to it (reference counting). With no subscriber, the thread sleeps on a
//! `Condvar` until someone subscribes; with one, it sleeps until the next interval or a
//! `poke`. Readings reach the UI thread through a channel; each reading rings the `Waker`,
//! and the hub wakes up to drain the channel — the UI is never touched off its thread.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use super::wake::Waker;

use crate::providers::{agents::AgentsSnapshot, ai_usage::UsageSnapshot, docker::DockerSnapshot, gpu::GpuSnapshot, media::MediaSnapshot, ports::PortsSnapshot, procs::ProcsSnapshot, sys::SysSnapshot};

/// A data provider. It is the name used in `uses` in the manifest (in snake_case) and the
/// key of everything on the bus: a wrong name does not compile (or fails manifest parsing).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Topic {
    Sys,
    Gpu,
    Docker,
    Media,
    AiUsage,
    Ports,
    Agents,
    /// Heaviest processes: for plugins only (the built-in apps don't use it).
    Procs,
}

impl Topic {
    pub fn name(self) -> &'static str {
        match self {
            Topic::Sys => "sys",
            Topic::Gpu => "gpu",
            Topic::Docker => "docker",
            Topic::Media => "media",
            Topic::AiUsage => "ai_usage",
            Topic::Ports => "ports",
            Topic::Agents => "agents",
            Topic::Procs => "procs",
        }
    }
}

/// A reading from a provider.
#[derive(Debug, Clone)]
pub enum Event {
    Sys(SysSnapshot),
    Gpu(GpuSnapshot),
    Docker(DockerSnapshot),
    Media(MediaSnapshot),
    AiUsage(UsageSnapshot),
    Ports(PortsSnapshot),
    Agents(AgentsSnapshot),
    Procs(ProcsSnapshot),
}

impl Event {
    /// Which provider it came from.
    pub fn topic(&self) -> Topic {
        match self {
            Event::Sys(_) => Topic::Sys,
            Event::Gpu(_) => Topic::Gpu,
            Event::Docker(_) => Topic::Docker,
            Event::Media(_) => Topic::Media,
            Event::AiUsage(_) => Topic::AiUsage,
            Event::Ports(_) => Topic::Ports,
            Event::Agents(_) => Topic::Agents,
            Event::Procs(_) => Topic::Procs,
        }
    }
}

/// State of a provider on the bus, shared with its thread.
struct Slot {
    subscribers: AtomicUsize,
    /// Asks for a reading now (e.g. right after restarting a container). Kept under the
    /// `Condvar` mutex, so the signal is not lost between the check and the thread's sleep.
    poke: Mutex<bool>,
    /// Wakes the thread: subscribed, unsubscribed or asked for a reading.
    changed: Condvar,
    /// The hub window is open (the same for every provider).
    visible: Arc<AtomicBool>,
}

impl Slot {
    fn lock(&self) -> MutexGuard<'_, bool> {
        self.poke.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn subscribed(&self) -> bool {
        self.subscribers.load(Ordering::Relaxed) > 0
    }

    /// Wakes the thread after changing something (with the mutex, so the signal is not lost).
    fn notify(&self) {
        let _guard = self.lock();
        self.changed.notify_all();
    }

    /// Sleeps until there is a subscriber.
    fn wait_subscriber(&self) {
        let mut guard = self.lock();
        while !self.subscribed() {
            guard = self.changed.wait(guard).unwrap_or_else(|e| e.into_inner());
        }
    }

    /// Sleeps until `tick` passes, a `poke` arrives or there are no subscribers left.
    fn wait_next(&self, tick: Duration) {
        let end = Instant::now() + tick;
        let mut guard = self.lock();
        while !*guard && self.subscribed() {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            guard = self.changed.wait_timeout(guard, left).unwrap_or_else(|e| e.into_inner()).0;
        }
    }

    fn take_poke(&self) -> bool {
        std::mem::take(&mut *self.lock())
    }
}

pub struct Bus {
    slots: HashMap<Topic, Arc<Slot>>,
    visible: Arc<AtomicBool>,
    tx: Sender<Event>,
    rx: Receiver<Event>,
    waker: Waker,
}

impl Default for Bus {
    fn default() -> Self {
        let (tx, rx) = channel();
        Bus { slots: HashMap::new(), visible: Arc::new(AtomicBool::new(false)), tx, rx, waker: Waker::default() }
    }
}

impl Bus {
    /// Registers a provider: `collect` runs every `interval` while there are subscribers.
    pub fn spawn<F>(&mut self, topic: Topic, interval: Duration, mut collect: F)
    where
        F: FnMut() -> Event + Send + 'static,
    {
        self.spawn_polled(topic, interval, move |_, _| Some(collect()));
    }

    /// Like `spawn`, but `collect` gets the provider's own `Poker` and whether the reading
    /// was asked for just now (`poked`). It may return `None` to publish nothing this round.
    pub fn spawn_polled<F>(&mut self, topic: Topic, tick: Duration, mut collect: F)
    where
        F: FnMut(&Poker, bool) -> Option<Event> + Send + 'static,
    {
        let slot = Arc::new(Slot {
            subscribers: AtomicUsize::new(0),
            poke: Mutex::new(false),
            changed: Condvar::new(),
            visible: self.visible.clone(),
        });
        self.slots.insert(topic, slot.clone());
        let tx = self.tx.clone();
        let waker = self.waker.clone();
        std::thread::Builder::new()
            .name(format!("provider-{}", topic.name()))
            .spawn(move || {
                let me = Poker(Some(slot.clone()));
                loop {
                    slot.wait_subscriber();
                    let poked = slot.take_poke();
                    if let Some(ev) = collect(&me, poked) {
                        if tx.send(ev).is_err() {
                            return; // the hub closed
                        }
                        waker.wake();
                    }
                    slot.wait_next(tick);
                }
            })
            .expect("could not create the provider thread");
    }

    /// The waker the readings ring (the hub installs what it does).
    pub fn waker(&self) -> &Waker {
        &self.waker
    }

    pub fn subscribe(&self, topic: Topic) {
        if let Some(t) = self.slots.get(&topic) {
            t.subscribers.fetch_add(1, Ordering::Relaxed);
            t.notify();
        }
    }

    pub fn unsubscribe(&self, topic: Topic) {
        if let Some(t) = self.slots.get(&topic) {
            let _ = t.subscribers.try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1));
            t.notify();
        }
    }

    /// Handle to ask for a reading from another thread (e.g. after a `docker restart`).
    pub fn poker(&self, topic: Topic) -> Poker {
        Poker(self.slots.get(&topic).cloned())
    }

    pub fn poke(&self, topic: Topic) {
        self.poker(topic).poke();
    }

    /// The window opened or was hidden: expensive providers collect more slowly while
    /// nobody is looking (see `Poker::visible`).
    pub fn set_visible(&self, on: bool) {
        self.visible.store(on, Ordering::Relaxed);
    }

    /// Pending readings (called on the UI thread).
    pub fn drain(&self) -> Vec<Event> {
        self.rx.try_iter().collect()
    }
}

/// Asks a provider for an immediate reading; can be sent to other threads.
#[derive(Clone)]
pub struct Poker(Option<Arc<Slot>>);

impl Poker {
    pub fn poke(&self) {
        if let Some(t) = &self.0 {
            *t.lock() = true;
            t.changed.notify_all();
        }
    }

    /// The provider has a subscriber right now (without one, the provider can stop working).
    pub fn subscribed(&self) -> bool {
        self.0.as_ref().is_some_and(|t| t.subscribed())
    }

    /// The hub window is open. While hidden, the data only feeds the tray.
    pub fn visible(&self) -> bool {
        self.0.as_ref().is_some_and(|t| t.visible.load(Ordering::Relaxed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_names_match_the_manifest() {
        #[derive(serde::Deserialize)]
        struct Uses {
            uses: Vec<Topic>,
        }
        let all = [Topic::Sys, Topic::Gpu, Topic::Docker, Topic::Media, Topic::AiUsage, Topic::Ports, Topic::Agents];
        let text = format!("uses = [{}]", all.map(|t| format!("\"{}\"", t.name())).join(", "));
        assert_eq!(toml::from_str::<Uses>(&text).unwrap().uses, all);
        assert!(toml::from_str::<Uses>("uses = [\"dockre\"]").is_err());
    }

    #[test]
    fn collects_only_with_a_subscriber_and_answers_the_poke() {
        let mut bus = Bus::default();
        let n = Arc::new(AtomicUsize::new(0));
        let count = n.clone();
        bus.spawn(Topic::Sys, Duration::from_secs(3600), move || {
            count.fetch_add(1, Ordering::Relaxed);
            Event::Gpu(GpuSnapshot::default())
        });
        let wait = |want: usize| {
            let end = Instant::now() + Duration::from_secs(5);
            while n.load(Ordering::Relaxed) < want && Instant::now() < end {
                std::thread::sleep(Duration::from_millis(20));
            }
            n.load(Ordering::Relaxed)
        };
        std::thread::sleep(Duration::from_millis(350));
        assert_eq!(n.load(Ordering::Relaxed), 0, "no collection without a subscriber");
        bus.subscribe(Topic::Sys);
        assert_eq!(wait(1), 1);
        bus.poke(Topic::Sys); // cuts the one-hour wait short
        assert_eq!(wait(2), 2);
        assert_eq!(bus.drain().len(), 2);
        assert!(!bus.poker(Topic::Sys).visible());
        bus.set_visible(true);
        assert!(bus.poker(Topic::Sys).visible());
    }
}
