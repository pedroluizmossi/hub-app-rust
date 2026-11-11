//! Páginas do painel: barra lateral, página aberta e edição (criar, renomear, mover).

use slint::{ModelRc, SharedString, VecModel};

use super::Hub;
use crate::core::config::Config;
use crate::mascot::{Mood, render, sprites, themes};
use crate::skin;
use crate::ui::PageInfo;

impl Hub {
    /// Página aberta, sempre válida (a config pode ter perdido páginas).
    pub(super) fn current_page(&self) -> usize {
        let n = self.host.config.borrow().pages.len().max(1);
        let p = self.page.get().min(n - 1);
        self.page.set(p);
        p
    }

    /// Abre uma aba pelo nome: "apps" (configurações), "panel" ou o nome de uma página.
    pub(super) fn open_tab(&self, tab: &str) {
        if tab == "apps" {
            self.ui.set_current("apps".into());
            return;
        }
        let found = {
            let cfg = self.host.config.borrow();
            cfg.pages.iter().position(|p| p.name.trim().eq_ignore_ascii_case(tab.trim()))
        };
        if let Some(i) = found {
            self.page.set(i);
        }
        self.ui.set_current("panel".into());
    }

    /// Humor da página: o pior dos apps ligados dela.
    pub(super) fn page_mood(&self, page: usize) -> Mood {
        let ids = self.host.config.borrow().page_apps(page);
        ids.iter()
            .filter_map(|id| self.registry.find(id))
            .map(|a| a.borrow().mood())
            .fold(Mood::Sleep, Mood::worst)
    }

    /// Barra lateral e nomes das páginas. Sem `force`, só refaz se algo visível mudou
    /// (nome, mascote, humor): é chamada a cada leitura dos provedores.
    pub(super) fn rebuild_pages(&self, force: bool) {
        let current = self.current_page();
        let (pages, skin_key) = {
            let cfg = self.host.config.borrow();
            let pages: Vec<(String, String, usize)> =
                (0..cfg.pages.len()).map(|i| (cfg.pages[i].name.clone(), cfg.pages[i].icon.clone(), cfg.page_apps(i).len())).collect();
            (pages, cfg.skin.clone())
        };
        let moods: Vec<Mood> = (0..pages.len()).map(|i| self.page_mood(i)).collect();
        let fingerprint = format!("{pages:?}|{moods:?}|{skin_key}|{current}");
        if !force && *self.pages_fingerprint.borrow() == fingerprint {
            return;
        }
        *self.pages_fingerprint.borrow_mut() = fingerprint;
        let theme = themes::get(skin::get(&skin_key).app_theme);
        let infos: Vec<PageInfo> = pages
            .iter()
            .zip(&moods)
            .map(|((name, icon, n), mood)| PageInfo {
                name: name.as_str().into(),
                icon: render::image(sprites::get(icon), theme, *mood, 1),
                icon_index: sprites::SPRITES.iter().position(|s| s.key == icon).unwrap_or(0) as i32,
                mood: mood.as_str().into(),
                theme: render::colors(theme),
                apps: *n as i32,
            })
            .collect();
        let names: Vec<SharedString> = pages.iter().map(|(n, _, _)| SharedString::from(n.as_str())).collect();
        let mascots: Vec<SharedString> = sprites::SPRITES.iter().map(|s| s.label.into()).collect();
        self.ui.set_pages(ModelRc::new(VecModel::from(infos)));
        self.ui.set_page_names(ModelRc::new(VecModel::from(names)));
        self.ui.set_mascots(ModelRc::new(VecModel::from(mascots)));
        self.ui.set_mascot_images(ModelRc::new(VecModel::from(super::settings::mascot_images(theme))));
        self.ui.set_page(current as i32);
    }

    /// Abre a página `i` no painel.
    pub(super) fn select_page(&self, i: usize) {
        if i >= self.host.config.borrow().pages.len() {
            return;
        }
        let changed = self.page.replace(i) != i;
        // edição em curso fica na página onde começou
        if changed {
            self.drag.borrow_mut().take();
            self.set_ghost(None);
        }
        self.ui.set_current("panel".into());
        self.rebuild_grid();
        self.rebuild_pages(false);
        self.anim_tick();
        self.schedule_backdrop();
    }

    /// Muda a config das páginas, salva e redesenha grade, barra e configurações.
    pub(super) fn edit_pages(&self, f: impl FnOnce(&mut Config) -> bool) {
        {
            let mut cfg = self.host.config.borrow_mut();
            if !f(&mut cfg) {
                return;
            }
            cfg.arrange();
            cfg.save();
        }
        self.rebuild_lists();
    }

    pub(super) fn add_page(&self) {
        let mut added = 0;
        self.edit_pages(|cfg| {
            added = cfg.add_page();
            true
        });
        self.select_page(added);
    }

    /// Apaga a página `i`; a aberta continua a mesma (ou a anterior, se foi ela que saiu).
    pub(super) fn remove_page(&self, i: usize) {
        if self.host.config.borrow().pages.len() <= 1 {
            return;
        }
        let cur = self.page.get();
        if cur >= i && cur > 0 {
            self.page.set(cur - 1);
        }
        self.edit_pages(|cfg| cfg.remove_page(i));
    }

    pub(super) fn rename_page(&self, i: usize, name: &str) {
        let name = name.trim().to_string();
        self.edit_pages(|cfg| match cfg.pages.get_mut(i) {
            Some(p) if !name.is_empty() && p.name != name => {
                p.name = name;
                true
            }
            _ => false,
        });
    }

    pub(super) fn page_icon(&self, i: usize, k: usize) {
        let Some(s) = sprites::SPRITES.get(k) else { return };
        self.edit_pages(|cfg| match cfg.pages.get_mut(i) {
            Some(p) if p.icon != s.key => {
                p.icon = s.key.into();
                true
            }
            _ => false,
        });
    }

    /// Sobe (`-1`) ou desce (`+1`) a página na barra lateral; a aberta continua aberta.
    pub(super) fn move_page(&self, i: usize, delta: i32) {
        let j = i as i64 + delta as i64;
        let n = self.host.config.borrow().pages.len() as i64;
        if !(0..n).contains(&j) || i as i64 >= n {
            return;
        }
        let j = j as usize;
        let cur = self.page.get();
        self.page.set(if cur == i { j } else if cur == j { i } else { cur });
        self.edit_pages(|cfg| {
            cfg.pages.swap(i, j);
            true
        });
    }

    pub(super) fn app_to_page(&self, id: &str, to: usize) {
        self.edit_pages(|cfg| {
            if cfg.page_of(id) == Some(to) || to >= cfg.pages.len() {
                return false;
            }
            cfg.move_to_page(id, to);
            true
        });
    }
}
