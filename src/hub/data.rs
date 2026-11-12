//! Os dados de cada app nas Configurações (`core/appdata.rs`): o que guarda e quanto,
//! exportar, apagar, e os dados que ficaram de apps removidos.

use std::time::Duration;

use slint::{ModelRc, VecModel};

use super::{Hub, with_hub};
use crate::core::appdata::{self, DataSpec};
use crate::ui::DataRow;

/// Quanto esperar o app soltar os arquivos (thread do script ou processo) antes de apagar.
const RELEASE_WAIT: Duration = Duration::from_secs(5);
const RELEASE_POLL: Duration = Duration::from_millis(100);

/// A linha "Dados" de um app: "banco SQLite e chave-valor · 1,2 MB (até 10 MB)", "arquivos ·
/// vazio"; vazio se o app não guarda nada. E se há algo para exportar ou apagar.
pub(super) fn describe(id: &str, spec: &DataSpec) -> (String, bool) {
    let dir = appdata::dir(id);
    let has = appdata::has_data(&dir);
    if spec.kinds.is_empty() && !has {
        return (String::new(), false);
    }
    let what = if spec.kinds.is_empty() { "dados".to_string() } else { spec.describe() };
    let size = if has {
        let (bytes, cut) = appdata::size(&dir);
        format!("{}{}", if cut { "mais de " } else { "" }, appdata::human(bytes))
    } else {
        "vazio".into()
    };
    let quota = if spec.has(appdata::Kind::Sqlite) { format!(" (banco até {} MB)", spec.max_mb) } else { String::new() };
    (format!("{what} · {size}{quota}"), has)
}

impl Hub {
    /// Abriu as Configurações: tamanhos novos e a lista dos dados sem app.
    pub(super) fn refresh_data(&self) {
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

    /// Copia os dados para `Downloads/miniutils-dados/<id>-<data>/` (numa thread) e abre a pasta.
    pub(super) fn export_data(&self, id: &str) {
        self.data_note(id, "exportando…".into(), false);
        let id = id.to_string();
        let spawned = std::thread::Builder::new().name("exportar".into()).spawn(move || {
            let r = appdata::export(&appdata::root(), &id, &appdata::export_dir());
            let _ = slint::invoke_from_event_loop(move || {
                with_hub(|h| match r {
                    Ok(dir) => {
                        let _ = crate::core::exec::open_dir(&dir);
                        h.data_note(&id, format!("exportado para {}", dir.display()), false);
                    }
                    Err(e) => h.data_note(&id, format!("não exportou: {e}"), true),
                })
            });
        });
        if let Err(e) = spawned {
            self.data_note("", format!("sem thread para exportar: {e}"), true);
        }
    }

    /// Apaga os dados de um app. O app primeiro solta os arquivos (o script fecha o banco,
    /// o processo sai); depois volta a rodar, se estava ligado, com os dados vazios.
    pub(super) fn delete_data(&self, id: &str) {
        match self.registry.find(id) {
            Some(app) => {
                app.borrow_mut().release_data(&self.ui);
                self.data_note(id, "apagando…".into(), false);
                wait_release(id.to_string(), 0);
            }
            // dados de um app que não está mais aqui
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
            Err("o app não parou a tempo; tente de novo".into())
        };
        app.borrow_mut().reload_data(&self.ui);
        self.after_delete(id, r);
    }

    fn after_delete(&self, id: &str, r: Result<(), String>) {
        match r {
            Ok(()) => {
                log::info!("dados de {id} apagados");
                self.data_note(id, "dados apagados".into(), false);
            }
            Err(e) => {
                log::warn!("dados de {id}: {e}");
                self.data_note(id, format!("não apagou: {e}"), true);
            }
        }
        self.refresh_data();
    }
}

fn wait_release(id: String, tries: u32) {
    slint::Timer::single_shot(RELEASE_POLL, move || with_hub(|h| h.finish_delete(&id, tries)));
}
