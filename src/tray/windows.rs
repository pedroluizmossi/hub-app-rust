//! Icon in the Windows notification area (crate `tray-icon`).
//!
//! The icon and the menu live on the UI thread: the window's event loop delivers the
//! clicks. So the `Tray` never leaves it (the hub already keeps it there).

use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::core::miniapp::TrayMedia;
use crate::tr;
use crate::mascot::{Mood, render, sprites::Sprite, themes::Theme};
use crate::ui::HubWindow;

/// Windows cuts the icon tooltip at 127 characters.
const TOOLTIP_MAX: usize = 120;

pub struct Tray {
    icon: TrayIcon,
    pinned: std::cell::Cell<bool>,
    /// What the menu shows now (rebuilt only when it changes).
    media: std::cell::RefCell<Option<TrayMedia>>,
}

impl Tray {
    /// Creates the icon. Must be called on the UI thread.
    pub fn spawn(ui: slint::Weak<HubWindow>, sprite: &Sprite, theme: &Theme, mood: Mood) -> anyhow::Result<Self> {
        let icon = TrayIconBuilder::new()
            .with_icon(icon(sprite, theme, mood)?)
            .with_tooltip(crate::core::config::display_name())
            .with_menu(Box::new(menu(false, None)?))
            // left click opens the popup; the menu is on the right click
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
                // middle click: toggles pin mode
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
            // the hub takes the command to the app that owns the media
            "prev" => crate::hub::tray_media_command("prev"),
            "play-pause" => crate::hub::tray_media_command("play-pause"),
            "next" => crate::hub::tray_media_command("next"),
            _ => {}
        }));
        Ok(Tray { icon, pinned: Default::default(), media: Default::default() })
    }

    /// The icon is visible (the window can be hidden and brought back through it). On
    /// Windows the notification area always exists.
    pub fn online(&self) -> bool {
        true
    }

    /// Updates the tooltip and menu; the icon is only redrawn when `icon` is given (the
    /// mascot, theme or mood changed).
    pub fn update(&self, new_icon: Option<(&Sprite, &Theme, Mood)>, tooltip: String, media: Option<TrayMedia>) {
        if let Some((sprite, theme, mood)) = new_icon
            && let Ok(image) = icon(sprite, theme, mood)
        {
            let _ = self.icon.set_icon(Some(image));
        }
        let name = crate::core::config::display_name();
        let text = if tooltip.is_empty() { name } else { format!("{name}\n{tooltip}") };
        let _ = self.icon.set_tooltip(Some(text.chars().take(TOOLTIP_MAX).collect::<String>()));
        if *self.media.borrow() != media {
            *self.media.borrow_mut() = media;
            self.rebuild_menu();
        }
    }

    /// Updates the "Pin window" check mark in the menu.
    pub fn set_pinned(&self, pinned: bool) {
        if self.pinned.replace(pinned) != pinned {
            self.rebuild_menu();
        }
    }

    fn rebuild_menu(&self) {
        match menu(self.pinned.get(), self.media.borrow().as_ref()) {
            Ok(menu) => self.icon.set_menu(Some(Box::new(menu))),
            Err(e) => log::warn!("tray menu failed: {e}"),
        }
    }
}

/// The whole menu, in the given state. Items are found by id in the `MenuEvent`.
fn menu(pinned: bool, media: Option<&TrayMedia>) -> anyhow::Result<Menu> {
    let menu = Menu::new();
    menu.append(&MenuItem::with_id("toggle", tr!("Show / hide"), true, None))?;
    menu.append(&CheckMenuItem::with_id("pin", tr!("Pin window"), true, pinned, None))?;
    if let Some(media) = media {
        menu.append(&PredefinedMenuItem::separator())?;
        // in a Windows menu, `&` marks the shortcut letter: doubled, it is plain text
        menu.append(&MenuItem::new(media.summary.replace('&', "&&"), false, None))?;
        for (label, verb, enabled) in [
            (tr!("Previous track"), "prev", media.can_previous),
            (if media.playing { tr!("Pause") } else { tr!("Play") }, "play-pause", media.can_play_pause),
            (tr!("Next track"), "next", media.can_next),
        ] {
            menu.append(&MenuItem::with_id(verb, label, enabled, None))?;
        }
    }
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&MenuItem::with_id("quit", tr!("Quit"), true, None))?;
    Ok(menu)
}

/// The mascot at 32×32 (Windows scales it down to 16 on unscaled screens).
fn icon(sprite: &Sprite, theme: &Theme, mood: Mood) -> anyhow::Result<Icon> {
    let (side, rgba) = render::rgba_square(sprite, theme, mood, 1, 2);
    Ok(Icon::from_rgba(rgba, side as u32, side as u32)?)
}
