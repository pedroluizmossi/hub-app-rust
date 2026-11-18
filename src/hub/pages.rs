//! Panel pages: sidebar, open page and editing (create, rename, move).

use slint::{ModelRc, SharedString, VecModel};

use super::Hub;
use crate::core::config::Config;
use crate::mascot::{Mood, render, sprites, themes};
use crate::skin;
use crate::ui::PageInfo;

impl Hub {
    /// Open page, always valid (the config may have lost pages).
    pub(super) fn current_page(&self) -> usize {
        let n = self.host.config.borrow().pages.len().max(1);
        let p = self.page.get().min(n - 1);
        self.page.set(p);
        p
    }

    /// Opens a tab by name: "apps" (settings), "store", "panel" or a page (by name or by an
    /// app on it, see `Config::find_page`).
    pub(super) fn open_tab(&self, tab: &str) {
        if tab == "apps" || tab == "store" {
            self.enter_tab(tab);
            return;
        }
        let found = self.host.config.borrow().find_page(tab);
        if let Some(i) = found
            && self.page.replace(i) != i
        {
            // an edit in progress stays on the page where it started
            self.drag.borrow_mut().take();
            self.set_ghost(None);
        }
        self.enter_tab("panel");
    }

    /// Switches to tab `id` ("panel", "apps" or "store") and refreshes what it shows. Every
    /// way in (tab click, `--tab`, `show` from another instance) goes through here.
    pub(super) fn enter_tab(&self, id: &str) {
        if id == "apps" {
            self.apply_system();
            self.refresh_data();
            self.refresh_usage(true);
        }
        self.ui.set_current(id.into());
        if id == "store" {
            self.store_reload();
        }
        self.rebuild_pages(true);
        self.anim_tick(); // draw the mascots right on the first frame
        self.schedule_backdrop();
    }

    /// Mood of the page: the worst of its enabled apps.
    pub(super) fn page_mood(&self, page: usize) -> Mood {
        let ids = self.host.config.borrow().page_apps(page);
        ids.iter()
            .filter_map(|id| self.registry.find(id))
            .map(|a| a.borrow().mood())
            .fold(Mood::Sleep, Mood::worst)
    }

    /// Sidebar and page names. Without `force`, only redone if something visible changed
    /// (name, mascot, mood): it is called on every provider reading.
    pub(super) fn rebuild_pages(&self, force: bool) {
        let current = self.current_page();
        let (pages, skin_key) = {
            let cfg = self.host.config.borrow();
            let pages: Vec<(String, String, usize)> =
                (0..cfg.pages.len()).map(|i| (cfg.pages[i].name.clone(), cfg.pages[i].icon.clone(), cfg.page_apps(i).len())).collect();
            (pages, cfg.skin.clone())
        };
        let moods: Vec<Mood> = (0..pages.len()).map(|i| self.page_mood(i)).collect();
        // the generation of the imported ones: the mascots folder changed, so did the picker list
        let fingerprint = format!("{pages:?}|{moods:?}|{skin_key}|{current}|{}", sprites::generation());
        if !force && *self.pages_fingerprint.borrow() == fingerprint {
            return;
        }
        *self.pages_fingerprint.borrow_mut() = fingerprint;
        let theme = themes::get(skin::get(&skin_key).app_theme);
        let all = sprites::all();
        let infos: Vec<PageInfo> = pages
            .iter()
            .zip(&moods)
            .map(|((name, icon, n), mood)| PageInfo {
                name: name.as_str().into(),
                icon: render::image(sprites::get(icon), theme, *mood, 1),
                icon_index: all.iter().position(|s| s.key == icon).unwrap_or(0) as i32,
                mood: mood.as_str().into(),
                theme: render::colors(theme),
                apps: *n as i32,
            })
            .collect();
        let names: Vec<SharedString> = pages.iter().map(|(n, _, _)| SharedString::from(n.as_str())).collect();
        let mascots: Vec<SharedString> = all.iter().map(|s| s.name().into()).collect();
        self.ui.set_pages(ModelRc::new(VecModel::from(infos)));
        self.ui.set_page_names(ModelRc::new(VecModel::from(names)));
        self.ui.set_mascots(ModelRc::new(VecModel::from(mascots)));
        self.ui.set_mascot_images(ModelRc::new(VecModel::from(super::settings::mascot_images(theme))));
        self.ui.set_page(current as i32);
    }

    /// Opens page `i` on the panel.
    pub(super) fn select_page(&self, i: usize) {
        if i >= self.host.config.borrow().pages.len() {
            return;
        }
        let changed = self.page.replace(i) != i;
        // an edit in progress stays on the page where it started
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

    /// Changes the pages config, saves it and redraws grid, sidebar and settings.
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

    /// Deletes page `i`; the open one stays the same (or the previous one, if it was the one removed).
    pub(super) fn remove_page(&self, i: usize) {
        if self.host.config.borrow().pages.len() <= 1 {
            return;
        }
        self.page.set(page_after_remove(self.page.get(), i));
        self.edit_pages(|cfg| cfg.remove_page(i));
    }

    /// Renames page `i`. An empty name is refused, and the field shows the saved name again
    /// (the user's typing broke its binding to the model).
    pub(super) fn rename_page(&self, i: usize, name: &str) {
        let mut changed = false;
        self.edit_pages(|cfg| {
            changed = rename(cfg, i, name);
            changed
        });
        if !changed {
            self.rebuild_pages(true);
        }
    }

    pub(super) fn page_icon(&self, i: usize, k: usize) {
        let Some(s) = sprites::all().get(k).copied() else { return };
        self.edit_pages(|cfg| match cfg.pages.get_mut(i) {
            Some(p) if p.icon != s.key => {
                p.icon = s.key.into();
                true
            }
            _ => false,
        });
    }

    /// Moves the page up (`-1`) or down (`+1`) in the sidebar; the open one stays open.
    pub(super) fn move_page(&self, i: usize, delta: i32) {
        let j = i as i64 + delta as i64;
        let n = self.host.config.borrow().pages.len() as i64;
        if !(0..n).contains(&j) || i as i64 >= n {
            return;
        }
        let j = j as usize;
        self.page.set(page_after_swap(self.page.get(), i, j));
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

/// Sets page `i`'s name (trimmed); false (nothing changed) for a blank name, the same name
/// or no such page.
fn rename(cfg: &mut Config, i: usize, name: &str) -> bool {
    let name = name.trim();
    match cfg.pages.get_mut(i) {
        Some(p) if !name.is_empty() && p.name != name => {
            p.name = name.to_string();
            true
        }
        _ => false,
    }
}

/// The open page after page `removed` is deleted: the same page (one index lower if it came
/// after the removed one), or the previous one if it was the one removed.
fn page_after_remove(cur: usize, removed: usize) -> usize {
    if cur >= removed && cur > 0 { cur - 1 } else { cur }
}

/// The open page after pages `i` and `j` swap places: it follows its page.
fn page_after_swap(cur: usize, i: usize, j: usize) -> usize {
    if cur == i {
        j
    } else if cur == j {
        i
    } else {
        cur
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_page_follows_a_removal() {
        // removed after the open one: unchanged
        assert_eq!(page_after_remove(1, 3), 1);
        // removed before: one index lower, same page
        assert_eq!(page_after_remove(3, 1), 2);
        // the open one removed: the previous one
        assert_eq!(page_after_remove(2, 2), 1);
        // page 0 open and removed: stays on 0 (the next page moved there)
        assert_eq!(page_after_remove(0, 0), 0);
        assert_eq!(page_after_remove(0, 2), 0);
    }

    #[test]
    fn open_page_follows_a_swap() {
        assert_eq!(page_after_swap(1, 1, 2), 2);
        assert_eq!(page_after_swap(2, 1, 2), 1);
        assert_eq!(page_after_swap(0, 1, 2), 0);
        assert_eq!(page_after_swap(3, 2, 1), 3);
    }

    #[test]
    fn blank_rename_is_ignored() {
        let mut cfg = Config::default();
        let before = cfg.pages[0].name.clone();
        assert!(!rename(&mut cfg, 0, ""));
        assert!(!rename(&mut cfg, 0, "  \t "));
        assert!(!rename(&mut cfg, 0, before.as_str()));
        assert!(!rename(&mut cfg, 99, "Other"));
        assert_eq!(cfg.pages[0].name, before);
        assert!(rename(&mut cfg, 0, "  Work "));
        assert_eq!(cfg.pages[0].name, "Work");
    }
}
