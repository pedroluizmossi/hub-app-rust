//! Ícone na bandeja (StatusNotifierItem via D-Bus): a mascote no humor do hub.

use ksni::blocking::TrayMethods;
use slint::ComponentHandle;

use crate::mascot::{Mood, render, sprites::Sprite, themes::Theme};
use crate::ui::HubWindow;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayMedia {
    pub summary: String,
    pub playing: bool,
    pub can_play_pause: bool,
    pub can_previous: bool,
    pub can_next: bool,
}

pub struct AppTray {
    icons: Vec<ksni::Icon>,
    tooltip: String,
    ui: slint::Weak<HubWindow>,
    /// Marca o item "Fixar janela" do menu.
    pinned: bool,
    media: Option<TrayMedia>,
}

impl ksni::Tray for AppTray {
    fn id(&self) -> String {
        "miniutils".into()
    }

    fn title(&self) -> String {
        crate::core::config::display_name()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        self.icons.clone()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip { title: crate::core::config::display_name(), description: self.tooltip.clone(), ..Default::default() }
    }

    fn activate(&mut self, x: i32, y: i32) {
        let _ = self.ui.upgrade_in_event_loop(move |ui| ui.invoke_toggle_requested(x, y));
    }

    // clique do meio: liga/desliga o modo fixar
    fn secondary_activate(&mut self, _x: i32, _y: i32) {
        let _ = self.ui.upgrade_in_event_loop(|ui| ui.invoke_toggle_pin());
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::*;
        let mut items: Vec<ksni::MenuItem<Self>> = vec![
            StandardItem {
                label: "Mostrar / esconder".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.ui.upgrade_in_event_loop(|ui| ui.invoke_toggle_requested(0, 0));
                }),
                ..Default::default()
            }
            .into(),
            CheckmarkItem {
                label: "Fixar janela".into(),
                checked: self.pinned,
                activate: Box::new(|t: &mut Self| {
                    let _ = t.ui.upgrade_in_event_loop(|ui| ui.invoke_toggle_pin());
                }),
                ..Default::default()
            }
            .into(),
        ];
        if let Some(media) = &self.media {
            items.push(MenuItem::Separator);
            // D-BusMenu interpreta sublinhados como atalhos de teclado.
            items.push(StandardItem {
                label: media.summary.replace('_', "__"),
                enabled: false,
                ..Default::default()
            }.into());
            for (label, verb, enabled) in [
                ("Faixa anterior", "prev", media.can_previous),
                (if media.playing { "Pausar" } else { "Tocar" }, "play-pause", media.can_play_pause),
                ("Próxima faixa", "next", media.can_next),
            ] {
                items.push(StandardItem {
                    label: label.into(),
                    enabled,
                    activate: Box::new(move |t: &mut Self| {
                        let _ = t.ui.upgrade_in_event_loop(move |ui| {
                            ui.global::<crate::ui::CdPlayerApp>().invoke_command(verb.into());
                        });
                    }),
                    ..Default::default()
                }.into());
            }
        }
        items.extend([
            MenuItem::Separator,
            StandardItem {
                label: "Sair".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.ui.upgrade_in_event_loop(|ui| ui.invoke_quit());
                }),
                ..Default::default()
            }
            .into(),
        ]);
        items
    }
}

fn icons(sprite: &Sprite, theme: &Theme, mood: Mood) -> Vec<ksni::Icon> {
    // dois tamanhos nítidos (2× e 4×); o painel escolhe o mais próximo
    [2, 4]
        .into_iter()
        .map(|scale| {
            let (side, data) = render::argb_square(sprite, theme, mood, 1, scale);
            ksni::Icon { width: side, height: side, data }
        })
        .collect()
}

pub struct Tray {
    handle: ksni::blocking::Handle<AppTray>,
}

impl Tray {
    /// Cria o ícone. Falha se não houver bandeja (ex.: GNOME sem a extensão AppIndicator).
    pub fn spawn(ui: slint::Weak<HubWindow>, sprite: &Sprite, theme: &Theme, mood: Mood) -> anyhow::Result<Self> {
        let tray = AppTray { icons: icons(sprite, theme, mood), tooltip: String::new(), ui, pinned: false, media: None };
        let handle = tray.spawn()?;
        Ok(Tray { handle })
    }

    /// Atualiza tooltip e menu; o ícone só é redesenhado quando `icon` vem (a mascote, o
    /// tema ou o humor mudaram). O ksni avisa o painel só do que mudou de fato.
    pub fn update(&self, icon: Option<(&Sprite, &Theme, Mood)>, tooltip: String, media: Option<TrayMedia>) {
        let icons = icon.map(|(sprite, theme, mood)| icons(sprite, theme, mood));
        self.handle.update(move |t| {
            if let Some(icons) = icons {
                t.icons = icons;
            }
            t.tooltip = tooltip;
            t.media = media;
        });
    }

    /// Atualiza a marca de "Fixar janela" no menu.
    pub fn set_pinned(&self, pinned: bool) {
        self.handle.update(move |t| t.pinned = pinned);
    }
}
