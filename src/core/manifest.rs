//! Format of a mini-app's `manifest.toml`.

use serde::Deserialize;

use super::appdata::DataSpec;
use super::bus::Topic;
use crate::mascot::sprites;
use crate::tr;

/// Newest version of the host/mini-app contract this binary understands. Older ones still
/// load: 2 only adds the `text` and `secret` settings, and a manifest that uses them must
/// declare `api = 2` (a hub that only knows 1 then refuses it with a clear message instead of
/// failing on an unknown setting type).
pub const API_VERSION: u32 = 2;

/// First version with the `text` and `secret` settings.
pub const API_TEXT_SETTINGS: u32 = 2;

/// Longest value of a `text` or `secret` setting when the manifest gives no `max_len`.
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
    Choice,
    /// One line of free text (a URL, a path, a name).
    Text,
    /// Like `Text`, but the field hides what was typed (a token, an API key). It is saved in
    /// the hub's config file like the other settings, which is written only for the user.
    Secret,
}

impl SettingKind {
    /// Name the settings screen uses for the field.
    pub fn as_str(self) -> &'static str {
        match self {
            SettingKind::Bool => "bool",
            SettingKind::Int => "int",
            SettingKind::Choice => "choice",
            SettingKind::Text => "text",
            SettingKind::Secret => "secret",
        }
    }

    pub fn is_text(self) -> bool {
        matches!(self, SettingKind::Text | SettingKind::Secret)
    }
}

/// A setting declared in the manifest; becomes a form field on the "Apps" tab.
#[derive(Debug, Clone, Deserialize)]
pub struct SettingDef {
    pub key: String,
    pub label: String,
    #[serde(rename = "type")]
    pub kind: SettingKind,
    pub default: toml::Value,
    #[serde(default)]
    pub min: Option<i64>,
    #[serde(default)]
    pub max: Option<i64>,
    #[serde(default)]
    pub step: Option<i64>,
    /// Options of a `choice`. For a mascot choice, the manifest's options are the suggested
    /// ones and come first; `Manifest::parse` completes the list with the other mascots.
    #[serde(default)]
    pub options: Vec<String>,
    /// Longest value of a `text` or `secret` (characters); `TEXT_MAX_LEN` without it.
    #[serde(default)]
    pub max_len: Option<usize>,
}

impl SettingDef {
    /// What the user typed in a `text` or `secret` field, as it is saved: one line, no
    /// surrounding spaces (a pasted token often brings a line break), at most `max_len`
    /// characters. `None` if the setting is not a text one.
    pub fn clean_text(&self, typed: &str) -> Option<String> {
        if !self.kind.is_text() {
            return None;
        }
        let max = self.max_len.unwrap_or(TEXT_MAX_LEN);
        Some(typed.trim().chars().filter(|c| !c.is_control()).take(max).collect())
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
            s.options.extend(rest);
        }
        m.check().map_err(anyhow::Error::msg)?;
        Ok(m)
    }

    /// Checks what serde can't: the API version, the settings (a default of the right type,
    /// within range, among the options; unique keys) and `[data]`. The errors are shown to
    /// the user (Settings › System, `--plugins`), so they are translated.
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
            if s.kind.is_text() && self.api < API_TEXT_SETTINGS {
                return Err(tr!("mini-app {}: setting {} is a {}, which needs api = {}", id, key, s.kind.as_str(), API_TEXT_SETTINGS));
            }
            let typed = match s.kind {
                SettingKind::Bool => s.default.is_bool(),
                SettingKind::Int => s.default.is_integer(),
                SettingKind::Choice | SettingKind::Text | SettingKind::Secret => s.default.is_str(),
            };
            if !typed {
                return Err(tr!("mini-app {}: the default of setting {} is not a {} value", id, key, s.kind.as_str()));
            }
            match s.kind {
                SettingKind::Int => {
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
                // the theme and mascot choices are completed at runtime (file themes, imported
                // mascots): their default can't be checked against the manifest alone
                SettingKind::Choice if key != "theme" && !sprites::is_choice(&s.options) => {
                    let d = s.default.as_str().unwrap_or_default();
                    if !s.options.iter().any(|o| o == d) {
                        return Err(tr!("mini-app {}: the default of setting {} is not one of its options", id, key));
                    }
                }
                SettingKind::Text | SettingKind::Secret if s.max_len == Some(0) => {
                    return Err(tr!("mini-app {}: setting {} has max_len = 0", id, key));
                }
                _ => {}
            }
        }
        if let Some(d) = &self.data {
            d.check()?;
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
}
