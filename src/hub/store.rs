//! The store screen (`ui/store.slint`): loads the index, shows the state of each plugin and
//! mascot pack and installs, updates or removes with two clicks. The work (downloading the
//! index, `git clone`, copying) runs in a thread; the result comes back through the UI
//! thread. A mascot pack's row id is `mascot:<id>` (plugin and mascot ids may repeat).

use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, ModelRc, VecModel};

use super::{Hub, with_hub};
use crate::core::{appdata, plugins};
use crate::store::{self, Catalog, MascotStatus, Source, Status, Trust};
use crate::tr;
use crate::ui::{StoreApp, StoreRow};

/// Time for the second click to confirm an action.
const ARM_TTL: Duration = Duration::from_secs(8);
/// Prefix of a mascot pack's row id.
const MASCOT: &str = "mascot:";

/// What an index load brings back: the catalog and the bytes of stored data of each
/// installed plugin that has some (measured in the store thread, not on the UI thread).
type Loaded = Result<(Catalog, HashMap<String, u64>), String>;

#[derive(Default)]
pub(super) struct StoreState {
    statuses: Vec<Status>,
    mascots: Vec<MascotStatus>,
    /// How the last index that loaded was checked (`None`: nothing loaded, or it failed).
    trust: Option<Trust>,
    /// Error of the last load or action ("" = none).
    error: String,
    /// Result of the last action ("x installed"): kept across the reload that the action
    /// starts and shown before the catalog summary.
    notice: String,
    loading: bool,
    /// Generation of the latest index load: the result of an older one is dropped.
    load_gen: u64,
    /// Stored data of the installed plugins, by id (bytes; only the ones with data).
    data: HashMap<String, u64>,
    /// Action waiting for the second click: (id, verb, when it was armed).
    armed: Option<(String, String, Instant)>,
    /// Plugin with an action in progress.
    busy: Option<String>,
}

impl StoreState {
    /// A new index load starts: returns its generation.
    fn begin_load(&mut self) -> u64 {
        self.armed = None;
        self.loading = true;
        self.notice.clear();
        self.error.clear();
        self.load_gen += 1;
        self.load_gen
    }

    /// Takes the result of load `generation`; `false` (and nothing changes) if a newer load
    /// was started meanwhile.
    fn apply_load(&mut self, generation: u64, result: Loaded) -> bool {
        if generation != self.load_gen {
            return false;
        }
        self.loading = false;
        match result {
            Ok((c, data)) => {
                self.trust = Some(c.trust);
                self.statuses = c.plugins;
                self.mascots = c.mascots;
                self.data = data;
                self.error.clear();
            }
            Err(e) => {
                self.trust = None;
                self.statuses.clear();
                self.mascots.clear();
                self.data.clear();
                self.notice.clear();
                self.error = e;
            }
        }
        true
    }

    /// The status line: the error; or the last action's result and the catalog summary
    /// ("x installed · signature verified · plugins: 3 · mascots: 1"). Built on every push,
    /// so it follows the interface language. While loading, only the notice (the screen
    /// adds "loading…").
    fn status_line(&self) -> String {
        if !self.error.is_empty() {
            return self.error.clone();
        }
        let summary = match (&self.trust, self.loading) {
            (Some(t), false) => tr!(
                "{} · plugins: {} · mascots: {}",
                if *t == Trust::Signed { tr!("signature verified") } else { tr!("local index, not signed") },
                self.statuses.len(),
                self.mascots.len()
            ),
            _ => String::new(),
        };
        match (self.notice.is_empty(), summary.is_empty()) {
            (true, _) => summary,
            (false, true) => self.notice.clone(),
            (false, false) => format!("{} · {summary}", self.notice),
        }
    }
}

/// Bytes of stored data of each installed plugin of the catalog that has some.
fn data_sizes(c: &Catalog) -> HashMap<String, u64> {
    c.plugins
        .iter()
        .filter(|s| s.installed.is_some())
        .filter_map(|s| {
            let dir = appdata::dir(&s.entry.id);
            appdata::has_data(&dir).then(|| (s.entry.id.clone(), appdata::size(&dir).0))
        })
        .collect()
}

impl Hub {
    pub(super) fn bind_store(hub: &Rc<Self>) {
        let g = hub.ui.global::<StoreApp>();
        g.on_reload(|| with_hub(|h| h.store_reload()));
        g.on_save_source(|index, key| {
            with_hub(|h| {
                {
                    let mut cfg = h.host.config.borrow_mut();
                    cfg.store_index = index.trim().to_string();
                    cfg.store_key = key.trim().to_string();
                    cfg.save();
                }
                h.store_reload();
            })
        });
        g.on_act(|id, verb| with_hub(|h| h.store_act(&id, &verb)));
    }

    /// The store was opened or a reload was asked for: downloads the index in a thread.
    /// Loads overlap (the store reopened, "Save and load" clicked twice): only the result of
    /// the latest one counts.
    pub(super) fn store_reload(&self) {
        let src = Source::from_config(&self.host.config.borrow());
        let g = self.ui.global::<StoreApp>();
        g.set_index(src.index.as_str().into());
        g.set_key(src.key.as_str().into());
        let generation = self.store.borrow_mut().begin_load();
        self.store_push();
        let spawned = std::thread::Builder::new().name("store".into()).spawn(move || {
            let result = store::statuses(&src, &plugins::dir(), &crate::skin::user::dir()).map(|c| {
                let data = data_sizes(&c);
                (c, data)
            });
            let _ = slint::invoke_from_event_loop(move || with_hub(|h| h.store_loaded(generation, result)));
        });
        if let Err(e) = spawned {
            self.store_failed(tr!("no thread for the store: {}", e));
        }
    }

    fn store_loaded(&self, generation: u64, result: Loaded) {
        if !self.store.borrow_mut().apply_load(generation, result) {
            log::debug!("store: dropped the result of an older index load");
            return;
        }
        self.store_push();
    }

    fn store_failed(&self, msg: String) {
        {
            let mut st = self.store.borrow_mut();
            st.loading = false;
            st.busy = None;
            st.notice.clear();
            st.error = msg;
        }
        self.store_push();
    }

    /// The first click arms (and shows what will happen); the second one, within `ARM_TTL`,
    /// does it.
    fn store_act(&self, id: &str, verb: &str) {
        let confirmed = {
            let mut st = self.store.borrow_mut();
            if st.busy.is_some() {
                return;
            }
            // "remove-purge" is the other button of the second click of "remove"
            let same = st.armed.as_ref().is_some_and(|(i, v, at)| {
                i == id && (v == verb || (v == "remove" && verb == "remove-purge")) && at.elapsed() < ARM_TTL
            });
            if same {
                st.armed = None;
                st.busy = Some(id.to_string());
            } else {
                st.armed = Some((id.to_string(), verb.to_string(), Instant::now()));
            }
            same
        };
        self.store_push();
        if !confirmed {
            // the "Confirm …" button goes back to its first label when the time runs out
            // (`store_push` drops an expired arm; a newer arm is still within its time)
            slint::Timer::single_shot(ARM_TTL + Duration::from_millis(50), || with_hub(|h| h.store_push()));
            return;
        }
        // a mascot pack is only drawings: nothing running to stop, no data
        if id.starts_with(MASCOT) {
            self.store_spawn(id, verb);
            return;
        }
        // the plugin logic stops before the folder is swapped (`store_done` restarts it); to
        // delete the data, it also waits for it to release the files
        crate::apps::plugin::suspend(id);
        if verb == "remove-purge" {
            wait_release(id.to_string(), 0);
        } else {
            self.store_spawn(id, verb);
        }
    }

    /// Installs, updates or removes in a thread; the result comes back in `store_done`.
    fn store_spawn(&self, id: &str, verb: &str) {
        let src = Source::from_config(&self.host.config.borrow());
        let (id, verb) = (id.to_string(), verb.to_string());
        let spawned = std::thread::Builder::new().name("store".into()).spawn(move || {
            let root = plugins::dir();
            // the confirmation was the second click, with the summary on screen
            let result = match (id.strip_prefix(MASCOT), verb.as_str()) {
                (Some(m), "remove") => store::remove_mascot(m, &crate::skin::user::dir()),
                (Some(m), _) => store::install_mascot(&src, m, &crate::skin::user::dir(), &mut |_| true),
                (None, "remove") => store::remove(&id, &root, &appdata::root(), false),
                (None, "remove-purge") => store::remove(&id, &root, &appdata::root(), true),
                (None, _) => store::install(&src, &id, &root, &mut |_| true).map(|_| tr!("{} installed", id)),
            };
            let _ = slint::invoke_from_event_loop(move || with_hub(|h| h.store_done(&id, &verb, result)));
        });
        if let Err(e) = spawned {
            self.store_failed(tr!("no thread for the store: {}", e));
        }
    }

    /// The action finished. On success the index loads again (the states changed) and the
    /// result stays on the status line, before the catalog summary, after that load.
    fn store_done(&self, id: &str, verb: &str, result: Result<String, String>) {
        self.store.borrow_mut().busy = None;
        let notice = match result {
            Ok(msg) if id.starts_with(MASCOT) => {
                // the new mascots show up in every picker at once
                self.reload_looks();
                Ok(msg)
            }
            Ok(msg) => {
                // the plugin comes into (or leaves) the hub without restarting
                self.reload_plugins();
                // just installed: enable it now, on the open page
                if verb == "install" && !self.host.config.borrow().is_enabled(id) {
                    self.ui.invoke_toggle_app(id.into());
                }
                Ok(if verb == "install" { tr!("{} and enabled on the panel", msg) } else { msg })
            }
            Err(e) => Err(e),
        };
        match notice {
            Ok(msg) => {
                self.store_reload();
                self.store.borrow_mut().notice = msg;
            }
            Err(e) => {
                let mut st = self.store.borrow_mut();
                st.notice.clear();
                st.error = e;
            }
        }
        // an external app stopped by `suspend` and not swapped: runs again (a mascot pack
        // has no app)
        if !id.starts_with(MASCOT) {
            crate::apps::plugin::resume(id);
        }
        self.store_push();
    }

    /// Takes the state to the screen.
    pub(super) fn store_push(&self) {
        let st = self.store.borrow();
        let armed = st.armed.as_ref().filter(|(_, _, at)| at.elapsed() < ARM_TTL);
        let mut rows: Vec<StoreRow> = st
            .statuses
            .iter()
            .map(|s| {
                let e = &s.entry;
                let verb = armed.filter(|(i, _, _)| *i == e.id).map(|(_, v, _)| v.as_str()).unwrap_or("");
                let perms = e.grants_text();
                // stored data (only matters for installed ones: removing asks about it)
                let data = match st.data.get(&e.id) {
                    Some(bytes) if s.installed.is_some() => appdata::human(*bytes),
                    _ => String::new(),
                };
                let new_perms = s.new_permissions();
                let state = match &s.installed {
                    None => tr!("not installed").to_string(),
                    Some(i) if s.has_update() => tr!("update: {} → {}", i.version, e.version),
                    Some(i) => tr!("installed {}", i.version),
                };
                let confirm = match verb {
                    "install" if e.full_access => {
                        tr!("Click again to install with FULL ACCESS: the app runs with your user's permissions.").to_string()
                    }
                    "install" => tr!("Click again to install. The plugin asks for: {}.", perms),
                    "update" if !new_perms.is_empty() => tr!("Click again to update and also allow: {}.", new_perms.join(", ")),
                    "update" => tr!("Click again to update (no new permission).").into(),
                    "remove" if !data.is_empty() => tr!(
                        "{} has {} of stored data. Keep: it stays for when you reinstall (and shows in Settings › System). Delete: it is all gone.",
                        e.name, data
                    ),
                    "remove" => tr!("Click again to remove {}.", e.name),
                    _ => String::new(),
                };
                StoreRow {
                    id: e.id.as_str().into(),
                    target: e.id.as_str().into(),
                    kind: "plugin".into(),
                    name: e.name.as_str().into(),
                    version: e.version.as_str().into(),
                    byline: format!("{} · {}", if e.author.is_empty() { tr!("no author") } else { &e.author }, e.license).into(),
                    description: e.description.as_str().into(),
                    permissions: perms.into(),
                    state: state.into(),
                    installed: s.installed.is_some(),
                    update: s.has_update(),
                    warning: if s.has_update() && !new_perms.is_empty() {
                        tr!("WARNING: the new version asks for new permissions: {}", new_perms.join(", ")).into()
                    } else if e.full_access && s.installed.is_none() {
                        tr!("WARNING: app with full access. It reads and writes your files, uses the network and runs other programs. Install only if you trust the author.")
                            .into()
                    } else {
                        "".into()
                    },
                    armed: verb.into(),
                    confirm: confirm.into(),
                    busy: st.busy.as_deref() == Some(e.id.as_str()),
                    data: data.into(),
                }
            })
            .collect();
        // mascot packs: only drawings, so no permissions, data or warnings
        rows.extend(st.mascots.iter().map(|s| {
            let e = &s.entry;
            let target = format!("{MASCOT}{}", e.id);
            let verb = armed.filter(|(i, _, _)| *i == target).map(|(_, v, _)| v.as_str()).unwrap_or("");
            let state = match &s.installed {
                None => tr!("not installed").to_string(),
                Some(i) if s.has_update() => tr!("update: {} → {}", i.version, e.version),
                Some(i) => tr!("installed {}", i.version),
            };
            let contents = match &s.installed {
                Some(i) => format!("{} · {}", tr!("only drawings: no code and no permissions"), i.contents()),
                None => tr!("only drawings: no code and no permissions").to_string(),
            };
            let confirm = match verb {
                "install" => tr!("Click again to install. The mascots show up in every mascot picker.").to_string(),
                "update" => tr!("Click again to update (no new permission).").into(),
                "remove" => tr!("Click again to remove {}. An app or page using one of its mascots goes back to the first one.", e.name),
                _ => String::new(),
            };
            StoreRow {
                id: e.id.as_str().into(),
                target: target.as_str().into(),
                kind: "mascot".into(),
                name: e.name.as_str().into(),
                version: e.version.as_str().into(),
                byline: format!("{} · {}", if e.author.is_empty() { tr!("no author") } else { &e.author }, e.license).into(),
                description: e.description.as_str().into(),
                permissions: contents.into(),
                state: state.into(),
                installed: s.installed.is_some(),
                update: s.has_update(),
                warning: "".into(),
                armed: verb.into(),
                confirm: confirm.into(),
                busy: st.busy.as_deref() == Some(target.as_str()),
                data: "".into(),
            }
        }));
        let g = self.ui.global::<StoreApp>();
        g.set_rows(ModelRc::new(VecModel::from(rows)));
        g.set_status(st.status_line().into());
        g.set_failed(!st.error.is_empty());
        g.set_loading(st.loading);
    }
}

/// Waits for the plugin logic to release the files (up to 5 s) and then removes it, deleting the data.
fn wait_release(id: String, tries: u32) {
    const POLL: Duration = Duration::from_millis(100);
    slint::Timer::single_shot(POLL, move || {
        with_hub(|h| {
            if crate::apps::plugin::released(&id) || tries >= 50 {
                h.store_spawn(&id, "remove-purge");
            } else {
                wait_release(id.clone(), tries + 1);
            }
        })
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> Catalog {
        Catalog { plugins: Vec::new(), mascots: Vec::new(), trust: Trust::Unsigned }
    }

    #[test]
    fn the_result_of_an_action_survives_the_reload_it_starts() {
        let mut st = StoreState::default();
        let g = st.begin_load();
        // `store_done`: reload, then the notice
        st.notice = "x installed".into();
        assert_eq!(st.status_line(), "x installed", "while loading, only the notice");
        assert!(st.apply_load(g, Ok((catalog(), HashMap::new()))));
        let line = st.status_line();
        assert!(line.starts_with("x installed · ") && line.ends_with("plugins: 0 · mascots: 0"), "{line}");
        // the next load (the user asked for it) forgets it
        let g = st.begin_load();
        assert!(st.apply_load(g, Ok((catalog(), HashMap::new()))));
        assert!(!st.status_line().contains("x installed"));
    }

    #[test]
    fn an_older_load_does_not_overwrite_a_newer_one() {
        let mut st = StoreState::default();
        let old = st.begin_load();
        let new = st.begin_load();
        assert!(st.apply_load(new, Ok((catalog(), HashMap::new()))));
        assert!(!st.apply_load(old, Err("timeout".into())), "stale result taken");
        assert!(st.error.is_empty() && !st.loading);
        // a failure shows the error and clears the lists
        let g = st.begin_load();
        assert!(st.apply_load(g, Err("bad signature".into())));
        assert_eq!(st.status_line(), "bad signature");
        assert!(st.trust.is_none());
    }
}
