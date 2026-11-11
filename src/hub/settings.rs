//! Aba Configurações: a lista de apps e o formulário gerado do manifesto de cada um.

use std::cell::RefCell;
use std::collections::HashMap;

use slint::{Image, ModelRc, SharedString, VecModel};

use super::Hub;
use crate::core::manifest::{Manifest, SettingKind};
use crate::core::miniapp::AppRef;
use crate::mascot::themes::Theme;
use crate::mascot::{Mood, render, sprites, themes};
use crate::ui::{AppInfo, SettingRow};

thread_local! {
    /// Mascotes já desenhadas para os seletores, por tema (a UI só existe nesta thread).
    static GALLERY: RefCell<HashMap<&'static str, Vec<Image>>> = RefCell::new(HashMap::new());
}

/// Todas as mascotes desenhadas no tema `theme`, na ordem de `sprites::SPRITES`: é o que
/// os seletores de mascote mostram. Desenha uma vez por tema e reaproveita.
pub(super) fn mascot_images(theme: &'static Theme) -> Vec<Image> {
    GALLERY.with(|g| {
        g.borrow_mut()
            .entry(theme.key)
            .or_insert_with(|| sprites::SPRITES.iter().map(|s| render::image(s, theme, Mood::Idle, 1)).collect())
            .clone()
    })
}

/// Desenho de cada alternativa de uma escolha de mascote ("auto" fica sem desenho, e o
/// seletor mostra um brilho). Vazio se a opção não é de mascote: aí a tela usa a lista.
fn option_images(options: &[String], theme: &'static Theme) -> Vec<Image> {
    if !sprites::is_choice(options) {
        return Vec::new();
    }
    let all = mascot_images(theme);
    options
        .iter()
        .map(|o| sprites::SPRITES.iter().position(|s| s.key == *o).map_or_else(Image::default, |i| all[i].clone()))
        .collect()
}

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
        let theme = self.host.theme_for(m);
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
                    images: ModelRc::new(VecModel::from(option_images(&s.options, theme))),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconhece_as_opcoes_de_mascote_dos_manifestos() {
        let docker = Manifest::parse(include_str!("../../miniapps/docker/manifest.toml")).unwrap();
        let vitals = Manifest::parse(include_str!("../../miniapps/vitals/manifest.toml")).unwrap();
        assert!(sprites::is_choice(&docker.setting("mascot").unwrap().options));
        assert!(!sprites::is_choice(&docker.setting("theme").unwrap().options), "tema não é mascote");
        // "auto" entre as alternativas continua sendo escolha de mascote, e fica sem desenho
        let gpu = &vitals.setting("gpu_mascot").unwrap().options;
        assert!(gpu.iter().any(|o| o == "auto") && sprites::is_choice(gpu));
        let images = option_images(gpu, themes::get("neon"));
        assert_eq!(images.len(), gpu.len());
        for (o, img) in gpu.iter().zip(&images) {
            assert_eq!(img.size().width == 0, o == "auto", "{o}");
        }
        assert!(option_images(&docker.setting("theme").unwrap().options, themes::get("neon")).is_empty());
        // só "auto" (sem nenhuma mascote) não é escolha de mascote
        assert!(!sprites::is_choice(&["auto".to_string()]));
    }
}
