//! Format of a mini-app's `manifest.toml`.

use serde::Deserialize;

use super::appdata::DataSpec;
use super::bus::Topic;
use crate::mascot::sprites;

/// Version of the host/mini-app contract this binary understands.
pub const API_VERSION: u32 = 1;

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
}

impl SettingDef {
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
        anyhow::ensure!(
            m.api == API_VERSION,
            "mini-app {} uses API {} (this hub understands {})",
            m.id,
            m.api,
            API_VERSION
        );
        if let Some(d) = &m.data {
            d.check().map_err(anyhow::Error::msg)?;
        }
        Ok(m)
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
            include_str!("../../miniapps/vitals/manifest.toml"),
            include_str!("../../miniapps/cdplayer/manifest.toml"),
            include_str!("../../miniapps/ai_usage/manifest.toml"),
            include_str!("../../miniapps/ports/manifest.toml"),
            include_str!("../../miniapps/todo/manifest.toml"),
            include_str!("../../miniapps/agents/manifest.toml"),
            include_str!("../../miniapps/mascots/manifest.toml"),
        ] {
            let m = Manifest::parse(text).unwrap();
            for s in &m.settings {
                match s.kind {
                    SettingKind::Bool => assert!(s.default.is_bool(), "{}", s.key),
                    SettingKind::Int => assert!(s.default.is_integer(), "{}", s.key),
                    SettingKind::Choice => {
                        assert!(s.live_options().contains(&s.default.as_str().unwrap().to_string()), "{}", s.key)
                    }
                }
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
    }
}
