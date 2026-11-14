//! Quick To-do mini-app: a short task list, saved in `todo.json` in the app's data folder
//! (`core/appdata.rs`: `~/.local/share/miniutils/app-data/cyber.todo/`).

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use serde::{Deserialize, Serialize};
use slint::{ComponentHandle, Model, ModelRc, VecModel};

use crate::core::appdata;
use crate::core::bus::Event;
use crate::core::manifest::Manifest;
use crate::core::miniapp::{Host, MiniApp};
use super::kit::{HeaderMascot, unix_now};
use crate::mascot::anim::Reaction;
use crate::mascot::{Mood, render, sprites};
use crate::tr;
use crate::ui::{HubWindow, TodoApp, TodoRow};

/// Maximum length of a task (the rest is cut).
const MAX_LEN: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    pub id: u64,
    pub text: String,
    pub done: bool,
    /// When it was created (Unix seconds).
    pub created: i64,
}

/// The list and the operations on it, without a screen (what the tests use).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct List {
    pub items: Vec<Item>,
}

impl List {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    /// Writes to a temporary file and swaps it in: a shutdown halfway doesn't corrupt the list.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self).unwrap_or_default())?;
        std::fs::rename(tmp, path)
    }

    /// Adds a task; returns `false` if the text was empty.
    pub fn add(&mut self, text: &str, now: i64) -> bool {
        let text: String = text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(MAX_LEN).collect();
        if text.is_empty() {
            return false;
        }
        let id = self.items.iter().map(|i| i.id).max().unwrap_or(0) + 1;
        self.items.push(Item { id, text, done: false, created: now });
        true
    }

    /// Checks/unchecks; returns the new state (or `None` if the id doesn't exist).
    pub fn toggle(&mut self, id: u64) -> Option<bool> {
        let it = self.items.iter_mut().find(|i| i.id == id)?;
        it.done = !it.done;
        Some(it.done)
    }

    pub fn remove(&mut self, id: u64) -> bool {
        let n = self.items.len();
        self.items.retain(|i| i.id != id);
        self.items.len() != n
    }

    pub fn clear_done(&mut self) -> usize {
        let n = self.items.len();
        self.items.retain(|i| !i.done);
        n - self.items.len()
    }

    pub fn pending(&self) -> usize {
        self.items.iter().filter(|i| !i.done).count()
    }

    pub fn done(&self) -> usize {
        self.items.len() - self.pending()
    }

    /// In screen order: with `done_last`, done ones go down (creation order is kept).
    pub fn ordered(&self, done_last: bool, hide_done: bool) -> Vec<&Item> {
        let mut v: Vec<&Item> = self.items.iter().filter(|i| !hide_done || !i.done).collect();
        if done_last {
            v.sort_by_key(|i| i.done);
        }
        v
    }
}

pub struct TodoMini {
    host: Rc<Host>,
    manifest: Manifest,
    path: PathBuf,
    list: List,
    /// Last failure when saving (shown on screen).
    error: String,
    mood: Mood,
    rows: Rc<VecModel<TodoRow>>,
    face: HeaderMascot,
}

impl TodoMini {
    pub fn create(host: Rc<Host>, ui: &HubWindow) -> Rc<RefCell<Self>> {
        let manifest =
            Manifest::parse(include_str!("../../miniapps/todo/manifest.toml")).expect("invalid To-do manifest");
        // the app's data folder (`core/appdata.rs`; the old one, in the config, comes over in `migrate_legacy`)
        let path = appdata::dir(&manifest.id).join("todo.json");
        let rows = Rc::new(VecModel::default());
        ui.global::<TodoApp>().set_rows(ModelRc::from(rows.clone()));
        let app = Rc::new(RefCell::new(TodoMini {
            host,
            manifest,
            list: List::load(&path),
            path,
            error: String::new(),
            mood: Mood::Sleep,
            rows,
            face: HeaderMascot::new(13),
        }));
        Self::bind(&app, ui);
        app
    }

    fn bind(app: &Rc<RefCell<Self>>, ui: &HubWindow) {
        let g = ui.global::<TodoApp>();
        let weak_ui = ui.as_weak();

        let a = Rc::downgrade(app);
        let w = weak_ui.clone();
        g.on_mascot_poked(move || {
            if let (Some(a), Some(ui)) = (a.upgrade(), w.upgrade()) {
                let mut a = a.borrow_mut();
                let (mood, reduce) = (a.mood, a.host.config.borrow().reduce_motion);
                if a.face.poke(mood, reduce) {
                    a.push_mascot(&ui);
                }
            }
        });

        let a = Rc::downgrade(app);
        let w = weak_ui.clone();
        g.on_add(move |text| {
            if let (Some(a), Some(ui)) = (a.upgrade(), w.upgrade()) {
                let mut a = a.borrow_mut();
                if a.list.add(&text, unix_now()) {
                    ui.global::<TodoApp>().set_draft("".into());
                    a.changed(&ui);
                }
            }
        });

        let a = Rc::downgrade(app);
        let w = weak_ui.clone();
        g.on_toggle(move |id| {
            if let (Some(a), Some(ui)) = (a.upgrade(), w.upgrade()) {
                let mut a = a.borrow_mut();
                if a.list.toggle(id as u64) == Some(true) && a.list.pending() == 0 {
                    // the list is cleared: celebrate
                    a.face.react(Reaction::Celebrate);
                }
                a.changed(&ui);
            }
        });

        let a = Rc::downgrade(app);
        let w = weak_ui.clone();
        g.on_remove(move |id| {
            if let (Some(a), Some(ui)) = (a.upgrade(), w.upgrade()) {
                let mut a = a.borrow_mut();
                if a.list.remove(id as u64) {
                    a.changed(&ui);
                }
            }
        });

        let a = Rc::downgrade(app);
        let w = weak_ui.clone();
        g.on_clear_done(move || {
            if let (Some(a), Some(ui)) = (a.upgrade(), w.upgrade()) {
                let mut a = a.borrow_mut();
                if a.list.clear_done() > 0 {
                    a.changed(&ui);
                }
            }
        });
    }

    /// The list changed: save, recompute the mood and redraw.
    fn changed(&mut self, ui: &HubWindow) {
        self.error = match self.list.save(&self.path) {
            Ok(()) => String::new(),
            Err(e) => tr!("Couldn't save: {}", e),
        };
        self.mood = self.compute_mood();
        self.refresh(ui);
        self.push_mascot(ui);
    }

    fn compute_mood(&self) -> Mood {
        if self.list.items.is_empty() {
            Mood::Sleep
        } else if self.list.pending() == 0 {
            Mood::Happy
        } else {
            Mood::Idle
        }
    }

    fn refresh(&self, ui: &HubWindow) {
        let (done_last, hide_done) = {
            let cfg = self.host.config.borrow();
            (cfg.bool(&self.manifest, "done_last"), cfg.bool(&self.manifest, "hide_done"))
        };
        let now = unix_now();
        let rows: Vec<TodoRow> = self
            .list
            .ordered(done_last, hide_done)
            .into_iter()
            .map(|i| TodoRow {
                id: i.id.min(i32::MAX as u64) as i32,
                text: i.text.clone().into(),
                done: i.done,
                age_text: age_text(now - i.created).into(),
            })
            .collect();
        let same = self.rows.row_count() == rows.len()
            && rows.iter().enumerate().all(|(i, r)| self.rows.row_data(i).as_ref() == Some(r));
        if !same {
            self.rows.set_vec(rows);
        }

        let g = ui.global::<TodoApp>();
        let (pending, done) = (self.list.pending(), self.list.done());
        g.set_pending(pending as i32);
        g.set_done(done as i32);
        g.set_mood(self.mood.as_str().into());
        g.set_error_text(self.error.as_str().into());
        g.set_speech(
            match (pending, done) {
                (0, 0) => tr!("Nothing to do… zzz").to_string(),
                (0, _) => tr!("All done! Good job.").into(),
                (1, _) => tr!("Just 1 task left.").into(),
                (n, _) => tr!("{} tasks pending.", n),
            }
            .into(),
        );
    }

    /// Redraws the mascot; `true` if it changed.
    fn refresh_mascot(&mut self) -> bool {
        let sprite = sprites::get(&self.mascot_key());
        self.face.refresh(sprite, self.host.theme_for(&self.manifest), self.mood)
    }

    fn push_mascot(&mut self, ui: &HubWindow) {
        self.refresh_mascot();
        let g = ui.global::<TodoApp>();
        g.set_mascot(self.face.image());
        g.set_reaction(self.face.reaction());
    }
}

/// Age of a task: "just now", "5 min ago", "3 h ago", "2 d ago".
pub fn age_text(secs: i64) -> String {
    match secs.max(0) {
        s if s < 60 => tr!("just now").into(),
        s if s < 3600 => tr!("{} min ago", s / 60),
        s if s < 86400 => tr!("{} h ago", s / 3600),
        s => tr!("{} d ago", s / 86400),
    }
}

impl MiniApp for TodoMini {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn reload_data(&mut self, ui: &HubWindow) {
        self.list = List::load(&self.path);
        self.mood = self.compute_mood();
        self.refresh(ui);
        self.push_mascot(ui);
    }

    fn enable(&mut self, ui: &HubWindow) {
        // read again: the list may have been edited by hand while the app was off
        self.list = List::load(&self.path);
        ui.global::<TodoApp>().set_theme(render::colors(self.host.theme_for(&self.manifest)));
        self.mood = self.compute_mood();
        self.refresh(ui);
        self.push_mascot(ui);
    }

    fn disable(&mut self, _ui: &HubWindow) {
        self.rows.set_vec(Vec::new());
    }

    fn on_event(&mut self, _ev: &Event, _ui: &HubWindow) {}

    fn tick(&mut self, frame: u32, ui: &HubWindow) {
        // the age of the tasks ("5 min ago") moves on its own; about once a minute is enough
        if frame.is_multiple_of(400) {
            self.refresh(ui);
        }
        if self.refresh_mascot() {
            self.push_mascot(ui);
        }
    }

    fn settings_changed(&mut self, ui: &HubWindow) {
        ui.global::<TodoApp>().set_theme(render::colors(self.host.theme_for(&self.manifest)));
        self.push_mascot(ui);
        self.refresh(ui);
    }

    fn mood(&self) -> Mood {
        self.mood
    }

    fn mascot_key(&self) -> String {
        self.host.config.borrow().string(&self.manifest, "mascot")
    }

    fn summary(&self) -> String {
        match self.list.pending() {
            0 => tr!("To-do: all done").into(),
            1 => tr!("To-do: 1 pending").into(),
            n => tr!("To-do: {} pending", n),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_checks_removes_and_clears() {
        let mut l = List::default();
        assert!(!l.add("   ", 0), "empty text isn't added");
        assert!(l.add("  buy   coffee ", 0));
        assert!(l.add("review PR", 0));
        assert_eq!(l.items[0].text, "buy coffee");
        assert_eq!((l.items[0].id, l.items[1].id), (1, 2));
        assert_eq!(l.toggle(1), Some(true));
        assert_eq!(l.toggle(99), None);
        assert_eq!((l.pending(), l.done()), (1, 1));
        // done ones go down; hidden ones disappear
        assert_eq!(l.ordered(true, false).iter().map(|i| i.id).collect::<Vec<_>>(), [2, 1]);
        assert_eq!(l.ordered(false, false).iter().map(|i| i.id).collect::<Vec<_>>(), [1, 2]);
        assert_eq!(l.ordered(true, true).len(), 1);
        assert_eq!(l.clear_done(), 1);
        assert!(l.remove(2));
        assert!(!l.remove(2));
        assert!(l.items.is_empty());
        // a new id doesn't reuse one that still exists
        l.add("a", 0);
        l.add("b", 0);
        l.remove(1);
        l.add("c", 0);
        assert_eq!(l.items.iter().map(|i| i.id).collect::<Vec<_>>(), [2, 3]);
    }

    #[test]
    fn cuts_long_text() {
        let mut l = List::default();
        l.add(&"x".repeat(500), 0);
        assert_eq!(l.items[0].text.chars().count(), MAX_LEN);
    }

    #[test]
    fn saves_and_reads_back() {
        let dir = std::env::temp_dir().join(format!("miniutils-todo-{}", std::process::id()));
        let path = dir.join("todo.json");
        let mut l = List::default();
        l.add("task", 42);
        l.toggle(1);
        l.save(&path).unwrap();
        assert_eq!(List::load(&path), l);
        assert_eq!(List::load(&dir.join("missing.json")), List::default());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn age() {
        assert_eq!(age_text(5), "just now");
        assert_eq!(age_text(300), "5 min ago");
        assert_eq!(age_text(7200), "2 h ago");
        assert_eq!(age_text(3 * 86400), "3 d ago");
    }
}
