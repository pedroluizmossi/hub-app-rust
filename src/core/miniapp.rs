//! O contrato entre o host e um mini-app.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender, channel};

use super::bus::{Bus, Event};
use super::chatter::{Fact, Voice};
use super::config::Config;
use super::exec::Exec;
use super::manifest::Manifest;
use super::notify::Notifier;
use super::wake::Waker;
use crate::mascot::{Mood, themes::Theme};
use crate::ui::HubWindow;

/// O que tocando e quais controles de mídia o menu da bandeja mostra.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayMedia {
    pub summary: String,
    pub playing: bool,
    pub can_play_pause: bool,
    pub can_previous: bool,
    pub can_next: bool,
}

/// O que uma ação em segundo plano tem a dizer ao app que a pediu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Report {
    /// Terminou bem.
    Done,
    /// Falhou; a mensagem já vem pronta para a tela.
    Error(String),
}

/// Alça que a thread de uma ação leva consigo para avisar o app do resultado.
///
/// A UI não é `Send`: a thread só deixa o recado, e o hub o entrega na thread da UI por
/// `MiniApp::on_report`. Assim o resultado passa pelo estado do app, e a próxima leitura
/// de um provedor não o apaga da tela.
#[derive(Clone)]
pub struct Reporter {
    app: String,
    tx: Sender<(String, Report)>,
    waker: Waker,
}

impl Reporter {
    pub fn done(&self) {
        self.send(Report::Done);
    }

    pub fn error(&self, msg: impl Into<String>) {
        self.send(Report::Error(msg.into()));
    }

    fn send(&self, report: Report) {
        if self.tx.send((self.app.clone(), report)).is_ok() {
            self.waker.wake();
        }
    }

    /// Recado conforme o resultado: `Done`, ou `Error` com "`prefix`: primeira linha do erro".
    pub fn result<T, E: std::fmt::Display>(&self, prefix: &str, r: &Result<T, E>) {
        match r {
            Ok(_) => self.done(),
            Err(e) => self.error(format!("{prefix}: {}", e.to_string().lines().next().unwrap_or_default())),
        }
    }
}

/// Serviços que o host oferece a todos os mini-apps.
pub struct Host {
    pub config: RefCell<Config>,
    pub bus: Bus,
    pub notifier: RefCell<Notifier>,
    /// Um app mudou a própria configuração: o hub redesenha o formulário da aba Apps.
    pub settings_dirty: Cell<bool>,
    /// Recados das ações em segundo plano (ver `Reporter`).
    reports: (Sender<(String, Report)>, Receiver<(String, Report)>),
}

impl Host {
    pub fn new(config: Config, bus: Bus) -> Self {
        Host {
            config: RefCell::new(config),
            bus,
            notifier: Default::default(),
            settings_dirty: Cell::new(false),
            reports: channel(),
        }
    }

    /// Alça para uma thread avisar o app `m` de como a ação terminou.
    pub fn reporter(&self, m: &Manifest) -> Reporter {
        Reporter { app: m.id.clone(), tx: self.reports.0.clone(), waker: self.bus.waker().clone() }
    }

    /// Recados pendentes, com o id do app de cada um (chamado na thread da UI).
    pub fn drain_reports(&self) -> Vec<(String, Report)> {
        self.reports.1.try_iter().collect()
    }

    /// Tema escolhido pelo mini-app (cada app tem o seu). "auto" acompanha o visual da janela.
    pub fn theme_for(&self, m: &Manifest) -> &'static Theme {
        let cfg = self.config.borrow();
        let key = cfg.string(m, "theme");
        let key = if key == "auto" { crate::skin::get(&cfg.skin).app_theme.to_string() } else { key };
        crate::mascot::themes::get(&key)
    }

    /// Muda uma opção do app (ex.: um botão da própria tela), grava e avisa as
    /// Configurações para redesenhar o formulário.
    pub fn set_setting(&self, m: &Manifest, key: &str, value: toml::Value) {
        {
            let mut cfg = self.config.borrow_mut();
            cfg.set(&m.id, key, value);
            cfg.save();
        }
        self.settings_dirty.set(true);
    }

    /// Inverte uma opção `bool` do app; devolve o valor novo.
    pub fn toggle_setting(&self, m: &Manifest, key: &str) -> bool {
        let on = !self.config.borrow().bool(m, key);
        self.set_setting(m, key, toml::Value::Boolean(on));
        on
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

    /// Uma ação em segundo plano do app terminou (ver `Host::reporter`). Chega mesmo com
    /// a página do app fechada.
    fn on_report(&mut self, _report: &Report, _ui: &HubWindow) {}

    /// A janela do hub abriu (o app pode pedir uma leitura nova para não mostrar dado velho).
    fn window_shown(&mut self, _ui: &HubWindow) {}

    /// Um quadro de animação (4 a 8/s), só com a janela aberta na página do app.
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

    /// Informações e controles opcionais de mídia para o menu da bandeja.
    fn tray_media(&self) -> Option<TrayMedia> {
        None
    }

    /// Escolheram um controle de mídia no menu da bandeja (só chega ao app cuja
    /// `tray_media` está no menu). Verbos: `prev`, `play-pause`, `next`.
    fn tray_command(&mut self, _verb: &str, _ui: &HubWindow) {}

    /// Fatos novos para a conversa entre mascotes (o hub recolhe depois de cada leitura).
    fn take_facts(&mut self) -> Vec<Fact> {
        Vec::new()
    }

    /// Mascotes deste app que podem responder numa conversa.
    fn voices(&self) -> Vec<Voice> {
        Vec::new()
    }

    /// Falas próprias das mascotes deste app: o conteúdo de `miniapps/<app>/lines.toml`
    /// (formato em `core/chatter.rs`). Vazio = só as falas genéricas do hub.
    fn lines(&self) -> &'static str {
        ""
    }
}

pub type AppRef = Rc<RefCell<dyn MiniApp>>;
