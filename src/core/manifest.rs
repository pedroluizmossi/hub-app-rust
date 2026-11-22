//! Format of a mini-app's `manifest.toml`.

pub mod when;

use serde::Deserialize;

use super::appdata::DataSpec;
use super::bus::Topic;
use crate::mascot::sprites;
use crate::tr;
use when::{HUB_RENDERER, When};

/// Newest version of the host/mini-app contract this binary understands. Older ones still
/// load: each version only adds settings features, and a manifest that uses one must declare
/// the version that brought it (a hub that doesn't know it then refuses the manifest with a
/// clear message instead of failing on an unknown field or setting type).
pub const API_VERSION: u32 = 3;

/// First version with the `text` and `secret` settings.
pub const API_TEXT_SETTINGS: u32 = 2;

/// First version with settings v2: the `float`, `color`, `file` and `folder` settings, and the
/// `help`, `group`, `when` and `restart` fields and labeled options.
pub const API_SETTINGS_V2: u32 = 3;

/// Longest value of a `text`, `secret`, `file` or `folder` setting when the manifest gives no
/// `max_len`.
pub const TEXT_MAX_LEN: usize = 512;

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Mascot used on the mini-app's tab.
    #[serde(default = "default_icon")]
    pub icon: String,
    pub api: u32,
    /// Data providers the app subscribes to while it is active.
    #[serde(default)]
    pub uses: Vec<Topic>,
    #[serde(default)]
    pub permissions: Permissions,
    #[serde(default)]
    pub settings: Vec<SettingDef>,
    /// What the app stores (`[data]`; see `core/appdata.rs`). Without the section, the
    /// default for the kind of app applies (`Plugin::data`).
    #[serde(default)]
    pub data: Option<DataSpec>,
}

fn default_icon() -> String {
    "neko".into()
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Permissions {
    /// Programs the app may run (the executor's allowlist).
    #[serde(default)]
    pub exec: Vec<String>,
    /// Domains the app may query over HTTPS (only plugins with a view; see `apps/plugin/script.rs`).
    #[serde(default)]
    pub net: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SettingKind {
    Bool,
    Int,
    /// A number with decimals, between the manifest's `min` and `max` (both required).
    Float,
    Choice,
    /// One line of free text (a URL, a path, a name).
    Text,
    /// Like `Text`, but the field hides what was typed (a token, an API key). It is saved in
    /// the hub's config file like the other settings, which is written only for the user.
    Secret,
    /// A color, saved as `"#rrggbb"`.
    Color,
    /// Path of a file (`~` allowed; see `Config::setting_path`), typed or picked in the system
    /// dialog.
    File,
    /// Path of a folder, like `File`.
    Folder,
    /// An item of the Library (`library::Kind` named by `asset`), saved as its key.
    Asset,
    /// Items of the Library, in order (a chain of shaders), saved as a list of keys.
    #[serde(rename = "asset-list")]
    AssetList,
}

impl SettingKind {
    /// Name the settings screen uses for the field (and the manifest's `type`).
    pub fn as_str(self) -> &'static str {
        match self {
            SettingKind::Bool => "bool",
            SettingKind::Int => "int",
            SettingKind::Float => "float",
            SettingKind::Choice => "choice",
            SettingKind::Text => "text",
            SettingKind::Secret => "secret",
            SettingKind::Color => "color",
            SettingKind::File => "file",
            SettingKind::Folder => "folder",
            SettingKind::Asset => "asset",
            SettingKind::AssetList => "asset-list",
        }
    }

    /// The `api` a manifest must declare to use this kind.
    pub fn api(self) -> u32 {
        match self {
            SettingKind::Bool | SettingKind::Int | SettingKind::Choice => 1,
            SettingKind::Text | SettingKind::Secret => API_TEXT_SETTINGS,
            SettingKind::Float
            | SettingKind::Color
            | SettingKind::File
            | SettingKind::Folder
            | SettingKind::Asset
            | SettingKind::AssetList => API_SETTINGS_V2,
        }
    }

    /// Whether `v` is a value of this kind (the default in the manifest, a saved value).
    pub fn accepts(self, v: &toml::Value) -> bool {
        match self {
            SettingKind::Bool => v.is_bool(),
            SettingKind::Int => v.is_integer(),
            SettingKind::Float => v.is_integer() || v.as_float().is_some_and(f64::is_finite),
            SettingKind::Color => v.as_str().is_some_and(is_color),
            SettingKind::Choice | SettingKind::Text | SettingKind::Secret | SettingKind::File | SettingKind::Folder | SettingKind::Asset => {
                v.is_str()
            }
            SettingKind::AssetList => v.as_array().is_some_and(|a| a.iter().all(toml::Value::is_str)),
        }
    }

    pub fn is_text(self) -> bool {
        matches!(self, SettingKind::Text | SettingKind::Secret)
    }

    pub fn is_path(self) -> bool {
        matches!(self, SettingKind::File | SettingKind::Folder)
    }
}

/// `"#rrggbb"` (hex digits in either case).
pub fn is_color(s: &str) -> bool {
    s.len() == 7 && s.starts_with('#') && s[1..].chars().all(|c| c.is_ascii_hexdigit())
}

/// What the user typed in a `color` field, as it is saved: `"#rrggbb"` in lowercase. The `#`
/// may be left out. `None` if it is not a color.
pub fn clean_color(typed: &str) -> Option<String> {
    let t = typed.trim();
    let t = if t.starts_with('#') { t.to_ascii_lowercase() } else { format!("#{}", t.to_ascii_lowercase()) };
    is_color(&t).then_some(t)
}

/// A `float` value: a TOML float or integer.
pub fn as_float(v: &toml::Value) -> Option<f64> {
    v.as_float().or_else(|| v.as_integer().map(|i| i as f64))
}

/// A setting declared in the manifest; becomes a form field on the "Apps" tab.
#[derive(Debug, Clone, Deserialize)]
#[serde(from = "RawSetting")]
pub struct SettingDef {
    pub key: String,
    pub label: String,
    pub kind: SettingKind,
    pub default: toml::Value,
    /// Range of an `int` (the manifest's `min`, `max` and `step`, whole numbers).
    pub min: Option<i64>,
    pub max: Option<i64>,
    pub step: Option<i64>,
    /// Range of a `float`: the same `min`, `max` and `step` keys of the manifest, read as
    /// numbers (an integer counts). `step` defaults to [`FLOAT_STEP`].
    pub float_min: Option<f64>,
    pub float_max: Option<f64>,
    pub float_step: Option<f64>,
    /// Values of a `choice` (what is saved). For a mascot choice, the manifest's options are
    /// the suggested ones and come first; `Manifest::parse` completes the list with the other
    /// mascots.
    pub options: Vec<String>,
    /// What the screen shows for each of `options` (same length): the `label` of an option
    /// written as `{ value = "…", label = "…" }`, translated; the value itself for an option
    /// written as a plain string.
    pub option_labels: Vec<String>,
    /// Longest value of a `text`, `secret`, `file` or `folder` (characters); `TEXT_MAX_LEN`
    /// without it.
    pub max_len: Option<usize>,
    /// Explanation shown under the label (translated; empty = none).
    pub help: String,
    /// Consecutive settings with the same group get a header above the first one (translated;
    /// empty = no group).
    pub group: String,
    /// Condition to show the setting (`when::When`); empty = always shown.
    pub when: String,
    /// The change only takes effect after restarting MiniUtils (the screen says so).
    pub restart: bool,
    /// The kind of Library item an `asset` or `asset-list` setting holds (`library::Kind`).
    pub asset: String,
    /// Some option was written as a `{ value, label }` table (settings v2).
    labeled_options: bool,
}

/// Step of a `float` whose manifest gives none.
pub const FLOAT_STEP: f64 = 0.1;

/// `SettingDef` as written in the TOML: `min`/`max`/`step` may be integers or floats, and an
/// option a plain string or a `{ value, label }` table.
#[derive(Deserialize)]
struct RawSetting {
    key: String,
    label: String,
    #[serde(rename = "type")]
    kind: SettingKind,
    default: toml::Value,
    #[serde(default)]
    min: Option<Number>,
    #[serde(default)]
    max: Option<Number>,
    #[serde(default)]
    step: Option<Number>,
    #[serde(default)]
    options: Vec<RawOption>,
    #[serde(default)]
    max_len: Option<usize>,
    #[serde(default)]
    help: String,
    #[serde(default)]
    group: String,
    #[serde(default)]
    when: String,
    #[serde(default)]
    restart: bool,
    #[serde(default)]
    asset: String,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(untagged)]
enum Number {
    Int(i64),
    Float(f64),
}

impl Number {
    fn int(n: Option<Number>) -> Option<i64> {
        match n {
            Some(Number::Int(i)) => Some(i),
            _ => None,
        }
    }

    fn float(n: Option<Number>) -> Option<f64> {
        n.map(|n| match n {
            Number::Int(i) => i as f64,
            Number::Float(f) => f,
        })
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawOption {
    Plain(String),
    Labeled {
        value: String,
        #[serde(default)]
        label: Option<String>,
    },
}

impl From<RawSetting> for SettingDef {
    fn from(r: RawSetting) -> Self {
        let labeled_options = r.options.iter().any(|o| matches!(o, RawOption::Labeled { .. }));
        let (options, option_labels) = r
            .options
            .into_iter()
            .map(|o| match o {
                RawOption::Plain(v) => (v.clone(), v),
                RawOption::Labeled { value, label } => {
                    let label = label.unwrap_or_else(|| value.clone());
                    (value, label)
                }
            })
            .unzip();
        SettingDef {
            key: r.key,
            label: r.label,
            kind: r.kind,
            default: r.default,
            min: Number::int(r.min),
            max: Number::int(r.max),
            step: Number::int(r.step),
            float_min: Number::float(r.min),
            float_max: Number::float(r.max),
            float_step: Number::float(r.step),
            options,
            option_labels,
            max_len: r.max_len,
            help: r.help,
            group: r.group,
            when: r.when,
            restart: r.restart,
            asset: r.asset,
            labeled_options,
        }
    }
}

impl SettingDef {
    /// A setting with only the required fields (the hub adds `theme` and `mascot` this way).
    pub fn new(key: &str, label: &str, kind: SettingKind, default: toml::Value) -> Self {
        SettingDef {
            key: key.into(),
            label: label.into(),
            kind,
            default,
            min: None,
            max: None,
            step: None,
            float_min: None,
            float_max: None,
            float_step: None,
            options: Vec::new(),
            option_labels: Vec::new(),
            max_len: None,
            help: String::new(),
            group: String::new(),
            when: String::new(),
            restart: false,
            asset: String::new(),
            labeled_options: false,
        }
    }

    /// What the user typed in a `text`, `secret`, `file` or `folder` field, as it is saved:
    /// one line, no surrounding spaces (a pasted token often brings a line break), at most
    /// `max_len` characters. `None` if the setting is not one of those.
    pub fn clean_text(&self, typed: &str) -> Option<String> {
        if !self.kind.is_text() && !self.kind.is_path() {
            return None;
        }
        let max = self.max_len.unwrap_or(TEXT_MAX_LEN);
        Some(typed.trim().chars().filter(|c| !c.is_control()).take(max).collect())
    }

    /// Replaces the options of a `choice`, each one labeled with its value.
    pub fn set_options(&mut self, options: Vec<String>) {
        self.option_labels = options.clone();
        self.options = options;
    }

    /// Step of a `float`.
    pub fn float_step(&self) -> f64 {
        self.float_step.unwrap_or(FLOAT_STEP)
    }

    /// The manifest's label of a `choice` value, if it has one.
    pub fn option_label(&self, value: &str) -> Option<&str> {
        let i = self.options.iter().position(|o| o == value)?;
        self.option_labels.get(i).map(String::as_str)
    }

    /// The parsed `when`; `None` when the setting is always shown (the condition was checked
    /// with the manifest).
    pub fn condition(&self) -> Option<When> {
        if self.when.trim().is_empty() { None } else { When::parse(&self.when) }
    }

    /// The options the screen shows. The theme one (`theme`) is always "auto" plus every
    /// theme, file themes included: a new theme shows up in every app without editing
    /// manifests. The others are the manifest's.
    pub fn live_options(&self) -> Vec<String> {
        if self.key == "theme" && self.kind == SettingKind::Choice {
            let mut v = vec!["auto".to_string()];
            v.extend(crate::mascot::themes::all().iter().map(|t| t.key.to_string()));
            return v;
        }
        // mascot choice: the manifest's, then the imported ones not listed yet (the mascot
        // folder changes without re-reading the manifests)
        if self.kind == SettingKind::Choice && sprites::is_choice(&self.options) {
            let mut v = self.options.clone();
            v.extend(sprites::all().iter().map(|s| s.key.to_string()).filter(|k| !self.options.contains(k)));
            return v;
        }
        self.options.clone()
    }

    /// The settings-v2 fields this setting uses (to refuse them below `API_SETTINGS_V2`).
    fn v2_field(&self) -> Option<&'static str> {
        if !self.help.is_empty() {
            Some("help")
        } else if !self.group.is_empty() {
            Some("group")
        } else if !self.when.is_empty() {
            Some("when")
        } else if self.restart {
            Some("restart")
        } else if self.labeled_options {
            Some("options.label")
        } else {
            None
        }
    }
}

impl Manifest {
    pub fn parse(text: &str) -> anyhow::Result<Self> {
        let mut m: Manifest = toml::from_str(text)?;
        // hub catalog: the built-in apps' texts (a plugin's own catalog comes after, in Plugin::load)
        crate::core::i18n::hub_catalog(crate::core::i18n::current()).localize_manifest(&mut m);
        // Mascot choice: the manifest lists the ones that suit the app (suggested), but any
        // mascot can be chosen. The others come after, in `SPRITES` order; so a new mascot
        // shows up in every app without editing manifests.
        for s in m.settings.iter_mut().filter(|s| s.kind == SettingKind::Choice && sprites::is_choice(&s.options)) {
            let rest: Vec<String> =
                sprites::SPRITES.iter().map(|sp| sp.key.to_string()).filter(|k| !s.options.contains(k)).collect();
            s.option_labels.extend(rest.iter().cloned());
            s.options.extend(rest);
        }
        m.check().map_err(anyhow::Error::msg)?;
        Ok(m)
    }

    /// Checks what serde can't: the API version, the settings (a default of the right type,
    /// within range, among the options; unique keys; a valid `when`; no feature newer than
    /// `api`) and `[data]`. The errors are shown to the user (Settings › System,
    /// `--plugins`), so they are translated.
    fn check(&self) -> Result<(), String> {
        let id = &self.id;
        if !(1..=API_VERSION).contains(&self.api) {
            return Err(tr!("mini-app {} uses API {} (this hub understands up to {})", id, self.api, API_VERSION));
        }
        for (i, s) in self.settings.iter().enumerate() {
            let key = &s.key;
            if key.trim().is_empty() {
                return Err(tr!("mini-app {}: a setting has an empty key", id));
            }
            if self.settings[..i].iter().any(|o| o.key == *key) {
                return Err(tr!("mini-app {}: setting {} is declared twice", id, key));
            }
            if self.api < s.kind.api() {
                return Err(tr!("mini-app {}: setting {} is a {}, which needs api = {}", id, key, s.kind.as_str(), s.kind.api()));
            }
            if let Some(field) = s.v2_field()
                && self.api < API_SETTINGS_V2
            {
                return Err(tr!("mini-app {}: setting {} uses {}, which needs api = {}", id, key, field, API_SETTINGS_V2));
            }
            if !s.kind.accepts(&s.default) {
                return Err(tr!("mini-app {}: the default of setting {} is not a {} value", id, key, s.kind.as_str()));
            }
            self.check_when(s)?;
            match s.kind {
                SettingKind::Int => {
                    // a bound with decimals is only read as a float
                    let whole = [(s.min, s.float_min), (s.max, s.float_max), (s.step, s.float_step)];
                    if whole.iter().any(|(int, float)| int.is_none() && float.is_some()) {
                        return Err(tr!("mini-app {}: setting {} needs whole numbers in min, max and step", id, key));
                    }
                    if let (Some(min), Some(max)) = (s.min, s.max)
                        && min > max
                    {
                        return Err(tr!("mini-app {}: setting {} has min greater than max", id, key));
                    }
                    let d = s.default.as_integer().unwrap_or_default();
                    if s.min.is_some_and(|min| d < min) || s.max.is_some_and(|max| d > max) {
                        return Err(tr!("mini-app {}: the default of setting {} is outside min and max", id, key));
                    }
                    if s.step.is_some_and(|step| step <= 0) {
                        return Err(tr!("mini-app {}: setting {} needs a step greater than 0", id, key));
                    }
                }
                SettingKind::Float => {
                    let (Some(min), Some(max)) = (s.float_min, s.float_max) else {
                        return Err(tr!("mini-app {}: setting {} needs min and max", id, key));
                    };
                    if !min.is_finite() || !max.is_finite() {
                        return Err(tr!("mini-app {}: setting {} needs min and max", id, key));
                    }
                    if min > max {
                        return Err(tr!("mini-app {}: setting {} has min greater than max", id, key));
                    }
                    let d = as_float(&s.default).unwrap_or_default();
                    if d < min || d > max {
                        return Err(tr!("mini-app {}: the default of setting {} is outside min and max", id, key));
                    }
                    let step = s.float_step();
                    if step.is_nan() || step <= 0.0 {
                        return Err(tr!("mini-app {}: setting {} needs a step greater than 0", id, key));
                    }
                }
                // the theme and mascot choices are completed at runtime (file themes, imported
                // mascots): their default can't be checked against the manifest alone
                SettingKind::Choice if key != "theme" && !sprites::is_choice(&s.options) => {
                    let d = s.default.as_str().unwrap_or_default();
                    if !s.options.iter().any(|o| o == d) {
                        return Err(tr!("mini-app {}: the default of setting {} is not one of its options", id, key));
                    }
                }
                SettingKind::Text | SettingKind::Secret | SettingKind::File | SettingKind::Folder
                    if s.max_len == Some(0) =>
                {
                    return Err(tr!("mini-app {}: setting {} has max_len = 0", id, key));
                }
                SettingKind::Asset | SettingKind::AssetList if crate::library::Kind::from_key(&s.asset).is_none() => {
                    return Err(tr!("mini-app {}: setting {} needs asset = one of shader, theme, skin, backdrop, mascot", id, key));
                }
                _ => {}
            }
        }
        if let Some(d) = &self.data {
            d.check()?;
        }
        Ok(())
    }

    /// A `when` follows the grammar and reads another setting of this app or a hub fact.
    fn check_when(&self, s: &SettingDef) -> Result<(), String> {
        if s.when.trim().is_empty() {
            return Ok(());
        }
        let Some(cond) = When::parse(&s.when) else {
            return Err(tr!("mini-app {}: setting {} has an invalid when: {}", self.id, s.key, s.when));
        };
        if cond.key != HUB_RENDERER && (cond.key == s.key || self.setting(&cond.key).is_none()) {
            return Err(tr!("mini-app {}: the when of setting {} reads {}, which is not another setting of this app", self.id, s.key, cond.key));
        }
        Ok(())
    }

    pub fn setting(&self, key: &str) -> Option<&SettingDef> {
        self.settings.iter().find(|s| s.key == key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_manifests_are_valid() {
        for text in [
            include_str!("../../miniapps/docker/manifest.toml"),
            include_str!("../../miniapps/kube/manifest.toml"),
            include_str!("../../miniapps/vitals/manifest.toml"),
            include_str!("../../miniapps/cdplayer/manifest.toml"),
            include_str!("../../miniapps/ai_usage/manifest.toml"),
            include_str!("../../miniapps/ports/manifest.toml"),
            include_str!("../../miniapps/todo/manifest.toml"),
            include_str!("../../miniapps/terminal/manifest.toml"),
            include_str!("../../miniapps/agents/manifest.toml"),
            include_str!("../../miniapps/mascots/manifest.toml"),
        ] {
            // parse checks the types, ranges and options; the theme and mascot choices are
            // completed at runtime, so their defaults are checked here
            let m = Manifest::parse(text).unwrap();
            for s in m.settings.iter().filter(|s| s.kind == SettingKind::Choice) {
                assert!(s.live_options().contains(&s.default.as_str().unwrap().to_string()), "{}", s.key);
            }
        }
    }

    #[test]
    fn mascot_choice_offers_all_with_the_suggested_first() {
        let vitals = Manifest::parse(include_str!("../../miniapps/vitals/manifest.toml")).unwrap();
        let gpu = &vitals.setting("gpu_mascot").unwrap().options;
        // the manifest's first, in its order ("auto" stays first)
        assert_eq!(gpu[..7], ["auto", "nvidia", "amd", "robo", "neko", "chip", "vagalume"]);
        assert_eq!(gpu.len(), sprites::SPRITES.len() + 1, "every mascot, plus auto");
        let mut sorted = gpu.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), gpu.len(), "no duplicates");
        // a choice that is not a mascot choice stays as in the manifest
        let theme = vitals.setting("theme").unwrap().live_options();
        assert!(theme.contains(&"auto".to_string()) && !theme.contains(&"neko".to_string()));
        // the theme one is generated: "auto" plus every theme, no list in the manifest
        assert_eq!(theme.len(), crate::mascot::themes::all().len() + 1);
        assert!(vitals.setting("theme").unwrap().options.is_empty());
    }

    #[test]
    fn rejects_unknown_provider() {
        assert!(Manifest::parse("id='x'\nname='x'\napi=1\nuses=['sys']").is_ok());
        assert!(Manifest::parse("id='x'\nname='x'\napi=1\nuses=['sis']").is_err());
    }

    #[test]
    fn rejects_unknown_api() {
        assert!(Manifest::parse("id='x'\nname='x'\napi=99").is_err());
        assert!(Manifest::parse("id='x'\nname='x'\napi=0").is_err());
        // the older contract still loads
        assert!(Manifest::parse("id='x'\nname='x'\napi=1").is_ok());
        assert!(Manifest::parse("id='x'\nname='x'\napi=2").is_ok());
        assert!(Manifest::parse("id='x'\nname='x'\napi=3").is_ok());
    }

    const TEXT_SETTINGS: &str = "id='x'\nname='x'\napi=2\n\
        [[settings]]\nkey='url'\nlabel='URL'\ntype='text'\ndefault='http://127.0.0.1:8765'\n\
        [[settings]]\nkey='token'\nlabel='Token'\ntype='secret'\ndefault=''\nmax_len=8\n";

    #[test]
    fn text_and_secret_settings() {
        let m = Manifest::parse(TEXT_SETTINGS).unwrap();
        assert_eq!(m.setting("url").unwrap().kind, SettingKind::Text);
        let token = m.setting("token").unwrap();
        assert_eq!(token.kind, SettingKind::Secret);
        // one line, trimmed, cut at max_len
        assert_eq!(token.clean_text("  cix_abc\n").as_deref(), Some("cix_abc"));
        assert_eq!(token.clean_text("cix_\tabcdefgh").as_deref(), Some("cix_abcd"));
        assert_eq!(m.setting("url").unwrap().clean_text(&"a".repeat(600)).unwrap().len(), TEXT_MAX_LEN);
        // not a text setting
        let b = Manifest::parse("id='x'\nname='x'\napi=1\n[[settings]]\nkey='b'\nlabel='B'\ntype='bool'\ndefault=true").unwrap();
        assert_eq!(b.setting("b").unwrap().clean_text("x"), None);
    }

    #[test]
    fn text_settings_need_api_2_and_a_text_default() {
        assert!(Manifest::parse(&TEXT_SETTINGS.replace("api=2", "api=1")).is_err());
        assert!(Manifest::parse(&TEXT_SETTINGS.replace("default=''", "default=0")).is_err());
        assert!(Manifest::parse(&TEXT_SETTINGS.replace("max_len=8", "max_len=0")).is_err());
    }

    #[test]
    fn settings_are_checked_on_parse() {
        let one = |setting: &str| Manifest::parse(&format!("id='x'\nname='x'\napi=1\n[[settings]]\nkey='k'\nlabel='K'\n{setting}"));
        assert!(one("type='int'\ndefault=5\nmin=1\nmax=10\nstep=1").is_ok());
        assert!(one("type='int'\ndefault=true").is_err(), "wrong type");
        assert!(one("type='bool'\ndefault=1").is_err(), "wrong type");
        assert!(one("type='int'\ndefault=5\nmin=10\nmax=1").is_err(), "min > max");
        assert!(one("type='int'\ndefault=50\nmin=1\nmax=10").is_err(), "default out of range");
        assert!(one("type='int'\ndefault=5\nstep=0").is_err(), "step 0");
        assert!(one("type='choice'\ndefault='a'\noptions=['a','b']").is_ok());
        assert!(one("type='choice'\ndefault='c'\noptions=['a','b']").is_err(), "default not an option");
        assert!(one("type='choice'\ndefault=1\noptions=['a']").is_err(), "wrong type");
        // two settings with the same key
        let twice = "id='x'\nname='x'\napi=1\n[[settings]]\nkey='k'\nlabel='K'\ntype='bool'\ndefault=true\n\
            [[settings]]\nkey='k'\nlabel='K'\ntype='bool'\ndefault=false";
        assert!(Manifest::parse(twice).unwrap_err().to_string().contains("twice"));
        assert!(one("type='bool'\ndefault=true").unwrap().setting("k").is_some());
        let empty = "id='x'\nname='x'\napi=1\n[[settings]]\nkey=' '\nlabel='K'\ntype='bool'\ndefault=true";
        assert!(Manifest::parse(empty).is_err(), "empty key");
    }

    /// One setting in a manifest with `api`.
    fn v2(api: u32, setting: &str) -> anyhow::Result<Manifest> {
        Manifest::parse(&format!(
            "id='x'\nname='x'\napi={api}\n\
             [[settings]]\nkey='on'\nlabel='On'\ntype='bool'\ndefault=true\n\
             [[settings]]\nkey='sort'\nlabel='Sort'\ntype='choice'\ndefault='a'\noptions=['a','b']\n\
             [[settings]]\nkey='k'\nlabel='K'\n{setting}"
        ))
    }

    #[test]
    fn options_may_be_tables_with_labels() {
        let m = v2(3, "type='choice'\ndefault='cpu'\noptions=['estado', { value='cpu', label='CPU' }, { value='ram' }]").unwrap();
        let k = m.setting("k").unwrap();
        assert_eq!(k.options, ["estado", "cpu", "ram"]);
        assert_eq!(k.option_labels, ["estado", "CPU", "ram"]);
        assert_eq!(k.option_label("cpu"), Some("CPU"));
        assert_eq!(k.option_label("nope"), None);
        // the default is checked against the values, not the labels
        assert!(v2(3, "type='choice'\ndefault='CPU'\noptions=[{ value='cpu', label='CPU' }]").is_err());
        // a table needs a value
        assert!(v2(3, "type='choice'\ndefault='a'\noptions=[{ label='A' }]").is_err());
        // the plain list keeps one label per value
        let sort = m.setting("sort").unwrap();
        assert_eq!(sort.options, sort.option_labels);
    }

    #[test]
    fn settings_v2_need_api_3() {
        let features = [
            "type='float'\ndefault=0.5\nmin=0\nmax=1",
            "type='color'\ndefault='#ff8800'",
            "type='file'\ndefault='~/notes.txt'",
            "type='folder'\ndefault=''",
            "type='bool'\ndefault=true\nhelp='Why'",
            "type='bool'\ndefault=true\ngroup='Alerts'",
            "type='bool'\ndefault=true\nwhen='on'",
            "type='bool'\ndefault=true\nrestart=true",
            "type='choice'\ndefault='a'\noptions=[{ value='a', label='A' }]",
        ];
        for f in features {
            assert!(v2(3, f).is_ok(), "{f}");
            let err = v2(2, f).unwrap_err().to_string();
            assert!(err.contains("api = 3"), "{f}: {err}");
        }
    }

    #[test]
    fn float_color_and_path_settings_are_checked() {
        assert!(v2(3, "type='float'\ndefault=1\nmin=0\nmax=2\nstep=0.25").is_ok(), "an integer default");
        assert!(v2(3, "type='float'\ndefault=0.5").is_err(), "no range");
        assert!(v2(3, "type='float'\ndefault=0.5\nmin=0").is_err(), "no max");
        assert!(v2(3, "type='float'\ndefault=5.0\nmin=0\nmax=1").is_err(), "default out of range");
        assert!(v2(3, "type='float'\ndefault=0.5\nmin=1\nmax=0").is_err(), "min > max");
        assert!(v2(3, "type='float'\ndefault=0.5\nmin=0\nmax=1\nstep=0").is_err(), "step 0");
        assert!(v2(3, "type='float'\ndefault='0.5'\nmin=0\nmax=1").is_err(), "wrong type");
        assert!(v2(3, "type='float'\ndefault=0.5\nmin=0\nmax=inf").is_err(), "infinite max");
        let f = v2(3, "type='float'\ndefault=0.5\nmin=0.1\nmax=1").unwrap();
        let k = f.setting("k").unwrap();
        assert_eq!((k.float_min, k.float_max, k.float_step()), (Some(0.1), Some(1.0), FLOAT_STEP));
        // an int keeps whole bounds
        assert!(v2(3, "type='int'\ndefault=1\nmin=0.5\nmax=2").is_err());
        assert_eq!(v2(3, "type='int'\ndefault=1\nmin=0\nmax=2").unwrap().setting("k").unwrap().max, Some(2));

        assert!(v2(3, "type='color'\ndefault='#FF8800'").is_ok());
        for bad in ["'ff8800'", "'#f80'", "'#ff880g'", "'#ff88001'", "1"] {
            assert!(v2(3, &format!("type='color'\ndefault={bad}")).is_err(), "{bad}");
        }
        assert!(v2(3, "type='folder'\ndefault=''\nmax_len=0").is_err());
        let file = v2(3, "type='file'\ndefault=''\nmax_len=4").unwrap();
        assert_eq!(file.setting("k").unwrap().clean_text(" ~/abcdef\n").as_deref(), Some("~/ab"));
    }

    #[test]
    fn colors_are_validated_and_normalized() {
        assert!(is_color("#a1B2c3"));
        for bad in ["", "#", "a1b2c3", "#a1b2c", "#a1b2c3d", "#a1b2cg", "# a1b2c"] {
            assert!(!is_color(bad), "{bad:?}");
        }
        assert_eq!(clean_color(" #A1B2C3 ").as_deref(), Some("#a1b2c3"));
        assert_eq!(clean_color("a1b2c3").as_deref(), Some("#a1b2c3"));
        assert_eq!(clean_color("red"), None);
        assert_eq!(clean_color("#abc"), None);
    }

    #[test]
    fn when_is_checked_against_the_settings() {
        for ok in ["on", "!on", "sort == 'b'", "sort != 'a'", "hub.renderer == 'opengl'"] {
            let m = v2(3, &format!("type='bool'\ndefault=true\nwhen=\"{ok}\""));
            assert!(m.is_ok(), "{ok}");
            assert!(m.unwrap().setting("k").unwrap().condition().is_some(), "{ok}");
        }
        for bad in ["nope", "k", "hub.other", "on ==", "sort = 'b'", "on && sort"] {
            assert!(v2(3, &format!("type='bool'\ndefault=true\nwhen=\"{bad}\"")).is_err(), "{bad}");
        }
        assert!(v2(3, "type='bool'\ndefault=true").unwrap().setting("k").unwrap().condition().is_none());
    }
}
