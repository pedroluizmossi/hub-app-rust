//! Configuração do usuário em `~/.config/cyberhub/config.toml`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::manifest::Manifest;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Mini-apps ligados, na ordem das abas.
    pub enabled: Vec<String>,
    /// Configurações de cada mini-app: `[apps."cyber.docker"]`.
    pub apps: BTreeMap<String, toml::Table>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            enabled: vec!["cyber.docker".into(), "cyber.vitals".into()],
            apps: BTreeMap::new(),
        }
    }
}

impl Config {
    pub fn path() -> PathBuf {
        dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("cyberhub/config.toml")
    }

    /// Lê a configuração; se não existir ou estiver quebrada, usa o padrão.
    pub fn load() -> Self {
        match std::fs::read_to_string(Self::path()) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
                eprintln!("cyberhub: config inválida ({e}); usando o padrão");
                Config::default()
            }),
            Err(_) => Config::default(),
        }
    }

    pub fn save(&self) {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match toml::to_string_pretty(self) {
            Ok(text) => {
                if let Err(e) = std::fs::write(&path, text) {
                    eprintln!("cyberhub: não consegui salvar {}: {e}", path.display());
                }
            }
            Err(e) => eprintln!("cyberhub: erro ao serializar config: {e}"),
        }
    }

    pub fn is_enabled(&self, id: &str) -> bool {
        self.enabled.iter().any(|e| e == id)
    }

    /// Valor de uma configuração do app, ou o padrão do manifesto.
    pub fn value(&self, m: &Manifest, key: &str) -> toml::Value {
        self.apps
            .get(&m.id)
            .and_then(|t| t.get(key))
            .cloned()
            .or_else(|| m.setting(key).map(|s| s.default.clone()))
            .unwrap_or(toml::Value::Boolean(false))
    }

    pub fn bool(&self, m: &Manifest, key: &str) -> bool {
        self.value(m, key).as_bool().unwrap_or(false)
    }

    pub fn int(&self, m: &Manifest, key: &str) -> i64 {
        self.value(m, key).as_integer().unwrap_or(0)
    }

    pub fn string(&self, m: &Manifest, key: &str) -> String {
        self.value(m, key).as_str().unwrap_or_default().to_string()
    }

    pub fn set(&mut self, app: &str, key: &str, value: toml::Value) {
        self.apps.entry(app.to_string()).or_default().insert(key.to_string(), value);
    }
}
