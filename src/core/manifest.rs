//! Formato do `manifest.toml` de um mini-app.

use serde::Deserialize;

use super::appdata::DataSpec;
use super::bus::Topic;
use crate::mascot::sprites;

/// Versão do contrato entre host e mini-apps que este binário entende.
pub const API_VERSION: u32 = 1;

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Mascote usada na aba do mini-app.
    #[serde(default = "default_icon")]
    pub icon: String,
    pub api: u32,
    /// Provedores de dados que o app assina enquanto está ativo.
    #[serde(default)]
    pub uses: Vec<Topic>,
    #[serde(default)]
    pub permissions: Permissions,
    #[serde(default)]
    pub settings: Vec<SettingDef>,
    /// O que o app guarda (`[data]`; ver `core/appdata.rs`). Sem a seção, vale o padrão
    /// do tipo de app (`Plugin::data`).
    #[serde(default)]
    pub data: Option<DataSpec>,
}

fn default_icon() -> String {
    "neko".into()
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Permissions {
    /// Programas que o app pode executar (allowlist do executor).
    #[serde(default)]
    pub exec: Vec<String>,
    /// Domínios que o app pode consultar por HTTPS (só plugins com tela; ver `apps/plugin/script.rs`).
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

/// Uma configuração declarada no manifesto; vira formulário na aba "Apps".
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
    /// Opções de um `choice`. Numa opção de mascote, as do manifesto são as sugeridas e
    /// vêm primeiro; `Manifest::parse` completa a lista com as outras mascotes.
    #[serde(default)]
    pub options: Vec<String>,
}

impl SettingDef {
    /// As opções que a tela mostra. A do tema (`theme`) é sempre "auto" mais todos os
    /// temas, inclusive os de arquivo: um tema novo aparece em todos os apps sem editar
    /// manifestos. As demais são as do manifesto.
    pub fn live_options(&self) -> Vec<String> {
        if self.key == "theme" && self.kind == SettingKind::Choice {
            let mut v = vec!["auto".to_string()];
            v.extend(crate::mascot::themes::all().iter().map(|t| t.key.to_string()));
            return v;
        }
        self.options.clone()
    }
}

impl Manifest {
    pub fn parse(text: &str) -> anyhow::Result<Self> {
        let mut m: Manifest = toml::from_str(text)?;
        // Opção de mascote: o manifesto lista as que combinam com o app (sugeridas), mas
        // qualquer mascote pode ser escolhida. As outras entram depois, na ordem de
        // `SPRITES`; assim uma mascote nova aparece em todos os apps sem editar manifestos.
        for s in m.settings.iter_mut().filter(|s| s.kind == SettingKind::Choice && sprites::is_choice(&s.options)) {
            let rest: Vec<String> =
                sprites::SPRITES.iter().map(|sp| sp.key.to_string()).filter(|k| !s.options.contains(k)).collect();
            s.options.extend(rest);
        }
        anyhow::ensure!(
            m.api == API_VERSION,
            "mini-app {} usa a API {} (este hub entende a {})",
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
    fn manifestos_embutidos_sao_validos() {
        for text in [
            include_str!("../../miniapps/docker/manifest.toml"),
            include_str!("../../miniapps/vitals/manifest.toml"),
            include_str!("../../miniapps/cdplayer/manifest.toml"),
            include_str!("../../miniapps/ai_usage/manifest.toml"),
            include_str!("../../miniapps/ports/manifest.toml"),
            include_str!("../../miniapps/todo/manifest.toml"),
            include_str!("../../miniapps/agents/manifest.toml"),
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
    fn opcao_de_mascote_oferece_todas_com_as_sugeridas_na_frente() {
        let vitals = Manifest::parse(include_str!("../../miniapps/vitals/manifest.toml")).unwrap();
        let gpu = &vitals.setting("gpu_mascot").unwrap().options;
        // as do manifesto primeiro, na ordem dele ("auto" continua em primeiro)
        assert_eq!(gpu[..7], ["auto", "nvidia", "amd", "robo", "neko", "chip", "vagalume"]);
        assert_eq!(gpu.len(), sprites::SPRITES.len() + 1, "todas as mascotes, mais o auto");
        let mut sorted = gpu.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), gpu.len(), "sem repetição");
        // opção que não é de mascote fica como está no manifesto
        let theme = vitals.setting("theme").unwrap().live_options();
        assert!(theme.contains(&"auto".to_string()) && !theme.contains(&"neko".to_string()));
        // a do tema é gerada: "auto" mais todos os temas, sem lista no manifesto
        assert_eq!(theme.len(), crate::mascot::themes::all().len() + 1);
        assert!(vitals.setting("theme").unwrap().options.is_empty());
    }

    #[test]
    fn rejeita_provedor_desconhecido() {
        assert!(Manifest::parse("id='x'\nname='x'\napi=1\nuses=['sys']").is_ok());
        assert!(Manifest::parse("id='x'\nname='x'\napi=1\nuses=['sis']").is_err());
    }

    #[test]
    fn rejeita_api_desconhecida() {
        assert!(Manifest::parse("id='x'\nname='x'\napi=99").is_err());
    }
}
