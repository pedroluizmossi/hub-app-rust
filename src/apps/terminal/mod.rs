//! Terminal mini-app: the user's shells in a window of their own, one per tab, drawn in
//! the app theme.
//!
//! The card (`miniapps/terminal/view.slint`) says whether a shell is running and shows the
//! last lines of the tab in front; "Open" shows the window (`window.slint`). Each shell
//! runs in a pseudo-terminal (`session.rs`) and keeps running while the window is closed
//! (setting `keep_running`); the emulator's screen becomes runs of text for the window
//! (`grid.rs`). The window's effects (skin backdrop, CRT, rain) are drawn once per size
//! and skin (`fx.rs`), and its mascot follows the shell. With the OpenGL renderer, the
//! Library's shaders in the `shaders` setting run over the screen (`library::postfx`).

mod fx;
mod grid;
mod session;

use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Model, ModelRc, Timer, TimerMode, VecModel};

use crate::core::bus::Event;
use crate::core::manifest::Manifest;
use crate::core::miniapp::{Host, MiniApp};
use crate::mascot::anim::Reaction;
use crate::library::postfx::{Colors, Frame, PostFx, Source};
use crate::mascot::{Mood, render, sprites};
use crate::tr;
use crate::ui::{AmbientItem, Anim, HubWindow, TermLine, TermRun, TermTab, TerminalApp, TerminalScreen, TerminalWindow};
use grid::{Mods, Palette, Run};
use session::{Launch, Session};
use super::kit::{Badge, HeaderMascot};

/// Lines of the screen shown on the card.
const PREVIEW_LINES: usize = 4;
/// Most tabs at once (each one is a shell and a thread reading it).
const MAX_TABS: usize = 12;
/// Animation frame of the window (rain, CRT band, mascot), like the hub's with the mouse
/// over it. It stops while the window has no focus.
const FRAME: Duration = Duration::from_millis(125);
/// The window mascot works while output arrived this recently…
const BUSY_FOR: Duration = Duration::from_millis(1500);
/// …and dozes off after this long without any.
const SLEEP_AFTER: Duration = Duration::from_secs(10 * 60);
/// How often the open window looks for a saved shader file.
const SHADER_CHECK: Duration = Duration::from_secs(1);

thread_local! {
    /// The app, for the reading threads' wake-ups (they arrive on the UI thread with no
    /// other way back to it).
    static APP: RefCell<Weak<RefCell<TerminalMini>>> = const { RefCell::new(Weak::new()) };
}

/// One tab: its shell, or why it did not start.
struct Tab {
    session: Option<Session>,
    /// Why the start failed ("" = it didn't).
    error: String,
    /// The shell's exit was already handled.
    exit_seen: bool,
}

impl Tab {
    fn running(&self) -> bool {
        self.session.as_ref().is_some_and(|s| s.exited().is_none())
    }

    fn title(&self) -> String {
        match &self.session {
            None => tr!("Terminal").into(),
            Some(s) => {
                let title = s.parser().callbacks().title.clone().filter(|t| !t.trim().is_empty());
                title.unwrap_or_else(|| s.launch.name())
            }
        }
    }
}

pub struct TerminalMini {
    host: Rc<Host>,
    manifest: Manifest,
    ui: slint::Weak<HubWindow>,
    tabs: Vec<Tab>,
    /// The tab in front.
    active: usize,
    /// Created on first use; closing only hides it.
    win: Option<TerminalWindow>,
    open: bool,
    lines: Rc<VecModel<TermLine>>,
    tab_rows: Rc<VecModel<TermTab>>,
    /// The runs on screen, line by line (only lines that change are replaced).
    shown: Vec<Vec<Run>>,
    palette: Palette,
    /// Mouse selection: where the drag started, and the selected cells.
    anchor: Option<(u16, u16)>,
    selection: Option<grid::Selection>,
    /// A wake-up is already on its way to the UI thread.
    pending: Arc<AtomicBool>,
    mood: Mood,
    face: HeaderMascot,
    /// The mascot in the window (its own: it follows the tab in front, not the card).
    win_mood: Mood,
    win_face: HeaderMascot,
    /// Animation clock of the window, running while it is open.
    clock: Timer,
    frame: u32,
    /// The window has the focus (without it, nothing animates).
    focused: bool,
    fx: Fx,
    /// The window's GPU shaders (`None` with the software renderer, or before the window).
    shaders: Option<PostFx>,
    /// The shader folder as last read (`library::shaders::current_stamp`): a saved file
    /// is compiled again.
    shader_stamp: u64,
    /// When the shader folder was last looked at.
    shaders_checked: Instant,
}

/// What the effects were last drawn for, and the drawing on its way.
#[derive(Default)]
struct Fx {
    /// Bumped on each request: a drawing that arrives late for an older one is dropped.
    generation: Arc<AtomicU32>,
    /// Skin, area (physical px) and which images: nothing changed = nothing to draw.
    drawn: Option<(crate::skin::Skin, u32, u32, bool, bool)>,
    /// Waits for the resize to settle before drawing.
    debounce: Timer,
}

impl TerminalMini {
    pub fn create(host: Rc<Host>, ui: &HubWindow) -> Rc<RefCell<Self>> {
        let manifest =
            Manifest::parse(include_str!("../../../miniapps/terminal/manifest.toml")).expect("invalid Terminal manifest");
        let theme = render::colors(host.theme_for(&manifest));
        let palette = Palette::new(&theme, true);
        let app = Rc::new(RefCell::new(TerminalMini {
            host,
            manifest,
            ui: ui.as_weak(),
            tabs: Vec::new(),
            active: 0,
            win: None,
            open: false,
            lines: Rc::new(VecModel::default()),
            tab_rows: Rc::new(VecModel::default()),
            shown: Vec::new(),
            palette,
            anchor: None,
            selection: None,
            pending: Arc::new(AtomicBool::new(false)),
            mood: Mood::Sleep,
            face: HeaderMascot::new(29),
            win_mood: Mood::Idle,
            win_face: HeaderMascot::new(31),
            clock: Timer::default(),
            frame: 0,
            focused: true,
            fx: Fx::default(),
            shaders: None,
            shader_stamp: 0,
            shaders_checked: Instant::now(),
        }));
        APP.with(|a| *a.borrow_mut() = Rc::downgrade(&app));

        let g = ui.global::<TerminalApp>();
        let a = Rc::downgrade(&app);
        g.on_open(move || {
            if let Some(a) = a.upgrade() {
                TerminalMini::open(&a, false);
            }
        });
        let a = Rc::downgrade(&app);
        g.on_new_tab(move || {
            if let Some(a) = a.upgrade() {
                TerminalMini::open(&a, true);
            }
        });
        let a = Rc::downgrade(&app);
        g.on_stop(move || {
            if let Some(a) = a.upgrade() {
                a.borrow_mut().stop_all();
            }
        });
        let a = Rc::downgrade(&app);
        g.on_mascot_poked(move || {
            if let Some(a) = a.upgrade() {
                let mut a = a.borrow_mut();
                let (mood, reduce) = (a.mood, a.host.config.borrow().reduce_motion);
                if a.face.poke(mood, reduce) {
                    a.push_mascot();
                }
            }
        });
        app
    }

    /// Shows the window (creating it on first use). Opens a tab if there is none, or a new
    /// one with `new_tab`.
    fn open(app: &Rc<RefCell<Self>>, new_tab: bool) {
        let win = {
            let mut me = app.borrow_mut();
            if me.win.is_none() {
                match TerminalWindow::new() {
                    Ok(win) => {
                        bind(&win, app);
                        // before the first show: the hook must be there when the GL context is made
                        me.shaders = PostFx::attach(&win, screen_frame);
                        let g = win.global::<TerminalScreen>();
                        g.set_lines(ModelRc::from(me.lines.clone()));
                        g.set_tabs(ModelRc::from(me.tab_rows.clone()));
                        me.win = Some(win);
                        me.style();
                    }
                    Err(e) => {
                        log::warn!("couldn't create the terminal window: {e}");
                        return;
                    }
                }
            }
            me.open = true;
            me.win.as_ref().map(|w| w.clone_strong())
        };
        let Some(win) = win else { return };
        // showing takes focus from the hub, which may react right away: the app is not borrowed
        if let Err(e) = win.show() {
            log::warn!("couldn't show the terminal window: {e}");
        }
        crate::popup::focus(win.window());
        win.invoke_focus_grid();
        let mut me = app.borrow_mut();
        // the native window exists only now: the icon goes after `show`
        me.apply_icon();
        me.sync_maximized();
        me.focused = true;
        me.start_clock();
        me.request_fx();
        // the files may have changed since the window was last open
        me.apply_shaders();
        if me.tabs.is_empty() || new_tab {
            me.new_tab();
        } else {
            me.redraw();
        }
    }

    /// The window's icon in the taskbar: the app's mascot with a prompt in the corner.
    fn apply_icon(&self) {
        let Some(win) = &self.win else { return };
        let icon = super::kit::window_icon(
            sprites::get(&self.mascot_key()),
            self.host.theme_for(&self.manifest),
            Some(Badge::Prompt),
        );
        crate::popup::set_icon_after_show(win, icon);
        crate::popup::label_after_show(win, crate::popup::Kind::Detail);
    }

    fn toggle_maximize(&mut self) {
        let Some(win) = &self.win else { return };
        let w = win.window();
        w.set_maximized(!w.is_maximized());
        self.sync_maximized();
    }

    /// The header button follows the window's state (it may change from the system too).
    fn sync_maximized(&self) {
        if let Some(win) = &self.win {
            win.global::<TerminalScreen>().set_maximized(win.window().is_maximized());
        }
    }

    /// Starts the window's animation frame (if it isn't running).
    fn start_clock(&self) {
        if !self.clock.running() {
            self.clock.start(TimerMode::Repeated, FRAME, || {
                with_app(TerminalMini::anim_tick);
            });
        }
    }

    /// One frame of the window: the clock of the rain and the band, and the mascot.
    fn anim_tick(&mut self) {
        if !self.open {
            self.clock.stop();
            return;
        }
        // a shader saved from an editor (the window may not have the focus then)
        if self.shaders.is_some() && self.shaders_checked.elapsed() >= SHADER_CHECK {
            self.shaders_checked = Instant::now();
            if crate::library::shaders::current_stamp() != self.shader_stamp {
                self.apply_shaders();
            }
        }
        if !self.focused {
            return;
        }
        let Some(win) = &self.win else { return };
        static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        let ms = START.get_or_init(Instant::now).elapsed().as_millis() % 3_600_000;
        self.frame = (self.frame + 1) % 600;
        let anim = win.global::<Anim>();
        anim.set_time(ms as i32);
        anim.set_frame(self.frame as i32);
        anim.set_calm(self.host.config.borrow().reduce_motion);
        self.push_win_mascot();
    }

    /// Mood of the window mascot: the shell in front writing (busy), quiet for long
    /// (asleep), gone with an error (alarm).
    fn front_mood(&self) -> Mood {
        let Some(tab) = self.tabs.get(self.active) else { return Mood::Sleep };
        if !tab.error.is_empty() {
            return Mood::Alarm;
        }
        let Some(s) = &tab.session else { return Mood::Sleep };
        match s.exited() {
            Some(Some(0) | None) => Mood::Sleep,
            Some(Some(_)) => Mood::Alarm,
            None => win_mood(s.quiet_for()),
        }
    }

    fn push_win_mascot(&mut self) {
        let Some(win) = &self.win else { return };
        let mood = self.front_mood();
        let sprite = sprites::get(&self.mascot_key());
        let changed = self.win_face.refresh(sprite, self.host.theme_for(&self.manifest), mood);
        if changed || mood != self.win_mood {
            self.win_mood = mood;
            let g = win.global::<TerminalScreen>();
            g.set_mascot(self.win_face.image());
            g.set_mood(mood.as_str().into());
            g.set_reaction(self.win_face.reaction());
        }
    }

    /// The effects need drawing again (new size, skin or settings): shortly, once.
    fn request_fx(&mut self) {
        self.fx.debounce.start(TimerMode::SingleShot, Duration::from_millis(80), || {
            with_app(TerminalMini::draw_fx);
        });
    }

    /// Passes the effects to the window. Rain and band are a list (made here); the backdrop
    /// and the CRT overlay are images, drawn on another thread when something changed.
    fn draw_fx(&mut self) {
        let Some(win) = &self.win else { return };
        let (backdrop_on, crt_on, rain_on, opacity, mascot_on) = {
            let cfg = self.host.config.borrow();
            let m = &self.manifest;
            (
                cfg.bool(m, "skin_background"),
                cfg.bool(m, "crt"),
                cfg.bool(m, "rain"),
                cfg.int(m, "opacity"),
                cfg.bool(m, "window_mascot"),
            )
        };
        let g = win.global::<TerminalScreen>();
        g.set_veil(opacity.clamp(0, 100) as f32 / 100.0);
        g.set_mascot_on(mascot_on);
        let (w, h) = (win.get_screen_width(), win.get_screen_height());
        if w < 1.0 || h < 1.0 {
            return;
        }
        let skin = crate::skin::current().unwrap_or_else(|| crate::skin::BUILTIN[0].clone());
        let items = |list: Vec<crate::skin::backdrop::Ambient>| -> ModelRc<AmbientItem> {
            ModelRc::new(VecModel::from(list.iter().map(crate::skin::backdrop::Ambient::item).collect::<Vec<_>>()))
        };
        g.set_rain(if rain_on { items(fx::rain(&skin, w, h)) } else { ModelRc::default() });
        g.set_scan(if crt_on { items(fx::scan(&skin, w, h)) } else { ModelRc::default() });
        g.set_rain_on(rain_on);
        // the images: only when the skin, the size or which ones changed
        let scale = win.window().scale_factor();
        let (pw, ph) = ((w * scale).round() as u32, (h * scale).round() as u32);
        let key = (skin.clone(), pw, ph, backdrop_on, crt_on);
        if self.fx.drawn.as_ref() == Some(&key) {
            g.set_backdrop_on(backdrop_on);
            g.set_crt_on(crt_on);
            return;
        }
        if !backdrop_on && !crt_on {
            g.set_backdrop_on(false);
            g.set_crt_on(false);
            self.fx.drawn = Some(key);
            return;
        }
        let id = self.fx.generation.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
        let generation = self.fx.generation.clone();
        let spawned = std::thread::Builder::new().name("miniutils-term-fx".into()).spawn(move || {
            let backdrop = backdrop_on
                .then(|| crate::skin::backdrop::render(crate::skin::backdrop::get(skin.backdrop), &skin, w, h, scale));
            let crt = crt_on.then(|| fx::crt_overlay(pw, ph, scale));
            let _ = slint::invoke_from_event_loop(move || {
                // a newer request is already on its way: these images are stale
                if generation.load(Ordering::Relaxed) != id {
                    return;
                }
                with_app(move |me| me.fx_ready(key, backdrop, crt));
            });
        });
        if let Err(e) = spawned {
            log::warn!("terminal: no thread for the effects ({e})");
        }
    }

    fn fx_ready(
        &mut self,
        key: (crate::skin::Skin, u32, u32, bool, bool),
        backdrop: Option<slint::SharedPixelBuffer<slint::Rgb8Pixel>>,
        crt: Option<slint::SharedPixelBuffer<slint::Rgba8Pixel>>,
    ) {
        let Some(win) = &self.win else { return };
        let g = win.global::<TerminalScreen>();
        if let Some(b) = backdrop {
            g.set_backdrop(slint::Image::from_rgb8(b));
        }
        if let Some(c) = crt {
            g.set_crt(slint::Image::from_rgba8(c));
        }
        g.set_backdrop_on(key.3);
        g.set_crt_on(key.4);
        self.fx.drawn = Some(key);
    }

    /// Opens a tab with a new shell and brings it to the front.
    fn new_tab(&mut self) {
        if self.tabs.len() >= MAX_TABS {
            return;
        }
        self.tabs.push(Tab { session: None, error: String::new(), exit_seen: false });
        self.active = self.tabs.len() - 1;
        self.start(self.active);
    }

    /// Starts (or starts again) the shell of tab `i`, in the window's current size.
    fn start(&mut self, i: usize) {
        let (cols, rows) = self.win.as_ref().map_or((80, 24), |w| (w.get_term_cols(), w.get_term_rows()));
        let (shell, dir, scrollback) = {
            let cfg = self.host.config.borrow();
            let m = &self.manifest;
            (cfg.string(m, "shell"), cfg.string(m, "dir"), cfg.int(m, "scrollback").clamp(0, 100_000) as usize)
        };
        let pending = self.pending.clone();
        let Some(tab) = self.tabs.get_mut(i) else { return };
        tab.session = None;
        tab.exit_seen = false;
        let started = Launch::from_settings(&shell, &dir)
            .and_then(|l| Session::start(l, clamp(rows), clamp(cols), scrollback, move || wake(&pending)));
        match started {
            Ok(s) => {
                tab.error.clear();
                tab.session = Some(s);
            }
            Err(e) => {
                log::warn!("terminal: couldn't start the shell: {e}");
                tab.error = tr!("Couldn't start the shell: {}", e);
            }
        }
        self.front_changed();
    }

    /// Closes tab `i` (ending its shell). The last one closes the window.
    fn close_tab(&mut self, i: usize) {
        if i >= self.tabs.len() {
            return;
        }
        self.tabs.remove(i);
        if self.active > i || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
        if self.tabs.is_empty()
            && let Some(win) = &self.win
        {
            let _ = win.hide();
            self.open = false;
        }
        self.front_changed();
    }

    fn select_tab(&mut self, i: usize) {
        if i < self.tabs.len() && i != self.active {
            self.active = i;
            self.front_changed();
        }
    }

    /// Ends every shell (the tabs go away).
    fn stop_all(&mut self) {
        self.tabs.clear();
        self.active = 0;
        if let Some(win) = &self.win {
            let _ = win.hide();
        }
        self.open = false;
        self.front_changed();
    }

    /// Another tab is in front (or the tabs changed): draw it from scratch.
    fn front_changed(&mut self) {
        self.drop_selection();
        self.shown.clear();
        if let Some(s) = self.tabs.get(self.active).and_then(|t| t.session.as_ref()) {
            // the window may have changed size while this tab was behind
            if let Some(win) = &self.win {
                s.resize(clamp(win.get_term_rows()), clamp(win.get_term_cols()));
            }
        }
        self.refresh_card();
        self.push_mascot();
        self.redraw();
    }

    /// Something arrived from a shell (output or its exit).
    fn on_output(&mut self) {
        self.pending.store(false, Ordering::Release);
        let close_on_exit = self.host.config.borrow().bool(&self.manifest, "close_on_exit");
        let mut exited = Vec::new();
        let mut error = false;
        for (i, tab) in self.tabs.iter_mut().enumerate() {
            let Some(s) = &tab.session else { continue };
            let (bell, failed) = {
                let mut parser = s.parser();
                let ev = parser.callbacks_mut();
                (std::mem::take(&mut ev.bell), std::mem::take(&mut ev.failed))
            };
            if bell {
                self.face.react(Reaction::Surprised);
                self.win_face.react(Reaction::Surprised);
            }
            // the shaders' `iTimeError`: only the tab on screen
            error |= (bell || failed) && i == self.active;
            if let Some(code) = s.exited()
                && !tab.exit_seen
            {
                tab.exit_seen = true;
                exited.push((i, code));
            }
        }
        if error && let (Some(win), Some(fx)) = (&self.win, &self.shaders) {
            fx.error(win);
        }
        if exited.is_empty() {
            self.redraw();
            return;
        }
        // an exit without an error closes its tab; one with an error stays, to be read
        if close_on_exit {
            for &(i, _) in exited.iter().rev().filter(|(_, code)| matches!(code, Some(0) | None)) {
                self.close_tab(i);
            }
        }
        self.front_changed();
    }

    /// Passes the screen of the tab in front to the window (only the lines that changed).
    fn redraw(&mut self) {
        let Some(win) = self.win.as_ref().filter(|_| self.open) else { return };
        let g = win.global::<TerminalScreen>();
        let tabs: Vec<TermTab> =
            self.tabs.iter().map(|t| TermTab { title: t.title().into(), ended: !t.running() }).collect();
        if self.tab_rows.iter().ne(tabs.iter().cloned()) {
            self.tab_rows.set_vec(tabs);
        }
        g.set_active(self.active as i32);
        g.set_can_add_tab(self.tabs.len() < MAX_TABS);
        let Some(tab) = self.tabs.get(self.active) else { return };
        g.set_title(tab.title().into());
        let Some(s) = &tab.session else {
            self.shown.clear();
            self.lines.set_vec(Vec::new());
            g.set_ended(true);
            g.set_ended_text(if tab.error.is_empty() { tr!("Session ended").into() } else { tab.error.as_str().into() });
            g.set_scrolled(0);
            return;
        };
        let p = s.parser();
        let screen = p.screen();
        let new = grid::lines(screen, &self.palette, self.selection);
        if new.len() != self.shown.len() {
            self.lines.set_vec(new.iter().map(|l| line_row(l)).collect::<Vec<_>>());
        } else {
            for (i, (n, old)) in new.iter().zip(&self.shown).enumerate() {
                if n != old {
                    self.lines.set_row_data(i, line_row(n));
                }
            }
        }
        self.shown = new;
        let (row, col) = screen.cursor_position();
        g.set_cursor_row(row as i32);
        g.set_cursor_col(col as i32);
        g.set_cursor_visible(!screen.hide_cursor());
        g.set_cursor_shape(cursor_shape(screen.cursor_style(), self.host.config.borrow().bool(&self.manifest, "bar_cursor")).into());
        g.set_scrolled(screen.scrollback() as i32);
        match s.exited() {
            None => g.set_ended(false),
            Some(code) => {
                g.set_ended(true);
                g.set_ended_text(match code {
                    Some(0) | None => tr!("The shell exited").into(),
                    Some(c) => tr!("The shell exited with code {}", c).into(),
                });
            }
        }
    }

    /// Card: what the mascot says, the state and the last lines.
    fn refresh_card(&mut self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let g = ui.global::<TerminalApp>();
        let running = self.tabs.iter().filter(|t| t.running()).count();
        let front = self.tabs.get(self.active);
        let error = front.map_or("", |t| t.error.as_str());
        self.mood = if !error.is_empty() {
            Mood::Alarm
        } else if running > 0 {
            Mood::Idle
        } else {
            Mood::Sleep
        };
        g.set_running(running > 0);
        g.set_sessions(self.tabs.len() as i32);
        g.set_failed(!error.is_empty());
        g.set_mood(self.mood.as_str().into());
        g.set_speech(
            match running {
                _ if !error.is_empty() => tr!("The shell didn't start.").into(),
                0 => tr!("Terminal ready.").into(),
                1 => tr!("Shell running.").into(),
                n => tr!("{} shells running.", n),
            }
            .into(),
        );
        let status = match front.and_then(|t| t.session.as_ref()) {
            _ if !error.is_empty() => error.to_string(),
            None => tr!("no session").into(),
            Some(s) if s.exited().is_some() => tr!("{}: exited", s.launch.name()),
            // the setting names a folder that isn't there: say so instead of the home folder
            Some(Session { launch: Launch { dir_missing: Some(dir), .. }, .. }) => {
                tr!("Start folder not found: {}; using home", dir)
            }
            Some(s) => format!("{} · {}", s.launch.name(), short_path(&s.launch.dir)),
        };
        g.set_status(status.into());
        g.set_preview(self.preview().into());
    }

    /// Last lines with text on the screen of the tab in front (the live one, not the history).
    fn preview(&self) -> String {
        let Some(s) = self.tabs.get(self.active).and_then(|t| t.session.as_ref()) else { return String::new() };
        preview_of(&s.parser().screen().contents())
    }

    /// Look and theme in the window (its globals are separate from the hub's).
    fn style(&mut self) {
        let (theme_colors, font, copy_on_select) = {
            let cfg = self.host.config.borrow();
            let m = &self.manifest;
            (cfg.bool(m, "theme_colors"), cfg.int(m, "font_size"), cfg.bool(m, "copy_on_select"))
        };
        let theme = render::colors(self.host.theme_for(&self.manifest));
        let cursor = theme.visor;
        self.palette = Palette::new(&theme, theme_colors);
        if let Some(ui) = self.ui.upgrade() {
            ui.global::<TerminalApp>().set_theme(theme.clone());
        }
        let Some(win) = &self.win else { return };
        crate::skin::apply_current(win);
        let g = win.global::<TerminalScreen>();
        g.set_theme(theme);
        g.set_background(self.palette.bg);
        g.set_font_size(font.clamp(6, 40) as i32);
        g.set_copy_on_select(copy_on_select);
        // new colors: every line is drawn again
        self.shown.clear();
        // new skin or settings: the effects too
        self.request_fx();
        if let Some(fx) = &self.shaders {
            fx.set_colors(shader_colors(&self.palette, cursor));
        }
        self.apply_shaders();
    }

    /// The Library shaders of the `shaders` setting, in its order, each with the user's
    /// adjustments written in. One that is gone (removed file) is left out.
    fn apply_shaders(&mut self) {
        self.shader_stamp = crate::library::shaders::current_stamp();
        let (Some(win), Some(fx)) = (&self.win, &self.shaders) else { return };
        let cfg = self.host.config.borrow();
        let sources = cfg
            .list(&self.manifest, "shaders")
            .into_iter()
            .filter_map(|key| {
                let Some(s) = crate::library::shaders::find(&key) else {
                    log::warn!("terminal: shader {key} is not in the Library");
                    return None;
                };
                let values = cfg.shader_params.get(&key).cloned().unwrap_or_default();
                Some(Source { glsl: crate::library::shaders::with_values(&s, &values), name: key })
            })
            .collect();
        fx.set(win, sources);
    }

    fn refresh_mascot(&mut self) -> bool {
        let sprite = sprites::get(&self.mascot_key());
        self.face.refresh(sprite, self.host.theme_for(&self.manifest), self.mood)
    }

    fn push_mascot(&mut self) {
        self.refresh_mascot();
        let Some(ui) = self.ui.upgrade() else { return };
        let g = ui.global::<TerminalApp>();
        g.set_mascot(self.face.image());
        g.set_reaction(self.face.reaction());
    }

    /// The session of the tab in front, while its shell runs.
    fn live(&self) -> Option<&Session> {
        self.tabs.get(self.active)?.session.as_ref().filter(|s| s.exited().is_none())
    }

    /// A key in the window. `false` = it sends nothing (Slint may use it).
    fn key(&mut self, text: &str, mods: Mods) -> bool {
        let Some(s) = self.live() else {
            // the shell is gone: Enter starts another in the same tab
            if !self.tabs.is_empty() && (text == "\n" || text == "\r") {
                self.start(self.active);
                return true;
            }
            return false;
        };
        let app_cursor = s.parser().screen().application_cursor();
        let Some(bytes) = grid::key_bytes(text, mods, app_cursor) else { return false };
        // typing goes back to the live screen and drops the selection
        s.parser().screen_mut().set_scrollback(0);
        s.write(&bytes);
        self.drop_selection();
        self.redraw();
        true
    }

    /// Ctrl+= / Ctrl+- (`by` = ±1) and Ctrl+0 (`by` = 0, back to the default): the text
    /// size, saved as the `font_size` setting (so Settings shows it too).
    fn zoom(&mut self, by: i32) {
        let Some(def) = self.manifest.setting("font_size") else { return };
        let (min, max) = (def.min.unwrap_or(9), def.max.unwrap_or(24));
        let default = def.default.as_integer().unwrap_or(13);
        let now = self.host.config.borrow().int(&self.manifest, "font_size");
        let size = if by == 0 { default } else { (now + by as i64).clamp(min, max) };
        if size == now {
            return;
        }
        self.host.set_setting(&self.manifest, "font_size", toml::Value::Integer(size));
        self.style();
        self.redraw();
    }

    fn paste(&mut self, text: &str) {
        let Some(s) = self.live() else { return };
        let bracketed = s.parser().screen().bracketed_paste();
        s.parser().screen_mut().set_scrollback(0);
        s.write(&grid::paste_bytes(text, bracketed));
        self.drop_selection();
        self.redraw();
    }

    /// Mouse wheel: the history, or arrows for full-screen programs (they have none).
    fn scroll(&mut self, lines: i32) {
        let Some(s) = self.tabs.get(self.active).and_then(|t| t.session.as_ref()) else { return };
        if s.parser().screen().alternate_screen() {
            let key = if lines > 0 { slint::platform::Key::UpArrow } else { slint::platform::Key::DownArrow };
            let app_cursor = s.parser().screen().application_cursor();
            if let Some(b) = grid::key_bytes(&String::from(char::from(key)), Mods::default(), app_cursor) {
                s.write(&b.repeat(lines.unsigned_abs().min(10) as usize));
            }
            return;
        }
        {
            let mut p = s.parser();
            let now = p.screen().scrollback() as i64;
            p.screen_mut().set_scrollback((now + lines as i64).max(0) as usize);
        }
        // the selection is in screen cells: after a scroll it would point elsewhere
        self.drop_selection();
        self.redraw();
    }

    fn select_start(&mut self, col: i32, row: i32) {
        self.anchor = Some((clamp(row), clamp(col)));
        if self.selection.take().is_some() {
            self.set_selection_text(String::new());
            self.redraw();
        }
    }

    fn select_to(&mut self, col: i32, row: i32) {
        let Some(anchor) = self.anchor else { return };
        let Some(s) = self.tabs.get(self.active).and_then(|t| t.session.as_ref()) else { return };
        let sel = grid::ordered(anchor, (clamp(row), clamp(col)));
        if self.selection == Some(sel) {
            return;
        }
        let ((r0, c0), (r1, c1)) = sel;
        let text = s.parser().screen().contents_between(r0, c0, r1, c1.saturating_add(1));
        self.selection = Some(sel);
        self.set_selection_text(text);
        self.redraw();
    }

    fn drop_selection(&mut self) {
        self.selection = None;
        self.anchor = None;
        self.set_selection_text(String::new());
    }

    fn set_selection_text(&self, text: String) {
        if let Some(win) = &self.win {
            win.global::<TerminalScreen>().set_selection(text.into());
        }
    }

    fn resized(&mut self, cols: i32, rows: i32) {
        if let Some(s) = self.tabs.get(self.active).and_then(|t| t.session.as_ref()) {
            s.resize(clamp(rows), clamp(cols));
            self.redraw();
        }
    }

    /// The window was closed: hidden, and the shells end too unless they should keep running.
    fn closed(&mut self) {
        self.open = false;
        if !self.host.config.borrow().bool(&self.manifest, "keep_running") {
            self.stop_all();
        }
    }
}

/// Wires the window's callbacks to the app.
fn bind(win: &TerminalWindow, app: &Rc<RefCell<TerminalMini>>) {
    let g = win.global::<TerminalScreen>();
    // a callback that only needs the app
    let with = |run: fn(&mut TerminalMini)| {
        let a = Rc::downgrade(app);
        move || {
            if let Some(a) = a.upgrade() {
                run(&mut a.borrow_mut());
            }
        }
    };
    let with_int = |run: fn(&mut TerminalMini, usize)| {
        let a = Rc::downgrade(app);
        move |i: i32| {
            if let Some(a) = a.upgrade() {
                run(&mut a.borrow_mut(), i.max(0) as usize);
            }
        }
    };
    let with_cell = |run: fn(&mut TerminalMini, i32, i32)| {
        let a = Rc::downgrade(app);
        move |x: i32, y: i32| {
            if let Some(a) = a.upgrade() {
                run(&mut a.borrow_mut(), x, y);
            }
        }
    };
    let a = Rc::downgrade(app);
    g.on_key(move |text, ctrl, alt, shift| {
        a.upgrade().is_some_and(|a| a.borrow_mut().key(&text, Mods { ctrl, alt, shift }))
    });
    let a = Rc::downgrade(app);
    g.on_paste(move |text| {
        if let Some(a) = a.upgrade() {
            a.borrow_mut().paste(&text);
        }
    });
    let a = Rc::downgrade(app);
    g.on_scroll(move |lines| {
        if let Some(a) = a.upgrade() {
            a.borrow_mut().scroll(lines);
        }
    });
    g.on_resized(with_cell(TerminalMini::resized));
    g.on_select_start(with_cell(TerminalMini::select_start));
    g.on_select_to(with_cell(TerminalMini::select_to));
    g.on_new_tab(with(TerminalMini::new_tab));
    let a = Rc::downgrade(app);
    g.on_zoom(move |by| {
        if let Some(a) = a.upgrade() {
            a.borrow_mut().zoom(by);
        }
    });
    g.on_select_tab(with_int(TerminalMini::select_tab));
    g.on_close_tab(with_int(TerminalMini::close_tab));
    g.on_restart(with(|a| a.start(a.active)));
    g.on_fx_resized(|_, _| with_app(TerminalMini::request_fx));
    g.on_toggle_maximize(with(TerminalMini::toggle_maximize));
    g.on_mascot_poked(with(|a| {
        let reduce = a.host.config.borrow().reduce_motion;
        if a.win_face.poke(a.win_mood, reduce) {
            a.push_win_mascot();
        }
    }));
    // focus: without it the window stops animating (the clock keeps running, idle)
    {
        use slint::winit_030::{EventResult, WinitWindowAccessor, winit::event::WindowEvent};
        win.window().on_winit_window_event(|_, ev| {
            // not now: the event may arrive while the app is busy with something else
            match ev {
                WindowEvent::Focused(on) => {
                    let on = *on;
                    slint::Timer::single_shot(Duration::ZERO, move || with_app(|a| a.focused = on));
                }
                // maximized or restored by the system (keyboard shortcut, double click)
                WindowEvent::Resized(_) => {
                    slint::Timer::single_shot(Duration::ZERO, || with_app(|a| a.sync_maximized()));
                }
                _ => {}
            }
            EventResult::Propagate
        });
    }
    let (a, w) = (Rc::downgrade(app), win.as_weak());
    g.on_close(move || {
        if let (Some(a), Some(win)) = (a.upgrade(), w.upgrade()) {
            let _ = win.hide();
            a.borrow_mut().closed();
        }
    });
    let w = win.as_weak();
    g.on_start_move(move || {
        if let Some(win) = w.upgrade() {
            crate::popup::drag_move(&win);
        }
    });
    let w = win.as_weak();
    g.on_start_resize(move |dir| {
        if let Some(win) = w.upgrade() {
            crate::popup::drag_resize(&win, &dir);
        }
    });
    let a = Rc::downgrade(app);
    win.window().on_close_requested(move || {
        // only hides: the window is reused next time
        if let Some(a) = a.upgrade() {
            a.borrow_mut().closed();
        }
        slint::CloseRequestResponse::HideWindow
    });
}

/// Where the shaders draw (the screen area, below the bar and the tabs) and where the
/// text cursor is, for `PostFx`.
fn screen_frame(win: &TerminalWindow) -> Option<Frame> {
    Some(Frame {
        area: (win.get_screen_x(), win.get_screen_y(), win.get_screen_width(), win.get_screen_height()),
        cursor: win
            .get_cursor_shown()
            .then(|| (win.get_cursor_x(), win.get_cursor_y(), win.get_cursor_w(), win.get_cursor_h())),
        focused: win.get_has_focus(),
    })
}

/// The terminal's colors as the shaders see them (`iBackgroundColor`, `iPalette`…).
fn shader_colors(p: &Palette, cursor: slint::Color) -> Colors {
    let rgb = |c: slint::Color| [c.red() as f32 / 255.0, c.green() as f32 / 255.0, c.blue() as f32 / 255.0];
    Colors::new(rgb(p.bg), rgb(p.fg), rgb(cursor), p.ansi.map(rgb))
}

/// Called on a reading thread: brings the news to the UI thread, once until it is read.
fn wake(pending: &AtomicBool) {
    if pending.swap(true, Ordering::AcqRel) {
        return;
    }
    let _ = slint::invoke_from_event_loop(|| with_app(TerminalMini::on_output));
}

/// Runs `f` with the app, if it exists and isn't busy (UI thread only: timers and
/// wake-ups arrive here with no other way back to it).
fn with_app(f: impl FnOnce(&mut TerminalMini)) {
    if let Some(app) = APP.with(|a| a.borrow().upgrade())
        && let Ok(mut me) = app.try_borrow_mut()
    {
        f(&mut me);
    }
}

/// Mood of a running shell by how long it has been quiet.
fn win_mood(quiet: Duration) -> Mood {
    if quiet < BUSY_FOR {
        Mood::Busy
    } else if quiet > SLEEP_AFTER {
        Mood::Sleep
    } else {
        Mood::Idle
    }
}

/// Cursor shape: what the program asked for (`ESC [ n SP q`), or the setting.
fn cursor_shape(asked: vt100::CursorStyle, bar: bool) -> &'static str {
    use vt100::CursorStyle as C;
    match asked {
        C::BlinkVerticalBar | C::SteadyVerticalBar => "bar",
        C::BlinkUnderline | C::SteadyUnderline => "underline",
        C::BlinkBlock | C::SteadyBlock => "block",
        _ if bar => "bar",
        _ => "block",
    }
}

fn line_row(runs: &[Run]) -> TermLine {
    let runs: Vec<TermRun> = runs
        .iter()
        .map(|r| TermRun {
            col: r.col as i32,
            len: r.len as i32,
            text: r.text.as_str().into(),
            fg: r.fg,
            bg: r.bg,
            bold: r.bold,
            italic: r.italic,
            underline: r.underline,
        })
        .collect();
    TermLine { runs: ModelRc::new(VecModel::from(runs)) }
}

/// The last lines with text, with long gaps (a prompt on the right edge) shortened: the
/// card is narrower than the terminal.
fn preview_of(text: &str) -> String {
    let lines: Vec<String> = text
        .lines()
        .map(|l| shorten_gaps(l.trim_end()))
        .filter(|l| !l.trim().is_empty())
        .collect();
    lines[lines.len().saturating_sub(PREVIEW_LINES)..].join("\n")
}

/// Runs of more than three spaces become three.
fn shorten_gaps(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut spaces = 0;
    for c in line.chars() {
        spaces = if c == ' ' { spaces + 1 } else { 0 };
        if spaces <= 3 {
            out.push(c);
        }
    }
    out
}

fn clamp(v: i32) -> u16 {
    v.clamp(0, u16::MAX as i32) as u16
}

/// A folder for the card, with the home folder as `~`.
fn short_path(dir: &std::path::Path) -> String {
    match dirs::home_dir().and_then(|h| dir.strip_prefix(&h).ok().map(|rest| rest.to_path_buf())) {
        Some(rest) if rest.as_os_str().is_empty() => "~".into(),
        Some(rest) => format!("~{}{}", std::path::MAIN_SEPARATOR, rest.display()),
        None => dir.display().to_string(),
    }
}

impl MiniApp for TerminalMini {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn enable(&mut self, _ui: &HubWindow) {
        self.style();
        self.refresh_card();
        self.push_mascot();
    }

    fn disable(&mut self, _ui: &HubWindow) {
        self.stop_all();
    }

    fn on_event(&mut self, _ev: &Event, _ui: &HubWindow) {}

    fn tick(&mut self, frame: u32, _ui: &HubWindow) {
        // the card's last lines: about twice a second is enough
        if frame.is_multiple_of(4) && !self.tabs.is_empty() {
            let preview = self.preview();
            if let Some(ui) = self.ui.upgrade() {
                let g = ui.global::<TerminalApp>();
                if g.get_preview() != preview.as_str() {
                    g.set_preview(preview.into());
                }
            }
        }
        if self.refresh_mascot() {
            self.push_mascot();
        }
    }

    fn settings_changed(&mut self, _ui: &HubWindow) {
        self.style();
        // another mascot or theme: the taskbar icon too
        self.apply_icon();
        self.refresh_card();
        self.push_mascot();
        self.redraw();
    }

    fn mood(&self) -> Mood {
        self.mood
    }

    fn mascot_key(&self) -> String {
        self.host.config.borrow().string(&self.manifest, "mascot")
    }

    fn summary(&self) -> String {
        match self.tabs.iter().filter(|t| t.running()).count() {
            0 => tr!("Terminal: off").into(),
            1 => tr!("Terminal: 1 shell running").into(),
            n => tr!("Terminal: {} shells running", n),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_paths() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(short_path(&home), "~");
        assert_eq!(short_path(&home.join("x")).replace('\\', "/"), "~/x");
        let elsewhere = std::env::temp_dir();
        if !elsewhere.starts_with(&home) {
            assert_eq!(short_path(&elsewhere), elsewhere.display().to_string());
        }
    }

    #[test]
    fn preview_keeps_the_last_lines_and_shortens_gaps() {
        let screen = "a\n\nb\nc\n> ls                         [ok]\nd\n\n\n";
        assert_eq!(preview_of(screen), "b\nc\n> ls   [ok]\nd");
        assert_eq!(preview_of(""), "");
    }

    #[test]
    fn window_mascot_follows_the_output() {
        assert_eq!(win_mood(Duration::from_millis(200)), Mood::Busy);
        assert_eq!(win_mood(Duration::from_secs(30)), Mood::Idle);
        assert_eq!(win_mood(Duration::from_secs(3600)), Mood::Sleep);
    }

    #[test]
    fn cursor_follows_the_program_then_the_setting() {
        use vt100::CursorStyle as C;
        assert_eq!(cursor_shape(C::Default, false), "block");
        assert_eq!(cursor_shape(C::Default, true), "bar");
        assert_eq!(cursor_shape(C::SteadyUnderline, true), "underline");
        assert_eq!(cursor_shape(C::BlinkBlock, true), "block");
    }
}
