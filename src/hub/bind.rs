//! Wires the screen callbacks (and the window events) to the hub methods.
//!
//! The callbacks reach the hub through [`with_hub`] (a weak reference kept by the UI thread):
//! a callback that arrives after the hub is gone does nothing.

use std::rc::Rc;
use std::time::Instant;

use slint::ComponentHandle;

use super::settings::Input;
use super::{Hub, RESIZE_QUIET, with_hub};
use crate::popup;

/// Index from the screen (`-1` = nothing chosen becomes 0).
fn idx(i: i32) -> usize {
    i.max(0) as usize
}

impl Hub {
    pub(super) fn bind(hub: &Rc<Self>) {
        let ui = &hub.ui;
        ui.on_select_tab(|id| with_hub(|h| h.enter_tab(&id)));
        ui.on_toggle_app(|id| {
            with_hub(|h| {
                let on = !h.host.config.borrow().is_enabled(&id);
                // enabled for the first time: goes to the open page
                if on && h.host.config.borrow().page_of(&id).is_none() {
                    let page = h.current_page();
                    h.host.config.borrow_mut().move_to_page(&id, page);
                }
                h.registry.set_enabled(&id, on, &h.ui);
                h.rebuild_lists();
            })
        });

        // Appearance and System
        ui.on_skin_chosen(|i| with_hub(|h| h.choose_skin(idx(i))));
        ui.on_accent_chosen(|i| with_hub(|h| h.choose_accent(idx(i))));
        ui.on_text_size_chosen(|i| with_hub(|h| h.choose_text_size(idx(i))));
        ui.on_ambient_toggled(|| with_hub(|h| h.toggle_ambient()));
        ui.on_open_looks_dir(|| with_hub(|h| h.open_looks_dir()));
        ui.on_reload_looks(|| with_hub(|h| h.reload_looks()));
        ui.on_tray_icon_chosen(|i| with_hub(|h| h.choose_tray_icon(idx(i))));
        ui.on_tray_theme_chosen(|i| with_hub(|h| h.choose_tray_theme(idx(i))));
        ui.on_nav_chosen(|i| with_hub(|h| h.choose_nav(idx(i))));
        ui.on_chatter_chosen(|i| with_hub(|h| h.choose_chatter(idx(i))));
        ui.on_language_chosen(|i| with_hub(|h| h.choose_language(idx(i))));
        ui.on_reduce_motion_toggled(|| with_hub(|h| h.toggle_motion()));
        ui.on_still_on_hover_toggled(|| with_hub(|h| h.toggle_still_on_hover()));
        ui.on_autostart_toggled(|| with_hub(|h| h.toggle_autostart()));
        ui.on_place_chosen(|i| with_hub(|h| h.choose_place(idx(i))));
        ui.on_desktop_app_toggled(|| with_hub(|h| h.toggle_desktop_app()));
        ui.on_open_logs(|| with_hub(|h| h.open_logs()));

        // apps: data, local folder and options
        ui.on_export_data(|id| with_hub(|h| h.export_data(&id)));
        ui.on_delete_data(|id| with_hub(|h| h.delete_data(&id)));
        ui.on_open_apps_dir(|| with_hub(|h| h.open_apps_dir()));
        ui.on_reload_apps(|| with_hub(|h| h.reload_plugins()));
        ui.on_setting_toggled(|app, key| with_hub(|h| h.change_setting(&app, &key, Input::Toggle)));
        ui.on_setting_int(|app, key, v| with_hub(|h| h.change_setting(&app, &key, Input::Int(v as i64))));
        ui.on_setting_chosen(|app, key, i| with_hub(|h| h.change_setting(&app, &key, Input::Choice(idx(i)))));
        ui.on_setting_text(|app, key, t| with_hub(|h| h.change_setting(&app, &key, Input::Text(t.to_string()))));

        // pages
        ui.on_page_selected(|i| with_hub(|h| h.select_page(idx(i))));
        ui.on_page_added(|| with_hub(|h| h.add_page()));
        ui.on_page_removed(|i| with_hub(|h| h.remove_page(idx(i))));
        ui.on_page_renamed(|i, name| with_hub(|h| h.rename_page(idx(i), &name)));
        ui.on_page_icon_chosen(|i, k| with_hub(|h| h.page_icon(idx(i), idx(k))));
        ui.on_page_moved(|i, d| with_hub(|h| h.move_page(idx(i), d)));
        ui.on_app_page_chosen(|id, k| with_hub(|h| h.app_to_page(&id, idx(k))));

        // panel: backdrop and grid editing
        ui.on_backdrop_resized(|bw, bh| {
            with_hub(|h| {
                if bw > 0.0 && bh > 0.0 && h.backdrop_size.replace((bw, bh)) != (bw, bh) {
                    h.schedule_backdrop();
                }
            })
        });
        ui.on_grid_size(|c, r| with_hub(|h| h.grid_size(c, r)));
        ui.on_edit_begin(|id, mode, c, r| with_hub(|h| h.edit_begin(&id, &mode, c, r)));
        ui.on_edit_drag(|c, r| with_hub(|h| h.edit_drag(c, r)));
        ui.on_edit_end(|| with_hub(|h| h.edit_end()));
        ui.on_edit_key(|id, dx, dy, resize| with_hub(|h| h.edit_key(&id, dx, dy, resize)));
        ui.global::<crate::ui::Chatter>().on_skip(|| with_hub(|h| h.chat_skip()));

        // window
        ui.on_hide_window(|| with_hub(|h| h.hide()));
        ui.on_start_resize(|dir| {
            with_hub(|h| {
                h.resize_until.set(Some(Instant::now() + RESIZE_QUIET));
                popup::drag_resize(h.ui.window(), &dir);
            })
        });
        ui.on_toggle_pin(|| {
            with_hub(|h| {
                let on = !h.pinned.get();
                h.set_pinned(on);
                // pinned from the tray: the menu steals focus and hides the popup first; reopen it
                if on && !h.shown.get() {
                    h.show(None);
                }
            })
        });
        ui.on_quit(|| {
            with_hub(|h| h.remember_window());
            popup::released();
            let _ = slint::quit_event_loop();
        });
        ui.on_toggle_requested(|x, y| with_hub(|h| h.toggle((x != 0 || y != 0).then_some((x, y)))));
        ui.on_show_requested(|tab| with_hub(|h| h.show_from_command(&tab)));
        ui.on_toggle_window_requested(|tab| with_hub(|h| h.toggle_from_command(&tab)));

        // popup: clicked outside (lost focus) -> hide
        {
            use slint::winit_030::{EventResult, WinitWindowAccessor, winit::event::WindowEvent};
            ui.window().on_winit_window_event(|_, ev| {
                if let WindowEvent::Focused(f) = ev {
                    log::debug!("focus {f}");
                }
                with_hub(|h| {
                    match ev {
                        WindowEvent::CursorEntered { .. } => h.set_hovered(true),
                        WindowEvent::CursorLeft { .. } => h.set_hovered(false),
                        _ => {}
                    }
                    match ev {
                        // minimized or fully covered (Windows, macOS); back on screen
                        WindowEvent::Occluded(o) => h.set_covered(*o),
                        // restored from the taskbar: focus comes back (X11 reports no `Occluded`)
                        WindowEvent::Focused(true) | WindowEvent::CursorEntered { .. } if h.covered.get() || !h.on_screen.get() => {
                            h.set_covered(false)
                        }
                        _ => {}
                    }
                    match ev {
                        WindowEvent::Focused(true) => {
                            h.had_focus.set(true);
                            h.resize_until.set(None);
                        }
                        // resizing by the edge: each new size postpones the end of the drag
                        WindowEvent::Resized(_) if h.resize_until.get().is_some() => {
                            h.resize_until.set(Some(Instant::now() + RESIZE_QUIET));
                        }
                        // pinned, in the middle of a resize or a desktop app: does not hide by itself
                        WindowEvent::Focused(false)
                            if h.shown.get()
                                && h.has_tray()
                                && h.had_focus.get()
                                && !h.pinned.get()
                                && !h.resizing()
                                && !h.desktop_app() =>
                        {
                            h.hide();
                            h.focus_hidden_at.set(Some(Instant::now()));
                        }
                        _ => {}
                    }
                });
                EventResult::Propagate
            });
        }

        // closing the window only hides it when there is a tray
        ui.window().on_close_requested(|| {
            let mut hidden = false;
            with_hub(|h| {
                // Alt+F4 or the taskbar's Close: keep a resize made since the last hide
                h.remember_window();
                if h.has_tray() {
                    h.set_shown(false);
                    hidden = true;
                }
            });
            if !hidden {
                let _ = slint::quit_event_loop();
            }
            slint::CloseRequestResponse::HideWindow
        });
    }
}
