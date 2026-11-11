//! Reações curtas das mascotes, por cima do humor.
//!
//! O `Mood` é o estado que dura (dormindo, normal, feliz, ocupado, alerta). Uma reação é um
//! pico curto disparado por um evento ("um container voltou") ou pelo usuário (easter egg);
//! quando acaba, a mascote volta ao humor. Cada mascote tem o seu `Animator`.
//!
//! Tudo é medido pelo relógio, não por quadros: o timer da janela muda de ritmo com o mouse
//! (ver `FRAME_HOT` em `hub.rs`), e uma reação contada em quadros duraria o dobro. Pelo mesmo
//! motivo, uma reação disparada com a mascote fora da tela expira sozinha.

use std::time::{Duration, Instant};

use super::Mood;

/// Quadro "virtual" de 125 ms: o desenho anima no mesmo ritmo com o timer rápido ou lento.
const FRAME: Duration = Duration::from_millis(125);
/// Quanto dura uma piscada.
const BLINK: Duration = Duration::from_millis(160);
/// Cliques na mascote para o easter egg, dentro desta janela.
const POKES: usize = 3;
const POKE_WINDOW: Duration = Duration::from_millis(1200);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reaction {
    /// Algo deu certo (um container voltou): pula e brilha.
    Celebrate,
    /// Easter egg: cutucaram a mascote.
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

    /// Tempo mínimo entre duas vezes da mesma reação.
    fn cooldown(self) -> Duration {
        match self {
            Reaction::Celebrate => Duration::from_secs(3),
            Reaction::Surprised => Duration::from_secs(8),
        }
    }

    /// Uma reação de prioridade maior não é interrompida por uma menor.
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

/// O que desenhar agora.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Look {
    pub mood: Mood,
    pub reaction: Option<Reaction>,
    /// Olhos fechados neste instante (piscada).
    pub blink: bool,
    /// Quadro virtual (LED, olhos do alerta, glitch).
    pub frame: u32,
}

impl Look {
    /// Sem `Animator`: piscada e quadro vêm do contador global (bandeja, ícones das listas).
    pub fn from_frame(mood: Mood, frame: u32) -> Self {
        Look { mood, reaction: None, blink: frame % 25 == 0, frame }
    }
}

/// Estado de animação de uma mascote.
pub struct Animator {
    epoch: Instant,
    /// Defasagem desta mascote: mascotes iguais não piscam juntas.
    phase: Duration,
    /// Intervalo entre piscadas (varia um pouco de uma mascote para outra).
    blink_every: Duration,
    active: Option<(Reaction, Instant)>,
    last: [Option<Instant>; 2],
    pokes: Vec<Instant>,
}

impl Animator {
    /// `seed` distingue as mascotes (o índice serve).
    pub fn new(seed: u32) -> Self {
        let s = seed.wrapping_mul(2_654_435_761); // espalha seeds vizinhas
        Animator {
            epoch: Instant::now(),
            phase: Duration::from_millis((s % 3000) as u64),
            blink_every: Duration::from_millis(2800 + (s >> 8) as u64 % 1600),
            active: None,
            last: [None; 2],
            pokes: Vec::new(),
        }
    }

    /// Dispara uma reação. Recusa se a mesma tocou há pouco ou se uma mais importante está
    /// tocando. Devolve `true` se começou.
    pub fn trigger(&mut self, r: Reaction, now: Instant) -> bool {
        if self.last[r.index()].is_some_and(|t| now.duration_since(t) < r.cooldown()) {
            return false;
        }
        if let Some(cur) = self.current(now) {
            if cur.priority() > r.priority() {
                return false;
            }
        }
        self.active = Some((r, now));
        self.last[r.index()] = Some(now);
        true
    }

    /// Clique na mascote. Alguns cliques seguidos disparam o easter egg, nunca durante um
    /// alerta (ele não pode esconder um problema). Devolve `true` se a reação começou.
    pub fn poke(&mut self, mood: Mood, now: Instant) -> bool {
        self.pokes.retain(|t| now.duration_since(*t) < POKE_WINDOW);
        self.pokes.push(now);
        if self.pokes.len() < POKES || mood == Mood::Alarm {
            return false;
        }
        self.pokes.clear();
        self.trigger(Reaction::Surprised, now)
    }

    /// Reação tocando agora, se houver.
    pub fn current(&self, now: Instant) -> Option<Reaction> {
        let (r, t0) = self.active?;
        (now.duration_since(t0) < r.duration()).then_some(r)
    }

    /// O que desenhar no instante `now` para o humor dado. Um alerta interrompe a reação.
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
        Look { mood, reaction, blink, frame }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn reacao_expira_pelo_relogio() {
        let mut a = Animator::new(0);
        let t0 = Instant::now();
        assert!(a.trigger(Reaction::Celebrate, t0));
        assert_eq!(a.look(Mood::Idle, t0 + ms(100)).reaction, Some(Reaction::Celebrate));
        assert_eq!(a.look(Mood::Idle, t0 + ms(2600)).reaction, None);
    }

    #[test]
    fn alerta_interrompe_a_reacao() {
        let mut a = Animator::new(0);
        let t0 = Instant::now();
        a.trigger(Reaction::Celebrate, t0);
        assert_eq!(a.look(Mood::Alarm, t0 + ms(100)).reaction, None);
        // e ela não volta quando o alerta passa
        assert_eq!(a.look(Mood::Idle, t0 + ms(200)).reaction, None);
    }

    #[test]
    fn intervalo_minimo_e_prioridade() {
        let mut a = Animator::new(0);
        let t0 = Instant::now();
        assert!(a.trigger(Reaction::Celebrate, t0));
        assert!(!a.trigger(Reaction::Celebrate, t0 + ms(500)));
        // o easter egg não interrompe a comemoração
        assert!(!a.trigger(Reaction::Surprised, t0 + ms(500)));
        assert!(a.trigger(Reaction::Celebrate, t0 + ms(3100)));
    }

    #[test]
    fn tres_cutucadas_disparam_o_easter_egg_mas_nao_no_alerta() {
        let t0 = Instant::now();
        let mut a = Animator::new(0);
        assert!(!a.poke(Mood::Idle, t0));
        assert!(!a.poke(Mood::Idle, t0 + ms(200)));
        assert!(a.poke(Mood::Idle, t0 + ms(400)));

        let mut b = Animator::new(1);
        for i in 0..3 {
            assert!(!b.poke(Mood::Alarm, t0 + ms(i * 100)));
        }
        // cliques espaçados não contam
        let mut c = Animator::new(2);
        for i in 0..3 {
            assert!(!c.poke(Mood::Idle, t0 + ms(i * 1000)));
        }
    }

    #[test]
    fn mascotes_diferentes_nao_piscam_juntas() {
        let t0 = Instant::now();
        let mut a = Animator::new(0);
        let mut b = Animator::new(1);
        // amostra 10 s a cada 20 ms: as piscadas não podem coincidir sempre
        let (mut ja, mut jb, mut juntas) = (0, 0, 0);
        for i in 0..500 {
            let t = t0 + ms(i * 20);
            let (pa, pb) = (a.look(Mood::Idle, t).blink, b.look(Mood::Idle, t).blink);
            ja += pa as u32;
            jb += pb as u32;
            juntas += (pa && pb) as u32;
        }
        assert!(ja > 0 && jb > 0);
        assert!(juntas < ja.min(jb));
    }
}
