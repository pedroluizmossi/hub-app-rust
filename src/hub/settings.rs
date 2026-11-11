//! Aba Configurações: a lista de apps e o formulário gerado do manifesto de cada um.

use slint::{ModelRc, SharedString, VecModel};

use super::Hub;
use crate::core::manifest::{Manifest, SettingKind};
use crate::core::miniapp::AppRef;
use crate::mascot::{Mood, render, sprites, themes};
use crate::ui::{AppInfo, SettingRow};

/// O que o usuário fez num campo da aba Apps.
pub(super) enum Input {
    Toggle,
    Int(i64),
    Choice(usize),
}

impl Hub {
    pub(super) fn app_info(&self, app: &AppRef) -> AppInfo {
        let a = app.borrow();
        let m = a.manifest();
        let settings = self.settings_of(m);
        let theme = self.host.theme_for(m);
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
        }
    }

    /// Grade do painel, barra lateral e lista da aba Apps.
    pub(super) fn rebuild_lists(&self) {
        self.rebuild_grid();
        self.rebuild_pages(true);
        self.rebuild_settings();
    }

    /// Lista das Configurações, cada app com o formulário gerado do manifesto.
    ///
    /// Só é refeita quando muda o que ela mostra: refazer a lista recria os campos e
    /// fecharia um select aberto (era o que acontecia a cada leitura de CPU).
    pub(super) fn rebuild_settings(&self) {
        let fingerprint = {
            let cfg = self.host.config.borrow();
            format!("{:?}|{:?}|{}|{:?}", cfg.enabled, cfg.apps, cfg.skin, cfg.pages)
        };
        if self.settings_fingerprint.replace(fingerprint.clone()) == fingerprint {
            return;
        }
        let apps: Vec<AppInfo> = self.registry.all().iter().map(|a| self.app_info(a)).collect();
        self.ui.set_apps(ModelRc::new(VecModel::from(apps)));
    }

    pub(super) fn settings_of(&self, m: &Manifest) -> Vec<SettingRow> {
        let cfg = self.host.config.borrow();
        m.settings
            .iter()
            .map(|s| {
                let v = cfg.value(m, &s.key);
                let (label, unit) = split_unit(&s.label);
                SettingRow {
                    app: m.id.clone().into(),
                    key: s.key.clone().into(),
                    label: label.into(),
                    unit: unit.into(),
                    kind: match s.kind {
                        SettingKind::Bool => "bool",
                        SettingKind::Int => "int",
                        SettingKind::Choice => "choice",
                    }
                    .into(),
                    bool_value: v.as_bool().unwrap_or(false),
                    int_value: v.as_integer().unwrap_or(0) as i32,
                    minimum: s.min.unwrap_or(0) as i32,
                    maximum: s.max.unwrap_or(i32::MAX as i64) as i32,
                    step: s.step.unwrap_or(1) as i32,
                    options: ModelRc::new(VecModel::from(
                        s.options.iter().map(|o| SharedString::from(option_label(o))).collect::<Vec<_>>(),
                    )),
                    index: s.options.iter().position(|o| Some(o.as_str()) == v.as_str()).unwrap_or(0) as i32,
                }
            })
            .collect()
    }

    pub(super) fn change_setting(&self, app_id: &str, key: &str, input: Input) {
        let Some(app) = self.registry.find(app_id).cloned() else { return };
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
                (SettingKind::Choice, Input::Choice(i)) => match def.options.get(i) {
                    Some(o) => toml::Value::String(o.clone()),
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
        // tema ou mascote podem ter mudado: os ícones da tela (a bandeja se compara sozinha)
        self.rebuild_lists();
    }
}

/// Rótulo de uma opção de `choice`: nome do tema ou da mascote, se for um.
fn option_label(key: &str) -> String {
    if key == "auto" {
        "Acompanha a janela".into()
    } else if let Some(t) = themes::THEMES.iter().find(|t| t.key == key) {
        t.label.into()
    } else if let Some(s) = sprites::SPRITES.iter().find(|s| s.key == key) {
        s.label.into()
    } else {
        key.into()
    }
}

/// Separa a unidade do fim do rótulo: "Temperatura quente (°C)" -> ("Temperatura quente", "°C").
fn split_unit(label: &str) -> (String, String) {
    for unit in ["%", "°C"] {
        if let Some(base) = label.strip_suffix(&format!(" ({unit})")) {
            return (base.to_string(), unit.to_string());
        }
    }
    (label.to_string(), String::new())
}
