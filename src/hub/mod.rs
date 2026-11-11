//! O hub: liga registro, provedores, janela e bandeja.
//!
//! Este arquivo guarda o estado (`Hub`), a montagem e os dois laços (dados e animação).
//! O resto do `impl Hub` está dividido por assunto nos módulos abaixo.

mod appearance;
mod bind;
mod chatter;
mod grid;
mod pages;
mod settings;
mod window;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Timer, TimerMode};

use self::grid::Drag;
use crate::core::bus::Bus;
use crate::core::config::Config;
use crate::core::layout;
use crate::core::miniapp::{AppRef, Host};
use crate::core::registry::Registry;
use crate::mascot::{Mood, sprites, themes};
use crate::tray::{Tray, TrayMedia};
use crate::ui::{Anim, HubWindow};

/// Depois do último tamanho novo durante um arrasto de borda, o foco ainda é ignorado este tempo.
const RESIZE_QUIET: Duration = Duration::from_millis(800);

/// Intervalo do quadro de animação (mascotes, costura e luz da borda). Com o mouse sobre a
/// janela, 8 quadros/s; fora dela, 4/s: cada quadro é desenhado na CPU.
const FRAME_HOT: Duration = Duration::from_millis(125);
const FRAME_COLD: Duration = Duration::from_millis(250);

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
    backdrop_gen: Rc<Cell<u32>>,
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
            backdrop_gen: Rc::new(Cell::new(0)),
            clock: RefCell::new(String::new()),
            settings_fingerprint: RefCell::new(String::new()),
            page: Cell::new(0),
            pages_fingerprint: RefCell::new(String::new()),
            chatter: RefCell::new(chatter::Director::default()),
            chat_shown: Cell::new(None),
        });
        hub.ui.set_max_columns(layout::MAX_COLS as i32);
        hub.ui.set_max_rows(layout::MAX_ROWS as i32);
        hub.apply_skin();
        hub.registry.start(&hub.ui);
        hub.open_tab(tab.as_deref().unwrap_or_default());
        hub.rebuild_lists();
        Self::bind(&hub);
        hub.load_lines();

        // bandeja: se não houver (GNOME sem AppIndicator), a janela fica sempre aberta
        match Tray::spawn(hub.ui.as_weak(), sprites::get("neko"), themes::get("neon"), Mood::Sleep) {
            Ok(t) => *hub.tray.borrow_mut() = Some(t),
            Err(e) => eprintln!("miniutils: sem bandeja do sistema ({e}); a janela fica aberta"),
        }

        // tamanho lembrado da última vez (lógico: o Slint converte pela escala da tela)
        if let Some((w, h)) = hub.host.config.borrow().window {
            hub.ui.window().set_size(slint::LogicalSize::new(w.max(360) as f32, h.max(280) as f32));
        }

        if !(start_hidden && hub.has_tray()) {
            hub.show(None);
        }
        Self::start_timers(&hub);
        Ok(hub)
    }

    fn start_timers(hub: &Rc<Self>) {
        // dados: esvazia o barramento e atualiza humor/bandeja
        let data = Timer::default();
        let w = Rc::downgrade(hub);
        data.start(TimerMode::Repeated, Duration::from_millis(100), move || {
            if let Some(h) = w.upgrade() {
                h.data_tick();
            }
        });
        // animação: só com a janela visível; o relógio fica no hub (`set_hovered` muda o ritmo)
        let w = Rc::downgrade(hub);
        hub.anim.start(TimerMode::Repeated, FRAME_COLD, move || {
            if let Some(h) = w.upgrade() {
                if h.shown.get() {
                    h.anim_tick();
                }
            }
        });
        // os timers vivem enquanto o event loop roda
        std::mem::forget(data);
    }

    fn data_tick(&self) {
        self.update_clock();
        let events = self.host.bus.drain();
        if !events.is_empty() {
            self.registry.dispatch(&events, &self.ui);
        }
        // resultado das ações em segundo plano (docker stop, comando do player…)
        let reports = self.host.drain_reports();
        if !reports.is_empty() {
            self.registry.deliver(&reports, &self.ui);
        }
        if self.host.settings_dirty.replace(false) {
            self.rebuild_settings();
        }
        // a barra lateral mostra o humor de cada página
        self.rebuild_pages(false);

        self.sync_tray();
        // conversa entre mascotes: fatos novos e a fala que está sendo "digitada"
        self.chat_tick();
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
            return;
        }
        if let Some(tray) = self.tray.borrow().as_ref() {
            // o ícone só é redesenhado quando a mascote, o tema ou o humor mudam
            let icon = (!same_icon).then(|| (sprites::get(&now.sprite), themes::get(now.theme), now.mood));
            tray.update(icon, now.tooltip.clone(), now.media.clone());
        }
        *last = Some((now, Instant::now()));
    }

    fn anim_tick(&self) {
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
    }
}
