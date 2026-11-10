//! O contrato entre o host e um mini-app.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use super::bus::{Bus, Event};
use super::config::Config;
use super::exec::Exec;
use super::manifest::Manifest;
use super::notify::Notifier;
use crate::mascot::{Mood, themes::Theme};
use crate::ui::HubWindow;

/// Serviços que o host oferece a todos os mini-apps.
pub struct Host {
    pub config: RefCell<Config>,
    pub bus: Bus,
    pub notifier: RefCell<Notifier>,
    /// Um app mudou a própria configuração: o hub redesenha o formulário da aba Apps.
    pub settings_dirty: Cell<bool>,
}

impl Host {
    /// Tema escolhido pelo mini-app (cada app tem o seu; o hub é neutro).
    pub fn theme_for(&self, m: &Manifest) -> &'static Theme {
        crate::mascot::themes::get(&self.config.borrow().string(m, "theme"))
    }

    /// Executor restrito à allowlist do manifesto.
    pub fn exec_for(&self, m: &Manifest) -> Exec {
        Exec::new(&m.id, &m.permissions.exec)
    }
}

/// Um mini-app hospedado pelo hub.
///
/// Todas as chamadas acontecem na thread da UI. Um app desligado não recebe nada.
pub trait MiniApp {
    fn manifest(&self) -> &Manifest;

    /// Ligou: prepara o estado e preenche a tela.
    fn enable(&mut self, _ui: &HubWindow) {}

    /// Desligou: solta o que estiver segurando.
    fn disable(&mut self, _ui: &HubWindow) {}

    /// Chegou uma leitura de um provedor que o app assina.
    fn on_event(&mut self, ev: &Event, ui: &HubWindow);

    /// Um quadro de animação (~8/s), só enquanto a aba do app está visível.
    fn tick(&mut self, _frame: u32, _ui: &HubWindow) {}

    /// O usuário mudou alguma configuração ou o tema.
    fn settings_changed(&mut self, _ui: &HubWindow) {}

    /// Humor do app; a bandeja mostra o app em pior estado.
    fn mood(&self) -> Mood;

    /// Mascote que representa o app agora (bandeja quando ele é o pior humor).
    fn mascot_key(&self) -> String {
        self.manifest().icon.clone()
    }

    /// Uma linha para o tooltip da bandeja.
    fn summary(&self) -> String;
}

pub type AppRef = Rc<RefCell<dyn MiniApp>>;
