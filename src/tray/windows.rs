//! Ícone na área de notificação do Windows (crate `tray-icon`).
//!
//! O ícone e o menu vivem na thread da UI: é o laço de eventos da janela que entrega os
//! cliques. Por isso o `Tray` não sai dela (o hub já o guarda lá).

use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::core::miniapp::TrayMedia;
use crate::mascot::{Mood, render, sprites::Sprite, themes::Theme};
use crate::ui::HubWindow;

/// O Windows corta a dica do ícone em 127 caracteres.
const TOOLTIP_MAX: usize = 120;

pub struct Tray {
    icon: TrayIcon,
    pinned: std::cell::Cell<bool>,
    /// O que o menu mostra agora (só é refeito quando muda).
    media: std::cell::RefCell<Option<TrayMedia>>,
}

impl Tray {
    /// Cria o ícone. Tem de ser chamado na thread da UI.
    pub fn spawn(ui: slint::Weak<HubWindow>, sprite: &Sprite, theme: &Theme, mood: Mood) -> anyhow::Result<Self> {
        let icon = TrayIconBuilder::new()
            .with_icon(icon(sprite, theme, mood)?)
            .with_tooltip(crate::core::config::display_name())
            .with_menu(Box::new(menu(false, None)?))
            // clique esquerdo abre o popup; o menu fica no direito
            .with_menu_on_left_click(false)
            .build()?;

        let weak = ui.clone();
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            let TrayIconEvent::Click { position, button, button_state: MouseButtonState::Up, .. } = event else { return };
            let (x, y) = (position.x.round() as i32, position.y.round() as i32);
            match button {
                MouseButton::Left => {
                    let _ = weak.upgrade_in_event_loop(move |ui| ui.invoke_toggle_requested(x, y));
                }
                // clique do meio: liga/desliga o modo fixar
                MouseButton::Middle => {
                    let _ = weak.upgrade_in_event_loop(|ui| ui.invoke_toggle_pin());
                }
                _ => {}
            }
        }));
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| match event.id.0.as_str() {
            "toggle" => {
                let _ = ui.upgrade_in_event_loop(|ui| ui.invoke_toggle_requested(0, 0));
            }
            "pin" => {
                let _ = ui.upgrade_in_event_loop(|ui| ui.invoke_toggle_pin());
            }
            "quit" => {
                let _ = ui.upgrade_in_event_loop(|ui| ui.invoke_quit());
            }
            // o hub leva o comando ao app dono da mídia
            "prev" => crate::hub::tray_media_command("prev"),
            "play-pause" => crate::hub::tray_media_command("play-pause"),
            "next" => crate::hub::tray_media_command("next"),
            _ => {}
        }));
        Ok(Tray { icon, pinned: Default::default(), media: Default::default() })
    }

    /// O ícone está visível (dá para esconder a janela e voltar por ele). No Windows a
    /// área de notificação existe sempre.
    pub fn online(&self) -> bool {
        true
    }

    /// Atualiza dica e menu; o ícone só é redesenhado quando `icon` vem (a mascote, o
    /// tema ou o humor mudaram).
    pub fn update(&self, new_icon: Option<(&Sprite, &Theme, Mood)>, tooltip: String, media: Option<TrayMedia>) {
        if let Some((sprite, theme, mood)) = new_icon {
            if let Ok(image) = icon(sprite, theme, mood) {
                let _ = self.icon.set_icon(Some(image));
            }
        }
        let name = crate::core::config::display_name();
        let text = if tooltip.is_empty() { name } else { format!("{name}\n{tooltip}") };
        let _ = self.icon.set_tooltip(Some(text.chars().take(TOOLTIP_MAX).collect::<String>()));
        if *self.media.borrow() != media {
            *self.media.borrow_mut() = media;
            self.rebuild_menu();
        }
    }

    /// Atualiza a marca de "Fixar janela" no menu.
    pub fn set_pinned(&self, pinned: bool) {
        if self.pinned.replace(pinned) != pinned {
            self.rebuild_menu();
        }
    }

    fn rebuild_menu(&self) {
        match menu(self.pinned.get(), self.media.borrow().as_ref()) {
            Ok(menu) => self.icon.set_menu(Some(Box::new(menu))),
            Err(e) => eprintln!("miniutils: menu da bandeja falhou: {e}"),
        }
    }
}

/// O menu inteiro, no estado dado. Os itens são achados pelo id no `MenuEvent`.
fn menu(pinned: bool, media: Option<&TrayMedia>) -> anyhow::Result<Menu> {
    let menu = Menu::new();
    menu.append(&MenuItem::with_id("toggle", "Mostrar / esconder", true, None))?;
    menu.append(&CheckMenuItem::with_id("pin", "Fixar janela", true, pinned, None))?;
    if let Some(media) = media {
        menu.append(&PredefinedMenuItem::separator())?;
        // no menu do Windows, `&` marca a letra de atalho: dobrado, vira texto
        menu.append(&MenuItem::new(media.summary.replace('&', "&&"), false, None))?;
        for (label, verb, enabled) in [
            ("Faixa anterior", "prev", media.can_previous),
            (if media.playing { "Pausar" } else { "Tocar" }, "play-pause", media.can_play_pause),
            ("Próxima faixa", "next", media.can_next),
        ] {
            menu.append(&MenuItem::with_id(verb, label, enabled, None))?;
        }
    }
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&MenuItem::with_id("quit", "Sair", true, None))?;
    Ok(menu)
}

/// A mascote em 32×32 (o Windows reduz para 16 em telas sem escala).
fn icon(sprite: &Sprite, theme: &Theme, mood: Mood) -> anyhow::Result<Icon> {
    let (side, rgba) = render::rgba_square(sprite, theme, mood, 1, 2);
    Ok(Icon::from_rgba(rgba, side as u32, side as u32)?)
}
