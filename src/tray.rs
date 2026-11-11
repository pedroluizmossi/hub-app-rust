//! Ícone na bandeja (StatusNotifierItem via D-Bus): a mascote no humor do hub.

use ksni::blocking::TrayMethods;

use crate::mascot::{Mood, render, sprites::Sprite, themes::Theme};
use crate::ui::HubWindow;

pub struct CyberTray {
    icons: Vec<ksni::Icon>,
    tooltip: String,
    ui: slint::Weak<HubWindow>,
    /// Marca o item "Fixar janela" do menu.
    pinned: bool,
}

impl ksni::Tray for CyberTray {
    fn id(&self) -> String {
        "cyberhub".into()
    }

    fn title(&self) -> String {
        "CyberHub".into()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        self.icons.clone()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip { title: "CyberHub".into(), description: self.tooltip.clone(), ..Default::default() }
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
        vec![
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
        ]
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
    handle: ksni::blocking::Handle<CyberTray>,
}

impl Tray {
    /// Cria o ícone. Falha se não houver bandeja (ex.: GNOME sem a extensão AppIndicator).
    pub fn spawn(ui: slint::Weak<HubWindow>, sprite: &Sprite, theme: &Theme, mood: Mood) -> anyhow::Result<Self> {
        let tray = CyberTray { icons: icons(sprite, theme, mood), tooltip: String::new(), ui, pinned: false };
        let handle = tray.spawn()?;
        Ok(Tray { handle })
    }

    pub fn update(&self, sprite: &Sprite, theme: &Theme, mood: Mood, tooltip: String) {
        let icons = icons(sprite, theme, mood);
        self.handle.update(move |t| {
            t.icons = icons;
            t.tooltip = tooltip;
        });
    }

    /// Atualiza a marca de "Fixar janela" no menu.
    pub fn set_pinned(&self, pinned: bool) {
        self.handle.update(move |t| t.pinned = pinned);
    }
}
