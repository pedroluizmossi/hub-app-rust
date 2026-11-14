//! The hub window as a tray popup: open, hide, pin, fade and size.

use std::time::{Duration, Instant};

use slint::{ComponentHandle, Timer};

use super::Hub;
use crate::instance;
use crate::popup;
use crate::tray::Tray;

impl Hub {
    /// Shows the popup next to `anchor` (point clicked on the tray).
    pub(super) fn show(&self, anchor: Option<(i32, i32)>) {
        let anchor = anchor.or(self.last_anchor.get());
        self.last_anchor.set(anchor);
        self.had_focus.set(false);
        // reopening: starts transparent and fades in (the first time the window does not exist yet)
        let fade_in = self.positionable && popup::has_window(self.ui.window());
        if fade_in {
            self.fade_gen.set(self.fade_gen.get() + 1); // cancels a pending fade-out
            popup::set_opacity(self.ui.window(), 0.0);
        }
        if self.ui.show().is_err() {
            return;
        }
        self.schedule_backdrop();
        if fade_in {
            self.fade(0.0, 1.0, false);
        }
        self.set_shown(true);
        let window = self.ui.window();
        if self.positionable {
            popup::skip_taskbar(window);
            popup::place(window, anchor);
        }
        popup::focus(window);
        // the window manager sometimes repositions on mapping: apply the position again.
        // On the first opening the native window only exists after the event loop runs, and the
        // calls above found nothing: the states (off the taskbar, on top, on all workspaces)
        // also go here. Repeating it the other times changes nothing.
        if self.positionable {
            let weak = self.ui.as_weak();
            Timer::single_shot(Duration::from_millis(60), move || {
                if let Some(ui) = weak.upgrade() {
                    popup::skip_taskbar(ui.window());
                    popup::place(ui.window(), anchor);
                }
            });
        }
    }

    /// Request from another instance: opens the popup (on the tab, if any). Already open, just brings it to the front.
    pub(super) fn show_from_command(&self, tab: &str) {
        if !tab.is_empty() {
            self.open_tab(tab);
            self.rebuild_lists();
        }
        if self.shown.get() {
            popup::focus(self.ui.window());
        } else {
            self.show(None);
        }
    }

    /// Serves the other instances (see `instance.rs`) through the UI thread.
    pub fn serve_commands(&self, server: &instance::Server) {
        let weak = self.ui.as_weak();
        let served = server.serve(move |cmd| {
            match cmd {
                instance::Command::Show(tab) => {
                    let tab = tab.unwrap_or_default();
                    let _ = weak.upgrade_in_event_loop(move |ui| ui.invoke_show_requested(tab.into()));
                }
                // the command-line store installed or removed a plugin
                instance::Command::ReloadPlugins => super::request_plugin_reload(),
            }
        });
        if let Err(e) = served {
            log::warn!("could not serve other instances ({e})");
        }
    }

    /// The window opened or hid. While hidden, the expensive providers collect more slowly
    /// and the animation stops; on opening, each app may refresh (see `MiniApp::window_shown`).
    pub(super) fn set_shown(&self, on: bool) {
        if self.shown.replace(on) == on {
            return;
        }
        self.host.bus.set_visible(on);
        if on {
            self.update_clock();
            self.anim.restart();
            for app in self.registry.enabled() {
                app.borrow_mut().window_shown(&self.ui);
            }
        } else {
            self.anim.stop();
        }
    }

    /// There is a visible tray icon now (the panel may come up after the app).
    pub(super) fn has_tray(&self) -> bool {
        self.tray.borrow().as_ref().is_some_and(Tray::online)
    }

    /// Saves the current window size, if it changed. In logical pixels, so it does not change
    /// size when the screen scale changes.
    pub(super) fn remember_size(&self) {
        let window = self.ui.window();
        let size = window.size();
        if size.width == 0 || size.height == 0 {
            return;
        }
        let scale = window.scale_factor();
        let logical = ((size.width as f32 / scale).round() as u32, (size.height as f32 / scale).round() as u32);
        let mut cfg = self.host.config.borrow_mut();
        if cfg.window != Some(logical) {
            cfg.window = Some(logical);
            cfg.save();
        }
    }

    /// Changes the frame interval depending on whether the mouse is over the window.
    pub(super) fn set_hovered(&self, on: bool) {
        if self.hovered.replace(on) != on {
            self.pace_frames();
        }
    }

    pub(super) fn hide(&self) {
        self.remember_size();
        // no `CursorLeft` arrives when hiding: the next time it shows, `CursorEntered` decides
        self.set_hovered(false);
        if !self.has_tray() {
            // without a tray there would be no way back: just minimize
            self.ui.window().set_minimized(true);
            return;
        }
        self.set_shown(false);
        if self.positionable {
            self.fade(1.0, 0.0, true); // fade out instead of blinking
        } else {
            let _ = self.ui.hide();
        }
    }

    /// Animates the window opacity from `from` to `to` in ~110 ms; `then_hide` hides it at the end.
    pub(super) fn fade(&self, from: f32, to: f32, then_hide: bool) {
        const STEPS: u32 = 6;
        let id = self.fade_gen.get() + 1;
        self.fade_gen.set(id);
        for i in 1..=STEPS {
            let weak = self.ui.as_weak();
            let current = self.fade_gen.clone();
            Timer::single_shot(Duration::from_millis(18 * i as u64), move || {
                if current.get() != id {
                    return; // another fade started
                }
                let Some(ui) = weak.upgrade() else { return };
                let t = i as f32 / STEPS as f32;
                popup::set_opacity(ui.window(), from + (to - from) * t);
                if i == STEPS && then_hide {
                    let _ = ui.hide();
                }
            });
        }
    }

    /// Turns pin mode on/off (pin button, middle click or tray menu).
    pub(super) fn set_pinned(&self, on: bool) {
        self.pinned.set(on);
        self.ui.set_pinned(on);
        if let Some(tray) = self.tray.borrow().as_ref() {
            tray.set_pinned(on);
        }
    }

    /// An edge drag is in progress (or just ended).
    pub(super) fn resizing(&self) -> bool {
        self.resize_until.get().is_some_and(|t| Instant::now() < t)
    }

    pub(super) fn toggle(&self, anchor: Option<(i32, i32)>) {
        // clicking the icon takes focus from the popup before the click gets here: if it just
        // hid because of that, the click was meant to close it, not to reopen it
        let just_hidden = self.focus_hidden_at.take().is_some_and(|t| t.elapsed() < Duration::from_millis(400));
        if self.shown.get() {
            self.hide();
        } else if !just_hidden {
            self.show(anchor);
        }
    }
}
