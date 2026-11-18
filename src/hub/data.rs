//! Each app's data in Settings (`core/appdata.rs`): what it keeps and how much, export,
//! delete, and the data left over from removed apps.

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::Duration;

use slint::{ModelRc, VecModel};

use super::{Hub, with_hub};
use crate::core::appdata::{self, DataSpec};
use crate::tr;
use crate::ui::DataRow;

/// How long to wait for the app to release its files (script thread or process) before deleting.
const RELEASE_WAIT: Duration = Duration::from_secs(5);
const RELEASE_POLL: Duration = Duration::from_millis(100);

thread_local! {
    /// Each app's data folder as last measured: (bytes, cut short, has data). The Settings
    /// list is rebuilt on every setting change; the folders are only walked again when
    /// Settings opens or data is deleted (`refresh_data`).
    static SIZES: RefCell<HashMap<String, (u64, bool, bool)>> = RefCell::new(HashMap::new());
}

/// The size of an app's data folder, measured once until `refresh_data` forgets it.
fn measured(id: &str) -> (u64, bool, bool) {
    SIZES.with(|c| {
        *c.borrow_mut().entry(id.to_string()).or_insert_with(|| {
            let dir = appdata::dir(id);
            let has = appdata::has_data(&dir);
            let (bytes, cut) = if has { appdata::size(&dir) } else { (0, false) };
            (bytes, cut, has)
        })
    })
}

/// An app's "Data" row: "SQLite database and key-value · 1.2 MB (database up to 10 MB)",
/// "files · empty"; empty if the app keeps nothing. And whether there is something to export or delete.
pub(super) fn describe(id: &str, spec: &DataSpec) -> (String, bool) {
    let (bytes, cut, has) = measured(id);
    if spec.kinds.is_empty() && !has {
        return (String::new(), false);
    }
    let what = if spec.kinds.is_empty() { tr!("data").to_string() } else { spec.describe() };
    let size = if has {
        if cut { tr!("over {}", appdata::human(bytes)) } else { appdata::human(bytes) }
    } else {
        tr!("empty").into()
    };
    let quota = if spec.has(appdata::Kind::Sqlite) { format!(" ({})", tr!("database up to {} MB", spec.max_mb)) } else { String::new() };
    (format!("{what} · {size}{quota}"), has)
}

impl Hub {
    /// Settings opened: new sizes and the list of data without an app.
    pub(super) fn refresh_data(&self) {
        SIZES.with(|c| c.borrow_mut().clear());
        self.settings_fingerprint.replace(String::new());
        self.rebuild_settings();
        let root = appdata::root();
        let known: Vec<String> = self.registry.all().iter().map(|a| a.borrow().manifest().id.clone()).collect();
        let rows: Vec<DataRow> = appdata::ids(&root)
            .into_iter()
            .filter(|id| !known.contains(id))
            .map(|id| {
                let (bytes, _) = appdata::size(&root.join(&id));
                DataRow { text: format!("{id} · {}", appdata::human(bytes)).into(), id: id.into() }
            })
            .collect();
        self.ui.set_orphan_data(ModelRc::new(VecModel::from(rows)));
    }

    fn data_note(&self, id: &str, text: String, bad: bool) {
        self.ui.set_data_note_app(id.into());
        self.ui.set_data_note(text.into());
        self.ui.set_data_note_bad(bad);
    }

    /// Copies the data to `<export folder>/<id>-<date>/` (see `appdata::export_dir`), in a thread, and opens the folder.
    pub(super) fn export_data(&self, id: &str) {
        self.data_note(id, tr!("exporting…").into(), false);
        let id = id.to_string();
        let spawned = std::thread::Builder::new().name("export".into()).spawn(move || {
            let r = appdata::export(&appdata::root(), &id, &appdata::export_dir());
            let _ = slint::invoke_from_event_loop(move || {
                with_hub(|h| match r {
                    Ok(dir) => {
                        let _ = crate::core::exec::open_dir(&dir);
                        h.data_note(&id, tr!("exported to {}", dir.display()), false);
                    }
                    Err(e) => h.data_note(&id, tr!("export failed: {}", e), true),
                })
            });
        });
        if let Err(e) = spawned {
            self.data_note("", tr!("no thread to export: {}", e), true);
        }
    }

    /// Deletes an app's data. The app first releases its files (the script closes the database,
    /// the process exits); then it runs again, if it was on, with empty data.
    pub(super) fn delete_data(&self, id: &str) {
        match self.registry.find(id) {
            Some(app) => {
                app.borrow_mut().release_data(&self.ui);
                self.data_note(id, tr!("deleting…").into(), false);
                wait_release(id.to_string(), 0);
            }
            // data of an app that is no longer here
            None => {
                let r = appdata::delete(&appdata::root(), id);
                self.after_delete(id, r);
            }
        }
    }

    fn finish_delete(&self, id: &str, tries: u32) {
        let Some(app) = self.registry.find(id) else { return };
        let released = app.borrow().data_released();
        if !released && RELEASE_POLL * tries < RELEASE_WAIT {
            wait_release(id.to_string(), tries + 1);
            return;
        }
        let r = if released {
            appdata::delete(&appdata::root(), id)
        } else {
            Err(tr!("the app did not stop in time; try again").into())
        };
        app.borrow_mut().reload_data(&self.ui);
        self.after_delete(id, r);
    }

    fn after_delete(&self, id: &str, r: Result<(), String>) {
        match r {
            Ok(()) => {
                log::info!("data of {id} deleted");
                self.data_note(id, tr!("data deleted").into(), false);
            }
            Err(e) => {
                log::warn!("data of {id}: {e}");
                self.data_note(id, tr!("delete failed: {}", e), true);
            }
        }
        self.refresh_data();
    }
}

fn wait_release(id: String, tries: u32) {
    slint::Timer::single_shot(RELEASE_POLL, move || with_hub(|h| h.finish_delete(&id, tries)));
}
