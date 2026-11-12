//! Reações curtas das mascotes, por cima do humor.
//!
//! O `Mood` é o estado que dura (dormindo, normal, feliz, ocupado, alerta). Uma reação é um
//! pico curto disparado por um evento ("um container voltou") ou pelo usuário (easter egg);
//! quando acaba, a mascote volta ao humor. Cada mascote tem o seu `Animator`.
//!
//! Tudo é medido pelo relógio, não por quadros: o timer da janela muda de ritmo com o mouse
//! (ver `FRAME_HOT` em `hub.rs`), e uma reação contada em quadros duraria o dobro. Pelo mesmo
//! motivo, uma reação disparada com a mascote fora da tela expira sozinha.

use std::cell::Cell;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::Mood;
use super::clips::{self, Clips, Grid};
use super::pack;
use super::sprites::Sprite;

/// "Reduzir movimento" (config global): as animações desenhadas ficam na pose base.
static CALM: AtomicBool = AtomicBool::new(false);

/// Liga ou desliga "reduzir movimento" para as animações desenhadas.
pub fn set_calm(on: bool) {
    CALM.store(on, Ordering::Relaxed);
}

/// "Reduzir movimento" está ligado.
pub fn calm() -> bool {
    CALM.load(Ordering::Relaxed)
}

thread_local! {
    /// Menor quadro (ms) pedido pelas mascotes desenhadas desde a última leitura do hub.
    static WANT_MS: Cell<u32> = const { Cell::new(0) };
}

pub(crate) fn request_frame_ms(ms: u32) {
    WANT_MS.with(|w| {
        if w.get() == 0 || ms < w.get() {
            w.set(ms);
        }
    });
}

/// O quadro mais curto que alguma animação na tela pediu desde a última chamada (o hub
/// acelera o timer só enquanto precisa). `None` = ninguém pediu.
pub fn take_requested_frame_ms() -> Option<u32> {
    let ms = WANT_MS.with(|w| w.replace(0));
    (ms > 0).then_some(ms)
}

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
    /// Quadro de uma animação desenhada (`clips.rs`); `None` = o desenho base.
    pub pose: Option<Grid>,
    /// Tempo desta mascote em ms: escolhe o quadro das mascotes importadas, que têm o
    /// próprio ritmo (`pack.rs`).
    pub ms: u64,
}

impl Look {
    /// Sem `Animator`: piscada e quadro vêm do contador global (bandeja, ícones das listas).
    pub fn from_frame(mood: Mood, frame: u32) -> Self {
        Look { mood, reaction: None, blink: frame.is_multiple_of(25), frame, pose: None, ms: frame as u64 * FRAME.as_millis() as u64 }
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
    player: Player,
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
            player: Player { rng: s as u64 | 1, ..Default::default() },
        }
    }

    /// Dispara uma reação. Recusa se a mesma tocou há pouco ou se uma mais importante está
    /// tocando. Devolve `true` se começou.
    pub fn trigger(&mut self, r: Reaction, now: Instant) -> bool {
        if self.last[r.index()].is_some_and(|t| now.duration_since(t) < r.cooldown()) {
            return false;
        }
        if let Some(cur) = self.current(now)
            && cur.priority() > r.priority() {
                return false;
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
        Look { mood, reaction, blink, frame, pose: None, ms: t.as_millis() as u64 }
    }

    /// Como `look`, com o quadro da animação desenhada de `sprite`, se ela tiver (ver
    /// `clips.rs`): a da reação enquanto ela dura; senão a do humor, com transições e
    /// variações. Com "reduzir movimento", fica na pose base.
    pub fn look_with(&mut self, sprite: &Sprite, mood: Mood, now: Instant) -> Look {
        let mut look = self.look(mood, now);
        // importada: o quadro sai do tempo (`pack::Art::pick`); só pede o ritmo dela
        if let Some(art) = pack::art(sprite) {
            if !calm() && let Some(ms) = art.frame_ms(&look) {
                request_frame_ms(ms);
            }
            return look;
        }
        if CALM.load(Ordering::Relaxed) {
            return look;
        }
        let Some(clips) = clips::of(sprite.key) else { return look };
        let reaction_clip = match (look.reaction, self.active) {
            (Some(r), Some((_, t0))) => clips.get(r.as_str()).map(|c| (c, now.duration_since(t0))),
            _ => None,
        };
        let pose = match reaction_clip {
            Some((clip, t)) => Some((*clip.frame_at(t.as_millis() as u64), clip)),
            None => self.player.pose(&clips, mood, now),
        };
        if let Some((grid, clip)) = pose {
            if clip.frames.len() > 1 {
                request_frame_ms(clip.shortest_ms());
            }
            look.pose = Some(grid);
        }
        look
    }
}

/// Qual animação desenhada toca agora: o loop do humor, a transição de entrada ou saída e,
/// de vez em quando, uma variação (ver `clips.rs`).
#[derive(Default)]
struct Player {
    /// Humor da última vez (para saber quando mudou).
    mood: Option<Mood>,
    /// Desde quando o loop do humor está tocando.
    loop_since: Option<Instant>,
    /// Transição ou variação tocando uma vez, e quando começou.
    once: Option<(String, Instant)>,
    queue: VecDeque<String>,
    next_variant: Option<Instant>,
    rng: u64,
}

impl Player {
    fn pose<'c>(&mut self, clips: &'c Clips, mood: Mood, now: Instant) -> Option<(Grid, &'c clips::Clip)> {
        if self.mood != Some(mood) {
            // mudou de humor: sai do antigo, entra no novo, e o loop recomeça
            self.queue.clear();
            self.once = None;
            let exit = self.mood.and_then(|m| clips.get(m.as_str())).and_then(|c| c.exit.clone());
            let enter = clips.get(mood.as_str()).and_then(|c| c.enter.clone());
            self.queue.extend(exit.into_iter().chain(enter));
            self.mood = Some(mood);
            self.loop_since = None;
            self.next_variant = None;
        }
        loop {
            if let Some((name, t0)) = &self.once {
                let t = now.duration_since(*t0).as_millis() as u64;
                match clips.get(name) {
                    Some(c) if !c.done_at(t) => return Some((*c.frame_at(t), c)),
                    // terminou (ou sumiu numa recarga): o loop recomeça do primeiro quadro
                    _ => {
                        self.once = None;
                        self.loop_since = Some(now);
                    }
                }
            }
            match self.queue.pop_front() {
                Some(next) => self.once = Some((next, now)),
                None => break,
            }
        }
        let clip = clips.get(mood.as_str())?;
        let since = *self.loop_since.get_or_insert(now);
        if !clip.variants.is_empty() {
            let next = match self.next_variant {
                Some(t) => t,
                None => {
                    let t = now + self.wait(clip.every);
                    self.next_variant = Some(t);
                    t
                }
            };
            if now >= next {
                let pick = clip.variants[(self.roll() % clip.variants.len() as u64) as usize].clone();
                self.next_variant = Some(now + self.wait(clip.every));
                self.once = Some((pick, now));
                return self.pose(clips, mood, now);
            }
        }
        let t = now.duration_since(since).as_millis() as u64;
        Some((*clip.frame_at(t), clip))
    }

    /// Espera sorteada entre `min` e `max` segundos.
    fn wait(&mut self, (min, max): (u32, u32)) -> Duration {
        let span = (max - min) as u64 * 1000;
        Duration::from_millis(min as u64 * 1000 + if span == 0 { 0 } else { self.roll() % (span + 1) })
    }

    /// Sorteio simples (xorshift): variação não precisa de mais que isso.
    fn roll(&mut self) -> u64 {
        let mut x = self.rng.max(1);
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng = x;
        x
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// Animações de teste na neko: cada uma com a 1ª linha marcada, para saber qual tocou.
    fn clips_de_teste() -> Clips {
        let neko = crate::mascot::sprites::get("neko");
        let q = |marca: &str| {
            let mut rows: Vec<&str> = neko.grid.to_vec();
            rows[0] = marca;
            crate::mascot::clips::tests::frame(&rows)
        };
        let (a, b, c, d, e) = (q("K..............."), q(".K.............."), q("..K............."), q("...K............"), q("....K..........."));
        let src = format!(
            "[idle]\nfps = 1\nexit = \"acorda\"\nvariants = [\"coca\"]\nevery = [5, 5]\nframes = [{a}]\n\
             [sleep]\nfps = 1\nenter = \"boceja\"\nframes = [{b}]\n\
             [boceja]\ndurations = [300]\nframes = [{c}]\n\
             [acorda]\ndurations = [200]\nframes = [{d}]\n\
             [coca]\ndurations = [400]\nframes = [{e}]\n"
        );
        crate::mascot::clips::parse(neko, &src).unwrap()
    }

    fn marca(p: &mut Player, clips: &Clips, mood: Mood, t: Instant) -> &'static str {
        p.pose(clips, mood, t).map(|(g, _)| g[0]).unwrap_or("base")
    }

    #[test]
    fn transicoes_ao_mudar_de_humor() {
        let clips = clips_de_teste();
        let mut p = Player { rng: 1, ..Default::default() };
        let t0 = Instant::now();
        assert_eq!(marca(&mut p, &clips, Mood::Idle, t0), "K...............");
        // idle -> sleep: sai do idle (acorda, 200 ms), entra no sleep (boceja, 300 ms), loop
        assert_eq!(marca(&mut p, &clips, Mood::Sleep, t0 + ms(1000)), "...K............");
        assert_eq!(marca(&mut p, &clips, Mood::Sleep, t0 + ms(1150)), "...K............");
        assert_eq!(marca(&mut p, &clips, Mood::Sleep, t0 + ms(1250)), "..K.............");
        assert_eq!(marca(&mut p, &clips, Mood::Sleep, t0 + ms(1600)), ".K..............");
        // humor sem animação: o desenho base
        assert_eq!(marca(&mut p, &clips, Mood::Busy, t0 + ms(2000)), "base");
    }

    #[test]
    fn variacao_de_vez_em_quando() {
        let clips = clips_de_teste();
        let mut p = Player { rng: 1, ..Default::default() };
        let t0 = Instant::now();
        assert_eq!(marca(&mut p, &clips, Mood::Idle, t0), "K...............");
        assert_eq!(marca(&mut p, &clips, Mood::Idle, t0 + ms(4900)), "K...............");
        // every = [5, 5]: aos 5 s entra a variação (400 ms) e depois volta ao loop
        assert_eq!(marca(&mut p, &clips, Mood::Idle, t0 + ms(5000)), "....K...........");
        assert_eq!(marca(&mut p, &clips, Mood::Idle, t0 + ms(5300)), "....K...........");
        assert_eq!(marca(&mut p, &clips, Mood::Idle, t0 + ms(5500)), "K...............");
    }

    #[test]
    fn reduzir_movimento_fica_na_pose_base() {
        let neko = crate::mascot::sprites::get("neko");
        let mut a = Animator::new(0);
        set_calm(true);
        let calmo = a.look_with(neko, Mood::Sleep, Instant::now()).pose;
        set_calm(false);
        assert_eq!(calmo, None);
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
