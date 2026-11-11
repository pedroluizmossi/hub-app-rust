//! Mini-app To-do rápido: lista curta de tarefas, salva em `~/.config/miniutils/todo.json`.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use serde::{Deserialize, Serialize};
use slint::{ComponentHandle, Model, ModelRc, VecModel};

use crate::core::bus::Event;
use crate::core::manifest::Manifest;
use crate::core::miniapp::{Host, MiniApp};
use super::kit::{HeaderMascot, unix_now};
use crate::mascot::anim::Reaction;
use crate::mascot::{Mood, render, sprites};
use crate::ui::{HubWindow, TodoApp, TodoRow};

/// Tamanho máximo de uma tarefa (o resto é cortado).
const MAX_LEN: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    pub id: u64,
    pub text: String,
    pub done: bool,
    /// Quando foi criada (segundos Unix).
    pub created: i64,
}

/// A lista e as operações sobre ela, sem tela (é o que os testes usam).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct List {
    pub items: Vec<Item>,
}

impl List {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    /// Grava num arquivo temporário e troca: um desligamento no meio não corrompe a lista.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self).unwrap_or_default())?;
        std::fs::rename(tmp, path)
    }

    /// Acrescenta uma tarefa; devolve `false` se o texto estava vazio.
    pub fn add(&mut self, text: &str, now: i64) -> bool {
        let text: String = text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(MAX_LEN).collect();
        if text.is_empty() {
            return false;
        }
        let id = self.items.iter().map(|i| i.id).max().unwrap_or(0) + 1;
        self.items.push(Item { id, text, done: false, created: now });
        true
    }

    /// Marca/desmarca; devolve o novo estado (ou `None` se o id não existe).
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

    /// Na ordem da tela: com `done_last`, as feitas descem (a ordem de criação se mantém).
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
    /// Última falha ao gravar (aparece na tela).
    error: String,
    mood: Mood,
    rows: Rc<VecModel<TodoRow>>,
    face: HeaderMascot,
}

impl TodoMini {
    pub fn create(host: Rc<Host>, ui: &HubWindow) -> Rc<RefCell<Self>> {
        let manifest =
            Manifest::parse(include_str!("../../miniapps/todo/manifest.toml")).expect("manifesto do To-do inválido");
        let path = dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("miniutils/todo.json");
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
                    // zerou a lista: comemora
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

    /// A lista mudou: grava, recalcula o humor e redesenha.
    fn changed(&mut self, ui: &HubWindow) {
        self.error = match self.list.save(&self.path) {
            Ok(()) => String::new(),
            Err(e) => format!("Não consegui salvar: {e}"),
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
                (0, 0) => "Nada para fazer… zzz".to_string(),
                (0, _) => "Tudo feito! Bom trabalho.".into(),
                (1, _) => "Falta só 1 tarefa.".into(),
                (n, _) => format!("{n} tarefas pendentes."),
            }
            .into(),
        );
    }

    /// Redesenha a mascote; `true` se mudou.
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

/// Idade de uma tarefa: "agora", "há 5 min", "há 3 h", "há 2 d".
pub fn age_text(secs: i64) -> String {
    match secs.max(0) {
        s if s < 60 => "agora".into(),
        s if s < 3600 => format!("há {} min", s / 60),
        s if s < 86400 => format!("há {} h", s / 3600),
        s => format!("há {} d", s / 86400),
    }
}

impl MiniApp for TodoMini {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn enable(&mut self, ui: &HubWindow) {
        // relê: a lista pode ter sido editada à mão com o app desligado
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
        // a idade das tarefas ("há 5 min") anda sozinha; ~1 vez por minuto basta
        if frame % 400 == 0 {
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
            0 => "To-do: tudo feito".into(),
            1 => "To-do: 1 pendente".into(),
            n => format!("To-do: {n} pendentes"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adiciona_marca_remove_e_limpa() {
        let mut l = List::default();
        assert!(!l.add("   ", 0), "texto vazio não entra");
        assert!(l.add("  comprar   café ", 0));
        assert!(l.add("revisar PR", 0));
        assert_eq!(l.items[0].text, "comprar café");
        assert_eq!((l.items[0].id, l.items[1].id), (1, 2));
        assert_eq!(l.toggle(1), Some(true));
        assert_eq!(l.toggle(99), None);
        assert_eq!((l.pending(), l.done()), (1, 1));
        // feitas descem; escondidas somem
        assert_eq!(l.ordered(true, false).iter().map(|i| i.id).collect::<Vec<_>>(), [2, 1]);
        assert_eq!(l.ordered(false, false).iter().map(|i| i.id).collect::<Vec<_>>(), [1, 2]);
        assert_eq!(l.ordered(true, true).len(), 1);
        assert_eq!(l.clear_done(), 1);
        assert!(l.remove(2));
        assert!(!l.remove(2));
        assert!(l.items.is_empty());
        // id novo não reaproveita um que ainda existe
        l.add("a", 0);
        l.add("b", 0);
        l.remove(1);
        l.add("c", 0);
        assert_eq!(l.items.iter().map(|i| i.id).collect::<Vec<_>>(), [2, 3]);
    }

    #[test]
    fn corta_texto_longo() {
        let mut l = List::default();
        l.add(&"x".repeat(500), 0);
        assert_eq!(l.items[0].text.chars().count(), MAX_LEN);
    }

    #[test]
    fn grava_e_le_de_volta() {
        let dir = std::env::temp_dir().join(format!("miniutils-todo-{}", std::process::id()));
        let path = dir.join("todo.json");
        let mut l = List::default();
        l.add("tarefa", 42);
        l.toggle(1);
        l.save(&path).unwrap();
        assert_eq!(List::load(&path), l);
        assert_eq!(List::load(&dir.join("nao-existe.json")), List::default());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn idade() {
        assert_eq!(age_text(5), "agora");
        assert_eq!(age_text(300), "há 5 min");
        assert_eq!(age_text(7200), "há 2 h");
        assert_eq!(age_text(3 * 86400), "há 3 d");
    }
}
