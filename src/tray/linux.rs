//! Tray icon (StatusNotifierItem over D-Bus): the mascot in the hub's mood.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ksni::blocking::TrayMethods;

use crate::mascot::{Mood, render, sprites::Sprite, themes::Theme};
use crate::ui::HubWindow;

use crate::core::miniapp::TrayMedia;
use crate::tr;

pub struct AppTray {
    icons: Vec<ksni::Icon>,
    tooltip: String,
    ui: slint::Weak<HubWindow>,
    /// Checks the "Pin window" menu item.
    pinned: bool,
    media: Option<TrayMedia>,
    /// A panel (`StatusNotifierWatcher`) is showing the icon right now.
    online: Arc<AtomicBool>,
    /// `spawn` returned: a `watcher_offline` from now on means the panel went away.
    spawned: Arc<AtomicBool>,
}

impl ksni::Tray for AppTray {
    // the panel may start after the app (autostart) or restart: ksni registers the icon
    // again when it comes back, and the hub only hides the window for good with a panel up
    fn watcher_online(&self) {
        self.online.store(true, Ordering::Relaxed);
    }

    fn watcher_offline(&self, _reason: ksni::OfflineReason) -> bool {
        let was_online = self.online.swap(false, Ordering::Relaxed);
        // the panel went away after the icon was up: a hidden window would be unreachable.
        // Not the first call, inside `spawn` (no panel at start): the hub decides that one.
        if was_online && self.spawned.load(Ordering::Relaxed) {
            crate::hub::tray_lost();
        }
        true
    }

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

    // middle click: toggles pin mode
    fn secondary_activate(&mut self, _x: i32, _y: i32) {
        let _ = self.ui.upgrade_in_event_loop(|ui| ui.invoke_toggle_pin());
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::*;
        let mut items: Vec<ksni::MenuItem<Self>> = vec![
            StandardItem {
                label: tr!("Show / hide").into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.ui.upgrade_in_event_loop(|ui| ui.invoke_toggle_requested(0, 0));
                }),
                ..Default::default()
            }
            .into(),
            CheckmarkItem {
                label: tr!("Pin window").into(),
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
            // D-BusMenu treats underscores as keyboard shortcuts.
            items.push(StandardItem {
                label: super::media_label(&media.summary).replace('_', "__"),
                enabled: false,
                ..Default::default()
            }.into());
            for (label, verb, enabled) in super::media_items(media) {
                items.push(StandardItem {
                    label: label.into(),
                    enabled,
                    // the hub takes the command to the app that owns the media
                    activate: Box::new(move |_: &mut Self| crate::hub::tray_media_command(verb)),
                    ..Default::default()
                }.into());
            }
        }
        items.extend([
            MenuItem::Separator,
            StandardItem {
                label: tr!("Quit").into(),
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
    // two crisp sizes (2× and 4×); the panel picks the closest
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
    online: Arc<AtomicBool>,
}

impl Tray {
    /// Creates the icon. With no panel up (still starting, or GNOME without the AppIndicator
    /// extension), the service waits and the icon shows up when a panel appears; `online`
    /// says whether it is visible now. Only fails without D-Bus.
    pub fn spawn(ui: slint::Weak<HubWindow>, sprite: &Sprite, theme: &Theme, mood: Mood) -> anyhow::Result<Self> {
        let online = Arc::new(AtomicBool::new(true));
        let spawned = Arc::new(AtomicBool::new(false));
        let tray = AppTray {
            icons: icons(sprite, theme, mood),
            tooltip: String::new(),
            ui,
            pinned: false,
            media: None,
            online: online.clone(),
            spawned: spawned.clone(),
        };
        // without a panel, ksni calls `watcher_offline` before returning from here
        let handle = tray.assume_sni_available(true).spawn()?;
        spawned.store(true, Ordering::Relaxed);
        Ok(Tray { handle, online })
    }

    /// The icon is on a panel now (the window can be hidden and brought back through it).
    pub fn online(&self) -> bool {
        self.online.load(Ordering::Relaxed)
    }

    /// Updates the tooltip and menu; the icon is only redrawn when `icon` is given (the
    /// mascot, theme or mood changed). ksni tells the panel only what actually changed.
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

    /// Updates the "Pin window" check mark in the menu.
    pub fn set_pinned(&self, pinned: bool) {
        self.handle.update(move |t| t.pinned = pinned);
    }
}
