//! Aparência: visual da janela, fundo, ícone da bandeja, movimento e relógio.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use slint::{ComponentHandle, Image, ModelRc, SharedString, Timer, VecModel};

use super::Hub;
use super::chatter::Pace;
use crate::core::config::Config;
use crate::mascot::{sprites, themes};
use crate::skin;
use crate::ui::Anim;

/// O fundo da área do painel: pedidos em curso e o último desenhado. Compartilhado com a
/// thread que desenha.
#[derive(Default)]
pub(super) struct BackdropState {
    /// Sobe a cada pedido; resultado de um pedido velho é descartado.
    generation: AtomicU32,
    /// Visual, largura e altura em pixels físicos e escala do fundo que está na tela.
    last: Mutex<Option<(String, u32, u32, u32)>>,
}

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
        let chatter = self.host.config.borrow().chatter.clone();
        self.ui.set_chatter_index(Pace::KEYS.iter().position(|k| *k == chatter).unwrap_or(1) as i32);
        self.apply_motion();
        self.apply_tray_choices();
        self.schedule_backdrop();
    }

    /// Opções e escolhas do ícone da bandeja (índice 0 = automático).
    pub(super) fn apply_tray_choices(&self) {
        let (icon, theme, skin_key) = {
            let cfg = self.host.config.borrow();
            (cfg.tray_icon.clone(), cfg.tray_theme.clone(), cfg.skin.clone())
        };
        // desenhos para o seletor: no tema escolhido para a bandeja ou, no automático, no
        // do visual da janela; o "Automático" (índice 0) fica sem desenho
        let shown = themes::THEMES.iter().find(|t| t.key == theme).unwrap_or(themes::get(skin::get(&skin_key).app_theme));
        let images: Vec<Image> =
            std::iter::once(Image::default()).chain(super::settings::mascot_images(shown)).collect();
        self.ui.set_tray_icon_images(ModelRc::new(VecModel::from(images)));
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
        crate::mascot::anim::set_calm(calm);
        self.ui.set_reduce_motion(calm);
        self.ui.global::<Anim>().set_calm(calm);
    }

    /// Cartão "Sistema" das configurações: iniciar com o sistema, versão e registros. Relido
    /// ao abrir as configurações (o autostart pode ter mudado por fora, no sistema).
    pub(super) fn apply_system(&self) {
        self.ui.set_autostart(crate::autostart::is_enabled());
        self.ui.set_version(crate::core::about::version_line().into());
        let dir = crate::core::os::log_dir().map(|d| d.display().to_string()).unwrap_or_default();
        self.ui.set_log_dir(dir.into());
    }

    /// O erro, se houver, vai para o registro; o interruptor mostra o estado de verdade.
    pub(super) fn toggle_autostart(&self) {
        let _ = crate::autostart::set(!crate::autostart::is_enabled());
        self.ui.set_autostart(crate::autostart::is_enabled());
    }

    pub(super) fn open_logs(&self) {
        if let Some(dir) = crate::core::os::log_dir()
            && !crate::core::exec::open_dir(&dir)
        {
            log::warn!("não consegui abrir {}", dir.display());
        }
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

    /// Quanto as mascotes conversam (índice em `Pace::KEYS`: nunca, às vezes, bastante).
    pub(super) fn choose_chatter(&self, i: usize) {
        let Some(key) = Pace::KEYS.get(i) else { return };
        {
            let mut cfg = self.host.config.borrow_mut();
            if cfg.chatter == *key {
                return;
            }
            cfg.chatter = key.to_string();
            cfg.save();
        }
        self.ui.set_chatter_index(i as i32);
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

    /// Pede um fundo novo daqui a pouco (um só, mesmo com vários pedidos seguidos). O
    /// desenho roda numa thread, e só quando o visual, o tamanho ou a escala mudaram:
    /// reabrir a janela não redesenha nada.
    pub(super) fn schedule_backdrop(&self) {
        let state = self.backdrop.clone();
        let id = state.generation.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
        let weak = self.ui.as_weak();
        let size = self.backdrop_size.get();
        let key = self.host.config.borrow().skin.clone();
        Timer::single_shot(Duration::from_millis(60), move || {
            if state.generation.load(Ordering::Relaxed) != id {
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
            let drawn = (key, (w * scale).round() as u32, (h * scale).round() as u32, scale.to_bits());
            if state.last.lock().is_ok_and(|last| last.as_ref() == Some(&drawn)) {
                return;
            }
            let (backdrop, bg) = (s.backdrop, s.bg);
            let spawned = std::thread::Builder::new().name("miniutils-backdrop".into()).spawn(move || {
                let buf = skin::backdrop::render(backdrop, bg, w, h, scale);
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    // um pedido mais novo já está a caminho: este fundo ficou velho
                    if state.generation.load(Ordering::Relaxed) != id {
                        return;
                    }
                    ui.set_backdrop(slint::Image::from_rgb8(buf));
                    if let Ok(mut last) = state.last.lock() {
                        *last = Some(drawn);
                    }
                });
            });
            if let Err(e) = spawned {
                log::warn!("sem thread para o fundo ({e})");
            }
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
    use chrono::Timelike;
    let now = chrono::Local::now();
    (now.hour() as i32, now.minute() as i32)
}
