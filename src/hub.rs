//! O hub: liga registro, provedores, janela e bandeja.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, ModelRc, SharedString, Timer, TimerMode, VecModel};

use crate::core::config::Config;
use crate::core::manifest::SettingKind;
use crate::core::miniapp::{AppRef, Host};
use crate::core::registry::Registry;
use crate::mascot::{Mood, render, sprites, themes};
use crate::popup;
use crate::tray::Tray;
use crate::ui::{AppInfo, HubWindow, SettingRow, Theme as UiTheme, Anim};

pub struct Hub {
    host: Rc<Host>,
    registry: Registry,
    ui: HubWindow,
    tray: RefCell<Option<Tray>>,
    shown: Cell<bool>,
    /// Dá para posicionar a janela (X11/XWayland).
    positionable: bool,
    /// Quando o popup se escondeu por perder o foco (ver `toggle`).
    focus_hidden_at: Cell<Option<Instant>>,
    /// O popup chegou a ganhar foco desde que abriu (o winit avisa "sem foco" ao mapear).
    had_focus: Cell<bool>,
    frame: Cell<u32>,
    last_tray: RefCell<(Mood, String, String)>,
}

impl Hub {
    pub fn new(start_hidden: bool, tab: Option<String>, positionable: bool) -> anyhow::Result<Rc<Self>> {
        let mut bus = crate::core::bus::Bus::default();
        let mut sys = crate::providers::sys::SysProvider::default();
        bus.spawn("sys", Duration::from_secs(2), move || crate::core::bus::Event::Sys(sys.collect()));
        let mut gpu = crate::providers::gpu::GpuProvider::detect();
        bus.spawn("gpu", Duration::from_secs(2), move || crate::core::bus::Event::Gpu(gpu.collect()));
        bus.spawn("docker", Duration::from_secs(3), || {
            crate::core::bus::Event::Docker(crate::providers::docker::collect())
        });

        let host = Rc::new(Host {
            config: RefCell::new(Config::load()),
            bus,
            notifier: Default::default(),
            settings_dirty: Cell::new(false),
        });
        let ui = HubWindow::new()?;

        let mut registry = Registry::new(host.clone());
        registry.add(crate::apps::docker::DockerMini::create(host.clone(), &ui) as AppRef);
        registry.add(crate::apps::vitals::VitalsMini::create(host.clone(), &ui) as AppRef);

        let hub = Rc::new(Hub {
            host,
            registry,
            ui,
            tray: RefCell::new(None),
            shown: Cell::new(false),
            positionable,
            focus_hidden_at: Cell::new(None),
            had_focus: Cell::new(false),
            frame: Cell::new(1),
            last_tray: RefCell::new((Mood::Sleep, String::new(), String::new())),
        });
        hub.registry.start(&hub.ui);
        if let Some(first) = hub.host.config.borrow().enabled.first() {
            hub.ui.set_current(first.into());
        }
        if let Some(tab) = tab {
            hub.ui.set_current(tab.into());
        }
        hub.apply_app_theme();
        hub.rebuild_lists();
        Self::bind(&hub);

        // bandeja: se não houver (GNOME sem AppIndicator), a janela fica sempre aberta
        match Tray::spawn(hub.ui.as_weak(), sprites::get("neko"), themes::get("neon"), Mood::Sleep) {
            Ok(t) => *hub.tray.borrow_mut() = Some(t),
            Err(e) => eprintln!("cyberhub: sem bandeja do sistema ({e}); a janela fica aberta"),
        }

        if !(start_hidden && hub.has_tray()) {
            hub.show(None);
        }
        Self::start_timers(&hub);
        Ok(hub)
    }

    // ---- janela -------------------------------------------------------------

    /// Mostra o popup junto de `anchor` (ponto clicado na bandeja).
    fn show(&self, anchor: Option<(i32, i32)>) {
        self.had_focus.set(false);
        if self.ui.show().is_err() {
            return;
        }
        self.shown.set(true);
        let window = self.ui.window();
        if self.positionable {
            popup::skip_taskbar(window);
            popup::place(window, anchor);
        }
        popup::focus(window);
        // o gerenciador de janelas às vezes reposiciona ao mapear: reaplica a posição
        if self.positionable {
            let weak = self.ui.as_weak();
            Timer::single_shot(Duration::from_millis(60), move || {
                if let Some(ui) = weak.upgrade() {
                    popup::place(ui.window(), anchor);
                }
            });
        }
    }

    fn has_tray(&self) -> bool {
        self.tray.borrow().is_some()
    }

    fn hide(&self) {
        if !self.has_tray() {
            // sem bandeja não teria como voltar: só minimiza
            self.ui.window().set_minimized(true);
            return;
        }
        let _ = self.ui.hide();
        self.shown.set(false);
    }

    fn toggle(&self, anchor: Option<(i32, i32)>) {
        // clicar no ícone tira o foco do popup antes do clique chegar aqui: se ele acabou
        // de se esconder por isso, o clique era para fechar, não para reabrir
        let just_hidden = self.focus_hidden_at.take().is_some_and(|t| t.elapsed() < Duration::from_millis(400));
        if self.shown.get() {
            self.hide();
        } else if !just_hidden {
            self.show(anchor);
        }
    }

    // ---- tema e listas --------------------------------------------------------

    /// O painel do app visível usa o tema dele; a moldura do hub é neutra.
    fn apply_app_theme(&self) {
        let current = self.ui.get_current();
        let Some(app) = self.registry.find(&current) else { return };
        let t = self.host.theme_for(app.borrow().manifest());
        let g = self.ui.global::<UiTheme>();
        let c = render::color;
        g.set_fur(c(t.fur));
        g.set_visor(c(t.visor));
        g.set_outline(c(t.outline));
        g.set_ok(c(t.ok));
        g.set_warn(c(t.warn));
        g.set_alarm(c(t.alarm));
        g.set_sleep(c(t.sleep));
        g.set_bg0(c(t.bg0));
        g.set_bg1(c(t.bg1));
        g.set_text(c(t.text));
        g.set_sub(c(t.sub));
        g.set_muted(c(t.muted));
    }

    fn app_info(&self, app: &AppRef, with_settings: bool) -> AppInfo {
        let a = app.borrow();
        let m = a.manifest();
        let settings = if with_settings { self.settings_of(m) } else { Vec::new() };
        AppInfo {
            id: m.id.clone().into(),
            name: m.name.clone().into(),
            description: m.description.clone().into(),
            icon: render::image(sprites::get(&m.icon), self.host.theme_for(m), Mood::Idle, 1),
            enabled: self.host.config.borrow().is_enabled(&m.id),
            mood: a.mood().as_str().into(),
            settings: ModelRc::new(VecModel::from(settings)),
        }
    }

    /// Abas, lista de apps e formulário de configurações.
    fn rebuild_lists(&self) {
        self.rebuild_tabs();
        self.rebuild_settings();
    }

    fn rebuild_tabs(&self) {
        let tabs: Vec<AppInfo> = self.registry.enabled().iter().map(|a| self.app_info(a, false)).collect();
        self.ui.set_tabs(ModelRc::new(VecModel::from(tabs)));
    }

    /// Lista da aba Apps, cada app com o formulário gerado do manifesto.
    fn rebuild_settings(&self) {
        let apps: Vec<AppInfo> = self.registry.all().iter().map(|a| self.app_info(a, true)).collect();
        self.ui.set_apps(ModelRc::new(VecModel::from(apps)));
    }

    fn settings_of(&self, m: &crate::core::manifest::Manifest) -> Vec<SettingRow> {
        let cfg = self.host.config.borrow();
        m.settings
            .iter()
            .map(|s| {
                let v = cfg.value(m, &s.key);
                SettingRow {
                    app: m.id.clone().into(),
                    key: s.key.clone().into(),
                    label: s.label.clone().into(),
                    kind: match s.kind {
                        SettingKind::Bool => "bool",
                        SettingKind::Int => "int",
                        SettingKind::Choice => "choice",
                    }
                    .into(),
                    bool_value: v.as_bool().unwrap_or(false),
                    int_value: v.as_integer().unwrap_or(0) as i32,
                    minimum: s.min.unwrap_or(0) as i32,
                    maximum: s.max.unwrap_or(i32::MAX as i64) as i32,
                    step: s.step.unwrap_or(1) as i32,
                    options: ModelRc::new(VecModel::from(
                        s.options.iter().map(|o| SharedString::from(option_label(o))).collect::<Vec<_>>(),
                    )),
                    index: s.options.iter().position(|o| Some(o.as_str()) == v.as_str()).unwrap_or(0) as i32,
                }
            })
            .collect()
    }

    fn change_setting(&self, app_id: &str, key: &str, input: Input) {
        let Some(app) = self.registry.find(app_id).cloned() else { return };
        {
            let a = app.borrow();
            let m = a.manifest();
            let Some(def) = m.setting(key) else { return };
            let mut cfg = self.host.config.borrow_mut();
            let cur = cfg.value(m, key);
            let next = match (def.kind, input) {
                (SettingKind::Bool, Input::Toggle) => toml::Value::Boolean(!cur.as_bool().unwrap_or(false)),
                (SettingKind::Int, Input::Int(v)) => {
                    toml::Value::Integer(v.clamp(def.min.unwrap_or(i64::MIN), def.max.unwrap_or(i64::MAX)))
                }
                (SettingKind::Choice, Input::Choice(i)) => match def.options.get(i) {
                    Some(o) => toml::Value::String(o.clone()),
                    None => return,
                },
                _ => return,
            };
            if next == cur {
                return;
            }
            cfg.set(app_id, key, next);
            cfg.save();
        }
        app.borrow_mut().settings_changed(&self.ui);
        // tema ou mascote podem ter mudado: ícones, painel e bandeja
        if self.ui.get_current() == app_id {
            self.apply_app_theme();
        }
        self.rebuild_lists();
        self.last_tray.borrow_mut().2.clear();
    }

    // ---- callbacks ----------------------------------------------------------

    fn bind(hub: &Rc<Self>) {
        let ui = &hub.ui;
        let w = Rc::downgrade(hub);
        ui.on_select_tab(move |id| {
            if let Some(h) = w.upgrade() {
                h.ui.set_current(id);
                h.apply_app_theme();
                h.anim_tick(); // desenha a mascote da aba já no primeiro quadro
            }
        });

        let w = Rc::downgrade(hub);
        ui.on_toggle_app(move |id| {
            let Some(h) = w.upgrade() else { return };
            let on = !h.host.config.borrow().is_enabled(&id);
            h.registry.set_enabled(&id, on, &h.ui);
            h.rebuild_lists();
        });

        let w = Rc::downgrade(hub);
        ui.on_setting_toggled(move |app, key| {
            if let Some(h) = w.upgrade() {
                h.change_setting(&app, &key, Input::Toggle);
            }
        });

        let w = Rc::downgrade(hub);
        ui.on_setting_int(move |app, key, v| {
            if let Some(h) = w.upgrade() {
                h.change_setting(&app, &key, Input::Int(v as i64));
            }
        });

        let w = Rc::downgrade(hub);
        ui.on_setting_chosen(move |app, key, i| {
            if let Some(h) = w.upgrade() {
                h.change_setting(&app, &key, Input::Choice(i.max(0) as usize));
            }
        });

        let w = Rc::downgrade(hub);
        ui.on_hide_window(move || {
            if let Some(h) = w.upgrade() {
                h.hide();
            }
        });

        let w = Rc::downgrade(hub);
        ui.on_toggle_requested(move |x, y| {
            if let Some(h) = w.upgrade() {
                let anchor = (x != 0 || y != 0).then_some((x, y));
                h.toggle(anchor);
            }
        });

        // popup: clicou fora (perdeu o foco) -> esconde
        let w = Rc::downgrade(hub);
        {
            use slint::winit_030::{EventResult, WinitWindowAccessor, winit::event::WindowEvent};
            ui.window().on_winit_window_event(move |_, ev| {
                if std::env::var_os("CYBERHUB_DEBUG").is_some() {
                    if let WindowEvent::Focused(f) = ev {
                        eprintln!("cyberhub: foco {f}");
                    }
                }
                let Some(h) = w.upgrade() else { return EventResult::Propagate };
                if let WindowEvent::Focused(true) = ev {
                    h.had_focus.set(true);
                }
                if let WindowEvent::Focused(false) = ev {
                    if h.shown.get() && h.has_tray() && h.had_focus.get() {
                        h.hide();
                        h.focus_hidden_at.set(Some(Instant::now()));
                    }
                }
                EventResult::Propagate
            });
        }

        ui.on_quit(|| {
            let _ = slint::quit_event_loop();
        });

        // fechar a janela só esconde quando há bandeja
        let w = Rc::downgrade(hub);
        ui.window().on_close_requested(move || {
            match w.upgrade() {
                Some(h) if h.has_tray() => {
                    h.shown.set(false);
                    slint::CloseRequestResponse::HideWindow
                }
                _ => {
                    let _ = slint::quit_event_loop();
                    slint::CloseRequestResponse::HideWindow
                }
            }
        });
    }

    // ---- laços --------------------------------------------------------------

    fn start_timers(hub: &Rc<Self>) {
        // dados: esvazia o barramento e atualiza humor/bandeja
        let data = Timer::default();
        let w = Rc::downgrade(hub);
        data.start(TimerMode::Repeated, Duration::from_millis(100), move || {
            if let Some(h) = w.upgrade() {
                h.data_tick();
            }
        });
        // animação: só com a janela visível
        let anim = Timer::default();
        let w = Rc::downgrade(hub);
        anim.start(TimerMode::Repeated, Duration::from_millis(125), move || {
            if let Some(h) = w.upgrade() {
                if h.shown.get() {
                    h.anim_tick();
                }
            }
        });
        // os timers vivem enquanto o event loop roda
        std::mem::forget(data);
        std::mem::forget(anim);
    }

    fn data_tick(&self) {
        let events = self.host.bus.drain();
        if !events.is_empty() {
            self.registry.dispatch(&events, &self.ui);
        }
        if self.host.settings_dirty.replace(false) {
            self.rebuild_settings();
        }

        // a bandeja mostra a mascote (e o tema) do app em pior estado
        let mood = self.registry.mood();
        let summary = self.registry.summary();
        let (sprite, theme) = match self.registry.worst() {
            Some(app) => {
                let a = app.borrow();
                (a.mascot_key(), self.host.theme_for(a.manifest()))
            }
            None => ("neko".to_string(), themes::get("neon")),
        };
        let theme_key = format!("{sprite}:{}", theme.key);
        let changed = {
            let last = self.last_tray.borrow();
            last.0 != mood || last.1 != summary || last.2 != theme_key
        };
        if !changed {
            return;
        }
        self.ui.set_summary(SharedString::from(summary.replace('\n', "   ·   ")));
        self.rebuild_tabs(); // humor das abas
        if let Some(tray) = self.tray.borrow().as_ref() {
            tray.update(sprites::get(&sprite), theme, mood, summary.clone());
        }
        *self.last_tray.borrow_mut() = (mood, summary, theme_key);
    }

    fn anim_tick(&self) {
        let f = self.frame.get().wrapping_add(1) % 600;
        self.frame.set(f);
        self.ui.global::<Anim>().set_frame(f as i32);
        // só o app da aba visível anima
        let current = self.ui.get_current();
        if let Some(app) = self.registry.find(&current) {
            if self.host.config.borrow().is_enabled(&current) {
                app.borrow_mut().tick(f, &self.ui);
            }
        }
    }
}

/// Rótulo de uma opção de `choice`: nome do tema ou da mascote, se for um.
fn option_label(key: &str) -> String {
    if key == "auto" {
        "Automática".into()
    } else if let Some(t) = themes::THEMES.iter().find(|t| t.key == key) {
        t.label.into()
    } else if let Some(s) = sprites::SPRITES.iter().find(|s| s.key == key) {
        s.label.into()
    } else {
        key.into()
    }
}

/// O que o usuário fez num campo da aba Apps.
enum Input {
    Toggle,
    Int(i64),
    Choice(usize),
}
