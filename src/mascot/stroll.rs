//! Subagentes andando com as mascotes importadas: cada um anda um pouco, para e faz uma
//! das animações do pacote (ataque, empurrar, arremessar, parado…), volta a andar, às
//! vezes dá meia-volta; corre enquanto está chamando ferramentas e pula ao terminar.
//!
//! Só números (que animação, que quadro, para que lado, onde na faixa): quem desenha é o
//! app, com os quadros de `pack::Troupe`. O tempo é em ms de um relógio qualquer que só
//! anda para a frente; a posição `x` vai de 0 (ponta esquerda) a 1 (direita).

/// As animações de uma mascote para andar na faixa (índices em `frames`/`ms`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Script {
    pub names: Vec<String>,
    /// Quadros de cada animação.
    pub frames: Vec<usize>,
    /// Quanto cada quadro fica na tela.
    pub ms: Vec<u32>,
    pub walk: usize,
    /// Correr: enquanto chama ferramentas.
    pub run: Option<usize>,
    /// Pular: ao terminar.
    pub jump: Option<usize>,
    pub idle: Option<usize>,
    /// Andando, além de `walk` (ex.: "walk+attack").
    pub strolls: Vec<usize>,
    /// Paradas no lugar: ataques, empurrar, arremessar, parado…
    pub extras: Vec<usize>,
}

impl Script {
    /// Uma volta inteira da animação `c`, em ms.
    fn lap(&self, c: usize) -> u64 {
        (self.frames[c] as u64 * self.ms[c].max(1) as u64).max(1)
    }
}

/// O que desenhar de um subagente agora.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pose {
    /// Mascote (índice da lista que o app passou).
    pub hero: usize,
    pub clip: usize,
    pub frame: usize,
    /// Olhando para a direita.
    pub right: bool,
    pub x: f32,
}

/// Faixa por segundo, andando e correndo.
const WALK_SPEED: f32 = 0.07;
const RUN_SPEED: f32 = 0.16;
/// Parada mais curta (a animação repete até passar disso).
const MIN_STAY_MS: u64 = 900;
/// Passo de tempo mais longo de uma vez (janela escondida não faz ninguém teleportar).
const MAX_STEP_MS: u64 = 400;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Act {
    Move(usize),
    Stay(usize),
    /// Terminou: pula uma vez e sai (`None` = sem pulo no pacote, sai já).
    Leave(Option<usize>),
}

#[derive(Debug, Clone)]
struct Walker {
    id: String,
    hero: usize,
    x: f32,
    right: bool,
    act: Act,
    start: u64,
    end: u64,
    active: bool,
    /// Passo próprio (0,8 a 1,2× a velocidade): dois correndo juntos não andam grudados.
    pace: f32,
    /// Última parada, para não repetir a mesma em seguida.
    last: Option<usize>,
    rng: u64,
}

impl Walker {
    /// Número de 0 a `n - 1` (xorshift: a sequência de cada subagente sai do id dele).
    fn roll(&mut self, n: u64) -> u64 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        self.rng % n.max(1)
    }

    /// Como anda agora: correndo se está chamando ferramentas; senão, em geral andando, às
    /// vezes outro jeito de andar do pacote.
    fn mover(&mut self, s: &Script) -> usize {
        if self.active && let Some(run) = s.run {
            return run;
        }
        if s.strolls.is_empty() || self.roll(4) > 0 {
            return s.walk;
        }
        s.strolls[self.roll(s.strolls.len() as u64) as usize]
    }

    /// Uma parada: "parado" um terço das vezes; senão, outra das extras, sem repetir a
    /// última.
    fn extra(&mut self, s: &Script) -> usize {
        if let Some(idle) = s.idle.filter(|_| self.roll(3) == 0) {
            return idle;
        }
        let pool: Vec<usize> = s.extras.iter().copied().filter(|c| Some(*c) != self.last && Some(*c) != s.idle).collect();
        if pool.is_empty() {
            return s.extras[self.roll(s.extras.len() as u64) as usize];
        }
        pool[self.roll(pool.len() as u64) as usize]
    }

    /// O que fazer quando o que estava fazendo acabou.
    fn next(&mut self, s: &Script, now: u64) {
        let stays = if self.active { 25 } else { 55 };
        let stop = matches!(self.act, Act::Move(_)) && !s.extras.is_empty() && self.roll(100) < stays;
        if stop {
            let c = self.extra(s);
            self.last = Some(c);
            let lap = s.lap(c);
            let laps = (1 + self.roll(3)).max(MIN_STAY_MS.div_ceil(lap));
            self.act = Act::Stay(c);
            self.end = now + lap * laps;
        } else {
            let turn = if matches!(self.act, Act::Stay(_)) { 40 } else { 30 };
            if self.roll(100) < turn {
                self.right = !self.right;
            }
            self.act = Act::Move(self.mover(s));
            self.end = now + 1500 + self.roll(2500);
        }
        self.start = now;
    }
}

/// Os subagentes andando numa faixa (a de uma sessão).
#[derive(Debug, Clone, Default)]
pub struct Stage {
    walkers: Vec<Walker>,
    at: Option<u64>,
}

impl Stage {
    pub fn is_empty(&self) -> bool {
        self.walkers.is_empty()
    }

    /// Quem anda agora: (id, mascote, chamando ferramentas). Quem chega começa num ponto
    /// qualquer da faixa; quem saiu pula uma vez e some.
    pub fn cast(&mut self, now: u64, here: &[(String, usize, bool)], scripts: &[&Script]) {
        for w in &mut self.walkers {
            if matches!(w.act, Act::Leave(_)) || here.iter().any(|(id, ..)| *id == w.id) {
                continue;
            }
            let jump = scripts.get(w.hero).and_then(|s| s.jump);
            w.act = Act::Leave(jump);
            w.start = now;
            w.end = now + jump.map_or(0, |j| scripts[w.hero].lap(j));
        }
        self.walkers.retain(|w| !matches!(w.act, Act::Leave(_)) || w.end > now);
        for (id, hero, active) in here {
            let Some(s) = scripts.get(*hero) else { continue };
            match self.walkers.iter_mut().find(|w| w.id == *id && !matches!(w.act, Act::Leave(_))) {
                Some(w) => {
                    if w.active != *active {
                        w.active = *active;
                        // começou ou parou de correr: troca o jeito de andar já
                        if let Act::Move(c) = w.act
                            && (Some(c) == s.run || *active)
                        {
                            w.act = Act::Move(w.mover(s));
                        }
                    }
                }
                None => {
                    let seed = id.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3));
                    let mut w = Walker {
                        id: id.clone(),
                        hero: *hero,
                        x: 0.0,
                        right: true,
                        act: Act::Move(s.walk),
                        start: now,
                        end: now,
                        active: *active,
                        pace: 1.0,
                        last: None,
                        rng: seed | 1,
                    };
                    w.pace = (80 + w.roll(41)) as f32 / 100.0;
                    w.x = (10 + w.roll(81)) as f32 / 100.0;
                    w.right = w.roll(2) == 0;
                    w.act = Act::Move(w.mover(s));
                    w.end = now + 1500 + w.roll(2500);
                    self.walkers.push(w);
                }
            }
        }
    }

    /// Anda o tempo até `now`: quem anda muda de lugar (e vira na ponta), quem acabou o
    /// que fazia passa ao próximo, quem saiu e já pulou some.
    pub fn step(&mut self, now: u64, scripts: &[&Script]) {
        let dt = self.at.map_or(0, |at| now.saturating_sub(at).min(MAX_STEP_MS)) as f32 / 1000.0;
        self.at = Some(now);
        for w in &mut self.walkers {
            let Some(s) = scripts.get(w.hero) else { continue };
            if let Act::Move(c) = w.act {
                let speed = if Some(c) == s.run { RUN_SPEED } else { WALK_SPEED } * w.pace;
                w.x += if w.right { speed } else { -speed } * dt;
                if w.x >= 1.0 {
                    (w.x, w.right) = (1.0, false);
                } else if w.x <= 0.0 {
                    (w.x, w.right) = (0.0, true);
                }
            }
            if now >= w.end && !matches!(w.act, Act::Leave(_)) {
                w.next(s, now);
            }
        }
        self.walkers.retain(|w| !matches!(w.act, Act::Leave(_)) || w.end > now);
    }

    /// O que desenhar agora, na ordem de chegada. Com "reduzir movimento" (`calm`),
    /// todos parados no primeiro quadro, espalhados pela faixa.
    pub fn poses(&self, now: u64, scripts: &[&Script], calm: bool) -> Vec<Pose> {
        let n = self.walkers.len();
        self.walkers
            .iter()
            .enumerate()
            .filter_map(|(k, w)| {
                let s = scripts.get(w.hero)?;
                if calm {
                    let clip = s.idle.unwrap_or(s.walk);
                    return Some(Pose { hero: w.hero, clip, frame: 0, right: true, x: (k as f32 + 0.5) / n as f32 });
                }
                let clip = match w.act {
                    Act::Move(c) | Act::Stay(c) | Act::Leave(Some(c)) => c,
                    Act::Leave(None) => s.walk,
                };
                let f = (now.saturating_sub(w.start) / s.ms[clip].max(1) as u64) as usize;
                // o pulo de saída não repete: para no último quadro
                let frame = if matches!(w.act, Act::Leave(_)) { f.min(s.frames[clip] - 1) } else { f % s.frames[clip] };
                Some(Pose { hero: w.hero, clip, frame, right: w.right, x: w.x })
            })
            .collect()
    }

    /// O quadro mais curto entre as animações na tela (o hub acelera o timer até ele).
    pub fn frame_ms(&self, scripts: &[&Script]) -> Option<u32> {
        self.walkers
            .iter()
            .filter_map(|w| {
                let s = scripts.get(w.hero)?;
                let c = match w.act {
                    Act::Move(c) | Act::Stay(c) | Act::Leave(Some(c)) => c,
                    Act::Leave(None) => return None,
                };
                Some(s.ms[c])
            })
            .min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// walk 0, run 1, jump 2, idle 3, attack 4, push 5, walk+attack 6.
    fn script() -> Script {
        let names = ["walk", "run", "jump", "idle", "attack", "push", "walk+attack"];
        Script {
            names: names.iter().map(|n| n.to_string()).collect(),
            frames: vec![6, 6, 8, 4, 4, 6, 6],
            ms: vec![125; 7],
            walk: 0,
            run: Some(1),
            jump: Some(2),
            idle: Some(3),
            strolls: vec![6],
            extras: vec![3, 4, 5],
        }
    }

    fn here(list: &[(&str, bool)]) -> Vec<(String, usize, bool)> {
        list.iter().map(|(id, active)| (id.to_string(), 0, *active)).collect()
    }

    #[test]
    fn anda_para_e_varia_as_animacoes() {
        let s = script();
        let mut stage = Stage::default();
        stage.cast(0, &here(&[("a", false)]), &[&s]);
        let mut seen = std::collections::BTreeSet::new();
        let mut xs = Vec::new();
        for t in (0..120_000).step_by(125) {
            stage.step(t, &[&s]);
            let p = stage.poses(t, &[&s], false)[0];
            assert!((0.0..=1.0).contains(&p.x) && p.frame < s.frames[p.clip]);
            seen.insert(p.clip);
            xs.push(p.x);
        }
        // em dois minutos: andou, parou em todas as extras e no "parado", nunca correu nem pulou
        for c in [0, 3, 4, 5, 6] {
            assert!(seen.contains(&c), "{} nunca apareceu: {seen:?}", s.names[c]);
        }
        assert!(!seen.contains(&1) && !seen.contains(&2), "{seen:?}");
        // e saiu do lugar
        let (lo, hi) = xs.iter().fold((1f32, 0f32), |(lo, hi), x| (lo.min(*x), hi.max(*x)));
        assert!(hi - lo > 0.3, "{lo}..{hi}");
    }

    #[test]
    fn corre_chamando_ferramentas_e_pula_ao_terminar() {
        let s = script();
        let mut stage = Stage::default();
        stage.cast(0, &here(&[("a", true)]), &[&s]);
        stage.step(0, &[&s]);
        assert_eq!(stage.poses(0, &[&s], false)[0].clip, 1, "chamando ferramenta: corre");
        // parou de chamar: volta a andar na hora
        stage.cast(500, &here(&[("a", false)]), &[&s]);
        assert_ne!(stage.poses(500, &[&s], false)[0].clip, 1);
        // terminou: pula uma volta (8 quadros de 125 ms), parando no último quadro, e some
        stage.cast(1000, &here(&[]), &[&s]);
        let p = stage.poses(1000 + 7 * 125 + 50, &[&s], false);
        assert_eq!((p[0].clip, p[0].frame), (2, 7));
        stage.step(1999, &[&s]);
        assert!(!stage.is_empty());
        stage.step(2000, &[&s]);
        assert!(stage.is_empty());
    }

    #[test]
    fn cada_um_no_seu_ritmo_e_parados_com_calma() {
        let s = script();
        let mut stage = Stage::default();
        stage.cast(0, &here(&[("a", false), ("b", false), ("c", false)]), &[&s]);
        let p = stage.poses(0, &[&s], false);
        assert_eq!(p.len(), 3);
        assert!(p[0].x != p[1].x || p[1].x != p[2].x, "{p:?}");
        let calm = stage.poses(0, &[&s], true);
        assert!(calm.iter().all(|p| p.frame == 0 && p.clip == 3));
        assert_eq!(calm.iter().map(|p| p.x).collect::<Vec<_>>(), [0.5 / 3.0, 1.5 / 3.0, 2.5 / 3.0]);
        // mesmo id, mesma sequência: a tela não muda de ideia ao redesenhar
        let mut again = Stage::default();
        again.cast(0, &here(&[("a", false), ("b", false), ("c", false)]), &[&s]);
        assert_eq!(again.poses(0, &[&s], false), p);
    }
}
