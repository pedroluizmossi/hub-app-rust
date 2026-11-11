//! Aparência: visual da janela, fundo, ícone da bandeja, movimento e relógio.

use std::time::Duration;

use slint::{ComponentHandle, ModelRc, SharedString, Timer, VecModel};

use super::Hub;
use crate::core::config::Config;
use crate::mascot::{sprites, themes};
use crate::skin;
use crate::ui::Anim;

impl Hub {
    /// Aplica o visual da config: tokens da tela, lista do seletor e fundo.
    pub(super) fn apply_skin(&self) {
        let (key, name) = {
            let cfg = self.host.config.borrow();
            (cfg.skin.clone(), crate::core::config::display_name())
        };
        skin::apply(&self.ui, skin::get(&key), &name);
        self.ui.set_app_name(name.into());
        let labels: Vec<SharedString> = skin::SKINS.iter().map(|s| s.label.into()).collect();
        self.ui.set_skins(ModelRc::new(VecModel::from(labels)));
        self.ui.set_skin_index(skin::index_of(&key) as i32);
        self.ui.set_nav_top(self.host.config.borrow().nav == "top");
        self.apply_motion();
        self.apply_tray_choices();
        self.schedule_backdrop();
    }

    /// Opções e escolhas do ícone da bandeja (índice 0 = automático).
    pub(super) fn apply_tray_choices(&self) {
        let (icon, theme) = {
            let cfg = self.host.config.borrow();
            (cfg.tray_icon.clone(), cfg.tray_theme.clone())
        };
        let auto = |label: &str| std::iter::once(SharedString::from(label));
        let icons: Vec<SharedString> =
            auto("Automático").chain(sprites::SPRITES.iter().map(|s| s.label.into())).collect();
        let themes_: Vec<SharedString> =
            auto("Automático").chain(themes::THEMES.iter().map(|t| t.label.into())).collect();
        let pos = |found: Option<usize>| found.map_or(0, |i| i as i32 + 1);
        self.ui.set_tray_icons(ModelRc::new(VecModel::from(icons)));
        self.ui.set_tray_icon_index(pos(sprites::SPRITES.iter().position(|s| s.key == icon)));
        self.ui.set_tray_themes(ModelRc::new(VecModel::from(themes_)));
        self.ui.set_tray_theme_index(pos(themes::THEMES.iter().position(|t| t.key == theme)));
    }

    /// Troca a mascote do ícone da bandeja. 0 = automático (o app em pior estado).
    pub(super) fn choose_tray_icon(&self, i: usize) {
        let key = if i == 0 { Some("auto") } else { sprites::SPRITES.get(i - 1).map(|s| s.key) };
        self.set_tray_choice(key, |cfg| &mut cfg.tray_icon);
    }

    /// Troca o tema do ícone da bandeja. 0 = automático (o tema do app em pior estado).
    pub(super) fn choose_tray_theme(&self, i: usize) {
        let key = if i == 0 { Some("auto") } else { themes::THEMES.get(i - 1).map(|t| t.key) };
        self.set_tray_choice(key, |cfg| &mut cfg.tray_theme);
    }

    /// Grava uma escolha do ícone da bandeja; o ícone em si muda no próximo `sync_tray`.
    fn set_tray_choice(&self, key: Option<&str>, field: impl FnOnce(&mut Config) -> &mut String) {
        let Some(key) = key else { return };
        {
            let mut cfg = self.host.config.borrow_mut();
            let campo = field(&mut cfg);
            if campo == key {
                return;
            }
            *campo = key.into();
            cfg.save();
        }
        self.apply_tray_choices();
    }

    /// Reduzir movimento: mascotes paradas na tela (`Anim.calm`); os apps deixam de disparar
    /// reações (eles leem `reduce_motion` da config).
    pub(super) fn apply_motion(&self) {
        let calm = self.host.config.borrow().reduce_motion;
        self.ui.set_reduce_motion(calm);
        self.ui.global::<Anim>().set_calm(calm);
    }

    pub(super) fn toggle_motion(&self) {
        {
            let mut cfg = self.host.config.borrow_mut();
            cfg.reduce_motion = !cfg.reduce_motion;
            cfg.save();
        }
        self.apply_motion();
    }

    /// Onde ficam as páginas: 0 = barra lateral, 1 = abas no cabeçalho.
    pub(super) fn choose_nav(&self, i: usize) {
        let nav = if i == 1 { "top" } else { "side" };
        {
            let mut cfg = self.host.config.borrow_mut();
            if cfg.nav == nav {
                return;
            }
            cfg.nav = nav.into();
            cfg.save();
        }
        // a área do painel muda de largura e avisa sozinha (`backdrop-resized`)
        self.ui.set_nav_top(nav == "top");
    }

    /// Troca o visual. Os apps com tema "auto" mudam junto.
    pub(super) fn choose_skin(&self, i: usize) {
        let Some(s) = skin::SKINS.get(i) else { return };
        {
            let mut cfg = self.host.config.borrow_mut();
            if cfg.skin == s.key {
                return;
            }
            cfg.skin = s.key.into();
            cfg.save();
        }
        self.apply_skin();
        for app in self.registry.enabled() {
            app.borrow_mut().settings_changed(&self.ui);
        }
        self.rebuild_lists();
    }

    /// Pede um fundo novo daqui a pouco (um só, mesmo com vários pedidos seguidos).
    pub(super) fn schedule_backdrop(&self) {
        let id = self.backdrop_gen.get().wrapping_add(1);
        self.backdrop_gen.set(id);
        let current = self.backdrop_gen.clone();
        let weak = self.ui.as_weak();
        let size = self.backdrop_size.get();
        let key = self.host.config.borrow().skin.clone();
        Timer::single_shot(Duration::from_millis(60), move || {
            if current.get() != id {
                return;
            }
            let Some(ui) = weak.upgrade() else { return };
            let s = skin::get(&key);
            let window = ui.window();
            let scale = window.scale_factor();
            // sem o tamanho exato da área ainda: estima pela janela
            let (w, h) = if size.0 > 0.0 && size.1 > 0.0 {
                size
            } else {
                let px = window.size();
                let (ww, wh) = (px.width as f32 / scale, px.height as f32 / scale);
                let chrome = 2.0 * s.border + s.header_h + s.header_line + s.panel_top_h + s.footer_h;
                ((ww - 2.0 * s.border).max(1.0), (wh - chrome).max(1.0))
            };
            let buf = skin::backdrop::render(s.backdrop, s.bg, w, h, scale);
            ui.set_backdrop(slint::Image::from_rgb8(buf));
        });
    }

    /// Relógio e saudação, atualizados quando muda o minuto.
    pub(super) fn update_clock(&self) {
        let (hour, min) = local_time();
        let text = format!("{hour:02}:{min:02}");
        if *self.clock.borrow() == text {
            return;
        }
        let part = match hour {
            5..=11 => "bom dia",
            12..=17 => "boa tarde",
            _ => "boa noite",
        };
        self.ui.set_greeting(format!("{part} · {text}").into());
        self.ui.set_clock(text.clone().into());
        *self.clock.borrow_mut() = text;
    }
}

/// Hora e minuto locais.
fn local_time() -> (i32, i32) {
    // SAFETY: time e localtime_r só escrevem no `tm` que passamos
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&now, &mut tm).is_null() {
            return (0, 0);
        }
        (tm.tm_hour, tm.tm_min)
    }
}
