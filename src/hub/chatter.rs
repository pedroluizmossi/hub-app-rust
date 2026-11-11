//! Diretor da conversa entre mascotes.
//!
//! Recebe os fatos dos apps (`MiniApp::take_facts`), monta uma conversa curta (quem conta e
//! quem responde) com as falas dos `lines.toml` e toca uma fala de cada vez, na caixa de
//! diálogo do app de quem fala. O diretor não conhece nenhum app: o que cada um diz vem do
//! `lines.toml` dele, e o que faltar vem das falas genéricas (`lines.toml` ao lado deste
//! arquivo). Formato das falas: `src/core/chatter.rs`.
//!
//! Só conversa com a janela aberta no painel e com a opção "Mascotes conversam" ligada.
//! Quem responde é sempre uma mascote da página aberta; quem conta pode estar em outra
//! página (só alarmes e recuperações), e aí a fala aparece no app de quem responde, com o
//! nome da página. Com tudo calmo por um tempo, sai um papo à toa.
//!
//! Para não virar falatório: um intervalo entre conversas puxadas por fatos, um tempo
//! antes de repetir o mesmo assunto, e cutucadas com um intervalo curto próprio. Fato que
//! não dá para mostrar é descartado (fatos são mudanças, não ficam velhos esperando).

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use slint::{ComponentHandle, TimerMode};

use super::Hub;
use crate::core::chatter::{Fact, FactKind, Lang, Lines, Say, Voice};
use crate::mascot::render;
use crate::ui::Chatter;

/// Falas genéricas: o que um app não declarou no próprio `lines.toml`.
const GENERIC: &str = include_str!("lines.toml");

/// Intervalo dos quadros enquanto uma fala está no ar.
const CHAT_FRAME: Duration = Duration::from_millis(100);
/// Letras por segundo ao "digitar" a fala.
const TYPE_RATE: f32 = 28.0;
/// Quanto a fala fica na tela depois de digitada: base + um tanto por letra (até o teto).
const HOLD_BASE: Duration = Duration::from_millis(1800);
const HOLD_PER_CHAR: Duration = Duration::from_millis(40);
const HOLD_MAX: Duration = Duration::from_millis(5500);
/// Pausa entre uma fala e a próxima.
const GAP: Duration = Duration::from_millis(350);
/// Intervalo mínimo entre conversas de cutucada.
const POKE_COOLDOWN: Duration = Duration::from_secs(10);
/// Quantas falas ditas o diretor lembra para não repetir (ver `pick_text`).
const RECENT: usize = 32;
/// Tempo antes de comentar de novo o mesmo assunto (app + assunto + tipo).
const TOPIC_COOLDOWN: Duration = Duration::from_secs(600);

/// Quanto as mascotes conversam (opção `chatter` da config).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pace {
    Off,
    Some,
    Lots,
}

impl Pace {
    pub const KEYS: [&str; 3] = ["off", "some", "lots"];

    pub fn from_key(key: &str) -> Pace {
        match key {
            "off" => Pace::Off,
            "lots" => Pace::Lots,
            _ => Pace::Some,
        }
    }

    /// Intervalo mínimo entre conversas puxadas por fatos (alarmes furam a fila).
    fn talk_cooldown(self) -> Duration {
        match self {
            Pace::Lots => Duration::from_secs(45),
            _ => Duration::from_secs(150),
        }
    }

    /// Quanto tempo calmo até um papo à toa (sorteado entre os dois).
    fn idle_range(self) -> (u64, u64) {
        match self {
            Pace::Lots => (120, 240),
            _ => (360, 600),
        }
    }
}

/// Uma fala pronta.
#[derive(Clone)]
pub struct Line {
    pub voice: Voice,
    pub text: String,
    /// Quem ouve (vira o olhar para quem fala).
    pub listener: Option<Voice>,
    /// Em qual app a caixa de diálogo aparece: o de quem fala, ou o de quem responde
    /// quando quem fala está em outra página.
    pub show_in: String,
}

/// O que a caixa de diálogo mostra agora.
pub struct View {
    /// Muda a cada fala nova (para trocar retrato e nome só quando precisa).
    pub seq: u64,
    pub line: Line,
    /// Parte já "digitada".
    pub typed: String,
    /// Terminou de digitar.
    pub done: bool,
}

pub struct Director {
    generic: Lines,
    /// Falas de cada app (id -> falas).
    lines: HashMap<String, Lines>,
    queue: VecDeque<Line>,
    current: Option<(Line, Instant)>,
    seq: u64,
    gap_until: Option<Instant>,
    last_talk: Option<Instant>,
    last_poke: Option<Instant>,
    topics: HashMap<String, Instant>,
    /// Desde quando está tudo calmo (sem conversa e com o painel à vista).
    quiet_since: Option<Instant>,
    /// Calma necessária para o próximo papo à toa (sorteada).
    idle_wait: Duration,
    /// Últimas falas ditas (o modelo, antes de preencher as lacunas), da mais antiga para
    /// a mais nova.
    recent: VecDeque<String>,
    rng: u64,
}

impl Default for Director {
    fn default() -> Self {
        let seed = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0x9e37_79b9, |d| d.as_nanos() as u64);
        Director::with_seed(seed)
    }
}

impl Director {
    pub fn with_seed(seed: u64) -> Self {
        Director {
            generic: Lines::parse(GENERIC).unwrap_or_default(),
            lines: HashMap::new(),
            queue: VecDeque::new(),
            current: None,
            seq: 0,
            gap_until: None,
            last_talk: None,
            last_poke: None,
            topics: HashMap::new(),
            quiet_since: None,
            idle_wait: Duration::from_secs(480),
            recent: VecDeque::new(),
            rng: seed | 1,
        }
    }

    /// Falas próprias de um app (o `lines.toml` dele).
    pub fn add_lines(&mut self, app: &str, lines: Lines) {
        self.lines.insert(app.to_string(), lines);
    }

    /// Avança a conversa. `voices` são as mascotes da página aberta; `visible` diz se dá
    /// para mostrar alguma coisa (janela aberta no painel).
    pub fn step(&mut self, now: Instant, facts: Vec<Fact>, voices: &[Voice], visible: bool, pace: Pace) {
        if !visible || pace == Pace::Off {
            self.clear();
            self.quiet_since = None;
            return;
        }
        // trocou de página no meio da conversa: a caixa não está mais na tela
        let on_page = |app: &str| voices.iter().any(|v| v.app == app);
        let gone = self.current.as_ref().is_some_and(|(l, _)| !on_page(&l.show_in))
            || self.queue.iter().any(|l| !on_page(&l.show_in));
        if gone {
            self.clear();
        }
        let quiet = *self.quiet_since.get_or_insert(now);
        if self.current.is_none() && self.queue.is_empty() {
            let started = self.start(now, facts, voices, pace)
                || (now.duration_since(quiet) >= self.idle_wait && self.idle(now, voices, pace));
            if started {
                self.quiet_since = Some(now);
                let (lo, hi) = pace.idle_range();
                self.idle_wait = Duration::from_secs(lo + self.next() % (hi - lo + 1));
            }
        }
        // fim da fala atual: pausa e depois a próxima
        if let Some((line, started)) = &self.current {
            if now >= *started + line_time(&line.text) {
                self.current = None;
                self.gap_until = Some(now + GAP);
            }
        }
        if self.current.is_none() && self.gap_until.is_none_or(|t| now >= t) {
            if let Some(line) = self.queue.pop_front() {
                self.seq += 1;
                self.current = Some((line, now));
                self.gap_until = None;
            }
        }
        // conversa no ar conta como "não calmo"
        if self.current.is_some() || !self.queue.is_empty() {
            self.quiet_since = Some(now);
        }
    }

    /// Há conversa no ar (uma fala ou outras na fila): a tela precisa de quadros.
    pub fn busy(&self) -> bool {
        self.current.is_some() || !self.queue.is_empty()
    }

    /// O que mostrar agora (nada entre falas).
    pub fn view(&self, now: Instant) -> Option<View> {
        let (line, started) = self.current.as_ref()?;
        let total = line.text.chars().count();
        let n = ((now - *started).as_secs_f32() * TYPE_RATE) as usize;
        Some(View {
            seq: self.seq,
            line: line.clone(),
            typed: line.text.chars().take(n.min(total)).collect(),
            done: n >= total,
        })
    }

    /// Pula para a próxima fala (clique na caixa).
    pub fn skip(&mut self, now: Instant) {
        if self.current.take().is_some() {
            self.gap_until = Some(now + GAP);
        }
    }

    fn clear(&mut self) {
        self.queue.clear();
        self.current = None;
        self.gap_until = None;
    }

    /// Escolhe o fato mais importante que pode virar conversa e enfileira as falas.
    fn start(&mut self, now: Instant, facts: Vec<Fact>, voices: &[Voice], pace: Pace) -> bool {
        let rank = |k: FactKind| match k {
            FactKind::Poked => 0,
            FactKind::Trouble => 1,
            FactKind::Recovered => 2,
            FactKind::Forecast => 3,
            FactKind::Strain => 4,
            FactKind::Idle => 5,
        };
        let mut facts = facts;
        facts.sort_by_key(|f| rank(f.kind));
        let cooled = |t: Option<Instant>, d: Duration| t.is_none_or(|t| now.duration_since(t) >= d);
        for fact in facts {
            // de outra página, só alarme e recuperação (o resto não vale interromper)
            let remote = !voices.iter().any(|v| v.app == fact.voice.app);
            if remote && !matches!(fact.kind, FactKind::Trouble | FactKind::Recovered) {
                continue;
            }
            let topic = format!("{}:{}:{:?}", fact.voice.app, fact.subject, fact.kind);
            let ok = match fact.kind {
                FactKind::Poked => cooled(self.last_poke, POKE_COOLDOWN),
                // alarme fura o intervalo entre conversas (mas não repete o assunto)
                FactKind::Trouble => cooled(self.topics.get(&topic).copied(), TOPIC_COOLDOWN),
                _ => {
                    cooled(self.last_talk, pace.talk_cooldown())
                        && cooled(self.topics.get(&topic).copied(), TOPIC_COOLDOWN)
                }
            };
            if !ok {
                continue;
            }
            let lines = self.compose(fact.kind, &fact.voice, &fact.subject, fact.value, voices);
            if lines.is_empty() {
                continue;
            }
            if fact.kind == FactKind::Poked {
                self.last_poke = Some(now);
            } else {
                self.last_talk = Some(now);
                self.topics.insert(topic, now);
            }
            self.queue.extend(lines);
            return true;
        }
        false
    }

    /// Papo à toa: uma mascote da página puxa assunto e outra responde.
    fn idle(&mut self, now: Instant, voices: &[Voice], pace: Pace) -> bool {
        let cooled = self.last_talk.is_none_or(|t| now.duration_since(t) >= pace.talk_cooldown());
        let Some(a) = (if cooled { self.pick(voices).cloned() } else { None }) else { return false };
        let lines = self.compose(FactKind::Idle, &a, "", None, voices);
        if lines.is_empty() {
            return false;
        }
        self.last_talk = Some(now);
        self.queue.extend(lines);
        true
    }

    /// Quem responde e as falas preenchidas. Vazio se não dá para montar a conversa.
    pub fn compose(&mut self, kind: FactKind, a: &Voice, subject: &str, value: Option<f32>, voices: &[Voice]) -> Vec<Line> {
        let subject = if subject.is_empty() { a.name.clone() } else { subject.to_string() };
        let remote = !voices.iter().any(|v| v.app == a.app);
        let b = self.responder(kind, a, &subject, voices);
        // de outra página, a fala só aparece se alguém daqui responder (é na caixa dele)
        if remote && b.is_none() {
            return Vec::new();
        }
        let ctx = Ctx { value, has_b: b.is_some() };
        let a_say = self.own_say(kind, &a.app, ctx);
        let b_say = b.as_ref().map(|b| self.answer_say(kind, &a.app, &b.app, &subject, ctx));
        let lang = self.lang(&a_say, b_say.as_ref(), ctx);
        let fill = |t: &str| {
            t.replace("{a}", &a.name)
                .replace("{b}", b.as_ref().map_or("", |b| b.name.as_str()))
                .replace("{s}", &subject)
                .replace("{v}", &value.map_or(String::new(), |v| format!("{v:.0}")))
        };
        let mut out = Vec::new();
        let show_a = match (&b, remote) {
            (Some(b), true) => b.app.clone(),
            _ => a.app.clone(),
        };
        if let Some(t) = self.pick_text(&a_say, lang, ctx) {
            out.push(Line { voice: a.clone(), text: fill(&t), listener: b.clone(), show_in: show_a });
        }
        if let (Some(b), Some(say)) = (&b, &b_say) {
            if let Some(t) = self.pick_text(say, lang, ctx) {
                out.push(Line { voice: b.clone(), text: fill(&t), listener: Some(a.clone()), show_in: b.app.clone() });
            }
        }
        out
    }

    /// Quem responde: uma mascote da página que não seja quem conta. Primeiro as que têm
    /// resposta própria para o caso (o Docker assumindo a culpa pela CPU), depois as de
    /// outro app, por último as do mesmo app.
    fn responder(&mut self, kind: FactKind, a: &Voice, subject: &str, voices: &[Voice]) -> Option<Voice> {
        let others: Vec<&Voice> = voices.iter().filter(|v| !v.same(a)).collect();
        let own_reply: Vec<&Voice> = others
            .iter()
            .copied()
            .filter(|v| v.app != a.app && self.lines.get(&v.app).and_then(|l| l.reply(kind)).is_some_and(|r| r.fits(subject)))
            .collect();
        let other_app: Vec<&Voice> = others.iter().copied().filter(|v| v.app != a.app).collect();
        let pool = if !own_reply.is_empty() { own_reply } else if !other_app.is_empty() { other_app } else { others };
        self.pick(&pool).map(|v| (*v).clone())
    }

    /// O que quem conta diz: do app dele, se tiver fala usável; senão, a genérica.
    fn own_say(&self, kind: FactKind, app: &str, ctx: Ctx) -> Say {
        self.lines
            .get(app)
            .and_then(|l| l.own(kind))
            .map(|e| e.say())
            .filter(|s| usable(s, Lang::Pt, ctx))
            .or_else(|| self.generic.own(kind).map(|e| e.say()))
            .unwrap_or_default()
    }

    /// O que quem responde diz: a resposta própria do app dele (se servir para o assunto),
    /// a resposta que o app de quem conta sugere (`answer`), ou a genérica.
    fn answer_say(&self, kind: FactKind, a_app: &str, b_app: &str, subject: &str, ctx: Ctx) -> Say {
        let own = self.lines.get(b_app).and_then(|l| l.reply(kind)).filter(|r| r.fits(subject)).cloned();
        let suggested = self.lines.get(a_app).and_then(|l| l.own(kind)).and_then(|e| e.answer.clone());
        [own, suggested, self.generic.reply(kind).cloned()]
            .into_iter()
            .flatten()
            .find(|s| usable(s, Lang::Pt, ctx))
            .unwrap_or_default()
    }

    /// Idioma da conversa inteira: português na maioria, inglês às vezes, alemão de vez
    /// em quando. Só sai um idioma que sirva para as duas falas; senão, português.
    fn lang(&mut self, a: &Say, b: Option<&Say>, ctx: Ctx) -> Lang {
        let roll = self.next() % 100;
        let lang = if roll < 55 { Lang::Pt } else if roll < 90 { Lang::En } else { Lang::De };
        if usable(a, lang, ctx) && b.is_none_or(|b| usable(b, lang, ctx)) { lang } else { Lang::Pt }
    }

    /// Sorteia uma variante sem repetir as últimas ditas: primeiro entre as que não saíram
    /// há pouco; se todas saíram, entre a metade dita há mais tempo (com duas variantes,
    /// elas se alternam; com uma só, não há o que fazer).
    fn pick_text(&mut self, say: &Say, lang: Lang, ctx: Ctx) -> Option<String> {
        let options: Vec<&String> = say.variants(lang).iter().filter(|t| ctx.allows(t)).collect();
        // posição na memória: quanto maior, mais recente; `None` = não saiu há pouco
        let said = |t: &String| self.recent.iter().rposition(|r| r == t);
        let fresh: Vec<&String> = options.iter().copied().filter(|t| said(t).is_none()).collect();
        let pool = if fresh.is_empty() {
            let mut old = options.clone();
            old.sort_by_key(|t| said(t));
            old.truncate(options.len().div_ceil(2));
            old
        } else {
            fresh
        };
        let text = self.pick(&pool).map(|t| t.to_string())?;
        self.recent.retain(|r| *r != text);
        self.recent.push_back(text.clone());
        if self.recent.len() > RECENT {
            self.recent.pop_front();
        }
        Some(text)
    }

    fn pick<'a, T>(&mut self, items: &'a [T]) -> Option<&'a T> {
        if items.is_empty() {
            return None;
        }
        let i = (self.next() % items.len() as u64) as usize;
        items.get(i)
    }

    /// xorshift64: basta para sortear falas.
    fn next(&mut self) -> u64 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng = x;
        x
    }
}

/// O que a conversa tem para preencher as lacunas.
#[derive(Clone, Copy)]
struct Ctx {
    value: Option<f32>,
    has_b: bool,
}

impl Ctx {
    /// A variante só usa lacunas que dá para preencher.
    fn allows(self, t: &str) -> bool {
        (self.value.is_some() || !t.contains("{v}")) && (self.has_b || !t.contains("{b}"))
    }
}

/// Tem alguma variante usável neste idioma.
fn usable(say: &Say, lang: Lang, ctx: Ctx) -> bool {
    say.variants(lang).iter().any(|t| ctx.allows(t))
}

/// Quanto uma fala ocupa: digitar + ficar na tela.
fn line_time(text: &str) -> Duration {
    let n = text.chars().count();
    let typing = Duration::from_secs_f32(n as f32 / TYPE_RATE);
    typing + (HOLD_BASE + HOLD_PER_CHAR * n as u32).min(HOLD_MAX)
}

fn voice_id(v: &Voice) -> String {
    v.id()
}

impl Hub {
    /// Carrega as falas de cada app (chamado uma vez, na montagem).
    pub(super) fn load_lines(&self) {
        let mut d = self.chatter.borrow_mut();
        for (app, src) in self.registry.lines() {
            match Lines::parse(src) {
                Ok(lines) => {
                    for p in lines.problems() {
                        log::warn!("falas de {app}: {p}");
                    }
                    d.add_lines(&app, lines);
                }
                Err(e) => log::warn!("falas de {app} inválidas: {e}"),
            }
        }
    }

    /// Recolhe os fatos dos apps, avança a conversa e leva a fala atual à tela.
    pub(super) fn chat_tick(&self) {
        let facts = self.registry.take_facts();
        let pace = Pace::from_key(&self.host.config.borrow().chatter);
        let visible = self.shown.get() && self.ui.get_current() == "panel";
        let voices = if visible {
            let ids = self.host.config.borrow().page_apps(self.current_page());
            self.registry.voices(&ids)
        } else {
            Vec::new()
        };
        let now = Instant::now();
        let (view, busy) = {
            let mut d = self.chatter.borrow_mut();
            d.step(now, facts, &voices, visible, pace);
            (d.view(now), d.busy())
        };
        // a fala é "digitada" a 28 letras/s: com conversa no ar, quadros a cada 100 ms; sem
        // ela, o quadro de animação basta para puxar o próximo papo
        if !busy {
            self.chat_timer.stop();
        } else if !self.chat_timer.running() {
            self.chat_timer.start(TimerMode::Repeated, CHAT_FRAME, || super::with_hub(Hub::chat_tick));
        }
        let key = view.as_ref().map(|v| (v.seq, v.typed.len(), v.done));
        if self.chat_shown.replace(key) == key {
            return;
        }
        let g = self.ui.global::<Chatter>();
        let Some(v) = view else {
            g.set_app("".into());
            g.set_speaker("".into());
            g.set_listener("".into());
            return;
        };
        if g.get_seq() != v.seq as i32 {
            let voice = &v.line.voice;
            // quem fala está em outra página: o nome leva a página junto
            let remote = voice.app != v.line.show_in;
            let name = if remote {
                let cfg = self.host.config.borrow();
                let page = cfg.page_of(&voice.app).and_then(|p| cfg.pages.get(p)).map(|p| p.name.clone());
                match page {
                    Some(page) => format!("{} · {}", voice.name, page),
                    None => voice.name.clone(),
                }
            } else {
                voice.name.clone()
            };
            g.set_seq(v.seq as i32);
            g.set_name(name.into());
            g.set_portrait(render::image(voice.sprite, voice.theme, voice.mood, 0));
            g.set_theme(render::colors(voice.theme));
            g.set_speaker(if remote { String::new() } else { voice_id(voice) }.into());
            g.set_listener(v.line.listener.as_ref().map_or(String::new(), voice_id).into());
        }
        g.set_text(v.typed.into());
        g.set_done(v.done);
        g.set_app(v.line.show_in.into());
    }

    /// Clique na caixa de diálogo: pula a fala.
    pub(super) fn chat_skip(&self) {
        self.chatter.borrow_mut().skip(Instant::now());
        self.chat_tick();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mascot::{Mood, sprites, themes};

    const VITALS: &str = "cyber.vitals";
    const DOCKER: &str = "cyber.docker";
    const AI: &str = "cyber.ai_usage";

    fn voice(app: &str, name: &str) -> Voice {
        Voice { app: app.into(), name: name.into(), sprite: sprites::get("neko"), theme: themes::get("neon"), mood: Mood::Idle }
    }

    fn fact(kind: FactKind, v: Voice, subject: &str, value: Option<f32>) -> Fact {
        Fact { kind, voice: v, subject: subject.into(), value }
    }

    /// Diretor com as falas de verdade dos apps.
    fn director(seed: u64) -> Director {
        let mut d = Director::with_seed(seed);
        d.add_lines(DOCKER, Lines::parse(include_str!("../../miniapps/docker/lines.toml")).unwrap());
        d.add_lines(VITALS, Lines::parse(include_str!("../../miniapps/vitals/lines.toml")).unwrap());
        d.add_lines(AI, Lines::parse(include_str!("../../miniapps/ai_usage/lines.toml")).unwrap());
        d
    }

    /// Roda a conversa por `secs` e devolve quem falou, na ordem.
    fn run(d: &mut Director, t: &mut Instant, voices: &[Voice], secs: u64) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        for _ in 0..secs * 10 {
            *t += Duration::from_millis(100);
            d.step(*t, Vec::new(), voices, true, Pace::Some);
            if let Some(v) = d.view(*t) {
                if names.last() != Some(&v.line.voice.name) {
                    names.push(v.line.voice.name.clone());
                }
            }
        }
        names
    }

    #[test]
    fn falas_genericas_e_dos_apps_estao_certas() {
        for (name, src) in [
            ("genéricas", GENERIC),
            ("docker", include_str!("../../miniapps/docker/lines.toml")),
            ("vitals", include_str!("../../miniapps/vitals/lines.toml")),
            ("ai_usage", include_str!("../../miniapps/ai_usage/lines.toml")),
        ] {
            let lines = Lines::parse(src).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(lines.problems(), Vec::<String>::new(), "{name}");
        }
        // as genéricas cobrem todos os casos, dos dois lados
        let g = Lines::parse(GENERIC).unwrap();
        for k in FactKind::ALL {
            assert!(g.own(k).is_some() && g.reply(k).is_some(), "genéricas sem {}", k.key());
        }
    }

    #[test]
    fn cutucada_vira_conversa_entre_apps() {
        let cpu = voice(VITALS, "CPU");
        let docker = voice(DOCKER, "Docker");
        let voices = [cpu.clone(), docker.clone()];
        let mut d = director(7);
        let mut t = Instant::now();
        d.step(t, vec![fact(FactKind::Poked, cpu.clone(), "", None)], &voices, true, Pace::Some);
        let first = d.view(t).expect("a primeira fala começa na hora");
        assert_eq!(first.line.voice.name, "CPU");
        assert_eq!(first.line.listener.as_ref().map(|l| l.name.as_str()), Some("Docker"));
        let mut names = vec!["CPU".to_string()];
        names.extend(run(&mut d, &mut t, &voices, 20).into_iter().filter(|n| n != "CPU"));
        assert_eq!(names, ["CPU", "Docker"]);
        assert!(d.view(t).is_none(), "a conversa termina");
    }

    #[test]
    fn nao_repete_a_mesma_fala_em_seguida() {
        let ctx = Ctx { value: None, has_b: true };
        let say = |n: usize| Say { pt: (0..n).map(|i| format!("fala {i}")).collect(), ..Default::default() };
        for seed in 1..10 {
            // quatro variantes: as quatro saem antes de qualquer repetição, e depois
            // nunca a mesma duas vezes seguidas (nem com só uma no meio)
            let mut d = Director::with_seed(seed);
            let four = say(4);
            let said: Vec<String> = (0..40).map(|_| d.pick_text(&four, Lang::Pt, ctx).unwrap()).collect();
            let mut first: Vec<&String> = said[..4].iter().collect();
            first.sort();
            first.dedup();
            assert_eq!(first.len(), 4, "{said:?}");
            assert!(said.windows(3).all(|w| w[0] != w[1] && w[0] != w[2]), "{said:?}");
            // duas variantes se alternam; uma só continua saindo
            let two = say(2);
            let said: Vec<String> = (0..10).map(|_| d.pick_text(&two, Lang::Pt, ctx).unwrap()).collect();
            assert!(said.windows(2).all(|w| w[0] != w[1]), "{said:?}");
            assert!(d.pick_text(&say(1), Lang::Pt, ctx).is_some());
            assert!(d.pick_text(&say(1), Lang::Pt, ctx).is_some());
        }
    }

    #[test]
    fn docker_assume_a_culpa_so_pela_cpu() {
        let cpu = voice(VITALS, "CPU");
        let disk = voice(VITALS, "DISCO");
        let voices = [cpu.clone(), disk.clone(), voice(DOCKER, "Docker"), voice(AI, "Claude")];
        for seed in 1..20 {
            let mut d = director(seed);
            let lines = d.compose(FactKind::Strain, &cpu, "CPU", Some(91.0), &voices);
            assert_eq!(lines.len(), 2);
            assert_eq!(lines[1].voice.app, DOCKER, "quem tem resposta própria responde");
            // o disco cheio não é culpa do Docker: responde qualquer um de outro app
            let lines = d.compose(FactKind::Strain, &disk, "DISCO", Some(91.0), &voices);
            assert_ne!(lines[1].voice.app, VITALS);
            assert!(lines.iter().all(|l| !l.text.contains('{')));
        }
    }

    #[test]
    fn falas_do_app_tem_preferencia_e_sem_numero_cai_na_que_nao_usa() {
        let claude = voice(AI, "Claude");
        let cpu = voice(VITALS, "CPU");
        for seed in 1..30 {
            let mut d = director(seed);
            // sozinho na página: só a primeira fala, sem número e sem `{b}`
            let lines = d.compose(FactKind::Trouble, &claude, "Claude", None, &[claude.clone()]);
            assert_eq!(lines.len(), 1);
            assert!(!lines[0].text.contains('%') && !lines[0].text.contains('{'), "{}", lines[0].text);
            // a resposta ao alarme da IA é a que o app de IA sugere (`trouble.answer`)
            let lines = d.compose(FactKind::Trouble, &claude, "Claude", Some(95.0), &[claude.clone(), cpu.clone()]);
            let ai = Lines::parse(include_str!("../../miniapps/ai_usage/lines.toml")).unwrap();
            let answer = ai.trouble.unwrap().answer.unwrap();
            let all: Vec<String> = Lang::ALL.iter().flat_map(|&l| answer.variants(l).iter().map(|t| t.replace("{a}", "Claude"))).collect();
            assert!(all.contains(&lines[1].text), "{}", lines[1].text);
        }
    }

    #[test]
    fn alarme_de_outra_pagina_aparece_no_app_de_quem_responde() {
        let web = voice(DOCKER, "Docker"); // o Docker está em outra página
        let cpu = voice(VITALS, "CPU");
        let voices = [cpu.clone()];
        let mut d = director(5);
        let t = Instant::now();
        d.step(t, vec![fact(FactKind::Trouble, web.clone(), "postgres", None)], &voices, true, Pace::Some);
        let v = d.view(t).expect("alarme de outra página entra");
        assert_eq!(v.line.voice.app, DOCKER);
        assert_eq!(v.line.show_in, VITALS, "a caixa aparece onde dá para ver");
        // cutucada ou agitação de outra página não interrompem
        let mut d = director(5);
        d.step(t, vec![fact(FactKind::Strain, web, "postgres", Some(80.0))], &voices, true, Pace::Some);
        assert!(d.view(t).is_none());
    }

    #[test]
    fn papo_a_toa_depois_de_um_tempo_calmo() {
        let voices = [voice(VITALS, "CPU"), voice(DOCKER, "Docker")];
        let mut d = director(11);
        let mut t = Instant::now();
        // começo calmo: nada antes do tempo mínimo
        assert!(run(&mut d, &mut t, &voices, 100).is_empty());
        // até o máximo do sorteio sai um papo, com as duas mascotes
        let names = run(&mut d, &mut t, &voices, 600);
        assert_eq!(names.len(), 2, "{names:?}");
        // desligado: nunca
        let mut d = director(11);
        for _ in 0..7000 {
            t += Duration::from_millis(100);
            d.step(t, Vec::new(), &voices, true, Pace::Off);
            assert!(d.view(t).is_none());
        }
    }

    #[test]
    fn nao_repete_assunto_nem_fala_escondido() {
        let cpu = voice(VITALS, "CPU");
        let voices = [cpu.clone(), voice(DOCKER, "Docker")];
        let trouble = || vec![fact(FactKind::Trouble, cpu.clone(), "CPU", Some(99.0))];
        let mut d = director(3);
        let mut t = Instant::now();
        // janela fechada: descarta
        d.step(t, trouble(), &voices, false, Pace::Some);
        assert!(d.view(t).is_none());
        d.step(t, trouble(), &voices, true, Pace::Some);
        assert!(d.view(t).is_some());
        // deixa a conversa inteira terminar
        run(&mut d, &mut t, &voices, 30);
        assert!(d.view(t).is_none());
        // o mesmo alarme logo depois não puxa outra conversa
        d.step(t, trouble(), &voices, true, Pace::Some);
        assert!(d.view(t).is_none());
        // cutucada de app fora da página também não
        d.step(t, vec![fact(FactKind::Poked, voice("cyber.cd", "CD"), "", None)], &voices, true, Pace::Some);
        assert!(d.view(t).is_none());
        // mas uma cutucada na página, sim
        d.step(t, vec![fact(FactKind::Poked, cpu.clone(), "", None)], &voices, true, Pace::Some);
        assert!(d.view(t).is_some());
    }
}
