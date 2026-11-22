//! The contract between the host and a mini-app.

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

/// What is playing and which media controls the tray menu shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayMedia {
    pub summary: String,
    pub playing: bool,
    pub can_play_pause: bool,
    pub can_previous: bool,
    pub can_next: bool,
}

/// What a background action has to tell the app that asked for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Report {
    /// Finished fine.
    Done,
    /// Failed; the message comes ready for the screen.
    Error(String),
    /// Finished fine and returned a text (the output of a command).
    Output(String),
}

/// Handle an action's thread carries to tell the app the result.
///
/// The UI is not `Send`: the thread only leaves the message, and the hub delivers it on the
/// UI thread through `MiniApp::on_report`. So the result goes through the app's state, and
/// the next provider reading does not wipe it off the screen.
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

    pub fn output(&self, text: impl Into<String>) {
        self.send(Report::Output(text.into()));
    }

    fn send(&self, report: Report) {
        if self.tx.send((self.app.clone(), report)).is_ok() {
            self.waker.wake();
        }
    }

    /// Message matching the result: `Done`, or `Error` with "`prefix`: first line of the error".
    pub fn result<T, E: std::fmt::Display>(&self, prefix: &str, r: &Result<T, E>) {
        match r {
            Ok(_) => self.done(),
            Err(e) => self.error(format!("{prefix}: {}", e.to_string().lines().next().unwrap_or_default())),
        }
    }
}

/// Services the host offers every mini-app.
/// An app setting that picks a mascot (a mascot choice in its manifest): where the
/// Library's Mascots tab can put one ("Use in").
#[derive(Debug, Clone, PartialEq)]
pub struct MascotTarget {
    pub app: String,
    /// The app's name, in the interface language.
    pub name: String,
    /// The setting's key (`mascot`, `gpu_mascot`…).
    pub setting: String,
    /// The setting's label, when the app has more than one.
    pub label: String,
}

/// The mascot settings of the enabled apps, filled in by the hub whenever it rebuilds the
/// Settings list (the apps' manifests live there).
static MASCOT_TARGETS: std::sync::RwLock<Vec<MascotTarget>> = std::sync::RwLock::new(Vec::new());

pub fn set_mascot_targets(list: Vec<MascotTarget>) {
    if let Ok(mut t) = MASCOT_TARGETS.write() {
        *t = list;
    }
}

pub fn mascot_targets() -> Vec<MascotTarget> {
    MASCOT_TARGETS.read().map(|t| t.clone()).unwrap_or_default()
}

pub struct Host {
    pub config: RefCell<Config>,
    pub bus: Bus,
    pub notifier: RefCell<Notifier>,
    /// An app changed its own settings: the hub redraws the form on the Apps tab.
    pub settings_dirty: Cell<bool>,
    /// Messages from background actions (see `Reporter`).
    reports: (Sender<AppReport>, Receiver<AppReport>),
}

/// A message for an app: the app id and the message.
type AppReport = (String, Report);

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

    /// Handle for a thread to tell app `m` how the action ended.
    pub fn reporter(&self, m: &Manifest) -> Reporter {
        Reporter { app: m.id.clone(), tx: self.reports.0.clone(), waker: self.bus.waker().clone() }
    }

    /// Pending messages, each with its app id (called on the UI thread).
    pub fn drain_reports(&self) -> Vec<(String, Report)> {
        self.reports.1.try_iter().collect()
    }

    /// Theme chosen for the mini-app (each app has its own). "auto" follows the window skin.
    pub fn theme_for(&self, m: &Manifest) -> &'static Theme {
        let cfg = self.config.borrow();
        let key = cfg.string(m, "theme");
        let key = if key == "auto" { crate::skin::get(&cfg.skin).app_theme.to_string() } else { key };
        crate::mascot::themes::get(&key)
    }

    /// Changes an app option (e.g. from a button on its own screen), saves it and tells
    /// Settings to redraw the form.
    pub fn set_setting(&self, m: &Manifest, key: &str, value: toml::Value) {
        {
            let mut cfg = self.config.borrow_mut();
            cfg.set(&m.id, key, value);
            cfg.save();
        }
        self.settings_dirty.set(true);
    }

    /// Flips a `bool` option of the app; returns the new value.
    pub fn toggle_setting(&self, m: &Manifest, key: &str) -> bool {
        let on = !self.config.borrow().bool(m, key);
        self.set_setting(m, key, toml::Value::Boolean(on));
        on
    }

    /// Executor restricted to the manifest's allowlist.
    pub fn exec_for(&self, m: &Manifest) -> Exec {
        Exec::new(&m.id, &m.permissions.exec)
    }
}

/// A mini-app hosted by the hub.
///
/// Every call happens on the UI thread. A disabled app receives nothing.
pub trait MiniApp {
    fn manifest(&self) -> &Manifest;

    /// Enabled: prepares the state and fills the screen.
    fn enable(&mut self, _ui: &HubWindow) {}

    /// Disabled: lets go of whatever it holds.
    fn disable(&mut self, _ui: &HubWindow) {}

    /// A reading arrived from a provider the app subscribes to.
    fn on_event(&mut self, ev: &Event, ui: &HubWindow);

    /// A background action of the app finished (see `Host::reporter`). Arrives even when
    /// the app's page is closed.
    fn on_report(&mut self, _report: &Report, _ui: &HubWindow) {}

    /// The hub window opened (the app may ask for a fresh reading so it doesn't show stale data).
    fn window_shown(&mut self, _ui: &HubWindow) {}

    /// An animation frame (4 to 8/s), only while the window is open on the app's page.
    fn tick(&mut self, _frame: u32, _ui: &HubWindow) {}

    /// The user changed a setting or the theme.
    fn settings_changed(&mut self, _ui: &HubWindow) {}

    /// What the app stores (`[data]`; see `core/appdata.rs`).
    fn data_spec(&self) -> crate::core::appdata::DataSpec {
        self.manifest().data.clone().unwrap_or_else(|| crate::core::appdata::DataSpec::with(&[]))
    }

    /// The app's data is about to be deleted (`core/appdata.rs`): stop using the files
    /// (close the database, stop the process). The hub waits for `data_released` and then
    /// calls `reload_data`.
    fn release_data(&mut self, _ui: &HubWindow) {}

    /// The data files are no longer in use (the script thread or the process ended).
    fn data_released(&self) -> bool {
        true
    }

    /// The app's own process, when it has one running (external app); the others run
    /// inside the hub. Settings › System shows its CPU and memory.
    fn process_id(&self) -> Option<u32> {
        None
    }

    /// The data changed from outside (deleted): read it again, and run again if enabled.
    fn reload_data(&mut self, _ui: &HubWindow) {}

    /// Mood of the app; the tray shows the app in the worst state.
    fn mood(&self) -> Mood;

    /// Mascot that represents the app now (in the tray when it has the worst mood).
    fn mascot_key(&self) -> String {
        self.manifest().icon.clone()
    }

    /// One line for the tray tooltip.
    fn summary(&self) -> String;

    /// Optional media info and controls for the tray menu.
    fn tray_media(&self) -> Option<TrayMedia> {
        None
    }

    /// A media control was picked in the tray menu (only reaches the app whose
    /// `tray_media` is in the menu). Verbs: `prev`, `play-pause`, `next`.
    fn tray_command(&mut self, _verb: &str, _ui: &HubWindow) {}

    /// New facts for the mascot chatter (the hub collects them after each reading).
    fn take_facts(&mut self) -> Vec<Fact> {
        Vec::new()
    }

    /// Mascots of this app that may answer in a conversation.
    fn voices(&self) -> Vec<Voice> {
        Vec::new()
    }

    /// Own lines of this app's mascots: the content of `miniapps/<app>/lines.toml`
    /// (format in `core/chatter.rs`). Empty = only the hub's generic lines.
    fn lines(&self) -> &'static str {
        ""
    }
}

pub type AppRef = Rc<RefCell<dyn MiniApp>>;
