//! Liga os callbacks da tela (e os eventos da janela) aos métodos do hub.

use std::rc::Rc;
use std::time::Instant;

use slint::ComponentHandle;

use super::settings::Input;
use super::{Hub, RESIZE_QUIET};
use crate::popup;

impl Hub {
    pub(super) fn bind(hub: &Rc<Self>) {
        let ui = &hub.ui;
        let w = Rc::downgrade(hub);
        ui.on_select_tab(move |id| {
            if let Some(h) = w.upgrade() {
                let store = id == "store";
                if id == "apps" {
                    h.apply_system();
                    h.refresh_data();
                }
                h.ui.set_current(id);
                if store {
                    h.store_reload();
                }
                h.rebuild_pages(true);
                h.anim_tick(); // desenha as mascotes já no primeiro quadro
                h.schedule_backdrop();
            }
        });

        let w = Rc::downgrade(hub);
        ui.on_toggle_app(move |id| {
            let Some(h) = w.upgrade() else { return };
            let on = !h.host.config.borrow().is_enabled(&id);
            // ligado pela primeira vez: entra na página aberta
            if on && h.host.config.borrow().page_of(&id).is_none() {
                let page = h.current_page();
                h.host.config.borrow_mut().move_to_page(&id, page);
            }
            h.registry.set_enabled(&id, on, &h.ui);
            h.rebuild_lists();
        });

        let w = Rc::downgrade(hub);
        ui.on_skin_chosen(move |i| {
            if let Some(h) = w.upgrade() {
                h.choose_skin(i.max(0) as usize);
            }
        });

        let w = Rc::downgrade(hub);
        ui.on_accent_chosen(move |i| {
            if let Some(h) = w.upgrade() {
                h.choose_accent(i.max(0) as usize);
            }
        });

        let w = Rc::downgrade(hub);
        ui.on_text_size_chosen(move |i| {
            if let Some(h) = w.upgrade() {
                h.choose_text_size(i.max(0) as usize);
            }
        });

        let w = Rc::downgrade(hub);
        ui.on_ambient_toggled(move || {
            if let Some(h) = w.upgrade() {
                h.toggle_ambient();
            }
        });

        let w = Rc::downgrade(hub);
        ui.on_open_looks_dir(move || {
            if let Some(h) = w.upgrade() {
                h.open_looks_dir();
            }
        });

        let w = Rc::downgrade(hub);
        ui.on_reload_looks(move || {
            if let Some(h) = w.upgrade() {
                h.reload_looks();
            }
        });

        let w = Rc::downgrade(hub);
        ui.on_tray_icon_chosen(move |i| {
            if let Some(h) = w.upgrade() {
                h.choose_tray_icon(i.max(0) as usize);
            }
        });

        let w = Rc::downgrade(hub);
        ui.on_tray_theme_chosen(move |i| {
            if let Some(h) = w.upgrade() {
                h.choose_tray_theme(i.max(0) as usize);
            }
        });

        let w = Rc::downgrade(hub);
        ui.on_nav_chosen(move |i| {
            if let Some(h) = w.upgrade() {
                h.choose_nav(i.max(0) as usize);
            }
        });

        let w = Rc::downgrade(hub);
        ui.on_chatter_chosen(move |i| {
            if let Some(h) = w.upgrade() {
                h.choose_chatter(i.max(0) as usize);
            }
        });

        let w = Rc::downgrade(hub);
        ui.on_reduce_motion_toggled(move || {
            if let Some(h) = w.upgrade() {
                h.toggle_motion();
            }
        });

        let w = Rc::downgrade(hub);
        ui.on_autostart_toggled(move || {
            if let Some(h) = w.upgrade() {
                h.toggle_autostart();
            }
        });

        let w = Rc::downgrade(hub);
        ui.on_open_logs(move || {
            if let Some(h) = w.upgrade() {
                h.open_logs();
            }
        });

        let w = Rc::downgrade(hub);
        ui.on_export_data(move |id| {
            if let Some(h) = w.upgrade() {
                h.export_data(&id);
            }
        });
        let w = Rc::downgrade(hub);
        ui.on_delete_data(move |id| {
            if let Some(h) = w.upgrade() {
                h.delete_data(&id);
            }
        });

        let w = Rc::downgrade(hub);
        ui.on_open_apps_dir(move || {
            if let Some(h) = w.upgrade() {
                h.open_apps_dir();
            }
        });

        let w = Rc::downgrade(hub);
        ui.on_reload_apps(move || {
            if let Some(h) = w.upgrade() {
                h.reload_plugins();
            }
        });

        let w = Rc::downgrade(hub);
        ui.on_page_selected(move |i| {
            if let Some(h) = w.upgrade() {
                h.select_page(i.max(0) as usize);
            }
        });

        let w = Rc::downgrade(hub);
        ui.on_page_added(move || {
            if let Some(h) = w.upgrade() {
                h.add_page();
            }
        });

        let w = Rc::downgrade(hub);
        ui.on_page_removed(move |i| {
            if let Some(h) = w.upgrade() {
                h.remove_page(i.max(0) as usize);
            }
        });

        let w = Rc::downgrade(hub);
        ui.on_page_renamed(move |i, name| {
            if let Some(h) = w.upgrade() {
                h.rename_page(i.max(0) as usize, &name);
            }
        });

        let w = Rc::downgrade(hub);
        ui.on_page_icon_chosen(move |i, k| {
            if let Some(h) = w.upgrade() {
                h.page_icon(i.max(0) as usize, k.max(0) as usize);
            }
        });

        let w = Rc::downgrade(hub);
        ui.on_page_moved(move |i, d| {
            if let Some(h) = w.upgrade() {
                h.move_page(i.max(0) as usize, d);
            }
        });

        let w = Rc::downgrade(hub);
        ui.on_app_page_chosen(move |id, k| {
            if let Some(h) = w.upgrade() {
                h.app_to_page(&id, k.max(0) as usize);
            }
        });

        let w = Rc::downgrade(hub);
        ui.on_backdrop_resized(move |bw, bh| {
            if let Some(h) = w.upgrade()
                && bw > 0.0 && bh > 0.0 && h.backdrop_size.replace((bw, bh)) != (bw, bh) {
                    h.schedule_backdrop();
                }
        });

        let w = Rc::downgrade(hub);
        ui.on_grid_size(move |c, r| {
            if let Some(h) = w.upgrade() {
                h.grid_size(c, r);
            }
        });

        let w = Rc::downgrade(hub);
        ui.on_edit_begin(move |id, mode, c, r| {
            if let Some(h) = w.upgrade() {
                h.edit_begin(&id, &mode, c, r);
            }
        });

        let w = Rc::downgrade(hub);
        ui.on_edit_drag(move |c, r| {
            if let Some(h) = w.upgrade() {
                h.edit_drag(c, r);
            }
        });

        let w = Rc::downgrade(hub);
        ui.on_edit_end(move || {
            if let Some(h) = w.upgrade() {
                h.edit_end();
            }
        });

        let w = Rc::downgrade(hub);
        ui.on_edit_key(move |id, dx, dy, resize| {
            if let Some(h) = w.upgrade() {
                h.edit_key(&id, dx, dy, resize);
            }
        });

        let w = Rc::downgrade(hub);
        ui.on_setting_toggled(move |app, key| {
            if let Some(h) = w.upgrade() {
                h.change_setting(&app, &key, Input::Toggle);
            }
        });

        let w = Rc::downgrade(hub);
        ui.on_setting_int(move |app, key, v| {
            if let Some(h) = w.upgrade() {
                h.change_setting(&app, &key, Input::Int(v as i64));
            }
        });

        let w = Rc::downgrade(hub);
        ui.on_setting_chosen(move |app, key, i| {
            if let Some(h) = w.upgrade() {
                h.change_setting(&app, &key, Input::Choice(i.max(0) as usize));
            }
        });

        let w = Rc::downgrade(hub);
        ui.global::<crate::ui::Chatter>().on_skip(move || {
            if let Some(h) = w.upgrade() {
                h.chat_skip();
            }
        });

        let w = Rc::downgrade(hub);
        ui.on_hide_window(move || {
            if let Some(h) = w.upgrade() {
                h.hide();
            }
        });

        let w = Rc::downgrade(hub);
        ui.on_start_resize(move |dir| {
            if let Some(h) = w.upgrade() {
                h.resize_until.set(Some(Instant::now() + RESIZE_QUIET));
                popup::drag_resize(h.ui.window(), &dir);
            }
        });

        let w = Rc::downgrade(hub);
        ui.on_toggle_pin(move || {
            if let Some(h) = w.upgrade() {
                let on = !h.pinned.get();
                h.set_pinned(on);
                // fixar pela bandeja: o menu rouba o foco e esconde o popup antes; reabre
                if on && !h.shown.get() {
                    h.show(None);
                }
            }
        });

        ui.on_quit({
            let w = Rc::downgrade(hub);
            move || {
                if let Some(h) = w.upgrade() {
                    h.remember_size();
                }
                let _ = slint::quit_event_loop();
            }
        });

        let w = Rc::downgrade(hub);
        ui.on_toggle_requested(move |x, y| {
            if let Some(h) = w.upgrade() {
                let anchor = (x != 0 || y != 0).then_some((x, y));
                h.toggle(anchor);
            }
        });

        let w = Rc::downgrade(hub);
        ui.on_show_requested(move |tab| {
            if let Some(h) = w.upgrade() {
                h.show_from_command(&tab);
            }
        });

        // popup: clicou fora (perdeu o foco) -> esconde
        let w = Rc::downgrade(hub);
        {
            use slint::winit_030::{EventResult, WinitWindowAccessor, winit::event::WindowEvent};
            ui.window().on_winit_window_event(move |_, ev| {
                if let WindowEvent::Focused(f) = ev {
                    log::debug!("foco {f}");
                }
                let Some(h) = w.upgrade() else { return EventResult::Propagate };
                match ev {
                    WindowEvent::CursorEntered { .. } => h.set_hovered(true),
                    WindowEvent::CursorLeft { .. } => h.set_hovered(false),
                    _ => {}
                }
                match ev {
                    WindowEvent::Focused(true) => {
                        h.had_focus.set(true);
                        h.resize_until.set(None);
                    }
                    // redimensionando pela borda: cada tamanho novo adia o fim do arrasto
                    WindowEvent::Resized(_) if h.resize_until.get().is_some() => {
                        h.resize_until.set(Some(Instant::now() + RESIZE_QUIET));
                    }
                    // fixado ou no meio de um redimensionamento: não some sozinho
                    WindowEvent::Focused(false)
                        if h.shown.get() && h.has_tray() && h.had_focus.get() && !h.pinned.get() && !h.resizing() => {
                            h.hide();
                            h.focus_hidden_at.set(Some(Instant::now()));
                        }
                    _ => {}
                }
                EventResult::Propagate
            });
        }


        // fechar a janela só esconde quando há bandeja
        let w = Rc::downgrade(hub);
        ui.window().on_close_requested(move || {
            match w.upgrade() {
                Some(h) if h.has_tray() => {
                    h.set_shown(false);
                    slint::CloseRequestResponse::HideWindow
                }
                _ => {
                    let _ = slint::quit_event_loop();
                    slint::CloseRequestResponse::HideWindow
                }
            }
        });
    }
}
