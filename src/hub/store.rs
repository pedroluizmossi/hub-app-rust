//! A tela da loja de plugins (`ui/store.slint`): carrega o índice, mostra o estado de cada
//! plugin e instala, atualiza ou remove com dois cliques. O trabalho (baixar o índice,
//! `git clone`, copiar) roda numa thread; o resultado volta pela thread da UI.

use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, ModelRc, VecModel};

use super::{Hub, with_hub};
use crate::core::plugins;
use crate::store::{self, Source, Status, Trust};
use crate::ui::{StoreApp, StoreRow};

/// Tempo para o segundo clique confirmar uma ação.
const ARM_TTL: Duration = Duration::from_secs(8);

#[derive(Default)]
pub(super) struct StoreState {
    statuses: Vec<Status>,
    trust: Option<Trust>,
    status: String,
    failed: bool,
    loading: bool,
    /// Ação esperando o segundo clique: (id, verbo, quando armou).
    armed: Option<(String, String, Instant)>,
    /// Plugin com uma ação em curso.
    busy: Option<String>,
}

impl Hub {
    pub(super) fn bind_store(hub: &Rc<Self>) {
        let g = hub.ui.global::<StoreApp>();
        let w = Rc::downgrade(hub);
        g.on_reload(move || {
            if let Some(h) = w.upgrade() {
                h.store_reload();
            }
        });
        let w = Rc::downgrade(hub);
        g.on_save_source(move |index, key| {
            if let Some(h) = w.upgrade() {
                {
                    let mut cfg = h.host.config.borrow_mut();
                    cfg.store_index = index.trim().to_string();
                    cfg.store_key = key.trim().to_string();
                    cfg.save();
                }
                h.store_reload();
            }
        });
        let w = Rc::downgrade(hub);
        g.on_act(move |id, verb| {
            if let Some(h) = w.upgrade() {
                h.store_act(&id, &verb);
            }
        });
    }

    /// Abriu a loja ou pediu para recarregar: baixa o índice numa thread.
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
        let spawned = std::thread::Builder::new().name("loja".into()).spawn(move || {
            let result = store::statuses(&src, &plugins::dir());
            let _ = slint::invoke_from_event_loop(move || with_hub(|h| h.store_loaded(result)));
        });
        if let Err(e) = spawned {
            self.store_failed(format!("sem thread para a loja: {e}"));
        }
    }

    fn store_loaded(&self, result: Result<(Vec<Status>, Trust), String>) {
        {
            let mut st = self.store.borrow_mut();
            st.loading = false;
            match result {
                Ok((list, trust)) => {
                    st.status = format!(
                        "{} · {} plugin(s)",
                        if trust == Trust::Signed { "assinatura conferida" } else { "índice local, sem assinatura" },
                        list.len()
                    );
                    st.statuses = list;
                    st.trust = Some(trust);
                    st.failed = false;
                }
                Err(e) => {
                    st.statuses.clear();
                    st.trust = None;
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

    /// Primeiro clique arma (e mostra o que vai acontecer); o segundo, dentro de
    /// `ARM_TTL`, faz.
    fn store_act(&self, id: &str, verb: &str) {
        let confirmed = {
            let mut st = self.store.borrow_mut();
            if st.busy.is_some() {
                return;
            }
            let same = st.armed.as_ref().is_some_and(|(i, v, at)| i == id && v == verb && at.elapsed() < ARM_TTL);
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
        let src = Source::from_config(&self.host.config.borrow());
        let (id, verb) = (id.to_string(), verb.to_string());
        let spawned = std::thread::Builder::new().name("loja".into()).spawn(move || {
            let root = plugins::dir();
            // a confirmação já foi o segundo clique, com o resumo na tela
            let result = match verb.as_str() {
                "remove" => store::remove(&id, &root),
                _ => store::install(&src, &id, &root, &mut |_| true).map(|_| format!("{id} instalado")),
            };
            let _ = slint::invoke_from_event_loop(move || with_hub(|h| h.store_done(&id, &verb, result)));
        });
        if let Err(e) = spawned {
            self.store_failed(format!("sem thread para a loja: {e}"));
        }
    }

    fn store_done(&self, id: &str, verb: &str, result: Result<String, String>) {
        self.store.borrow_mut().busy = None;
        match result {
            Ok(msg) => {
                // o plugin entra (ou sai) do hub sem reiniciar
                self.reload_plugins();
                // instalado agora: já liga, na página aberta
                if verb == "install" && !self.host.config.borrow().is_enabled(id) {
                    self.ui.invoke_toggle_app(id.into());
                }
                self.store_reload();
                let mut st = self.store.borrow_mut();
                st.status = if verb == "install" { format!("{msg} e ligado no painel") } else { msg };
                st.failed = false;
            }
            Err(e) => {
                let mut st = self.store.borrow_mut();
                st.status = e;
                st.failed = true;
            }
        }
        self.store_push();
    }

    /// Leva o estado à tela.
    fn store_push(&self) {
        let st = self.store.borrow();
        let armed = st.armed.as_ref().filter(|(_, _, at)| at.elapsed() < ARM_TTL);
        let rows: Vec<StoreRow> = st
            .statuses
            .iter()
            .map(|s| {
                let e = &s.entry;
                let verb = armed.filter(|(i, _, _)| *i == e.id).map(|(_, v, _)| v.as_str()).unwrap_or("");
                let perms = if e.permissions.is_empty() { "não executa programas".into() } else { format!("executa: {}", e.permissions.join(", ")) };
                let new_perms = s.new_permissions();
                let state = match &s.installed {
                    None => "não instalado".to_string(),
                    Some(i) if s.has_update() => format!("atualização: {} → {}", i.version, e.version),
                    Some(i) => format!("instalado {}", i.version),
                };
                let confirm = match verb {
                    "install" => format!("Clique de novo para instalar. O plugin vai poder {perms}."),
                    "update" if !new_perms.is_empty() => format!("Clique de novo para atualizar e permitir também: {}.", new_perms.join(", ")),
                    "update" => "Clique de novo para atualizar (sem permissão nova).".into(),
                    "remove" => format!("Clique de novo para remover {}.", e.name),
                    _ => String::new(),
                };
                StoreRow {
                    id: e.id.as_str().into(),
                    name: e.name.as_str().into(),
                    version: e.version.as_str().into(),
                    byline: format!("{} · {}", if e.author.is_empty() { "sem autor" } else { &e.author }, e.license).into(),
                    description: e.description.as_str().into(),
                    permissions: perms.into(),
                    state: state.into(),
                    installed: s.installed.is_some(),
                    update: s.has_update(),
                    warning: if s.has_update() && !new_perms.is_empty() {
                        format!("ATENÇÃO: a versão nova pede permissão para executar {}", new_perms.join(", ")).into()
                    } else {
                        "".into()
                    },
                    armed: verb.into(),
                    confirm: confirm.into(),
                    busy: st.busy.as_deref() == Some(e.id.as_str()),
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
