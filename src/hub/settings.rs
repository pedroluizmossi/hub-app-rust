//! Settings tab: the list of apps and the form generated from each one's manifest.

use std::cell::RefCell;
use std::collections::HashMap;

use slint::{Color, Image, ModelRc, SharedString, VecModel};

use super::{Hub, with_hub};
use crate::core::config::Config;
use crate::core::i18n::{self, Language};
use crate::core::manifest::{self, Manifest, SettingDef, SettingKind, when::HUB_RENDERER};
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
    /// Position on the stepper of a `float` (0 = `min`; see [`FloatRange`]).
    Step(i64),
    Choice(usize),
    /// What was typed in a `text`, `secret`, `color`, `file` or `folder` field (Enter or
    /// leaving the field), or the path picked in the system dialog.
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
        let mut group = "";
        m.settings
            .iter()
            .filter(|s| shown(&cfg, m, s))
            .map(|s| {
                let v = cfg.value(m, &s.key);
                // the unit goes next to the number (only a stepper shows it)
                let (label, unit) = if matches!(s.kind, SettingKind::Int | SettingKind::Float) {
                    split_unit(&s.label)
                } else {
                    (s.label.clone(), String::new())
                };
                let options = s.live_options();
                // the header goes above the first of a run of settings in the same group
                let group_head = if !s.group.is_empty() && s.group != group { s.group.as_str() } else { "" };
                group = &s.group;
                let float = FloatRange::of(s);
                let fv = cfg.float(m, &s.key);
                let (int_value, minimum, maximum, step) = match &float {
                    Some(r) => (r.index(fv) as i32, 0, r.steps() as i32, 1),
                    None => (
                        v.as_integer().unwrap_or(0) as i32,
                        s.min.unwrap_or(0) as i32,
                        s.max.unwrap_or(i32::MAX as i64) as i32,
                        s.step.unwrap_or(1) as i32,
                    ),
                };
                let text = |x: f64| float.as_ref().map(|r| format!("{}{unit}", r.text(x))).unwrap_or_default();
                SettingRow {
                    app: m.id.clone().into(),
                    key: s.key.clone().into(),
                    label: label.into(),
                    help: s.help.clone().into(),
                    group_head: group_head.into(),
                    restart: s.restart,
                    modified: cfg.saved(m, &s.key).is_some() && differs(s, &v),
                    kind: s.kind.as_str().into(),
                    bool_value: v.as_bool().unwrap_or(false),
                    int_value,
                    minimum,
                    maximum,
                    step,
                    float_text: text(fv).into(),
                    float_widest: text(s.float_max.unwrap_or_default()).into(),
                    options: ModelRc::new(VecModel::from(
                        options.iter().map(|o| SharedString::from(choice_label(s, o))).collect::<Vec<_>>(),
                    )),
                    index: options.iter().position(|o| Some(o.as_str()) == v.as_str()).unwrap_or(0) as i32,
                    images: ModelRc::new(VecModel::from(option_images(&options, theme))),
                    text_value: match s.kind {
                        SettingKind::Text
                        | SettingKind::Secret
                        | SettingKind::Color
                        | SettingKind::File
                        | SettingKind::Folder => v.as_str().unwrap_or_default().into(),
                        SettingKind::Bool | SettingKind::Int | SettingKind::Float | SettingKind::Choice => {
                            SharedString::new()
                        }
                        // what is chosen, by name, in order
                        SettingKind::Asset | SettingKind::AssetList => asset_summary(s, &v).into(),
                    },
                    color_value: if s.kind == SettingKind::Color {
                        color_of(v.as_str().unwrap_or_default())
                    } else {
                        Color::default()
                    },
                    unit: unit.into(),
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
                (SettingKind::Float, Input::Step(i)) => match FloatRange::of(def) {
                    Some(r) => toml::Value::Float(r.at(i)),
                    None => return,
                },
                (SettingKind::Choice, Input::Choice(i)) => match def.live_options().get(i) {
                    Some(o) => toml::Value::String(o.clone()),
                    None => return,
                },
                (SettingKind::Text | SettingKind::Secret | SettingKind::File | SettingKind::Folder, Input::Text(t)) => {
                    match def.clean_text(&t) {
                        Some(t) => toml::Value::String(t),
                        None => return,
                    }
                }
                (SettingKind::Color, Input::Text(t)) => match manifest::clean_color(&t) {
                    Some(c) => toml::Value::String(c),
                    None => {
                        // not a color: the field goes back to the saved value
                        drop(cfg);
                        drop(a);
                        self.settings_fingerprint.replace(String::new());
                        self.rebuild_settings();
                        return;
                    }
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

    /// The reset button: forgets the saved value, so the manifest default applies again.
    pub(super) fn reset_setting(&self, app_id: &str, key: &str) {
        let Some(app) = self.registry.find(app_id) else { return };
        if app.borrow().manifest().setting(key).is_none() {
            return;
        }
        {
            let mut cfg = self.host.config.borrow_mut();
            if !cfg.unset(app_id, key) {
                return;
            }
            cfg.save();
        }
        app.borrow_mut().settings_changed(&self.ui);
        self.rebuild_lists();
    }

    /// The "Choose…" of a `file` or `folder` setting: the system dialog, in a thread (it
    /// blocks until the user answers); the chosen path is saved like a typed one. For an
    /// `asset` or `asset-list` setting, the Library, where its items are chosen.
    pub(super) fn pick_setting_path(&self, app_id: &str, key: &str) {
        let Some(app) = self.registry.find(app_id) else { return };
        let asset = app.borrow().manifest().setting(key).and_then(|d| crate::library::Kind::from_key(&d.asset));
        if let Some(kind) = asset {
            self.open_library(Some(kind), None);
            return;
        }
        let (folder, title, start) = {
            let a = app.borrow();
            let m = a.manifest();
            let Some(def) = m.setting(key).filter(|d| d.kind.is_path()) else { return };
            (def.kind == SettingKind::Folder, def.label.clone(), self.host.config.borrow().setting_path(m, key))
        };
        let (app_id, key) = (app_id.to_string(), key.to_string());
        let spawned = std::thread::Builder::new().name("setting-pick".into()).spawn(move || {
            let mut dialog = rfd::FileDialog::new().set_title(title);
            // start where the current value is
            let dir = if folder || start.is_dir() { Some(start.as_path()) } else { start.parent() };
            if let Some(dir) = dir.filter(|d| d.is_dir()) {
                dialog = dialog.set_directory(dir);
            }
            let chosen = if folder { dialog.pick_folder() } else { dialog.pick_file() };
            let Some(path) = chosen else { return };
            let text = path.to_string_lossy().into_owned();
            let _ = slint::invoke_from_event_loop(move || {
                with_hub(|h| h.change_setting(&app_id, &key, Input::Text(text)));
            });
        });
        if let Err(e) = spawned {
            log::warn!("no thread for the file dialog: {e}");
        }
    }
}

/// The names of the Library items an `asset`/`asset-list` value holds: "CRT → Bloom".
fn asset_summary(s: &SettingDef, v: &toml::Value) -> String {
    let Some(kind) = crate::library::Kind::from_key(&s.asset) else { return String::new() };
    let keys: Vec<&str> = match v.as_array() {
        Some(list) => list.iter().filter_map(|x| x.as_str()).collect(),
        None => v.as_str().into_iter().filter(|k| !k.is_empty()).collect(),
    };
    keys.iter().map(|k| crate::library::name_of(kind, k)).collect::<Vec<_>>().join(" → ")
}

/// Whether the setting's `when` holds now (a setting without one is always shown).
fn shown(cfg: &Config, m: &Manifest, s: &SettingDef) -> bool {
    let Some(cond) = s.condition() else { return true };
    let value = if cond.key == HUB_RENDERER {
        toml::Value::String(crate::popup::renderer().key().into())
    } else {
        cfg.value(m, &cond.key)
    };
    cond.holds(&value)
}

/// Whether `v` (the setting's current value) is not its default: the reset button shows.
fn differs(s: &SettingDef, v: &toml::Value) -> bool {
    match s.kind {
        // `1` and `1.0` are the same number
        SettingKind::Float => manifest::as_float(v) != manifest::as_float(&s.default),
        _ => *v != s.default,
    }
}

/// Label of a `choice` option: the manifest's, or the name of the theme or mascot.
fn choice_label(s: &SettingDef, value: &str) -> String {
    match s.option_label(value) {
        Some(label) if label != value => label.to_string(),
        _ => option_label(value),
    }
}

/// The swatch of a `color` setting (`"#rrggbb"`; black if it is not one).
fn color_of(hex: &str) -> Color {
    let channel = |i: usize| hex.get(i..i + 2).and_then(|h| u8::from_str_radix(h, 16).ok()).unwrap_or(0);
    if manifest::is_color(hex) { Color::from_rgb_u8(channel(1), channel(3), channel(5)) } else { Color::default() }
}

/// The stepper of a `float`: positions 0..=`steps()` from `min` to `max`, `step` apart (the
/// last one is `max` even when the range is not a whole number of steps).
struct FloatRange {
    min: f64,
    max: f64,
    step: f64,
    /// Decimals shown and saved: those of `step` and `min`.
    decimals: usize,
}

impl FloatRange {
    fn of(s: &SettingDef) -> Option<FloatRange> {
        if s.kind != SettingKind::Float {
            return None;
        }
        let (min, max, step) = (s.float_min?, s.float_max?, s.float_step());
        Some(FloatRange { min, max, step, decimals: decimals(step).max(decimals(min)) })
    }

    fn steps(&self) -> i64 {
        ((self.max - self.min) / self.step - 1e-9).ceil().max(0.0) as i64
    }

    /// Position of `v` (the nearest one).
    fn index(&self, v: f64) -> i64 {
        ((v - self.min) / self.step).round().clamp(0.0, self.steps() as f64) as i64
    }

    /// Value at position `i`, rounded to the shown decimals (no `0.30000000000000004` in the
    /// config).
    fn at(&self, i: i64) -> f64 {
        let i = i.clamp(0, self.steps());
        let v = if i == self.steps() { self.max } else { self.min + i as f64 * self.step };
        let scale = 10f64.powi(self.decimals as i32);
        ((v * scale).round() / scale).clamp(self.min, self.max)
    }

    /// `v` as the stepper writes it, with the interface's decimal separator.
    fn text(&self, v: f64) -> String {
        let text = format!("{v:.*}", self.decimals);
        if i18n::current() == Language::PtBr { text.replace('.', ",") } else { text }
    }
}

/// Decimals needed to write `x` (up to 6).
fn decimals(x: f64) -> usize {
    (0..6)
        .find(|&d| {
            let scaled = x * 10f64.powi(d as i32);
            (scaled - scaled.round()).abs() < 1e-6
        })
        .unwrap_or(6)
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

    fn float_setting(range: &str) -> SettingDef {
        let m = Manifest::parse(&format!("id='x'\nname='x'\napi=3\n[[settings]]\nkey='k'\nlabel='K'\ntype='float'\n{range}"))
            .unwrap();
        m.setting("k").unwrap().clone()
    }

    #[test]
    fn float_stepper_positions() {
        let r = FloatRange::of(&float_setting("default=0.5\nmin=0\nmax=1")).unwrap();
        assert_eq!((r.steps(), r.decimals), (10, 1));
        assert_eq!((r.index(0.5), r.at(5)), (5, 0.5));
        // rounded to the decimals of the step: no 0.30000000000000004
        assert_eq!(r.at(3), 0.3);
        // out of range: clamped
        assert_eq!((r.at(-4), r.at(99)), (0.0, 1.0));
        assert_eq!((r.index(-1.0), r.index(7.0)), (0, 10));
        // a range that is not a whole number of steps ends on max
        let r = FloatRange::of(&float_setting("default=1\nmin=0\nmax=1\nstep=0.3")).unwrap();
        assert_eq!(r.steps(), 4);
        assert_eq!((r.at(3), r.at(4)), (0.9, 1.0));
        let r = FloatRange::of(&float_setting("default=1.25\nmin=0.5\nmax=2\nstep=0.25")).unwrap();
        assert_eq!((r.decimals, r.steps(), r.index(1.25)), (2, 6, 3));
        assert_eq!(decimals(0.05), 2);
        assert_eq!(decimals(3.0), 0);
    }

    #[test]
    fn choice_labels_reset_and_colors() {
        let docker = Manifest::parse(include_str!("../../miniapps/docker/manifest.toml")).unwrap();
        let sort = docker.setting("sort").unwrap();
        // the manifest's label (translated) is shown, the value is saved
        assert_eq!(sort.options, ["estado", "nome", "cpu", "ram"]);
        assert_eq!(choice_label(sort, "estado"), i18n::tr("State"));
        // theme and mascot choices keep their names
        let mascot = docker.setting("mascot").unwrap();
        assert_eq!(choice_label(mascot, "neko"), option_label("neko"));
        // the reset button: a float equal to its default is not modified
        let f = float_setting("default=1\nmin=0\nmax=2");
        assert!(!differs(&f, &toml::Value::Float(1.0)));
        assert!(differs(&f, &toml::Value::Float(1.5)));
        assert!(differs(sort, &toml::Value::String("cpu".into())));
        assert!(!differs(sort, &toml::Value::String("estado".into())));
        assert_eq!(color_of("#ff8000"), Color::from_rgb_u8(255, 128, 0));
        assert_eq!(color_of("nope"), Color::default());
    }
}

