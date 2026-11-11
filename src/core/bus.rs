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

use crate::providers::{ai_usage::UsageSnapshot, docker::DockerSnapshot, gpu::GpuSnapshot, media::MediaSnapshot, sys::SysSnapshot};

/// Um provedor de dados. É o nome usado em `uses` no manifesto (em snake_case) e a chave
/// de tudo no barramento: um nome errado não compila (ou não passa do parse do manifesto).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Topic {
    Sys,
    Gpu,
    Docker,
    Media,
    AiUsage,
}

impl Topic {
    pub fn name(self) -> &'static str {
        match self {
            Topic::Sys => "sys",
            Topic::Gpu => "gpu",
            Topic::Docker => "docker",
            Topic::Media => "media",
            Topic::AiUsage => "ai_usage",
        }
    }
}

/// Uma leitura de um provedor.
#[derive(Debug, Clone)]
pub enum Event {
    Sys(SysSnapshot),
    Gpu(GpuSnapshot),
    Docker(DockerSnapshot),
    Media(MediaSnapshot),
    AiUsage(UsageSnapshot),
}

impl Event {
    /// De qual provedor veio.
    pub fn topic(&self) -> Topic {
        match self {
            Event::Sys(_) => Topic::Sys,
            Event::Gpu(_) => Topic::Gpu,
            Event::Docker(_) => Topic::Docker,
            Event::Media(_) => Topic::Media,
            Event::AiUsage(_) => Topic::AiUsage,
        }
    }
}

/// Estado de um provedor no barramento, compartilhado com a thread dele.
struct Slot {
    subscribers: AtomicUsize,
    /// Pede uma leitura agora (ex.: logo depois de reiniciar um container).
    poke: AtomicBool,
    /// A janela do hub está aberta (o mesmo para todos os provedores).
    visible: Arc<AtomicBool>,
}

pub struct Bus {
    slots: HashMap<Topic, Arc<Slot>>,
    visible: Arc<AtomicBool>,
    tx: Sender<Event>,
    rx: Receiver<Event>,
}

impl Default for Bus {
    fn default() -> Self {
        let (tx, rx) = channel();
        Bus { slots: HashMap::new(), visible: Arc::new(AtomicBool::new(false)), tx, rx }
    }
}

impl Bus {
    /// Registra um provedor: `collect` roda a cada `interval` enquanto houver assinantes.
    pub fn spawn<F>(&mut self, topic: Topic, interval: Duration, mut collect: F)
    where
        F: FnMut() -> Event + Send + 'static,
    {
        self.spawn_polled(topic, interval, move |_, _| Some(collect()));
    }

    /// Como `spawn`, mas `collect` recebe o `Poker` do próprio provedor e se a leitura foi
    /// pedida agora (`poked`). Pode devolver `None` para não publicar nada nesta rodada.
    pub fn spawn_polled<F>(&mut self, topic: Topic, tick: Duration, mut collect: F)
    where
        F: FnMut(&Poker, bool) -> Option<Event> + Send + 'static,
    {
        let slot = Arc::new(Slot {
            subscribers: AtomicUsize::new(0),
            poke: AtomicBool::new(false),
            visible: self.visible.clone(),
        });
        self.slots.insert(topic, slot.clone());
        let tx = self.tx.clone();
        std::thread::Builder::new()
            .name(format!("provider-{}", topic.name()))
            .spawn(move || {
                let me = Poker(Some(slot.clone()));
                loop {
                    if slot.subscribers.load(Ordering::Relaxed) == 0 {
                        std::thread::sleep(Duration::from_millis(250));
                        continue;
                    }
                    let poked = slot.poke.swap(false, Ordering::Relaxed);
                    if let Some(ev) = collect(&me, poked) {
                        if tx.send(ev).is_err() {
                            return; // o hub fechou
                        }
                    }
                    let start = Instant::now();
                    while start.elapsed() < tick
                        && !slot.poke.load(Ordering::Relaxed)
                        && slot.subscribers.load(Ordering::Relaxed) > 0
                    {
                        std::thread::sleep(Duration::from_millis(100));
                    }
                }
            })
            .expect("não consegui criar a thread do provedor");
    }

    pub fn subscribe(&self, topic: Topic) {
        if let Some(t) = self.slots.get(&topic) {
            t.subscribers.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn unsubscribe(&self, topic: Topic) {
        if let Some(t) = self.slots.get(&topic) {
            let _ = t.subscribers.try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1));
        }
    }

    /// Alça para pedir leitura de outra thread (ex.: depois de um `docker restart`).
    pub fn poker(&self, topic: Topic) -> Poker {
        Poker(self.slots.get(&topic).cloned())
    }

    pub fn poke(&self, topic: Topic) {
        self.poker(topic).poke();
    }

    /// A janela abriu ou se escondeu: os provedores que custam caro coletam mais devagar
    /// enquanto ninguém está olhando (ver `Poker::visible`).
    pub fn set_visible(&self, on: bool) {
        self.visible.store(on, Ordering::Relaxed);
    }

    /// Leituras pendentes (chamado na thread da UI).
    pub fn drain(&self) -> Vec<Event> {
        self.rx.try_iter().collect()
    }
}

/// Pede uma leitura imediata a um provedor; pode ir para outras threads.
#[derive(Clone)]
pub struct Poker(Option<Arc<Slot>>);

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

    /// A janela do hub está aberta. Escondida, os dados só alimentam a bandeja.
    pub fn visible(&self) -> bool {
        self.0.as_ref().is_some_and(|t| t.visible.load(Ordering::Relaxed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nomes_dos_topicos_batem_com_o_manifesto() {
        #[derive(serde::Deserialize)]
        struct Uses {
            uses: Vec<Topic>,
        }
        let all = [Topic::Sys, Topic::Gpu, Topic::Docker, Topic::Media, Topic::AiUsage];
        let text = format!("uses = [{}]", all.map(|t| format!("\"{}\"", t.name())).join(", "));
        assert_eq!(toml::from_str::<Uses>(&text).unwrap().uses, all);
        assert!(toml::from_str::<Uses>("uses = [\"dockre\"]").is_err());
    }

    #[test]
    fn so_coleta_com_assinante_e_atende_o_poke() {
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
        assert_eq!(n.load(Ordering::Relaxed), 0, "sem assinante não coleta");
        bus.subscribe(Topic::Sys);
        assert_eq!(wait(1), 1);
        bus.poke(Topic::Sys); // fura a espera de uma hora
        assert_eq!(wait(2), 2);
        assert_eq!(bus.drain().len(), 2);
        assert!(!bus.poker(Topic::Sys).visible());
        bus.set_visible(true);
        assert!(bus.poker(Topic::Sys).visible());
    }
}
