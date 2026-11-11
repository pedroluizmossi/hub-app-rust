//! Despertador da thread da UI.
//!
//! Provedores e ações em segundo plano deixam o resultado num canal e tocam o despertador;
//! o hub acorda, esvazia os canais e atualiza a tela e a bandeja. Sem nada chegando, a
//! thread da UI não faz nada (nenhum timer de varredura).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

type Ring = Box<dyn Fn() + Send + Sync>;

#[derive(Clone, Default)]
pub struct Waker(Arc<Inner>);

#[derive(Default)]
struct Inner {
    /// Já tocou e o hub ainda não atendeu: os toques seguintes não agendam outra vez.
    pending: AtomicBool,
    ring: OnceLock<Ring>,
}

impl Waker {
    /// O que tocar faz (o hub agenda a atualização na thread da UI). Só vale a primeira vez.
    pub fn install(&self, ring: impl Fn() + Send + Sync + 'static) {
        let _ = self.0.ring.set(Box::new(ring));
    }

    /// Avisa que chegou algo. Vários toques antes do hub atender viram um só.
    pub fn wake(&self) {
        if self.0.pending.swap(true, Ordering::AcqRel) {
            return;
        }
        // sem ninguém instalado (montagem, testes), o primeiro `take` atende
        if let Some(ring) = self.0.ring.get() {
            ring();
        }
    }

    /// O hub vai atender agora: os próximos toques agendam de novo. Chamar antes de
    /// esvaziar os canais, para nada que chegue no meio ficar sem aviso.
    pub fn take(&self) -> bool {
        self.0.pending.swap(false, Ordering::AcqRel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn varios_toques_viram_um_ate_atender() {
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
