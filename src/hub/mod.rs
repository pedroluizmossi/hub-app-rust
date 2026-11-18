//! The hub: ties together registry, providers, window and tray.
//!
//! This file holds the state (`Hub`), the setup and what drives the screen:
//! - `update`: runs when the bus or a background action rings the waker (`core::wake`).
//!   With nothing arriving, nothing runs.
//! - `anim_tick`: the animation frame, only with the window open.
//!
//! The rest of `impl Hub` is split by topic into the modules below.

mod appearance;
mod bind;
mod chatter;
mod data;
mod grid;
mod pages;
mod settings;
mod store;
mod usage;
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

/// After the last new size during an edge drag, focus is still ignored for this long.
const RESIZE_QUIET: Duration = Duration::from_millis(800);

/// Interval of the animation frame (mascots, stitching and edge light). With the mouse over
/// the window, 8 frames/s; outside it, 4/s: each frame is drawn on the CPU.
const FRAME_HOT: Duration = Duration::from_millis(125);
const FRAME_COLD: Duration = Duration::from_millis(250);
/// Starting with `--hidden` and no panel yet: checks for the tray this many times, every
/// `TRAY_WAIT_STEP`, before showing the window (~8 s).
const TRAY_WAIT_TRIES: u32 = 16;
const TRAY_WAIT_STEP: Duration = Duration::from_millis(500);
/// Fastest frame a drawn animation can ask for (30/s): only while it plays.
const FRAME_FASTEST: Duration = Duration::from_millis(33);
/// Animation frames refresh sidebar moods and tray at most this often (see `anim_tick`).
const DERIVED_EVERY: Duration = Duration::from_millis(500);

thread_local! {
    /// The hub of the UI thread, for whoever arrives from another thread through the event
    /// loop (waker, tray menu). `Send` closures cannot carry an `Rc`.
    static CURRENT: RefCell<Weak<Hub>> = const { RefCell::new(Weak::new()) };
}

/// Runs `f` with the hub, if it still exists (UI thread only).
fn with_hub(f: impl FnOnce(&Hub)) {
    if let Some(hub) = CURRENT.with(|c| c.borrow().upgrade()) {
        f(&hub);
    }
}

/// The hub window (UI thread only): for whoever receives messages from other threads.
pub fn ui_handle() -> Option<HubWindow> {
    CURRENT.with(|c| c.borrow().upgrade()).map(|h| h.ui.clone_strong())
}

/// The installed plugins changed, or the reload command (from another thread or
/// instance): reads apps and appearance again on the UI thread.
pub fn request_plugin_reload() {
    let _ = slint::invoke_from_event_loop(|| {
        with_hub(|h| {
            h.reload_plugins();
            h.reload_looks();
        })
    });
}

/// The panel that showed the tray icon went away (crashed, or the session has none now):
/// with the window hidden there would be no way back, so it shows. Called from the tray's
/// thread. Only the Linux tray (StatusNotifier) can tell; Windows keeps its icon itself.
#[cfg(target_os = "linux")]
pub fn tray_lost() {
    let _ = slint::invoke_from_event_loop(|| {
        with_hub(|h| {
            if !h.shown.get() {
                log::info!("the tray icon is gone: showing the window");
                h.show(None);
            }
        })
    });
}

/// Media command picked in the tray menu (called on the ksni thread): goes to the app that
/// gave the media information, on the UI thread.
pub fn tray_media_command(verb: &'static str) {
    let _ = slint::invoke_from_event_loop(move || with_hub(|h| h.tray_command(verb)));
}

pub struct Hub {
    host: Rc<Host>,
    registry: Registry,
    ui: HubWindow,
    tray: RefCell<Option<Tray>>,
    shown: Cell<bool>,
    /// The shown window was minimized or covered (see `set_covered`).
    covered: Cell<bool>,
    /// The window can be seen: shown, not minimized, not covered (see `sync_on_screen`).
    on_screen: Cell<bool>,
    /// The window can be positioned (X11/XWayland).
    positionable: bool,
    /// When the popup hid because it lost focus (see `toggle`).
    focus_hidden_at: Cell<Option<Instant>>,
    /// The popup did get focus since it opened (winit reports "unfocused" when mapping).
    had_focus: Cell<bool>,
    /// The window icon (side, RGBA), kept to set it again when the native window is
    /// created (it does not exist before the first `show`).
    window_icon: RefCell<Option<(i32, Vec<u8>)>>,
    /// Pin mode: losing focus does not hide the popup.
    pinned: Cell<bool>,
    /// Last tray point where the popup opened (to reopen in the same place).
    last_anchor: Cell<Option<(i32, i32)>>,
    /// Until when to ignore the loss of focus because of a resize by the edge.
    resize_until: Cell<Option<Instant>>,
    frame: Cell<u32>,
    /// Generation of the fade in progress: a new fade cancels the previous one.
    fade_gen: Rc<Cell<u32>>,
    /// What the tray shows and when it got the last update (see `sync_tray`).
    last_tray: RefCell<Option<(TrayState, Instant)>>,
    /// The mouse is over the window (winit's `CursorEntered`/`CursorLeft`).
    hovered: Cell<bool>,
    /// Clock of the animation frame; the interval follows `hovered` (see `FRAME_HOT`) and
    /// speeds up while a drawn animation asks for short frames (see `pace_frames`).
    anim: Timer,
    /// When `refresh_derived` last ran (the animation frame runs it at most every `DERIVED_EVERY`).
    derived_at: Cell<Option<Instant>>,
    /// Shortest frame asked for by the animations in the last frame (ms).
    frame_wanted: Cell<Option<u32>>,
    /// Drag in progress in edit mode.
    drag: RefCell<Option<Drag>>,
    /// (Logical) size of the panel area, to draw the backdrop; and the generation of the
    /// drawing request (several requests in a row during a resize become one).
    backdrop_size: Cell<(f32, f32)>,
    backdrop: std::sync::Arc<appearance::BackdropState>,
    /// Minute shown on the clock ("21:57").
    clock: RefCell<String>,
    /// CPU and memory of the hub and its external apps (Settings › System, `usage.rs`).
    usage: RefCell<usage::UsageState>,
    /// What the Settings list shows now (see `rebuild_settings`).
    settings_fingerprint: RefCell<String>,
    /// Page open on the panel (index in `config.pages`).
    page: Cell<usize>,
    /// What the sidebar shows now (see `rebuild_pages`).
    pages_fingerprint: RefCell<String>,
    /// Conversation between mascots (see `chatter.rs`) and what the dialog box shows now.
    chatter: RefCell<chatter::Director>,
    chat_shown: Cell<Option<(u64, usize, bool)>>,
    /// Frames of the line being "typed": runs only while a conversation is on.
    chat_timer: Timer,
    /// Postponed tooltip (see `TOOLTIP_EVERY`): sends the text when the time is up.
    tray_later: Timer,
    /// Plugin store screen (see `store.rs`).
    store: RefCell<store::StoreState>,
}

/// How often the tray tooltip follows numbers that only change value ("CPU 37%").
/// Mood, mascot and the playing track change at once.
const TOOLTIP_EVERY: Duration = Duration::from_secs(15);

/// What the tray shows now, so it is only talked to when something changes.
#[derive(Clone, PartialEq)]
struct TrayState {
    mood: Mood,
    sprite: String,
    theme: &'static str,
    media: Option<TrayMedia>,
    tooltip: String,
}

impl TrayState {
    /// The icon is the same (it is only redrawn when the mascot, the theme or the mood change).
    fn same_icon(&self, other: &TrayState) -> bool {
        (self.mood, &self.sprite, self.theme) == (other.mood, &other.sprite, other.theme)
    }
}

impl Hub {
    pub fn new(start_hidden: bool, tab: Option<String>, positionable: bool) -> anyhow::Result<Rc<Self>> {
        let mut bus = Bus::default();
        crate::providers::spawn_all(&mut bus);
        let config = Config::load();
        // before the apps parse their manifests, which are translated on load
        crate::core::i18n::set(crate::core::i18n::Language::from_setting(&config.language));
        // after the language is set: the notice is translated
        let notice = config.load_notice();
        let host = Rc::new(Host::new(config, bus));
        if let Some(body) = notice {
            host.notifier.borrow_mut().send("config:load", &crate::core::config::display_name(), body, true);
        }
        let ui = HubWindow::new()?;
        crate::core::i18n::apply_to_ui();

        let registry = Registry::new(host.clone());
        registry.add(crate::apps::docker::DockerMini::create(host.clone(), &ui) as AppRef, None);
        registry.add(crate::apps::vitals::VitalsMini::create(host.clone(), &ui) as AppRef, None);
        registry.add(crate::apps::cdplayer::CdPlayerMini::create(host.clone(), &ui) as AppRef, None);
        registry.add(crate::apps::ai_usage::AiUsageMini::create(host.clone(), &ui) as AppRef, None);
        registry.add(crate::apps::ports::PortsMini::create(host.clone(), &ui) as AppRef, None);
        registry.add(crate::apps::todo::TodoMini::create(host.clone(), &ui) as AppRef, None);
        registry.add(crate::apps::terminal::TerminalMini::create(host.clone(), &ui) as AppRef, None);
        registry.add(crate::apps::mascots::MascotsMini::create(host.clone(), &ui) as AppRef, None);
        registry.add(crate::apps::agents::AgentsMini::create(host.clone(), &ui) as AppRef, None);
        // installed community plugins (see `store.rs`)
        crate::apps::plugin::sync(&host, &ui, &registry, true);

        let hub = Rc::new(Hub {
            host,
            registry,
            ui,
            tray: RefCell::new(None),
            shown: Cell::new(false),
            covered: Cell::new(false),
            on_screen: Cell::new(false),
            positionable,
            focus_hidden_at: Cell::new(None),
            had_focus: Cell::new(false),
            window_icon: RefCell::new(None),
            pinned: Cell::new(false),
            last_anchor: Cell::new(None),
            resize_until: Cell::new(None),
            frame: Cell::new(1),
            fade_gen: Rc::new(Cell::new(0)),
            last_tray: RefCell::new(None),
            hovered: Cell::new(false),
            anim: Timer::default(),
            derived_at: Cell::new(None),
            frame_wanted: Cell::new(None),
            drag: RefCell::new(None),
            backdrop_size: Cell::new((0.0, 0.0)),
            backdrop: Default::default(),
            clock: RefCell::new(String::new()),
            usage: Default::default(),
            settings_fingerprint: RefCell::new(String::new()),
            page: Cell::new(0),
            pages_fingerprint: RefCell::new(String::new()),
            chatter: RefCell::new(chatter::Director::default()),
            chat_shown: Cell::new(None),
            chat_timer: Timer::default(),
            tray_later: Timer::default(),
            store: Default::default(),
        });
        hub.ui.set_max_columns(layout::MAX_COLS as i32);
        hub.ui.set_max_rows(layout::MAX_ROWS as i32);
        // file themes, skins and backdrops before the first drawing
        hub.load_looks();
        hub.apply_skin();
        hub.apply_system();
        hub.registry.start(&hub.ui);
        hub.open_tab(tab.as_deref().unwrap_or_default());
        hub.rebuild_lists();
        // the screen callbacks reach the hub through `with_hub`
        CURRENT.with(|c| *c.borrow_mut() = Rc::downgrade(&hub));
        Self::bind(&hub);
        Self::bind_store(&hub);
        hub.load_lines();

        // tray: while there is no panel (starting up, or GNOME without AppIndicator), the
        // window stays open; the icon shows up by itself when the panel appears
        match Tray::spawn(hub.ui.as_weak(), sprites::get("neko"), themes::get("neon"), Mood::Sleep) {
            Ok(t) => *hub.tray.borrow_mut() = Some(t),
            Err(e) => log::warn!("no system tray ({e}); the window stays open"),
        }

        // size remembered from last time (logical: Slint converts by the screen scale)
        if let Some((w, h)) = hub.host.config.borrow().window {
            hub.ui.window().set_size(slint::LogicalSize::new(w.max(360) as f32, h.max(280) as f32));
        }

        Self::start_loops(&hub);
        if !start_hidden || hub.tray.borrow().is_none() {
            hub.show(None);
        } else if hub.has_tray() {
            log::info!("starting hidden in the tray");
        } else {
            // at login the panel often comes up after the autostart: wait for it a little
            // before deciding the window has to show
            log::info!("starting hidden: waiting for the panel to show the tray icon");
            window::wait_for_tray(TRAY_WAIT_TRIES);
        }
        Ok(hub)
    }

    fn start_loops(hub: &Rc<Self>) {
        // data: each reading or report rings the waker, which schedules `update` on the UI
        // thread (several rings in a row become one update)
        hub.host.bus.waker().install(|| {
            let _ = slint::invoke_from_event_loop(|| with_hub(Hub::update));
        });
        // what arrived before installing it (the setup takes a while)
        hub.update();
        // animation: only with the window on screen (`sync_on_screen` turns it on and off; `set_hovered` changes the pace)
        hub.anim.start(TimerMode::Repeated, FRAME_COLD, || with_hub(Hub::anim_tick));
        if !hub.on_screen.get() {
            hub.anim.stop();
        }
    }

    /// Readings from the providers or reports from the actions arrived: hands them to the apps
    /// and updates what depends on them.
    fn update(&self) {
        // before draining: whatever arrives from now on rings again
        self.host.bus.waker().take();
        let events = self.host.bus.drain();
        if !events.is_empty() {
            self.registry.dispatch(&events, &self.ui);
        }
        // result of the background actions (docker stop, player command…)
        let reports = self.host.drain_reports();
        if !reports.is_empty() {
            self.registry.deliver(&reports, &self.ui);
        }
        self.refresh_derived();
    }

    /// Updates what depends on the apps' state: Settings form, page moods, tray and
    /// conversation. Cheap when nothing changed (each part compares first).
    fn refresh_derived(&self) {
        self.derived_at.set(Some(Instant::now()));
        if self.host.settings_dirty.replace(false) {
            self.rebuild_settings();
        }
        // the sidebar shows the mood of each page
        self.rebuild_pages(false);
        self.sync_tray();
        // conversation between mascots: new facts and the line being "typed"
        self.chat_tick();
    }

    /// Reads the plugins folder again (installed, updated, removed, or the Reload button of
    /// the settings): new apps come in, changed ones reload, removed ones leave.
    fn reload_plugins(&self) {
        if crate::apps::plugin::sync(&self.host, &self.ui, &self.registry, false) {
            self.rebuild_lists();
            self.rebuild_grid();
            self.rebuild_pages(true);
            self.refresh_derived();
        }
        // count and refused folders, even without a new app (e.g. renamed the wrong folder)
        self.apply_apps_dir();
    }

    /// Media command from the tray menu: goes to the app that gave the media information.
    fn tray_command(&self, verb: &str) {
        let owner = self.registry.enabled().into_iter().find(|a| a.borrow().tray_media().is_some());
        if let Some(app) = owner {
            app.borrow_mut().tray_command(verb, &self.ui);
        }
        self.refresh_derived();
    }

    /// The tray shows the mascot (and theme) of the app in the worst state, or the one chosen in the config.
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
            if sprites::find(&cfg.tray_icon).is_some() {
                sprite = cfg.tray_icon.clone();
            }
            if let Some(t) = themes::find(&cfg.tray_theme) {
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

    /// Takes the state to the tray only when it changes. Talking to it is costly: it is a
    /// blocking D-Bus call on the UI thread, and the panel rereads the item on every signal.
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
                // only the text changed ("CPU 37%" -> "CPU 41%", on every reading): it can wait
                let urgent = !same_icon || old.media != now.media || sent.elapsed() >= TOOLTIP_EVERY;
                (same_icon, urgent)
            }
        };
        if !urgent {
            // only the text changed: send it when the time is up, even if nothing else arrives
            if !self.tray_later.running() {
                let left = last.as_ref().map_or(Duration::ZERO, |(_, sent)| TOOLTIP_EVERY.saturating_sub(sent.elapsed()));
                self.tray_later.start(TimerMode::SingleShot, left, || with_hub(Hub::sync_tray));
            }
            return;
        }
        self.tray_later.stop();
        if !same_icon {
            // the window icon follows the tray's (the taskbar shows it in desktop app mode)
            let (sprite, theme) = (sprites::get(&now.sprite), themes::get(now.theme));
            let icon = crate::mascot::render::rgba_square(sprite, theme, now.mood, 1, 4);
            crate::popup::set_icon(self.ui.window(), &icon);
            *self.window_icon.borrow_mut() = Some(icon);
        }
        if let Some(tray) = self.tray.borrow().as_ref() {
            // the icon is only redrawn when the mascot, the theme or the mood change
            let icon = (!same_icon).then(|| (sprites::get(&now.sprite), themes::get(now.theme), now.mood));
            tray.update(icon, now.tooltip.clone(), now.media.clone());
        }
        *last = Some((now, Instant::now()));
    }

    /// One frame: mascots, clock and what a click on a screen may have changed (the apps
    /// handle their own clicks; the hub checks mood, tray and conversation here).
    fn anim_tick(&self) {
        if !self.shown.get() {
            return;
        }
        // minimized by the window manager's button or the taskbar: no event says so on every
        // system, so the frame checks; restoring brings focus, which turns it back on
        if crate::popup::is_minimized(self.ui.window()) {
            self.sync_on_screen();
            return;
        }
        self.update_clock();
        let f = self.frame.get().wrapping_add(1) % 600;
        self.frame.set(f);
        self.ui.global::<Anim>().set_frame(f as i32);
        // clock of the animated backdrop (ms; back to zero every hour, a jump barely seen)
        static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        let ms = START.get_or_init(Instant::now).elapsed().as_millis() % 3_600_000;
        self.ui.global::<Anim>().set_time(ms as i32);
        // on the panel, only the apps of the open page are visible and animate
        if self.ui.get_current() == "panel" {
            let ids = self.host.config.borrow().page_apps(self.current_page());
            for app in ids.iter().filter_map(|id| self.registry.find(id)) {
                app.borrow_mut().tick(f, &self.ui);
            }
        } else if self.ui.get_current() == "apps" {
            // Settings › System: the hub's own CPU and memory
            self.refresh_usage(false);
        }
        self.frame_wanted.set(crate::mascot::anim::take_requested_frame_ms());
        self.pace_frames();
        // the sidebar moods and the tray only change with a reading (which runs `update`) or
        // a click inside an app: a few times a second is enough to catch the clicks, instead
        // of rebuilding their fingerprints on every frame
        if self.derived_at.get().is_some_and(|t| t.elapsed() < DERIVED_EVERY) {
            if self.host.settings_dirty.replace(false) {
                self.rebuild_settings();
            }
            self.chat_tick();
        } else {
            self.refresh_derived();
        }
    }

    /// Frame pace: 8/s with the mouse on the window, 4/s outside, and faster (up to 30/s)
    /// only while an animation drawn on the screen has shorter frames than that.
    fn pace_frames(&self) {
        let base = if self.hovered.get() { FRAME_HOT } else { FRAME_COLD };
        let interval = match self.frame_wanted.get() {
            Some(ms) => base.min(Duration::from_millis(ms as u64).max(FRAME_FASTEST)),
            None => base,
        };
        if self.anim.interval() != interval {
            self.anim.set_interval(interval);
        }
    }
}
