//! Barramento dos provedores de dados.
//!
//! Cada provedor roda numa thread própria e só coleta enquanto algum mini-app ativo o
//! assina (contagem de referências). Sem assinante, a thread dorme numa `Condvar` até
//! alguém assinar; com assinante, dorme até o próximo intervalo ou até um `poke`.
//! As leituras chegam à thread da UI por um canal; cada leitura toca o `Waker`, e o hub
//! acorda para esvaziar o canal — a UI nunca é tocada fora da thread dela.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use super::wake::Waker;

use crate::providers::{agents::AgentsSnapshot, ai_usage::UsageSnapshot, docker::DockerSnapshot, gpu::GpuSnapshot, media::MediaSnapshot, ports::PortsSnapshot, procs::ProcsSnapshot, sys::SysSnapshot};

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
    Ports,
    Agents,
    /// Processos mais pesados: só para plugins (os apps embutidos não usam).
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

/// Uma leitura de um provedor.
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
    /// De qual provedor veio.
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

/// Estado de um provedor no barramento, compartilhado com a thread dele.
struct Slot {
    subscribers: AtomicUsize,
    /// Pede uma leitura agora (ex.: logo depois de reiniciar um container). Fica sob o
    /// mutex da `Condvar`, para o aviso não se perder entre o teste e o sono da thread.
    poke: Mutex<bool>,
    /// Acorda a thread: assinou, deixou de assinar ou pediu leitura.
    changed: Condvar,
    /// A janela do hub está aberta (o mesmo para todos os provedores).
    visible: Arc<AtomicBool>,
}

impl Slot {
    fn lock(&self) -> MutexGuard<'_, bool> {
        self.poke.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn subscribed(&self) -> bool {
        self.subscribers.load(Ordering::Relaxed) > 0
    }

    /// Acorda a thread depois de mudar algo (com o mutex, para não perder o aviso).
    fn notify(&self) {
        let _guard = self.lock();
        self.changed.notify_all();
    }

    /// Dorme até haver assinante.
    fn wait_subscriber(&self) {
        let mut guard = self.lock();
        while !self.subscribed() {
            guard = self.changed.wait(guard).unwrap_or_else(|e| e.into_inner());
        }
    }

    /// Dorme até `tick` passar, chegar um `poke` ou não haver mais assinante.
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
                            return; // o hub fechou
                        }
                        waker.wake();
                    }
                    slot.wait_next(tick);
                }
            })
            .expect("não consegui criar a thread do provedor");
    }

    /// O despertador que as leituras tocam (o hub instala o que ele faz).
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
            *t.lock() = true;
            t.changed.notify_all();
        }
    }

    /// O provedor tem algum assinante agora (quem o assina pode parar de trabalhar).
    pub fn subscribed(&self) -> bool {
        self.0.as_ref().is_some_and(|t| t.subscribed())
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
        let all = [Topic::Sys, Topic::Gpu, Topic::Docker, Topic::Media, Topic::AiUsage, Topic::Ports, Topic::Agents];
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
