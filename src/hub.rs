//! O hub: liga registro, provedores, janela e bandeja.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, ModelRc, SharedString, Timer, TimerMode, VecModel};

use crate::core::config::Config;
use crate::core::manifest::SettingKind;
use crate::core::miniapp::{AppRef, Host};
use crate::core::registry::Registry;
use crate::instance;
use crate::mascot::{Mood, render, sprites, themes};
use crate::popup;
use crate::skin;
use crate::tray::Tray;
use crate::core::layout::{self, Rect};
use crate::ui::{AppInfo, Ghost, GridCell, HubWindow, SettingRow, Anim};

/// Depois do último tamanho novo durante um arrasto de borda, o foco ainda é ignorado este tempo.
const RESIZE_QUIET: Duration = Duration::from_millis(800);

/// Intervalo do quadro de animação (mascotes, costura e luz da borda). Com o mouse sobre a
/// janela, 8 quadros/s; fora dela, 4/s, porque o renderizador por software redesenha tudo.
const FRAME_HOT: Duration = Duration::from_millis(125);
const FRAME_COLD: Duration = Duration::from_millis(250);

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
    /// Modo fixar: perder o foco não esconde o popup.
    pinned: Cell<bool>,
    /// Último ponto da bandeja onde o popup abriu (para reabrir no mesmo lugar).
    last_anchor: Cell<Option<(i32, i32)>>,
    /// Até quando ignorar a perda de foco por causa de um redimensionamento pela borda.
    resize_until: Cell<Option<Instant>>,
    frame: Cell<u32>,
    /// Geração do fade em curso: um fade novo cancela o anterior.
    fade_gen: Rc<Cell<u32>>,
    last_tray: RefCell<(Mood, String, String)>,
    /// O mouse está sobre a janela (`CursorEntered`/`CursorLeft` do winit).
    hovered: Cell<bool>,
    /// Relógio do quadro de animação; o intervalo segue `hovered` (ver `FRAME_HOT`).
    anim: Timer,
    /// Arrasto em curso no modo de edição.
    drag: RefCell<Option<Drag>>,
    /// Tamanho (lógico) da área do painel, para desenhar o fundo; e a geração do pedido
    /// de desenho (vários pedidos seguidos num redimensionamento viram um só).
    backdrop_size: Cell<(f32, f32)>,
    backdrop_gen: Rc<Cell<u32>>,
    /// Minuto mostrado no relógio ("21:57").
    clock: RefCell<String>,
}

/// Um app sendo movido ou redimensionado no modo de edição.
struct Drag {
    id: String,
    /// "move", "r" (largura), "b" (altura) ou "rb" (as duas).
    mode: String,
    /// Célula agarrada dentro do app (ao mover).
    off: (i32, i32),
    /// Onde ele iria parar e se os outros acham lugar.
    target: Rect,
    ok: bool,
}

impl Hub {
    pub fn new(start_hidden: bool, tab: Option<String>, positionable: bool) -> anyhow::Result<Rc<Self>> {
        let mut bus = crate::core::bus::Bus::default();
        let mut sys = crate::providers::sys::SysProvider::default();
        bus.spawn("sys", Duration::from_secs(2), move || crate::core::bus::Event::Sys(sys.collect()));
        let mut gpu = crate::providers::gpu::GpuProvider::detect();
        bus.spawn("gpu", Duration::from_secs(2), move || crate::core::bus::Event::Gpu(gpu.collect()));
        // `docker events` traz as mudanças; ps é rede de segurança e stats roda à parte
        let mut docker = crate::providers::docker::DockerProvider::default();
        bus.spawn_polled("docker", Duration::from_secs(1), move |poker, poked| {
            docker.collect(poker, poked).map(crate::core::bus::Event::Docker)
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
            pinned: Cell::new(false),
            last_anchor: Cell::new(None),
            resize_until: Cell::new(None),
            frame: Cell::new(1),
            fade_gen: Rc::new(Cell::new(0)),
            last_tray: RefCell::new((Mood::Sleep, String::new(), String::new())),
            hovered: Cell::new(false),
            anim: Timer::default(),
            drag: RefCell::new(None),
            backdrop_size: Cell::new((0.0, 0.0)),
            backdrop_gen: Rc::new(Cell::new(0)),
            clock: RefCell::new(String::new()),
        });
        hub.apply_skin();
        hub.registry.start(&hub.ui);
        hub.ui.set_current(tab_name(tab.as_deref()));
        hub.rebuild_lists();
        Self::bind(&hub);

        // bandeja: se não houver (GNOME sem AppIndicator), a janela fica sempre aberta
        match Tray::spawn(hub.ui.as_weak(), sprites::get("neko"), themes::get("neon"), Mood::Sleep) {
            Ok(t) => *hub.tray.borrow_mut() = Some(t),
            Err(e) => eprintln!("miniutils: sem bandeja do sistema ({e}); a janela fica aberta"),
        }

        // tamanho lembrado da última vez (lógico: o Slint converte pela escala da tela)
        if let Some((w, h)) = hub.host.config.borrow().window {
            hub.ui.window().set_size(slint::LogicalSize::new(w.max(360) as f32, h.max(280) as f32));
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
        let anchor = anchor.or(self.last_anchor.get());
        self.last_anchor.set(anchor);
        self.had_focus.set(false);
        // reabrindo: começa transparente e aparece com fade (a 1ª vez a janela ainda não existe)
        let fade_in = self.positionable && popup::has_window(self.ui.window());
        if fade_in {
            self.fade_gen.set(self.fade_gen.get() + 1); // cancela um fade de saída pendente
            popup::set_opacity(self.ui.window(), 0.0);
        }
        if self.ui.show().is_err() {
            return;
        }
        self.schedule_backdrop();
        if fade_in {
            self.fade(0.0, 1.0, false);
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

    /// Pedido de outra instância: abre o popup (na aba, se houver). Já aberto, só traz para frente.
    fn show_from_command(&self, tab: &str) {
        if !tab.is_empty() {
            self.ui.set_current(tab_name(Some(tab)));
        }
        if self.shown.get() {
            popup::focus(self.ui.window());
        } else {
            self.show(None);
        }
    }

    /// Atende as outras instâncias (ver `instance.rs`) pela thread da UI.
    pub fn serve_commands(&self, server: &instance::Server) {
        let weak = self.ui.as_weak();
        let served = server.serve(move |cmd| {
            let instance::Command::Show(tab) = cmd;
            let tab = tab.unwrap_or_default();
            let _ = weak.upgrade_in_event_loop(move |ui| ui.invoke_show_requested(tab.into()));
        });
        if let Err(e) = served {
            eprintln!("miniutils: não consegui atender outras instâncias ({e})");
        }
    }

    fn has_tray(&self) -> bool {
        self.tray.borrow().is_some()
    }

    /// Guarda o tamanho atual da janela, se mudou. Em pixels lógicos, para não mudar de
    /// tamanho quando a escala da tela muda.
    fn remember_size(&self) {
        let window = self.ui.window();
        let size = window.size();
        if size.width == 0 || size.height == 0 {
            return;
        }
        let scale = window.scale_factor();
        let logical = ((size.width as f32 / scale).round() as u32, (size.height as f32 / scale).round() as u32);
        let mut cfg = self.host.config.borrow_mut();
        if cfg.window != Some(logical) {
            cfg.window = Some(logical);
            cfg.save();
        }
    }

    /// Muda o intervalo do quadro conforme o mouse está (ou não) sobre a janela.
    fn set_hovered(&self, on: bool) {
        if self.hovered.replace(on) != on {
            self.anim.set_interval(if on { FRAME_HOT } else { FRAME_COLD });
        }
    }

    fn hide(&self) {
        self.remember_size();
        // ao esconder não chega `CursorLeft`: a próxima vez que aparecer, o `CursorEntered` decide
        self.set_hovered(false);
        if !self.has_tray() {
            // sem bandeja não teria como voltar: só minimiza
            self.ui.window().set_minimized(true);
            return;
        }
        self.shown.set(false);
        if self.positionable {
            self.fade(1.0, 0.0, true); // some com fade em vez de piscar
        } else {
            let _ = self.ui.hide();
        }
    }

    /// Anima a opacidade da janela de `from` a `to` em ~110 ms; `then_hide` esconde no fim.
    fn fade(&self, from: f32, to: f32, then_hide: bool) {
        const STEPS: u32 = 6;
        let id = self.fade_gen.get() + 1;
        self.fade_gen.set(id);
        for i in 1..=STEPS {
            let weak = self.ui.as_weak();
            let current = self.fade_gen.clone();
            Timer::single_shot(Duration::from_millis(18 * i as u64), move || {
                if current.get() != id {
                    return; // outro fade começou
                }
                let Some(ui) = weak.upgrade() else { return };
                let t = i as f32 / STEPS as f32;
                popup::set_opacity(ui.window(), from + (to - from) * t);
                if i == STEPS && then_hide {
                    let _ = ui.hide();
                }
            });
        }
    }

    /// Liga/desliga o modo fixar (alfinete, clique do meio ou menu da bandeja).
    fn set_pinned(&self, on: bool) {
        self.pinned.set(on);
        self.ui.set_pinned(on);
        if let Some(tray) = self.tray.borrow().as_ref() {
            tray.set_pinned(on);
        }
    }

    /// Um arrasto de borda está em curso (ou acabou há pouco).
    fn resizing(&self) -> bool {
        self.resize_until.get().is_some_and(|t| Instant::now() < t)
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

    // ---- visual da janela ---------------------------------------------------------

    /// Aplica o visual da config: tokens da tela, lista do seletor e fundo.
    fn apply_skin(&self) {
        let (key, name) = {
            let cfg = self.host.config.borrow();
            (cfg.skin.clone(), crate::core::config::display_name())
        };
        skin::apply(&self.ui, skin::get(&key), &name);
        self.ui.set_app_name(name.into());
        let labels: Vec<SharedString> = skin::SKINS.iter().map(|s| s.label.into()).collect();
        self.ui.set_skins(ModelRc::new(VecModel::from(labels)));
        self.ui.set_skin_index(skin::index_of(&key) as i32);
        self.schedule_backdrop();
    }

    /// Troca o visual. Os apps com tema "auto" mudam junto.
    fn choose_skin(&self, i: usize) {
        let Some(s) = skin::SKINS.get(i) else { return };
        {
            let mut cfg = self.host.config.borrow_mut();
            if cfg.skin == s.key {
                return;
            }
            cfg.skin = s.key.into();
            cfg.save();
        }
        self.apply_skin();
        for app in self.registry.enabled() {
            app.borrow_mut().settings_changed(&self.ui);
        }
        self.rebuild_lists();
        self.last_tray.borrow_mut().2.clear(); // a bandeja pode ter mudado de tema
    }

    /// Pede um fundo novo daqui a pouco (um só, mesmo com vários pedidos seguidos).
    fn schedule_backdrop(&self) {
        let id = self.backdrop_gen.get().wrapping_add(1);
        self.backdrop_gen.set(id);
        let current = self.backdrop_gen.clone();
        let weak = self.ui.as_weak();
        let size = self.backdrop_size.get();
        let key = self.host.config.borrow().skin.clone();
        Timer::single_shot(Duration::from_millis(60), move || {
            if current.get() != id {
                return;
            }
            let Some(ui) = weak.upgrade() else { return };
            let s = skin::get(&key);
            let window = ui.window();
            let scale = window.scale_factor();
            // sem o tamanho exato da área ainda: estima pela janela
            let (w, h) = if size.0 > 0.0 && size.1 > 0.0 {
                size
            } else {
                let px = window.size();
                let (ww, wh) = (px.width as f32 / scale, px.height as f32 / scale);
                let chrome = 2.0 * s.border + s.header_h + s.header_line + s.panel_top_h + s.footer_h;
                ((ww - 2.0 * s.border).max(1.0), (wh - chrome).max(1.0))
            };
            let buf = skin::backdrop::render(s.backdrop, s.bg, w, h, scale);
            ui.set_backdrop(slint::Image::from_rgb8(buf));
        });
    }

    /// Relógio e saudação, atualizados quando muda o minuto.
    fn update_clock(&self) {
        let (hour, min) = local_time();
        let text = format!("{hour:02}:{min:02}");
        if *self.clock.borrow() == text {
            return;
        }
        let part = match hour {
            5..=11 => "bom dia",
            12..=17 => "boa tarde",
            _ => "boa noite",
        };
        self.ui.set_greeting(format!("{part} · {text}").into());
        self.ui.set_clock(text.clone().into());
        *self.clock.borrow_mut() = text;
    }

    // ---- tema e listas --------------------------------------------------------

    fn app_info(&self, app: &AppRef) -> AppInfo {
        let a = app.borrow();
        let m = a.manifest();
        let settings = self.settings_of(m);
        let theme = self.host.theme_for(m);
        AppInfo {
            id: m.id.clone().into(),
            name: m.name.clone().into(),
            description: m.description.clone().into(),
            icon: render::image(sprites::get(&m.icon), theme, Mood::Idle, 1),
            enabled: self.host.config.borrow().is_enabled(&m.id),
            mood: a.mood().as_str().into(),
            theme: render::colors(theme),
            settings: ModelRc::new(VecModel::from(settings)),
        }
    }

    /// Grade do painel e lista da aba Apps.
    fn rebuild_lists(&self) {
        self.rebuild_grid();
        self.rebuild_settings();
    }

    /// Posiciona os apps ligados na grade (cada um onde a config diz).
    fn rebuild_grid(&self) {
        {
            let mut cfg = self.host.config.borrow_mut();
            if cfg.arrange() {
                cfg.save();
            }
        }
        let cfg = self.host.config.borrow();
        let (cols, rows) = (cfg.columns, cfg.rows);
        let rects = cfg.rects();
        let cells: Vec<GridCell> = self
            .registry
            .enabled()
            .iter()
            .zip(&rects)
            .map(|(a, r)| {
                let a = a.borrow();
                let m = a.manifest();
                GridCell {
                    id: m.id.clone().into(),
                    name: m.name.clone().into(),
                    col: r.x as i32,
                    row: r.y as i32,
                    w: r.w as i32,
                    h: r.h as i32,
                    theme: render::colors(self.host.theme_for(m)),
                }
            })
            .collect();
        let n = cells.len();
        let fits = |c: u32, r: u32| {
            (1..=layout::MAX_COLS).contains(&c) && (1..=layout::MAX_ROWS).contains(&r) && layout::settle(&rects, None, c, r).is_some()
        };
        self.ui.set_cols_down_ok(fits(cols.wrapping_sub(1), rows));
        self.ui.set_cols_up_ok(fits(cols + 1, rows));
        self.ui.set_rows_down_ok(fits(cols, rows.wrapping_sub(1)));
        self.ui.set_rows_up_ok(fits(cols, rows + 1));
        self.ui.set_grid_tag(format!("// {n} {} · {cols} × {rows}", if n == 1 { "APP" } else { "APPS" }).into());
        self.ui.set_columns(cols as i32);
        self.ui.set_rows(rows as i32);
        self.ui.set_cells(ModelRc::new(VecModel::from(cells)));
    }

    /// Grava um novo arranjo da grade (já validado por `layout::settle`).
    fn apply_layout(&self, cols: u32, rows: u32, rects: Vec<Rect>) {
        {
            let mut cfg = self.host.config.borrow_mut();
            cfg.columns = cols;
            cfg.rows = rows;
            let ids = cfg.enabled.clone();
            for (id, r) in ids.into_iter().zip(rects) {
                cfg.layout.insert(id, r);
            }
            cfg.save();
        }
        self.rebuild_grid();
    }

    /// Mostra (ou esconde) a prévia do arrasto.
    fn set_ghost(&self, drag: Option<&Drag>) {
        self.ui.set_ghost(match drag {
            Some(d) => Ghost {
                visible: true,
                id: d.id.clone().into(),
                col: d.target.x as i32,
                row: d.target.y as i32,
                w: d.target.w as i32,
                h: d.target.h as i32,
                ok: d.ok,
            },
            None => Ghost::default(),
        });
    }

    /// Grade e app (índice e retângulo) para os cálculos do modo de edição.
    fn grid_of(&self, id: &str) -> Option<(u32, u32, Vec<Rect>, usize)> {
        let cfg = self.host.config.borrow();
        let i = cfg.enabled.iter().position(|e| e == id)?;
        Some((cfg.columns, cfg.rows, cfg.rects(), i))
    }

    fn edit_begin(&self, id: &str, mode: &str, col: i32, row: i32) {
        let Some((_, _, rects, i)) = self.grid_of(id) else { return };
        let r = rects[i];
        let drag = Drag { id: id.into(), mode: mode.into(), off: (col - r.x as i32, row - r.y as i32), target: r, ok: true };
        self.set_ghost(Some(&drag));
        *self.drag.borrow_mut() = Some(drag);
    }

    fn edit_drag(&self, col: i32, row: i32) {
        let mut slot = self.drag.borrow_mut();
        let Some(d) = slot.as_mut() else { return };
        let Some((cols, rows, rects, i)) = self.grid_of(&d.id) else { return };
        let r = rects[i];
        let t = if d.mode == "move" {
            let x = (col - d.off.0).clamp(0, (cols - r.w) as i32) as u32;
            let y = (row - d.off.1).clamp(0, (rows - r.h) as i32) as u32;
            Rect::new(x, y, r.w, r.h)
        } else {
            let w = if d.mode.contains('r') { (col - r.x as i32 + 1).max(1) as u32 } else { r.w };
            let h = if d.mode.contains('b') { (row - r.y as i32 + 1).max(1) as u32 } else { r.h };
            Rect::new(r.x, r.y, w, h)
        };
        if t == d.target {
            return;
        }
        d.target = t;
        d.ok = layout::settle(&rects, Some((i, t)), cols, rows).is_some();
        self.set_ghost(Some(d));
    }

    fn edit_end(&self) {
        let Some(d) = self.drag.borrow_mut().take() else { return };
        self.set_ghost(None);
        if !d.ok {
            return;
        }
        let Some((cols, rows, rects, i)) = self.grid_of(&d.id) else { return };
        if rects[i] == d.target {
            return;
        }
        if let Some(out) = layout::settle(&rects, Some((i, d.target)), cols, rows) {
            self.apply_layout(cols, rows, out);
        }
    }

    /// Setas: move uma célula; com Shift, muda o tamanho.
    fn edit_key(&self, id: &str, dx: i32, dy: i32, resize: bool) {
        let Some((cols, rows, rects, i)) = self.grid_of(id) else { return };
        let r = rects[i];
        let (x, y, w, h) = if resize {
            (r.x as i32, r.y as i32, (r.w as i32 + dx).max(1), (r.h as i32 + dy).max(1))
        } else {
            (r.x as i32 + dx, r.y as i32 + dy, r.w as i32, r.h as i32)
        };
        if x < 0 || y < 0 {
            return;
        }
        let t = Rect::new(x as u32, y as u32, w as u32, h as u32);
        if let Some(out) = layout::settle(&rects, Some((i, t)), cols, rows) {
            self.apply_layout(cols, rows, out);
        }
    }

    /// Colunas e linhas da grade; recusa se algum app ficaria sem lugar.
    fn grid_size(&self, cols: i32, rows: i32) {
        if !(1..=layout::MAX_COLS as i32).contains(&cols) || !(1..=layout::MAX_ROWS as i32).contains(&rows) {
            return;
        }
        let (cols, rows) = (cols as u32, rows as u32);
        let rects = self.host.config.borrow().rects();
        if let Some(out) = layout::settle(&rects, None, cols, rows) {
            self.apply_layout(cols, rows, out);
        }
    }

    /// Lista da aba Apps, cada app com o arranjo e o formulário gerado do manifesto.
    fn rebuild_settings(&self) {
        let apps: Vec<AppInfo> = self.registry.all().iter().map(|a| self.app_info(a)).collect();
        self.ui.set_apps(ModelRc::new(VecModel::from(apps)));
    }

    fn settings_of(&self, m: &crate::core::manifest::Manifest) -> Vec<SettingRow> {
        let cfg = self.host.config.borrow();
        m.settings
            .iter()
            .map(|s| {
                let v = cfg.value(m, &s.key);
                let (label, unit) = split_unit(&s.label);
                SettingRow {
                    app: m.id.clone().into(),
                    key: s.key.clone().into(),
                    label: label.into(),
                    unit: unit.into(),
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
        // tema ou mascote podem ter mudado: ícones e bandeja
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
                h.anim_tick(); // desenha as mascotes já no primeiro quadro
                h.schedule_backdrop();
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
        ui.on_skin_chosen(move |i| {
            if let Some(h) = w.upgrade() {
                h.choose_skin(i.max(0) as usize);
            }
        });

        let w = Rc::downgrade(hub);
        ui.on_backdrop_resized(move |bw, bh| {
            if let Some(h) = w.upgrade() {
                if bw > 0.0 && bh > 0.0 && h.backdrop_size.replace((bw, bh)) != (bw, bh) {
                    h.schedule_backdrop();
                }
            }
        });

        let w = Rc::downgrade(hub);
        ui.on_grid_size(move |c, r| {
            if let Some(h) = w.upgrade() {
                h.grid_size(c, r);
            }
        });

        let w = Rc::downgrade(hub);
        ui.on_edit_begin(move |id, mode, c, r| {
            if let Some(h) = w.upgrade() {
                h.edit_begin(&id, &mode, c, r);
            }
        });

        let w = Rc::downgrade(hub);
        ui.on_edit_drag(move |c, r| {
            if let Some(h) = w.upgrade() {
                h.edit_drag(c, r);
            }
        });

        let w = Rc::downgrade(hub);
        ui.on_edit_end(move || {
            if let Some(h) = w.upgrade() {
                h.edit_end();
            }
        });

        let w = Rc::downgrade(hub);
        ui.on_edit_key(move |id, dx, dy, resize| {
            if let Some(h) = w.upgrade() {
                h.edit_key(&id, dx, dy, resize);
            }
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
        ui.on_start_resize(move |dir| {
            if let Some(h) = w.upgrade() {
                h.resize_until.set(Some(Instant::now() + RESIZE_QUIET));
                popup::drag_resize(h.ui.window(), &dir);
            }
        });

        let w = Rc::downgrade(hub);
        ui.on_toggle_pin(move || {
            if let Some(h) = w.upgrade() {
                let on = !h.pinned.get();
                h.set_pinned(on);
                // fixar pela bandeja: o menu rouba o foco e esconde o popup antes; reabre
                if on && !h.shown.get() {
                    h.show(None);
                }
            }
        });

        ui.on_quit({
            let w = Rc::downgrade(hub);
            move || {
                if let Some(h) = w.upgrade() {
                    h.remember_size();
                }
                let _ = slint::quit_event_loop();
            }
        });

        let w = Rc::downgrade(hub);
        ui.on_toggle_requested(move |x, y| {
            if let Some(h) = w.upgrade() {
                let anchor = (x != 0 || y != 0).then_some((x, y));
                h.toggle(anchor);
            }
        });

        let w = Rc::downgrade(hub);
        ui.on_show_requested(move |tab| {
            if let Some(h) = w.upgrade() {
                h.show_from_command(&tab);
            }
        });

        // popup: clicou fora (perdeu o foco) -> esconde
        let w = Rc::downgrade(hub);
        {
            use slint::winit_030::{EventResult, WinitWindowAccessor, winit::event::WindowEvent};
            ui.window().on_winit_window_event(move |_, ev| {
                if std::env::var_os("MINIUTILS_DEBUG").is_some() {
                    if let WindowEvent::Focused(f) = ev {
                        eprintln!("miniutils: foco {f}");
                    }
                }
                let Some(h) = w.upgrade() else { return EventResult::Propagate };
                match ev {
                    WindowEvent::CursorEntered { .. } => h.set_hovered(true),
                    WindowEvent::CursorLeft { .. } => h.set_hovered(false),
                    _ => {}
                }
                match ev {
                    WindowEvent::Focused(true) => {
                        h.had_focus.set(true);
                        h.resize_until.set(None);
                    }
                    // redimensionando pela borda: cada tamanho novo adia o fim do arrasto
                    WindowEvent::Resized(_) if h.resize_until.get().is_some() => {
                        h.resize_until.set(Some(Instant::now() + RESIZE_QUIET));
                    }
                    // fixado ou no meio de um redimensionamento: não some sozinho
                    WindowEvent::Focused(false) => {
                        if h.shown.get() && h.has_tray() && h.had_focus.get() && !h.pinned.get() && !h.resizing() {
                            h.hide();
                            h.focus_hidden_at.set(Some(Instant::now()));
                        }
                    }
                    _ => {}
                }
                EventResult::Propagate
            });
        }


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
        // animação: só com a janela visível; o relógio fica no hub (`set_hovered` muda o ritmo)
        let w = Rc::downgrade(hub);
        hub.anim.start(TimerMode::Repeated, FRAME_COLD, move || {
            if let Some(h) = w.upgrade() {
                if h.shown.get() {
                    h.anim_tick();
                }
            }
        });
        // os timers vivem enquanto o event loop roda
        std::mem::forget(data);
    }

    fn data_tick(&self) {
        self.update_clock();
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
        self.rebuild_settings(); // humor dos apps na aba Apps
        if let Some(tray) = self.tray.borrow().as_ref() {
            tray.update(sprites::get(&sprite), theme, mood, summary.clone());
        }
        *self.last_tray.borrow_mut() = (mood, summary, theme_key);
    }

    fn anim_tick(&self) {
        let f = self.frame.get().wrapping_add(1) % 600;
        self.frame.set(f);
        self.ui.global::<Anim>().set_frame(f as i32);
        // no painel, todos os apps ligados estão visíveis e animam
        if self.ui.get_current() == "panel" {
            for app in self.registry.enabled() {
                app.borrow_mut().tick(f, &self.ui);
            }
        }
    }
}

/// Aba pelo nome: "apps" são as configurações; o resto, o painel.
fn tab_name(tab: Option<&str>) -> SharedString {
    if tab == Some("apps") { "apps".into() } else { "panel".into() }
}

/// Rótulo de uma opção de `choice`: nome do tema ou da mascote, se for um.
fn option_label(key: &str) -> String {
    if key == "auto" {
        "Acompanha a janela".into()
    } else if let Some(t) = themes::THEMES.iter().find(|t| t.key == key) {
        t.label.into()
    } else if let Some(s) = sprites::SPRITES.iter().find(|s| s.key == key) {
        s.label.into()
    } else {
        key.into()
    }
}

/// Hora e minuto locais.
fn local_time() -> (i32, i32) {
    // SAFETY: time e localtime_r só escrevem no `tm` que passamos
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&now, &mut tm).is_null() {
            return (0, 0);
        }
        (tm.tm_hour, tm.tm_min)
    }
}

/// Separa a unidade do fim do rótulo: "Temperatura quente (°C)" -> ("Temperatura quente", "°C").
fn split_unit(label: &str) -> (String, String) {
    for unit in ["%", "°C"] {
        if let Some(base) = label.strip_suffix(&format!(" ({unit})")) {
            return (base.to_string(), unit.to_string());
        }
    }
    (label.to_string(), String::new())
}

/// O que o usuário fez num campo da aba Apps.
enum Input {
    Toggle,
    Int(i64),
    Choice(usize),
}
