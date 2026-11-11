//! Barramento dos provedores de dados.
//!
//! Cada provedor roda numa thread própria e só coleta enquanto algum mini-app ativo o
//! assina (contagem de referências). As leituras chegam à thread da UI por um canal que
//! o hub esvazia num timer — a UI nunca é tocada fora da thread dela.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::providers::{docker::DockerSnapshot, gpu::GpuSnapshot, sys::SysSnapshot};

/// Uma leitura de um provedor.
#[derive(Debug, Clone)]
pub enum Event {
    Sys(SysSnapshot),
    Gpu(GpuSnapshot),
    Docker(DockerSnapshot),
}

#[derive(Default)]
struct Topic {
    subscribers: AtomicUsize,
    /// Pede uma leitura agora (ex.: logo depois de reiniciar um container).
    poke: AtomicBool,
}

pub struct Bus {
    topics: HashMap<&'static str, Arc<Topic>>,
    tx: Sender<Event>,
    rx: Receiver<Event>,
}

impl Default for Bus {
    fn default() -> Self {
        let (tx, rx) = channel();
        Bus { topics: HashMap::new(), tx, rx }
    }
}

impl Bus {
    /// Registra um provedor: `collect` roda a cada `interval` enquanto houver assinantes.
    pub fn spawn<F>(&mut self, name: &'static str, interval: Duration, mut collect: F)
    where
        F: FnMut() -> Event + Send + 'static,
    {
        self.spawn_polled(name, interval, move |_, _| Some(collect()));
    }

    /// Como `spawn`, mas `collect` recebe o `Poker` do próprio provedor e se a leitura foi
    /// pedida agora (`poked`). Pode devolver `None` para não publicar nada nesta rodada.
    pub fn spawn_polled<F>(&mut self, name: &'static str, tick: Duration, mut collect: F)
    where
        F: FnMut(&Poker, bool) -> Option<Event> + Send + 'static,
    {
        let topic = Arc::new(Topic::default());
        self.topics.insert(name, topic.clone());
        let tx = self.tx.clone();
        std::thread::Builder::new()
            .name(format!("provider-{name}"))
            .spawn(move || {
                let me = Poker(Some(topic.clone()));
                loop {
                    if topic.subscribers.load(Ordering::Relaxed) == 0 {
                        std::thread::sleep(Duration::from_millis(250));
                        continue;
                    }
                    let poked = topic.poke.swap(false, Ordering::Relaxed);
                    if let Some(ev) = collect(&me, poked) {
                        if tx.send(ev).is_err() {
                            return; // o hub fechou
                        }
                    }
                    let start = Instant::now();
                    while start.elapsed() < tick
                        && !topic.poke.load(Ordering::Relaxed)
                        && topic.subscribers.load(Ordering::Relaxed) > 0
                    {
                        std::thread::sleep(Duration::from_millis(100));
                    }
                }
            })
            .expect("não consegui criar a thread do provedor");
    }

    pub fn subscribe(&self, name: &str) {
        if let Some(t) = self.topics.get(name) {
            t.subscribers.fetch_add(1, Ordering::Relaxed);
        } else {
            eprintln!("cyberhub: provedor desconhecido \"{name}\"");
        }
    }

    pub fn unsubscribe(&self, name: &str) {
        if let Some(t) = self.topics.get(name) {
            let _ = t.subscribers.try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1));
        }
    }

    /// Alça para pedir leitura de outra thread (ex.: depois de um `docker restart`).
    pub fn poker(&self, name: &str) -> Poker {
        Poker(self.topics.get(name).cloned())
    }

    pub fn poke(&self, name: &str) {
        if let Some(t) = self.topics.get(name) {
            t.poke.store(true, Ordering::Relaxed);
        }
    }

    /// Leituras pendentes (chamado na thread da UI).
    pub fn drain(&self) -> Vec<Event> {
        self.rx.try_iter().collect()
    }
}

/// Pede uma leitura imediata a um provedor; pode ir para outras threads.
#[derive(Clone)]
pub struct Poker(Option<Arc<Topic>>);

impl Poker {
    pub fn poke(&self) {
        if let Some(t) = &self.0 {
            t.poke.store(true, Ordering::Relaxed);
        }
    }

    /// O provedor tem algum assinante agora (quem o assina pode parar de trabalhar).
    pub fn subscribed(&self) -> bool {
        self.0.as_ref().is_some_and(|t| t.subscribers.load(Ordering::Relaxed) > 0)
    }
}
