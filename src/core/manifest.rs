//! Formato do `manifest.toml` de um mini-app.

use serde::Deserialize;

use super::bus::Topic;

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
}

fn default_icon() -> String {
    "neko".into()
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Permissions {
    /// Programas que o app pode executar (allowlist do executor).
    #[serde(default)]
    pub exec: Vec<String>,
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
    /// Opções de um `choice`.
    #[serde(default)]
    pub options: Vec<String>,
}

impl Manifest {
    pub fn parse(text: &str) -> anyhow::Result<Self> {
        let m: Manifest = toml::from_str(text)?;
        anyhow::ensure!(
            m.api == API_VERSION,
            "mini-app {} usa a API {} (este hub entende a {})",
            m.id,
            m.api,
            API_VERSION
        );
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
        ] {
            let m = Manifest::parse(text).unwrap();
            for s in &m.settings {
                match s.kind {
                    SettingKind::Bool => assert!(s.default.is_bool(), "{}", s.key),
                    SettingKind::Int => assert!(s.default.is_integer(), "{}", s.key),
                    SettingKind::Choice => assert!(s.options.contains(&s.default.as_str().unwrap().to_string())),
                }
            }
        }
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
