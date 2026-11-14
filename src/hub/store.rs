//! The plugin store screen (`ui/store.slint`): loads the index, shows the state of each
//! plugin and installs, updates or removes with two clicks. The work (downloading the index,
//! `git clone`, copying) runs in a thread; the result comes back through the UI thread.

use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, ModelRc, VecModel};

use super::{Hub, with_hub};
use crate::core::{appdata, plugins};
use crate::store::{self, Source, Status, Trust};
use crate::tr;
use crate::ui::{StoreApp, StoreRow};

/// Time for the second click to confirm an action.
const ARM_TTL: Duration = Duration::from_secs(8);

#[derive(Default)]
pub(super) struct StoreState {
    statuses: Vec<Status>,
    status: String,
    failed: bool,
    loading: bool,
    /// Action waiting for the second click: (id, verb, when it was armed).
    armed: Option<(String, String, Instant)>,
    /// Plugin with an action in progress.
    busy: Option<String>,
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
    pub(super) fn store_reload(&self) {
        let src = Source::from_config(&self.host.config.borrow());
        let g = self.ui.global::<StoreApp>();
        g.set_index(src.index.as_str().into());
        g.set_key(src.key.as_str().into());
        {
            let mut st = self.store.borrow_mut();
            st.armed = None;
            st.loading = true;
        }
        self.store_push();
        let spawned = std::thread::Builder::new().name("store".into()).spawn(move || {
            let result = store::statuses(&src, &plugins::dir());
            let _ = slint::invoke_from_event_loop(move || with_hub(|h| h.store_loaded(result)));
        });
        if let Err(e) = spawned {
            self.store_failed(tr!("no thread for the store: {}", e));
        }
    }

    fn store_loaded(&self, result: Result<(Vec<Status>, Trust), String>) {
        {
            let mut st = self.store.borrow_mut();
            st.loading = false;
            match result {
                Ok((list, trust)) => {
                    st.status = format!(
                        "{} · plugins: {}",
                        if trust == Trust::Signed { tr!("signature verified") } else { tr!("local index, not signed") },
                        list.len()
                    );
                    st.statuses = list;
                    st.failed = false;
                }
                Err(e) => {
                    st.statuses.clear();
                    st.status = e;
                    st.failed = true;
                }
            }
        }
        self.store_push();
    }

    fn store_failed(&self, msg: String) {
        {
            let mut st = self.store.borrow_mut();
            st.loading = false;
            st.busy = None;
            st.status = msg;
            st.failed = true;
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
            let result = match verb.as_str() {
                "remove" => store::remove(&id, &root, &appdata::root(), false),
                "remove-purge" => store::remove(&id, &root, &appdata::root(), true),
                _ => store::install(&src, &id, &root, &mut |_| true).map(|_| tr!("{} installed", id)),
            };
            let _ = slint::invoke_from_event_loop(move || with_hub(|h| h.store_done(&id, &verb, result)));
        });
        if let Err(e) = spawned {
            self.store_failed(tr!("no thread for the store: {}", e));
        }
    }

    fn store_done(&self, id: &str, verb: &str, result: Result<String, String>) {
        self.store.borrow_mut().busy = None;
        match result {
            Ok(msg) => {
                // the plugin comes into (or leaves) the hub without restarting
                self.reload_plugins();
                // just installed: enable it now, on the open page
                if verb == "install" && !self.host.config.borrow().is_enabled(id) {
                    self.ui.invoke_toggle_app(id.into());
                }
                self.store_reload();
                let mut st = self.store.borrow_mut();
                st.status = if verb == "install" { tr!("{} and enabled on the panel", msg) } else { msg };
                st.failed = false;
            }
            Err(e) => {
                let mut st = self.store.borrow_mut();
                st.status = e;
                st.failed = true;
            }
        }
        // an external app stopped by `suspend` and not swapped: runs again
        crate::apps::plugin::resume(id);
        self.store_push();
    }

    /// Takes the state to the screen.
    fn store_push(&self) {
        let st = self.store.borrow();
        let armed = st.armed.as_ref().filter(|(_, _, at)| at.elapsed() < ARM_TTL);
        let rows: Vec<StoreRow> = st
            .statuses
            .iter()
            .map(|s| {
                let e = &s.entry;
                let verb = armed.filter(|(i, _, _)| *i == e.id).map(|(_, v, _)| v.as_str()).unwrap_or("");
                let perms = e.grants_text();
                // stored data (only matters for installed ones: removing asks about it)
                let dir = appdata::dir(&e.id);
                let data = if s.installed.is_some() && appdata::has_data(&dir) {
                    appdata::human(appdata::size(&dir).0)
                } else {
                    String::new()
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
        let g = self.ui.global::<StoreApp>();
        g.set_rows(ModelRc::new(VecModel::from(rows)));
        g.set_status(st.status.as_str().into());
        g.set_failed(st.failed);
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
