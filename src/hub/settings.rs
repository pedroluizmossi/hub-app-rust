//! Settings tab: the list of apps and the form generated from each one's manifest.

use std::cell::RefCell;
use std::collections::HashMap;

use slint::{Image, ModelRc, SharedString, VecModel};

use super::Hub;
use crate::core::manifest::{Manifest, SettingKind};
use crate::core::miniapp::AppRef;
use crate::mascot::themes::Theme;
use crate::mascot::{Mood, render, sprites, themes};
use crate::tr;
use crate::ui::{AppInfo, SettingRow};

thread_local! {
    /// Mascots already drawn for the pickers, by theme (the UI only exists on this thread).
    /// The key carries the generation of the imported ones: the folder changed, draw again.
    static GALLERY: RefCell<HashMap<(&'static str, u64), Vec<Image>>> = RefCell::new(HashMap::new());
}

/// All mascots drawn in theme `theme`, in the order of `sprites::all`: what the mascot
/// pickers show. Drawn once per theme and reused.
pub(super) fn mascot_images(theme: &'static Theme) -> Vec<Image> {
    GALLERY.with(|g| {
        g.borrow_mut()
            .entry((theme.key, sprites::generation()))
            .or_insert_with(|| sprites::all().iter().map(|s| render::image(s, theme, Mood::Idle, 1)).collect())
            .clone()
    })
}

/// Drawing of each alternative of a mascot choice ("auto" has no drawing, and the picker
/// shows a sparkle). Empty if the option is not a mascot one: then the screen uses the list.
fn option_images(options: &[String], theme: &'static Theme) -> Vec<Image> {
    if !sprites::is_choice(options) {
        return Vec::new();
    }
    let all = mascot_images(theme);
    let keys = sprites::all();
    options
        .iter()
        .map(|o| keys.iter().position(|s| s.key == *o).map_or_else(Image::default, |i| all[i].clone()))
        .collect()
}

/// What the user did in a field of the Apps tab.
pub(super) enum Input {
    Toggle,
    Int(i64),
    Choice(usize),
    /// What was typed in a `text` or `secret` field (Enter or leaving the field).
    Text(String),
}

impl Hub {
    pub(super) fn app_info(&self, app: &AppRef) -> AppInfo {
        let a = app.borrow();
        let m = a.manifest();
        let settings = self.settings_of(m);
        let theme = self.host.theme_for(m);
        let (data, has_data) = super::data::describe(&m.id, &a.data_spec());
        AppInfo {
            id: m.id.clone().into(),
            name: m.name.clone().into(),
            description: m.description.clone().into(),
            icon: render::image(sprites::get(&m.icon), theme, Mood::Idle, 1),
            enabled: self.host.config.borrow().is_enabled(&m.id),
            mood: a.mood().as_str().into(),
            theme: render::colors(theme),
            settings: ModelRc::new(VecModel::from(settings)),
            page: self.host.config.borrow().page_of(&m.id).unwrap_or(0) as i32,
            origin: origin(&m.id).into(),
            data: data.into(),
            has_data,
        }
    }

    /// Panel grid, sidebar and the Apps tab list.
    pub(super) fn rebuild_lists(&self) {
        self.rebuild_grid();
        self.rebuild_pages(true);
        self.rebuild_settings();
    }

    /// The Settings list, each app with the form generated from its manifest.
    ///
    /// Only redone when what it shows changes: redoing the list recreates the fields and
    /// would close an open select (which used to happen on every CPU reading).
    pub(super) fn rebuild_settings(&self) {
        let fingerprint = {
            let cfg = self.host.config.borrow();
            // the loaded ids too: a local app comes in disabled (without changing the config)
            let ids: Vec<String> = self.registry.all().iter().map(|a| a.borrow().manifest().id.clone()).collect();
            format!("{ids:?}|{:?}|{:?}|{}|{:?}", cfg.enabled, cfg.apps, cfg.skin, cfg.pages)
        };
        if self.settings_fingerprint.replace(fingerprint.clone()) == fingerprint {
            return;
        }
        let apps: Vec<AppInfo> = self.registry.all().iter().map(|a| self.app_info(a)).collect();
        self.ui.set_apps(ModelRc::new(VecModel::from(apps)));
        // where the Mascots app can put a mascot: the enabled apps' mascot settings
        let mut targets = Vec::new();
        for app in self.registry.enabled() {
            let a = app.borrow();
            let m = a.manifest();
            // the manifest's texts are already in the interface language (Manifest::parse)
            for s in m.settings.iter().filter(|s| sprites::is_choice(&s.live_options())) {
                targets.push(crate::core::miniapp::MascotTarget {
                    app: m.id.clone(),
                    name: m.name.clone(),
                    setting: s.key.clone(),
                    label: s.label.clone(),
                });
            }
        }
        crate::core::miniapp::set_mascot_targets(targets);
    }

    pub(super) fn settings_of(&self, m: &Manifest) -> Vec<SettingRow> {
        let cfg = self.host.config.borrow();
        let theme = self.host.theme_for(m);
        m.settings
            .iter()
            .map(|s| {
                let v = cfg.value(m, &s.key);
                // the unit goes next to the number (only a stepper shows it)
                let (label, unit) =
                    if matches!(s.kind, SettingKind::Int) { split_unit(&s.label) } else { (s.label.clone(), String::new()) };
                let options = s.live_options();
                SettingRow {
                    app: m.id.clone().into(),
                    key: s.key.clone().into(),
                    label: label.into(),
                    unit: unit.into(),
                    kind: s.kind.as_str().into(),
                    bool_value: v.as_bool().unwrap_or(false),
                    int_value: v.as_integer().unwrap_or(0) as i32,
                    minimum: s.min.unwrap_or(0) as i32,
                    maximum: s.max.unwrap_or(i32::MAX as i64) as i32,
                    step: s.step.unwrap_or(1) as i32,
                    options: ModelRc::new(VecModel::from(
                        options.iter().map(|o| SharedString::from(option_label(o))).collect::<Vec<_>>(),
                    )),
                    index: options.iter().position(|o| Some(o.as_str()) == v.as_str()).unwrap_or(0) as i32,
                    images: ModelRc::new(VecModel::from(option_images(&options, theme))),
                    text_value: if s.kind.is_text() { v.as_str().unwrap_or_default().into() } else { SharedString::new() },
                }
            })
            .collect()
    }

    pub(super) fn change_setting(&self, app_id: &str, key: &str, input: Input) {
        let Some(app) = self.registry.find(app_id) else { return };
        {
            let a = app.borrow();
            let m = a.manifest();
            let Some(def) = m.setting(key) else { return };
            let mut cfg = self.host.config.borrow_mut();
            let cur = cfg.value(m, key);
            let next = match (def.kind, input) {
                (SettingKind::Bool, Input::Toggle) => toml::Value::Boolean(!cur.as_bool().unwrap_or(false)),
                (SettingKind::Int, Input::Int(v)) => {
                    toml::Value::Integer(v.clamp(def.min.unwrap_or(i64::MIN), def.max.unwrap_or(i64::MAX)))
                }
                (SettingKind::Choice, Input::Choice(i)) => match def.live_options().get(i) {
                    Some(o) => toml::Value::String(o.clone()),
                    None => return,
                },
                (SettingKind::Text | SettingKind::Secret, Input::Text(t)) => match def.clean_text(&t) {
                    Some(t) => toml::Value::String(t),
                    None => return,
                },
                _ => return,
            };
            if next == cur {
                return;
            }
            cfg.set(app_id, key, next);
            cfg.save();
        }
        app.borrow_mut().settings_changed(&self.ui);
        // theme or mascot may have changed: the screen icons (the tray compares by itself)
        self.rebuild_lists();
    }
}

/// Label of a `choice` option: name of the theme or mascot, if it is one.
fn option_label(key: &str) -> String {
    if key == "auto" {
        tr!("Follows the window").into()
    } else if let Some(t) = themes::find(key) {
        t.name().into()
    } else if let Some(s) = sprites::find(key) {
        s.name().into()
    } else {
        key.into()
    }
}

/// Splits the unit off the end of the label, as the stepper writes it after the number:
/// "Hot temperature (°C)" -> ("Hot temperature", "°C"); "Refresh every (min)" -> ("Refresh
/// every", " min"). Symbols stick to the number ("80%"), words get a space ("30 s").
fn split_unit(label: &str) -> (String, String) {
    const UNITS: [(&str, &str); 5] = [("%", "%"), ("°C", "°C"), ("s", " s"), ("min", " min"), ("px", " px")];
    for (unit, shown) in UNITS {
        if let Some(base) = label.strip_suffix(&format!(" ({unit})")) {
            return (base.to_string(), shown.to_string());
        }
    }
    (label.to_string(), String::new())
}

/// Where the app came from, for the list badge: "" (built-in), "dev" (`--plugin-dev`), "store"
/// (has the `installed.json` the store writes), "unreviewed" (that file says it came from
/// `--install-from`, no store reviewed it) or "local" (put by hand in the apps folder).
fn origin(id: &str) -> &'static str {
    use crate::core::plugins::{self, Plugin};
    if id.starts_with("cyber.") {
        return "";
    }
    // a plugin in development takes the place of an installed one with the same id
    if plugins::dev_dirs().iter().filter_map(|d| Plugin::load(d).ok()).any(|p| p.manifest.id == id) {
        return "dev";
    }
    match crate::store::installed(&plugins::dir(), id) {
        Some(i) if i.unreviewed => "unreviewed",
        Some(_) => "store",
        None => "local",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_the_unit_off_the_label() {
        let split = split_unit;
        assert_eq!(split("Hot temperature (°C)"), ("Hot temperature".into(), "°C".into()));
        assert_eq!(split("Warn at (%)"), ("Warn at".into(), "%".into()));
        assert_eq!(split("Skip forward/back step (s)"), ("Skip forward/back step".into(), " s".into()));
        assert_eq!(split("Refresh every (min)"), ("Refresh every".into(), " min".into()));
        assert_eq!(split("Terminal text size (px)"), ("Terminal text size".into(), " px".into()));
        // the pt-BR labels keep the same suffix
        assert_eq!(split("Atualizar a cada (min)"), ("Atualizar a cada".into(), " min".into()));
        // no unit, or a parenthesis that is not one
        assert_eq!(split("Columns"), ("Columns".into(), String::new()));
        assert_eq!(split("Shell (empty = the system's)"), ("Shell (empty = the system's)".into(), String::new()));
        assert_eq!(split("Pages (ms)"), ("Pages (ms)".into(), String::new()));
        assert_eq!(split("(min)"), ("(min)".into(), String::new()));
    }

    #[test]
    fn recognizes_the_mascot_options_of_the_manifests() {
        let docker = Manifest::parse(include_str!("../../miniapps/docker/manifest.toml")).unwrap();
        let vitals = Manifest::parse(include_str!("../../miniapps/vitals/manifest.toml")).unwrap();
        assert!(sprites::is_choice(&docker.setting("mascot").unwrap().options));
        assert!(!sprites::is_choice(&docker.setting("theme").unwrap().live_options()), "a theme is not a mascot");
        // "auto" among the alternatives is still a mascot choice, and has no drawing
        let gpu = &vitals.setting("gpu_mascot").unwrap().options;
        assert!(gpu.iter().any(|o| o == "auto") && sprites::is_choice(gpu));
        let images = option_images(gpu, themes::get("neon"));
        assert_eq!(images.len(), gpu.len());
        for (o, img) in gpu.iter().zip(&images) {
            assert_eq!(img.size().width == 0, o == "auto", "{o}");
        }
        assert!(option_images(&docker.setting("theme").unwrap().live_options(), themes::get("neon")).is_empty());
        // only "auto" (with no mascot) is not a mascot choice
        assert!(!sprites::is_choice(&["auto".to_string()]));
    }
}

