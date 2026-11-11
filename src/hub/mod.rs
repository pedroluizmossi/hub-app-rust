//! O hub: liga registro, provedores, janela e bandeja.
//!
//! Este arquivo guarda o estado (`Hub`), a montagem e o que move a tela:
//! - `update`: roda quando o barramento ou uma ação em segundo plano tocam o despertador
//!   (`core::wake`). Sem nada chegando, nada roda.
//! - `anim_tick`: o quadro de animação, só com a janela aberta.
//! O resto do `impl Hub` está dividido por assunto nos módulos abaixo.

mod appearance;
mod bind;
mod chatter;
mod grid;
mod pages;
mod settings;
mod window;

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Timer, TimerMode};

use self::grid::Drag;
use crate::core::bus::Bus;
use crate::core::config::Config;
use crate::core::layout;
use crate::core::miniapp::{AppRef, Host, TrayMedia};
use crate::core::registry::Registry;
use crate::mascot::{Mood, sprites, themes};
use crate::tray::Tray;
use crate::ui::{Anim, HubWindow};

/// Depois do último tamanho novo durante um arrasto de borda, o foco ainda é ignorado este tempo.
const RESIZE_QUIET: Duration = Duration::from_millis(800);

/// Intervalo do quadro de animação (mascotes, costura e luz da borda). Com o mouse sobre a
/// janela, 8 quadros/s; fora dela, 4/s: cada quadro é desenhado na CPU.
const FRAME_HOT: Duration = Duration::from_millis(125);
const FRAME_COLD: Duration = Duration::from_millis(250);

thread_local! {
    /// O hub da thread da UI, para quem chega de outra thread pelo event loop
    /// (despertador, menu da bandeja). Closures `Send` não podem levar um `Rc`.
    static CURRENT: RefCell<Weak<Hub>> = const { RefCell::new(Weak::new()) };
}

/// Roda `f` com o hub, se ele ainda existe (só na thread da UI).
fn with_hub(f: impl FnOnce(&Hub)) {
    if let Some(hub) = CURRENT.with(|c| c.borrow().upgrade()) {
        f(&hub);
    }
}

/// Comando de mídia escolhido no menu da bandeja (chamado na thread do ksni): vai para o
/// app que deu as informações de mídia, na thread da UI.
pub fn tray_media_command(verb: &'static str) {
    let _ = slint::invoke_from_event_loop(move || with_hub(|h| h.tray_command(verb)));
}

pub struct Hub {
    host: Rc<Host>,
    registry: Registry,
    ui: HubWindow,
    tray: RefCell<Option<Tray>>,
    shown: Cell<bool>,
    /// Dá para posicionar a janela (X11/XWayland).
    positionable: bool,
    /// Quando o popup se escondeu por perder o foco (ver `toggle`).
    focus_hidden_at: Cell<Option<Instant>>,
    /// O popup chegou a ganhar foco desde que abriu (o winit avisa "sem foco" ao mapear).
    had_focus: Cell<bool>,
    /// Modo fixar: perder o foco não esconde o popup.
    pinned: Cell<bool>,
    /// Último ponto da bandeja onde o popup abriu (para reabrir no mesmo lugar).
    last_anchor: Cell<Option<(i32, i32)>>,
    /// Até quando ignorar a perda de foco por causa de um redimensionamento pela borda.
    resize_until: Cell<Option<Instant>>,
    frame: Cell<u32>,
    /// Geração do fade em curso: um fade novo cancela o anterior.
    fade_gen: Rc<Cell<u32>>,
    /// O que a bandeja mostra e quando recebeu a última atualização (ver `sync_tray`).
    last_tray: RefCell<Option<(TrayState, Instant)>>,
    /// O mouse está sobre a janela (`CursorEntered`/`CursorLeft` do winit).
    hovered: Cell<bool>,
    /// Relógio do quadro de animação; o intervalo segue `hovered` (ver `FRAME_HOT`).
    anim: Timer,
    /// Arrasto em curso no modo de edição.
    drag: RefCell<Option<Drag>>,
    /// Tamanho (lógico) da área do painel, para desenhar o fundo; e a geração do pedido
    /// de desenho (vários pedidos seguidos num redimensionamento viram um só).
    backdrop_size: Cell<(f32, f32)>,
    backdrop: std::sync::Arc<appearance::BackdropState>,
    /// Minuto mostrado no relógio ("21:57").
    clock: RefCell<String>,
    /// O que a lista das Configurações mostra agora (ver `rebuild_settings`).
    settings_fingerprint: RefCell<String>,
    /// Página aberta no painel (índice em `config.pages`).
    page: Cell<usize>,
    /// O que a barra lateral mostra agora (ver `rebuild_pages`).
    pages_fingerprint: RefCell<String>,
    /// Conversa entre mascotes (ver `chatter.rs`) e o que a caixa de diálogo mostra agora.
    chatter: RefCell<chatter::Director>,
    chat_shown: Cell<Option<(u64, usize, bool)>>,
    /// Quadros da fala sendo "digitada": só corre enquanto há conversa no ar.
    chat_timer: Timer,
    /// Tooltip adiado (ver `TOOLTIP_EVERY`): manda o texto quando o prazo vence.
    tray_later: Timer,
}

/// De quanto em quanto tempo o tooltip da bandeja acompanha números que só mudam de valor
/// ("CPU 37%"). Humor, mascote e faixa tocando mudam na hora.
const TOOLTIP_EVERY: Duration = Duration::from_secs(15);

/// O que a bandeja mostra agora, para só falar com ela quando algo muda.
#[derive(Clone, PartialEq)]
struct TrayState {
    mood: Mood,
    sprite: String,
    theme: &'static str,
    media: Option<TrayMedia>,
    tooltip: String,
}

impl TrayState {
    /// O ícone é o mesmo (só é redesenhado quando a mascote, o tema ou o humor mudam).
    fn same_icon(&self, other: &TrayState) -> bool {
        (self.mood, &self.sprite, self.theme) == (other.mood, &other.sprite, other.theme)
    }
}

impl Hub {
    pub fn new(start_hidden: bool, tab: Option<String>, positionable: bool) -> anyhow::Result<Rc<Self>> {
        let mut bus = Bus::default();
        crate::providers::spawn_all(&mut bus);
        let host = Rc::new(Host::new(Config::load(), bus));
        let ui = HubWindow::new()?;

        let mut registry = Registry::new(host.clone());
        registry.add(crate::apps::docker::DockerMini::create(host.clone(), &ui) as AppRef);
        registry.add(crate::apps::vitals::VitalsMini::create(host.clone(), &ui) as AppRef);
        registry.add(crate::apps::cdplayer::CdPlayerMini::create(host.clone(), &ui) as AppRef);
        registry.add(crate::apps::ai_usage::AiUsageMini::create(host.clone(), &ui) as AppRef);
        registry.add(crate::apps::ports::PortsMini::create(host.clone(), &ui) as AppRef);
        registry.add(crate::apps::todo::TodoMini::create(host.clone(), &ui) as AppRef);
        registry.add(crate::apps::agents::AgentsMini::create(host.clone(), &ui) as AppRef);

        let hub = Rc::new(Hub {
            host,
            registry,
            ui,
            tray: RefCell::new(None),
            shown: Cell::new(false),
            positionable,
            focus_hidden_at: Cell::new(None),
            had_focus: Cell::new(false),
            pinned: Cell::new(false),
            last_anchor: Cell::new(None),
            resize_until: Cell::new(None),
            frame: Cell::new(1),
            fade_gen: Rc::new(Cell::new(0)),
            last_tray: RefCell::new(None),
            hovered: Cell::new(false),
            anim: Timer::default(),
            drag: RefCell::new(None),
            backdrop_size: Cell::new((0.0, 0.0)),
            backdrop: Default::default(),
            clock: RefCell::new(String::new()),
            settings_fingerprint: RefCell::new(String::new()),
            page: Cell::new(0),
            pages_fingerprint: RefCell::new(String::new()),
            chatter: RefCell::new(chatter::Director::default()),
            chat_shown: Cell::new(None),
            chat_timer: Timer::default(),
            tray_later: Timer::default(),
        });
        hub.ui.set_max_columns(layout::MAX_COLS as i32);
        hub.ui.set_max_rows(layout::MAX_ROWS as i32);
        hub.apply_skin();
        hub.registry.start(&hub.ui);
        hub.open_tab(tab.as_deref().unwrap_or_default());
        hub.rebuild_lists();
        Self::bind(&hub);
        hub.load_lines();

        // bandeja: enquanto não houver painel (subindo, ou GNOME sem AppIndicator), a
        // janela fica aberta; o ícone aparece sozinho quando o painel surgir
        match Tray::spawn(hub.ui.as_weak(), sprites::get("neko"), themes::get("neon"), Mood::Sleep) {
            Ok(t) => *hub.tray.borrow_mut() = Some(t),
            Err(e) => eprintln!("miniutils: sem bandeja do sistema ({e}); a janela fica aberta"),
        }

        // tamanho lembrado da última vez (lógico: o Slint converte pela escala da tela)
        if let Some((w, h)) = hub.host.config.borrow().window {
            hub.ui.window().set_size(slint::LogicalSize::new(w.max(360) as f32, h.max(280) as f32));
        }

        Self::start_loops(&hub);
        if !(start_hidden && hub.has_tray()) {
            hub.show(None);
        }
        Ok(hub)
    }

    fn start_loops(hub: &Rc<Self>) {
        CURRENT.with(|c| *c.borrow_mut() = Rc::downgrade(hub));
        // dados: cada leitura ou recado toca o despertador, que agenda `update` na thread
        // da UI (vários toques seguidos viram uma atualização)
        hub.host.bus.waker().install(|| {
            let _ = slint::invoke_from_event_loop(|| with_hub(Hub::update));
        });
        // o que chegou antes de instalar (a montagem leva um tempo)
        hub.update();
        // animação: só com a janela visível (`set_shown` liga e desliga; `set_hovered` muda o ritmo)
        let w = Rc::downgrade(hub);
        hub.anim.start(TimerMode::Repeated, FRAME_COLD, move || {
            if let Some(h) = w.upgrade() {
                h.anim_tick();
            }
        });
        if !hub.shown.get() {
            hub.anim.stop();
        }
    }

    /// Chegaram leituras dos provedores ou recados das ações: entrega aos apps e atualiza
    /// o que depende deles.
    fn update(&self) {
        // antes de esvaziar: o que chegar daqui em diante toca de novo
        self.host.bus.waker().take();
        let events = self.host.bus.drain();
        if !events.is_empty() {
            self.registry.dispatch(&events, &self.ui);
        }
        // resultado das ações em segundo plano (docker stop, comando do player…)
        let reports = self.host.drain_reports();
        if !reports.is_empty() {
            self.registry.deliver(&reports, &self.ui);
        }
        self.refresh_derived();
    }

    /// Atualiza o que depende do estado dos apps: formulário das Configurações, humor das
    /// páginas, bandeja e conversa. Barato quando nada mudou (cada parte compara antes).
    fn refresh_derived(&self) {
        if self.host.settings_dirty.replace(false) {
            self.rebuild_settings();
        }
        // a barra lateral mostra o humor de cada página
        self.rebuild_pages(false);
        self.sync_tray();
        // conversa entre mascotes: fatos novos e a fala que está sendo "digitada"
        self.chat_tick();
    }

    /// Comando de mídia do menu da bandeja: vai ao app que deu as informações de mídia.
    fn tray_command(&self, verb: &str) {
        let owner = self.registry.enabled().into_iter().find(|a| a.borrow().tray_media().is_some());
        if let Some(app) = owner {
            app.borrow_mut().tray_command(verb, &self.ui);
        }
        self.refresh_derived();
    }

    /// A bandeja mostra a mascote (e o tema) do app em pior estado, ou a escolhida na config.
    fn tray_state(&self) -> TrayState {
        let enabled = self.registry.enabled();
        let worst = Registry::worst_of(&enabled);
        let (mut sprite, mut theme) = match &worst {
            Some(app) => {
                let a = app.borrow();
                (a.mascot_key(), self.host.theme_for(a.manifest()))
            }
            None => ("neko".to_string(), themes::get("neon")),
        };
        {
            let cfg = self.host.config.borrow();
            if sprites::SPRITES.iter().any(|s| s.key == cfg.tray_icon) {
                sprite = cfg.tray_icon.clone();
            }
            if let Some(t) = themes::THEMES.iter().find(|t| t.key == cfg.tray_theme) {
                theme = t;
            }
        }
        TrayState {
            mood: Registry::mood_of(&enabled),
            sprite,
            theme: theme.key,
            media: enabled.iter().find_map(|a| a.borrow().tray_media()),
            tooltip: Registry::summary_of(&enabled),
        }
    }

    /// Leva o estado à bandeja só quando ele muda. Falar com ela custa: é uma chamada
    /// D-Bus bloqueante na thread da UI, e o painel relê o item a cada sinal.
    fn sync_tray(&self) {
        let now = self.tray_state();
        let mut last = self.last_tray.borrow_mut();
        let (same_icon, urgent) = match last.as_ref() {
            None => (false, true),
            Some((old, sent)) => {
                if *old == now {
                    return;
                }
                let same_icon = old.same_icon(&now);
                // só o texto mudou ("CPU 37%" -> "CPU 41%", a cada leitura): pode esperar
                let urgent = !same_icon || old.media != now.media || sent.elapsed() >= TOOLTIP_EVERY;
                (same_icon, urgent)
            }
        };
        if !urgent {
            // só o texto mudou: manda quando o prazo vencer, mesmo que nada mais chegue
            if !self.tray_later.running() {
                let left = last.as_ref().map_or(Duration::ZERO, |(_, sent)| TOOLTIP_EVERY.saturating_sub(sent.elapsed()));
                self.tray_later.start(TimerMode::SingleShot, left, || with_hub(Hub::sync_tray));
            }
            return;
        }
        self.tray_later.stop();
        if let Some(tray) = self.tray.borrow().as_ref() {
            // o ícone só é redesenhado quando a mascote, o tema ou o humor mudam
            let icon = (!same_icon).then(|| (sprites::get(&now.sprite), themes::get(now.theme), now.mood));
            tray.update(icon, now.tooltip.clone(), now.media.clone());
        }
        *last = Some((now, Instant::now()));
    }

    /// Um quadro: mascotes, relógio e o que um clique numa tela pode ter mudado (os
    /// apps tratam os próprios cliques; o hub confere humor, bandeja e conversa aqui).
    fn anim_tick(&self) {
        if !self.shown.get() {
            return;
        }
        self.update_clock();
        let f = self.frame.get().wrapping_add(1) % 600;
        self.frame.set(f);
        self.ui.global::<Anim>().set_frame(f as i32);
        // no painel, só os apps da página aberta estão visíveis e animam
        if self.ui.get_current() == "panel" {
            let ids = self.host.config.borrow().page_apps(self.current_page());
            for app in ids.iter().filter_map(|id| self.registry.find(id).cloned()) {
                app.borrow_mut().tick(f, &self.ui);
            }
        }
        self.refresh_derived();
    }
}
