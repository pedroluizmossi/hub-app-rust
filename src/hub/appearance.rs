//! Aparência: visual da janela, fundo, ícone da bandeja, movimento e relógio.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use slint::{ComponentHandle, Image, ModelRc, SharedString, Timer, VecModel};

use super::Hub;
use super::chatter::Pace;
use crate::core::config::Config;
use crate::core::plugins;
use crate::mascot::{sprites, themes};
use crate::skin;
use crate::ui::{AmbientItem, Anim};

/// Tamanhos de texto oferecidos (em % do design), na ordem do seletor.
pub(super) const TEXT_SIZES: &[u32] = &[90, 100, 110, 125];

/// Onde ficam as páginas (`nav` da config), na ordem da escolha em Aparência › Páginas
/// (`ui/settings.slint`): barra à esquerda, à direita, embaixo, abas no cabeçalho.
/// `"side"` é a barra à esquerda (nome antigo, mantido para as configs que já existem).
pub(super) const NAVS: &[&str] = &["side", "right", "bottom", "top"];

/// Posição de `nav` em `NAVS`; valor desconhecido (config editada à mão) vira a da esquerda.
fn nav_index(nav: &str) -> usize {
    NAVS.iter().position(|n| *n == nav).unwrap_or(0)
}

/// O fundo da área do painel: pedidos em curso e o último desenhado. Compartilhado com a
/// thread que desenha.
#[derive(Default)]
pub(super) struct BackdropState {
    /// Sobe a cada pedido; resultado de um pedido velho é descartado.
    generation: AtomicU32,
    /// Visual (com o destaque), largura e altura em pixels físicos e escala do fundo que
    /// está na tela.
    last: Mutex<Option<(String, u32, u32, u32)>>,
}

impl BackdropState {
    /// Esquece o último fundo: o próximo pedido redesenha mesmo com o mesmo visual (um
    /// fundo de arquivo pode ter mudado).
    pub(super) fn forget(&self) {
        if let Ok(mut last) = self.last.lock() {
            *last = None;
        }
    }
}

impl Hub {
    /// Aplica o visual da config (com a cor de destaque e o tamanho do texto): tokens da
    /// tela, listas dos seletores e fundo.
    pub(super) fn apply_skin(&self) {
        let (key, name, accent, scale, ambient) = {
            let cfg = self.host.config.borrow();
            (cfg.skin.clone(), crate::core::config::display_name(), cfg.accent.clone(), cfg.text_scale, cfg.ambient)
        };
        let effective = skin::effective(skin::get(&key), &accent);
        skin::apply(&self.ui, &effective, &name, text_scale(scale));
        self.ui.set_app_name(name.into());
        let labels: Vec<SharedString> = skin::all().iter().map(|s| s.label.into()).collect();
        self.ui.set_skins(ModelRc::new(VecModel::from(labels)));
        self.ui.set_skin_index(skin::index_of(&key) as i32);
        let accents: Vec<SharedString> =
            std::iter::once("Do visual".into()).chain(skin::ACCENTS.iter().map(|a| a.1.into())).collect();
        self.ui.set_accents(ModelRc::new(VecModel::from(accents)));
        self.ui.set_accent_index(skin::ACCENTS.iter().position(|a| a.0 == accent).map_or(0, |i| i as i32 + 1));
        let sizes: Vec<SharedString> = TEXT_SIZES.iter().map(|p| format!("{p}%").into()).collect();
        self.ui.set_text_sizes(ModelRc::new(VecModel::from(sizes)));
        self.ui.set_text_size_index(TEXT_SIZES.iter().position(|p| *p == scale).unwrap_or(1) as i32);
        self.ui.set_ambient_on(ambient);
        self.apply_nav();
        let chatter = self.host.config.borrow().chatter.clone();
        self.ui.set_chatter_index(Pace::KEYS.iter().position(|k| *k == chatter).unwrap_or(1) as i32);
        self.apply_motion();
        self.apply_tray_choices();
        self.schedule_backdrop();
    }

    /// Opções e escolhas do ícone da bandeja (índice 0 = automático).
    pub(super) fn apply_tray_choices(&self) {
        let (icon, theme, skin_key) = {
            let cfg = self.host.config.borrow();
            (cfg.tray_icon.clone(), cfg.tray_theme.clone(), cfg.skin.clone())
        };
        // desenhos para o seletor: no tema escolhido para a bandeja ou, no automático, no
        // do visual da janela; o "Automático" (índice 0) fica sem desenho
        let shown = themes::find(&theme).unwrap_or(themes::get(skin::get(&skin_key).app_theme));
        let images: Vec<Image> =
            std::iter::once(Image::default()).chain(super::settings::mascot_images(shown)).collect();
        self.ui.set_tray_icon_images(ModelRc::new(VecModel::from(images)));
        let auto = |label: &str| std::iter::once(SharedString::from(label));
        let icons: Vec<SharedString> =
            auto("Automático").chain(sprites::SPRITES.iter().map(|s| s.label.into())).collect();
        let themes_: Vec<SharedString> =
            auto("Automático").chain(themes::all().iter().map(|t| t.label.into())).collect();
        let pos = |found: Option<usize>| found.map_or(0, |i| i as i32 + 1);
        self.ui.set_tray_icons(ModelRc::new(VecModel::from(icons)));
        self.ui.set_tray_icon_index(pos(sprites::SPRITES.iter().position(|s| s.key == icon)));
        self.ui.set_tray_themes(ModelRc::new(VecModel::from(themes_)));
        self.ui.set_tray_theme_index(pos(themes::all().iter().position(|t| t.key == theme)));
    }

    /// Troca a mascote do ícone da bandeja. 0 = automático (o app em pior estado).
    pub(super) fn choose_tray_icon(&self, i: usize) {
        let key = if i == 0 { Some("auto") } else { sprites::SPRITES.get(i - 1).map(|s| s.key) };
        self.set_tray_choice(key, |cfg| &mut cfg.tray_icon);
    }

    /// Troca o tema do ícone da bandeja. 0 = automático (o tema do app em pior estado).
    pub(super) fn choose_tray_theme(&self, i: usize) {
        let key = if i == 0 { Some("auto") } else { themes::all().get(i - 1).map(|t| t.key) };
        self.set_tray_choice(key, |cfg| &mut cfg.tray_theme);
    }

    /// Grava uma escolha do ícone da bandeja; o ícone em si muda no próximo `sync_tray`.
    fn set_tray_choice(&self, key: Option<&str>, field: impl FnOnce(&mut Config) -> &mut String) {
        let Some(key) = key else { return };
        {
            let mut cfg = self.host.config.borrow_mut();
            let campo = field(&mut cfg);
            if campo == key {
                return;
            }
            *campo = key.into();
            cfg.save();
        }
        self.apply_tray_choices();
    }

    /// Reduzir movimento: mascotes paradas na tela (`Anim.calm`); os apps deixam de disparar
    /// reações (eles leem `reduce_motion` da config).
    pub(super) fn apply_motion(&self) {
        let calm = self.host.config.borrow().reduce_motion;
        crate::mascot::anim::set_calm(calm);
        self.ui.set_reduce_motion(calm);
        self.ui.global::<Anim>().set_calm(calm);
    }

    /// Cartão "Sistema" das configurações: iniciar com o sistema, versão e registros. Relido
    /// ao abrir as configurações (o autostart pode ter mudado por fora, no sistema).
    pub(super) fn apply_system(&self) {
        self.ui.set_autostart(crate::autostart::is_enabled());
        self.ui.set_version(crate::core::about::version_line().into());
        let dir = crate::core::os::log_dir().map(|d| d.display().to_string()).unwrap_or_default();
        self.ui.set_log_dir(dir.into());
        self.apply_apps_dir();
    }

    /// Linha "Apps locais" do cartão Sistema: a pasta, quantos apps há nela e as pastas
    /// recusadas com o motivo (pasta com nome diferente do id, manifesto com erro…).
    pub(super) fn apply_apps_dir(&self) {
        let root = plugins::dir();
        let (list, skipped) = plugins::scan(&root);
        let from_store = list.iter().filter(|p| crate::store::installed(&root, &p.manifest.id).is_some()).count();
        let local = list.len() - from_store;
        let summary = match (local, from_store) {
            (0, 0) => "nenhum app na pasta".to_string(),
            _ => format!("{local} {} · {from_store} da loja", if local == 1 { "app local" } else { "apps locais" }),
        };
        let skipped: Vec<String> = skipped.iter().map(|s| format!("{}/ ignorada: {}", s.folder, s.reason)).collect();
        self.ui.set_apps_dir(root.display().to_string().into());
        self.ui.set_apps_summary(summary.into());
        self.ui.set_apps_skipped(skipped.join("\n").into());
    }

    /// Abre a pasta dos apps no gerenciador de arquivos (cria antes, se ainda não existe).
    pub(super) fn open_apps_dir(&self) {
        let dir = plugins::dir();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            log::warn!("não consegui criar {}: {e}", dir.display());
        } else if !crate::core::exec::open_dir(&dir) {
            log::warn!("não consegui abrir {}", dir.display());
        }
    }

    /// O erro, se houver, vai para o registro; o interruptor mostra o estado de verdade.
    pub(super) fn toggle_autostart(&self) {
        let _ = crate::autostart::set(!crate::autostart::is_enabled());
        self.ui.set_autostart(crate::autostart::is_enabled());
    }

    pub(super) fn open_logs(&self) {
        if let Some(dir) = crate::core::os::log_dir()
            && !crate::core::exec::open_dir(&dir)
        {
            log::warn!("não consegui abrir {}", dir.display());
        }
    }

    pub(super) fn toggle_motion(&self) {
        {
            let mut cfg = self.host.config.borrow_mut();
            cfg.reduce_motion = !cfg.reduce_motion;
            cfg.save();
        }
        self.apply_motion();
    }

    /// Onde ficam as páginas: índice em `NAVS`.
    pub(super) fn choose_nav(&self, i: usize) {
        let Some(nav) = NAVS.get(i).copied() else { return };
        {
            let mut cfg = self.host.config.borrow_mut();
            if cfg.nav == nav {
                return;
            }
            cfg.nav = nav.into();
            cfg.save();
        }
        // a área do painel muda de tamanho e avisa sozinha (`backdrop-resized`)
        self.apply_nav();
    }

    fn apply_nav(&self) {
        let i = nav_index(&self.host.config.borrow().nav);
        self.ui.set_nav(NAVS[i].into());
        self.ui.set_nav_index(i as i32);
    }

    /// Quanto as mascotes conversam (índice em `Pace::KEYS`: nunca, às vezes, bastante).
    pub(super) fn choose_chatter(&self, i: usize) {
        let Some(key) = Pace::KEYS.get(i) else { return };
        {
            let mut cfg = self.host.config.borrow_mut();
            if cfg.chatter == *key {
                return;
            }
            cfg.chatter = key.to_string();
            cfg.save();
        }
        self.ui.set_chatter_index(i as i32);
    }

    /// Troca a cor de destaque (0 = a do visual).
    pub(super) fn choose_accent(&self, i: usize) {
        let key = if i == 0 { "" } else { skin::ACCENTS.get(i - 1).map_or("", |a| a.0) };
        {
            let mut cfg = self.host.config.borrow_mut();
            if cfg.accent == key {
                return;
            }
            cfg.accent = key.into();
            cfg.save();
        }
        self.apply_skin();
    }

    /// Troca o tamanho do texto. Os apps refazem a tela (os plugins recebem a escala no `theme`).
    pub(super) fn choose_text_size(&self, i: usize) {
        let Some(&pct) = TEXT_SIZES.get(i) else { return };
        {
            let mut cfg = self.host.config.borrow_mut();
            if cfg.text_scale == pct {
                return;
            }
            cfg.text_scale = pct;
            cfg.save();
        }
        self.apply_skin();
        for app in self.registry.enabled() {
            app.borrow_mut().settings_changed(&self.ui);
        }
    }

    /// Liga ou desliga o fundo animado.
    pub(super) fn toggle_ambient(&self) {
        let on = {
            let mut cfg = self.host.config.borrow_mut();
            cfg.ambient = !cfg.ambient;
            cfg.save();
            cfg.ambient
        };
        self.ui.set_ambient_on(on);
    }

    /// Lê de novo a pasta de temas, visuais e fundos e mostra o resultado na linha
    /// "Temas, visuais e fundos seus" (sem aplicar: ver `reload_looks`).
    pub(super) fn load_looks(&self) {
        let loaded = skin::user::reload();
        self.ui.set_looks_dir(skin::user::dir().display().to_string().into());
        self.ui.set_looks_summary(loaded.summary().into());
        let skipped: Vec<String> = loaded.skipped.iter().map(|s| format!("{}: {}", s.file, s.reason)).collect();
        self.ui.set_looks_skipped(skipped.join("\n").into());
    }

    /// Botão Recarregar da Aparência (e `miniutils --recarregar`): relê os arquivos e
    /// redesenha tudo que usa tema, visual ou fundo.
    pub(super) fn reload_looks(&self) {
        self.load_looks();
        self.backdrop.forget();
        self.apply_skin();
        for app in self.registry.enabled() {
            app.borrow_mut().settings_changed(&self.ui);
        }
        // a lista das Configurações muda mesmo sem mudar a config (opções de tema novas)
        self.settings_fingerprint.replace(String::new());
        self.rebuild_lists();
    }

    /// Abre a pasta de temas, visuais e fundos (cria as três subpastas antes).
    pub(super) fn open_looks_dir(&self) {
        let root = skin::user::dir();
        for sub in [skin::user::TEMAS, skin::user::VISUAIS, skin::user::FUNDOS] {
            if let Err(e) = std::fs::create_dir_all(root.join(sub)) {
                log::warn!("não consegui criar {}: {e}", root.join(sub).display());
                return;
            }
        }
        if !crate::core::exec::open_dir(&root) {
            log::warn!("não consegui abrir {}", root.display());
        }
    }

    /// Troca o visual. Os apps com tema "auto" mudam junto.
    pub(super) fn choose_skin(&self, i: usize) {
        let all = skin::all();
        let Some(s) = all.get(i) else { return };
        {
            let mut cfg = self.host.config.borrow_mut();
            if cfg.skin == s.key {
                return;
            }
            cfg.skin = s.key.into();
            cfg.save();
        }
        self.apply_skin();
        for app in self.registry.enabled() {
            app.borrow_mut().settings_changed(&self.ui);
        }
        self.rebuild_lists();
    }

    /// Pede um fundo novo daqui a pouco (um só, mesmo com vários pedidos seguidos). O
    /// desenho roda numa thread, e só quando o visual, o tamanho ou a escala mudaram:
    /// reabrir a janela não redesenha nada.
    pub(super) fn schedule_backdrop(&self) {
        let state = self.backdrop.clone();
        let id = state.generation.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
        let weak = self.ui.as_weak();
        let size = self.backdrop_size.get();
        let (key, accent) = {
            let cfg = self.host.config.borrow();
            (cfg.skin.clone(), cfg.accent.clone())
        };
        Timer::single_shot(Duration::from_millis(60), move || {
            if state.generation.load(Ordering::Relaxed) != id {
                return;
            }
            let Some(ui) = weak.upgrade() else { return };
            let s = skin::effective(skin::get(&key), &accent);
            let window = ui.window();
            let scale = window.scale_factor();
            // sem o tamanho exato da área ainda: estima pela janela
            let (w, h) = if size.0 > 0.0 && size.1 > 0.0 {
                size
            } else {
                let px = window.size();
                let (ww, wh) = (px.width as f32 / scale, px.height as f32 / scale);
                let chrome = 2.0 * s.border + s.header_h + s.header_line + s.panel_top_h + s.footer_h;
                ((ww - 2.0 * s.border).max(1.0), (wh - chrome).max(1.0))
            };
            let drawn = (format!("{key}|{accent}"), (w * scale).round() as u32, (h * scale).round() as u32, scale.to_bits());
            if state.last.lock().is_ok_and(|last| last.as_ref() == Some(&drawn)) {
                return;
            }
            let fundo = skin::backdrop::get(s.backdrop);
            let spawned = std::thread::Builder::new().name("miniutils-backdrop".into()).spawn(move || {
                let buf = skin::backdrop::render(fundo, &s, w, h, scale);
                let ambient = skin::backdrop::ambient(fundo, &s, w, h);
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    // um pedido mais novo já está a caminho: este fundo ficou velho
                    if state.generation.load(Ordering::Relaxed) != id {
                        return;
                    }
                    ui.set_backdrop(slint::Image::from_rgb8(buf));
                    let items: Vec<AmbientItem> = ambient.iter().map(ambient_item).collect();
                    ui.set_ambient(ModelRc::new(VecModel::from(items)));
                    if let Ok(mut last) = state.last.lock() {
                        *last = Some(drawn);
                    }
                });
            });
            if let Err(e) = spawned {
                log::warn!("sem thread para o fundo ({e})");
            }
        });
    }

    /// Relógio e saudação, atualizados quando muda o minuto.
    pub(super) fn update_clock(&self) {
        let (hour, min) = local_time();
        let text = format!("{hour:02}:{min:02}");
        if *self.clock.borrow() == text {
            return;
        }
        let part = match hour {
            5..=11 => "bom dia",
            12..=17 => "boa tarde",
            _ => "boa noite",
        };
        self.ui.set_greeting(format!("{part} · {text}").into());
        self.ui.set_clock(text.clone().into());
        *self.clock.borrow_mut() = text;
    }
}

/// Hora e minuto locais.
fn local_time() -> (i32, i32) {
    use chrono::Timelike;
    let now = chrono::Local::now();
    (now.hour() as i32, now.minute() as i32)
}

/// % da config para o fator da tela, dentro dos tamanhos oferecidos.
fn text_scale(pct: u32) -> f32 {
    pct.clamp(TEXT_SIZES[0], TEXT_SIZES[TEXT_SIZES.len() - 1]) as f32 / 100.0
}

fn ambient_item(a: &skin::backdrop::Ambient) -> AmbientItem {
    AmbientItem {
        kind: a.kind.into(),
        x: a.x,
        y: a.y,
        w: a.w,
        h: a.h,
        color: crate::mascot::render::color(a.color),
        alpha: a.alpha,
        phase: a.phase,
        period: a.period.max(1),
        on: a.on,
        speed: a.speed,
        slant: if a.kind == "fall" { skin::backdrop::RAIN_SLANT } else { 0.0 },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posicao_das_paginas() {
        assert_eq!(nav_index("side"), 0);
        assert_eq!(nav_index("right"), 1);
        assert_eq!(nav_index("bottom"), 2);
        assert_eq!(nav_index("top"), 3);
        // config editada à mão: volta para a barra à esquerda
        assert_eq!(nav_index("esquerda"), 0);
    }
}
