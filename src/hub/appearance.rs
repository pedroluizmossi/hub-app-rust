//! Appearance: window skin, backdrop, tray icon, motion and clock.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use slint::{ComponentHandle, Image, ModelRc, SharedString, Timer, VecModel};

use super::Hub;
use super::chatter::Pace;
use crate::core::config::Config;
use crate::core::i18n::{self, Language};
use crate::core::plugins;
use crate::mascot::{sprites, themes};
use crate::popup;
use crate::skin;
use crate::tr;
use crate::ui::{AmbientItem, Anim};

/// Text sizes offered (in % of the design), in the order of the picker.
pub(super) const TEXT_SIZES: &[u32] = &[90, 100, 110, 125];

/// Where the pages go (`nav` in the config), in the order of the choice in Appearance › Pages
/// (`ui/settings.slint`): bar on the left, on the right, at the bottom, tabs in the header.
/// `"side"` is the bar on the left (old name, kept for the configs that already exist).
pub(super) const NAVS: &[&str] = &["side", "right", "bottom", "top"];

/// Position of `nav` in `NAVS`; an unknown value (config edited by hand) becomes the left bar.
fn nav_index(nav: &str) -> usize {
    NAVS.iter().position(|n| *n == nav).unwrap_or(0)
}

/// The backdrop of the panel area: requests in progress and the last one drawn. Shared with
/// the drawing thread.
#[derive(Default)]
pub(super) struct BackdropState {
    /// Goes up with each request; the result of an old request is dropped.
    generation: AtomicU32,
    /// Skin (with the accent), width and height in physical pixels, and scale of the backdrop
    /// on screen.
    last: Mutex<Option<(String, u32, u32, u32)>>,
}

impl BackdropState {
    /// Forgets the last backdrop: the next request redraws even with the same skin (a backdrop
    /// file may have changed).
    pub(super) fn forget(&self) {
        if let Ok(mut last) = self.last.lock() {
            *last = None;
        }
    }
}

impl Hub {
    /// Applies the config's skin (with the accent color and the text size): screen tokens,
    /// picker lists and backdrop.
    pub(super) fn apply_skin(&self) {
        let (key, name, accent, scale, ambient) = {
            let cfg = self.host.config.borrow();
            (cfg.skin.clone(), crate::core::config::display_name(), cfg.accent.clone(), cfg.text_scale, cfg.ambient)
        };
        let effective = skin::effective(skin::get(&key), &accent);
        skin::apply(&self.ui, &effective, &name, text_scale(scale));
        self.ui.set_app_name(name.into());
        let labels: Vec<SharedString> = skin::all().iter().map(|s| s.name().into()).collect();
        self.ui.set_skins(ModelRc::new(VecModel::from(labels)));
        self.ui.set_skin_index(skin::index_of(&key) as i32);
        let accents: Vec<SharedString> =
            std::iter::once(tr!("Skin's own").into()).chain(skin::ACCENTS.iter().map(|a| i18n::tr(a.1).into())).collect();
        self.ui.set_accents(ModelRc::new(VecModel::from(accents)));
        self.ui.set_accent_index(skin::ACCENTS.iter().position(|a| a.0 == accent).map_or(0, |i| i as i32 + 1));
        let sizes: Vec<SharedString> = TEXT_SIZES.iter().map(|p| format!("{p}%").into()).collect();
        self.ui.set_text_sizes(ModelRc::new(VecModel::from(sizes)));
        self.ui.set_text_size_index(TEXT_SIZES.iter().position(|p| *p == scale).unwrap_or(1) as i32);
        self.ui.set_ambient_on(ambient);
        self.apply_nav();
        let chatter = self.host.config.borrow().chatter.clone();
        self.ui.set_chatter_index(Pace::KEYS.iter().position(|k| *k == chatter).unwrap_or(1) as i32);
        let language = self.host.config.borrow().language.clone();
        self.ui.set_language_index(Language::KEYS.iter().position(|k| *k == language).unwrap_or(0) as i32);
        self.apply_motion();
        self.apply_tray_choices();
        self.schedule_backdrop();
    }

    /// Options and choices of the tray icon (index 0 = automatic).
    pub(super) fn apply_tray_choices(&self) {
        let (icon, theme, skin_key) = {
            let cfg = self.host.config.borrow();
            (cfg.tray_icon.clone(), cfg.tray_theme.clone(), cfg.skin.clone())
        };
        // drawings for the picker: in the theme chosen for the tray or, when automatic, in the
        // window skin's; "Automatic" (index 0) has no drawing
        let shown = themes::find(&theme).unwrap_or(themes::get(skin::get(&skin_key).app_theme));
        let images: Vec<Image> =
            std::iter::once(Image::default()).chain(super::settings::mascot_images(shown)).collect();
        self.ui.set_tray_icon_images(ModelRc::new(VecModel::from(images)));
        let auto = |label: &str| std::iter::once(SharedString::from(label));
        let icons: Vec<SharedString> =
            auto(tr!("Automatic")).chain(sprites::all().iter().map(|s| s.name().into())).collect();
        let themes_: Vec<SharedString> =
            auto(tr!("Automatic")).chain(themes::all().iter().map(|t| t.name().into())).collect();
        let pos = |found: Option<usize>| found.map_or(0, |i| i as i32 + 1);
        self.ui.set_tray_icons(ModelRc::new(VecModel::from(icons)));
        self.ui.set_tray_icon_index(pos(sprites::all().iter().position(|s| s.key == icon)));
        self.ui.set_tray_themes(ModelRc::new(VecModel::from(themes_)));
        self.ui.set_tray_theme_index(pos(themes::all().iter().position(|t| t.key == theme)));
    }

    /// Changes the tray icon's mascot. 0 = automatic (the app in the worst state).
    pub(super) fn choose_tray_icon(&self, i: usize) {
        let key = if i == 0 { Some("auto") } else { sprites::all().get(i - 1).map(|s| s.key) };
        self.set_tray_choice(key, |cfg| &mut cfg.tray_icon);
    }

    /// Changes the tray icon's theme. 0 = automatic (the theme of the app in the worst state).
    pub(super) fn choose_tray_theme(&self, i: usize) {
        let key = if i == 0 { Some("auto") } else { themes::all().get(i - 1).map(|t| t.key) };
        self.set_tray_choice(key, |cfg| &mut cfg.tray_theme);
    }

    /// Saves a tray icon choice; the icon itself changes on the next `sync_tray`.
    fn set_tray_choice(&self, key: Option<&str>, field: impl FnOnce(&mut Config) -> &mut String) {
        let Some(key) = key else { return };
        {
            let mut cfg = self.host.config.borrow_mut();
            let slot = field(&mut cfg);
            if slot == key {
                return;
            }
            *slot = key.into();
            cfg.save();
        }
        self.apply_tray_choices();
    }

    /// Reduce motion: mascots stand still on screen (`Anim.calm`); the apps stop firing
    /// reactions (they read `reduce_motion` from the config). Also "still on hover", which
    /// works on its own: the mascot does not perk up when its card is hovered.
    pub(super) fn apply_motion(&self) {
        let (calm, still) = {
            let cfg = self.host.config.borrow();
            (cfg.reduce_motion, cfg.still_on_hover)
        };
        crate::mascot::anim::set_calm(calm);
        self.ui.set_reduce_motion(calm);
        self.ui.set_still_on_hover(still);
        let anim = self.ui.global::<Anim>();
        anim.set_calm(calm);
        anim.set_still_on_hover(still);
    }

    /// "System" card of the settings: start with the system, version and logs. Read again
    /// when opening the settings (autostart may have changed outside, in the system).
    pub(super) fn apply_system(&self) {
        self.ui.set_autostart(crate::autostart::is_enabled());
        self.ui.set_autostart_error("".into());
        self.apply_window_mode();
        self.ui.set_version(crate::core::about::version_line().into());
        let dir = crate::core::os::log_dir().map(|d| d.display().to_string()).unwrap_or_default();
        self.ui.set_log_dir(dir.into());
        self.apply_apps_dir();
    }

    /// "Local apps" row of the System card: the folder, how many apps it has and the folders
    /// refused, with the reason (folder named differently from the id, broken manifest…).
    pub(super) fn apply_apps_dir(&self) {
        let root = plugins::dir();
        let (list, skipped) = plugins::scan(&root);
        let from_store = list.iter().filter(|p| crate::store::installed(&root, &p.manifest.id).is_some()).count();
        let local = list.len() - from_store;
        let summary = match (local, from_store) {
            (0, 0) => tr!("no apps in the folder").to_string(),
            _ => tr!("local apps: {} · from the store: {}", local, from_store),
        };
        let skipped: Vec<String> = skipped.iter().map(|s| tr!("{}/ skipped: {}", s.folder, s.reason)).collect();
        self.ui.set_apps_dir(root.display().to_string().into());
        self.ui.set_apps_summary(summary.into());
        self.ui.set_apps_skipped(skipped.join("\n").into());
    }

    /// Opens the apps folder in the file manager (creating it first, if it doesn't exist yet).
    pub(super) fn open_apps_dir(&self) {
        let dir = plugins::dir();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            log::warn!("could not create {}: {e}", dir.display());
        } else if !crate::core::exec::open_dir(&dir) {
            log::warn!("could not open {}", dir.display());
        }
    }

    /// Where the window opens and whether it is a desktop app, on the screen (the System
    /// card and the window's own "on top").
    fn apply_window_mode(&self) {
        let cfg = self.host.config.borrow();
        let place = popup::Place::index_of(&cfg.window_place).unwrap_or(0);
        self.ui.set_place_index(place as i32);
        self.ui.set_desktop_app(cfg.desktop_app);
        // on native Wayland the system decides where windows go and what the taskbar shows
        self.ui.set_can_place(self.positionable);
    }

    /// Where the window opens: index in `popup::Place::KEYS`. The window goes there now,
    /// so the choice shows at once.
    pub(super) fn choose_place(&self, i: usize) {
        let Some(key) = popup::Place::KEYS.get(i).copied() else { return };
        {
            let mut cfg = self.host.config.borrow_mut();
            if cfg.window_place == key {
                return;
            }
            // "where it was left" starts from where it is now
            if key == "last" {
                drop(cfg);
                self.remember_position();
                cfg = self.host.config.borrow_mut();
            }
            cfg.window_place = key.into();
            cfg.save();
        }
        self.apply_window_mode();
        if self.positionable && self.shown.get() {
            self.place_window(self.last_anchor.get(), true);
        }
    }

    /// Desktop app on/off: in the taskbar, not on top and not hiding when it loses focus.
    pub(super) fn toggle_desktop_app(&self) {
        {
            let mut cfg = self.host.config.borrow_mut();
            cfg.desktop_app = !cfg.desktop_app;
            cfg.save();
        }
        self.apply_window_mode();
        // pin mode only makes sense for the popup
        if self.desktop_app() {
            self.set_pinned(false);
        }
        if self.positionable && self.shown.get() {
            self.place_window(self.last_anchor.get(), false);
        }
    }

    /// The switch shows the real state; an error also shows under the row (and goes to the log).
    pub(super) fn toggle_autostart(&self) {
        let error = match crate::autostart::set(!crate::autostart::is_enabled()) {
            Ok(()) => String::new(),
            Err(e) => tr!("Could not change it: {}", e),
        };
        self.ui.set_autostart_error(error.into());
        self.ui.set_autostart(crate::autostart::is_enabled());
    }

    pub(super) fn open_logs(&self) {
        if let Some(dir) = crate::core::os::log_dir()
            && !crate::core::exec::open_dir(&dir)
        {
            log::warn!("could not open {}", dir.display());
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

    pub(super) fn toggle_still_on_hover(&self) {
        {
            let mut cfg = self.host.config.borrow_mut();
            cfg.still_on_hover = !cfg.still_on_hover;
            cfg.save();
        }
        self.apply_motion();
    }

    /// Where the pages go: index in `NAVS`.
    pub(super) fn choose_nav(&self, i: usize) {
        let Some(nav) = NAVS.get(i).copied() else { return };
        {
            let mut cfg = self.host.config.borrow_mut();
            if cfg.nav == nav {
                return;
            }
            cfg.nav = nav.into();
            cfg.save();
        }
        // the panel area changes size and reports it by itself (`backdrop-resized`)
        self.apply_nav();
    }

    fn apply_nav(&self) {
        let i = nav_index(&self.host.config.borrow().nav);
        self.ui.set_nav(NAVS[i].into());
        self.ui.set_nav_index(i as i32);
    }

    /// How much the mascots chat (index in `Pace::KEYS`: never, sometimes, a lot).
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

    /// Interface language (index in `Language::KEYS`: auto, en, pt-BR). The compiled UI
    /// switches at once; texts built in Rust as the screens refresh; manifests and plugin
    /// views on the next start.
    pub(super) fn choose_language(&self, i: usize) {
        let Some(key) = Language::KEYS.get(i) else { return };
        {
            let mut cfg = self.host.config.borrow_mut();
            if cfg.language == *key {
                return;
            }
            cfg.language = key.to_string();
            cfg.save();
        }
        crate::core::i18n::set(Language::from_setting(key));
        self.ui.set_language_index(i as i32);
        // `apply_skin` also redoes the tray choices
        self.apply_skin();
        self.apply_system();
        // the Settings list has texts built in Rust: forget its fingerprint so it is redone
        self.settings_fingerprint.replace(String::new());
        self.rebuild_lists();
        // the greeting only changes with the minute, and the store rows with a push
        self.clock.borrow_mut().clear();
        self.update_clock();
        self.store_push();
    }

    /// Changes the accent color (0 = the skin's own).
    pub(super) fn choose_accent(&self, i: usize) {
        let key = if i == 0 { "" } else { skin::ACCENTS.get(i - 1).map_or("", |a| a.0) };
        {
            let mut cfg = self.host.config.borrow_mut();
            if cfg.accent == key {
                return;
            }
            cfg.accent = key.into();
            cfg.save();
        }
        self.apply_skin();
    }

    /// Changes the text size. The apps redo their screens (plugins get the scale in `theme`).
    pub(super) fn choose_text_size(&self, i: usize) {
        let Some(&pct) = TEXT_SIZES.get(i) else { return };
        {
            let mut cfg = self.host.config.borrow_mut();
            if cfg.text_scale == pct {
                return;
            }
            cfg.text_scale = pct;
            cfg.save();
        }
        self.apply_skin();
        for app in self.registry.enabled() {
            app.borrow_mut().settings_changed(&self.ui);
        }
    }

    /// Turns the animated backdrop on or off.
    pub(super) fn toggle_ambient(&self) {
        let on = {
            let mut cfg = self.host.config.borrow_mut();
            cfg.ambient = !cfg.ambient;
            cfg.save();
            cfg.ambient
        };
        self.ui.set_ambient_on(on);
    }

    /// Reads the folder of themes, skins and backdrops again and shows the result in the
    /// "Your themes, skins and backdrops" row (without applying it: see `reload_looks`).
    pub(super) fn load_looks(&self) {
        let loaded = skin::user::reload();
        self.ui.set_looks_dir(skin::user::dir().display().to_string().into());
        self.ui.set_looks_summary(loaded.summary().into());
        let skipped: Vec<String> = loaded.skipped.iter().map(|s| format!("{}: {}", s.file, s.reason)).collect();
        self.ui.set_looks_skipped(skipped.join("\n").into());
    }

    /// Reload button of Appearance (and the reload command line flag): reads the files again
    /// and redraws everything that uses a theme, skin or backdrop.
    pub(super) fn reload_looks(&self) {
        self.load_looks();
        self.backdrop.forget();
        self.apply_skin();
        for app in self.registry.enabled() {
            app.borrow_mut().settings_changed(&self.ui);
        }
        // the Settings list changes even without a config change (new theme options)
        self.settings_fingerprint.replace(String::new());
        self.rebuild_lists();
    }

    /// Opens the folder of themes, skins, backdrops, sprites and mascots (creating the
    /// subfolders first).
    pub(super) fn open_looks_dir(&self) {
        let root = skin::user::dir();
        for sub in [skin::user::THEMES, skin::user::SKINS, skin::user::BACKDROPS, crate::mascot::file::DIR, crate::mascot::pack::DIR] {
            if let Err(e) = std::fs::create_dir_all(root.join(sub)) {
                log::warn!("could not create {}: {e}", root.join(sub).display());
                return;
            }
        }
        if !crate::core::exec::open_dir(&root) {
            log::warn!("could not open {}", root.display());
        }
    }

    /// Changes the skin. The apps with the "auto" theme change along.
    pub(super) fn choose_skin(&self, i: usize) {
        let all = skin::all();
        let Some(s) = all.get(i) else { return };
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

    /// Requests a new backdrop shortly (just one, even with several requests in a row). The
    /// drawing runs in a thread, and only when the skin, the size or the scale changed:
    /// reopening the window redraws nothing.
    pub(super) fn schedule_backdrop(&self) {
        let state = self.backdrop.clone();
        let id = state.generation.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
        let weak = self.ui.as_weak();
        let size = self.backdrop_size.get();
        let (key, accent) = {
            let cfg = self.host.config.borrow();
            (cfg.skin.clone(), cfg.accent.clone())
        };
        Timer::single_shot(Duration::from_millis(60), move || {
            if state.generation.load(Ordering::Relaxed) != id {
                return;
            }
            let Some(ui) = weak.upgrade() else { return };
            let s = skin::effective(skin::get(&key), &accent);
            let window = ui.window();
            let scale = window.scale_factor();
            // without the exact size of the area yet: estimate from the window
            let (w, h) = if size.0 > 0.0 && size.1 > 0.0 {
                size
            } else {
                let px = window.size();
                let (ww, wh) = (px.width as f32 / scale, px.height as f32 / scale);
                let chrome = 2.0 * s.border + s.header_h + s.header_line + s.panel_top_h + s.footer_h;
                ((ww - 2.0 * s.border).max(1.0), (wh - chrome).max(1.0))
            };
            let drawn = (format!("{key}|{accent}"), (w * scale).round() as u32, (h * scale).round() as u32, scale.to_bits());
            if state.last.lock().is_ok_and(|last| last.as_ref() == Some(&drawn)) {
                return;
            }
            let backdrop = skin::backdrop::get(s.backdrop);
            let spawned = std::thread::Builder::new().name("miniutils-backdrop".into()).spawn(move || {
                let buf = skin::backdrop::render(backdrop, &s, w, h, scale);
                let ambient = skin::backdrop::ambient(backdrop, &s, w, h);
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    // a newer request is already on its way: this backdrop is stale
                    if state.generation.load(Ordering::Relaxed) != id {
                        return;
                    }
                    ui.set_backdrop(slint::Image::from_rgb8(buf));
                    let items: Vec<AmbientItem> = ambient.iter().map(skin::backdrop::Ambient::item).collect();
                    ui.set_ambient(ModelRc::new(VecModel::from(items)));
                    if let Ok(mut last) = state.last.lock() {
                        *last = Some(drawn);
                    }
                });
            });
            if let Err(e) = spawned {
                log::warn!("no thread for the backdrop ({e})");
            }
        });
    }

    /// Clock and greeting, updated when the minute changes.
    pub(super) fn update_clock(&self) {
        let (hour, min) = local_time();
        let text = format!("{hour:02}:{min:02}");
        if *self.clock.borrow() == text {
            return;
        }
        let part = match hour {
            5..=11 => tr!("good morning"),
            12..=17 => tr!("good afternoon"),
            _ => tr!("good evening"),
        };
        self.ui.set_greeting(format!("{part} · {text}").into());
        self.ui.set_clock(text.clone().into());
        *self.clock.borrow_mut() = text;
    }
}

/// Local hour and minute.
fn local_time() -> (i32, i32) {
    use chrono::Timelike;
    let now = chrono::Local::now();
    (now.hour() as i32, now.minute() as i32)
}

/// % from the config to the screen factor, within the sizes offered.
fn text_scale(pct: u32) -> f32 {
    pct.clamp(TEXT_SIZES[0], TEXT_SIZES[TEXT_SIZES.len() - 1]) as f32 / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_of_the_pages() {
        assert_eq!(nav_index("side"), 0);
        assert_eq!(nav_index("right"), 1);
        assert_eq!(nav_index("bottom"), 2);
        assert_eq!(nav_index("top"), 3);
        // config edited by hand: back to the bar on the left
        assert_eq!(nav_index("left"), 0);
    }
}
