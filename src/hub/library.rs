//! The Library window (`ui/library.slint`): shaders, app themes, window skins, backdrops
//! and mascots, listed the same way (`crate::library`).
//!
//! What it changes goes where the rest of the hub reads it: the Terminal's chain is its
//! `shaders` setting, a shader's adjustments are `[shader_params.<key>]` in the config, the
//! skin is the config's `skin`. Files it writes (copies, imports, removals) are read back by
//! the usual reload. The Mascots tab is its own module (`hub::mascots`), told when it comes
//! into view and when it leaves.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Image, ModelRc, SharedString, Timer, TimerMode, VecModel};

use super::mascots::MascotsTab;
use super::{Hub, with_hub};
use crate::library::postfx::{self, Colors, Frame, PostFx, Source};
use crate::library::{self, Item, Kind, Origin, shaders};
use crate::tr;
use crate::ui::{LibItem, LibLink, LibParam, LibraryUi, LibraryWindow};

/// The app whose `shaders` setting is the chain the Library edits.
const TERMINAL: &str = "cyber.terminal";
/// How often the open window looks for changed files and compile errors.
const POLL: Duration = Duration::from_millis(700);
/// Second click of "Remove": valid for this long.
const ARM: Duration = Duration::from_secs(6);
/// A dragged adjustment is applied this long after the last move (each change compiles).
const SETTLE: Duration = Duration::from_millis(60);

/// The window and what it shows.
pub(super) struct Library {
    win: LibraryWindow,
    /// The shader preview (`None` with the software renderer).
    fx: Option<PostFx>,
    tab: Kind,
    filter: String,
    /// Keys of the list, in its order (what Up/Down walk).
    listed: Vec<String>,
    selected: Option<String>,
    armed: Option<Instant>,
    poll: Timer,
    settle: Timer,
    /// What the list was made from: the shader folder, the compile errors, the looks.
    stamps: (u64, u64, u64),
    /// The theme, skin and backdrop folders as last read (`None` before the first look).
    looks: Option<u64>,
    mascots: Rc<RefCell<MascotsTab>>,
}

/// The Library's Mascots tab, if the window was created.
pub(super) fn mascots_tab() -> Option<Rc<RefCell<MascotsTab>>> {
    let mut out = None;
    with_hub(|h| out = h.library.borrow().as_ref().map(|l| l.mascots.clone()));
    out
}

/// With the Library borrowed (UI thread).
fn with_library(f: impl FnOnce(&Hub, &mut Library)) {
    with_hub(|h| {
        let mut lib = h.library.borrow_mut();
        if let Some(l) = lib.as_mut() {
            f(h, l);
        }
    });
}

impl Hub {
    pub(super) fn open_library(&self, kind: Option<Kind>, select: Option<String>) {
        if self.library.borrow().is_none() && !self.create_library() {
            return;
        }
        let win = {
            let mut lib = self.library.borrow_mut();
            let Some(l) = lib.as_mut() else { return };
            if let Some(kind) = kind {
                l.tab = kind;
            }
            if select.is_some() {
                l.selected = select;
            }
            l.win.clone_strong()
        };
        crate::skin::apply_current(&win);
        if let Err(e) = win.show() {
            log::warn!("couldn't show the Library: {e}");
            return;
        }
        if let Some(icon) = self.window_icon.borrow().clone() {
            crate::popup::set_icon_after_show(&win, icon);
        }
        crate::popup::label_after_show(&win, crate::popup::Kind::Detail);
        crate::popup::focus(win.window());
        win.invoke_focus_list();
        self.library_tab_changed();
        with_library(|_, l| {
            l.poll.start(TimerMode::Repeated, POLL, || with_library(|h, l| h.library_poll(l)));
        });
    }

    fn create_library(&self) -> bool {
        let win = match LibraryWindow::new() {
            Ok(w) => w,
            Err(e) => {
                log::warn!("couldn't create the Library window: {e}");
                return false;
            }
        };
        bind(&win);
        let fx = PostFx::attach(&win, |w: &LibraryWindow| {
            w.get_preview_shown().then(|| Frame {
                area: (w.get_preview_x(), w.get_preview_y(), w.get_preview_width(), w.get_preview_height()),
                cursor: Some((w.get_preview_cursor_x(), w.get_preview_cursor_y(), w.get_preview_cursor_w(), w.get_preview_cursor_h())),
                focused: w.get_focused(),
            })
        });
        if let Some(fx) = &fx {
            fx.set_colors(sample_colors());
        }
        win.global::<LibraryUi>().set_gl(fx.is_some());
        let mascots = MascotsTab::create(self.host.clone(), &win);
        *self.library.borrow_mut() = Some(Library {
            win,
            fx,
            tab: Kind::Shader,
            filter: String::new(),
            listed: Vec::new(),
            selected: None,
            armed: None,
            poll: Timer::default(),
            settle: Timer::default(),
            stamps: (0, 0, 0),
            looks: None,
            mascots,
        });
        true
    }

    /// A new tab (or the window opened): the list from scratch; the Mascots tab is told.
    fn library_tab_changed(&self) {
        let (tab, mascots) = {
            let mut lib = self.library.borrow_mut();
            let Some(l) = lib.as_mut() else { return };
            l.armed = None;
            l.win.global::<LibraryUi>().set_tab(l.tab.key().into());
            (l.tab, l.mascots.clone())
        };
        MascotsTab::shown(&mascots, tab == Kind::Mascot);
        with_library(|h, l| {
            h.library_list(l);
            h.library_detail(l);
        });
    }

    fn library_close(&self) {
        let mascots = {
            let lib = self.library.borrow();
            let Some(l) = lib.as_ref() else { return };
            l.poll.stop();
            let _ = l.win.hide();
            l.mascots.clone()
        };
        MascotsTab::shown(&mascots, false);
    }

    /// Changed files, new compile errors, reloaded looks: the list and the detail again.
    fn library_poll(&self, l: &mut Library) {
        if l.armed.is_some_and(|at| at.elapsed() >= ARM) {
            l.armed = None;
            l.win.global::<LibraryUi>().set_armed_remove(false);
        }
        if l.tab == Kind::Mascot {
            return;
        }
        // a theme, skin or backdrop file saved from an editor: the hub reads them again
        let looks = looks_stamp();
        if l.looks.replace(looks).is_some_and(|before| before != looks) {
            self.reload_looks();
            crate::skin::apply_current(&l.win);
            self.library_list(l);
            self.library_detail(l);
        }
        let stamps = (shaders::current_stamp(), postfx::errors_stamp(), crate::mascot::sprites::generation());
        if stamps != l.stamps {
            l.stamps = stamps;
            self.library_list(l);
            self.library_detail(l);
        }
    }

    /// The items of the tab that pass the search.
    fn library_list(&self, l: &mut Library) {
        let items = library::items(l.tab);
        let filter = l.filter.trim().to_lowercase();
        let shown: Vec<&Item> = items
            .iter()
            .filter(|i| filter.is_empty() || i.name.to_lowercase().contains(&filter) || i.key.contains(&filter))
            .collect();
        l.listed = shown.iter().map(|i| i.key.clone()).collect();
        if l.selected.as_ref().is_none_or(|s| !items.iter().any(|i| &i.key == s)) {
            l.selected = shown.first().map(|i| i.key.clone());
        }
        let rows: Vec<LibItem> = shown
            .iter()
            .map(|i| LibItem {
                key: i.key.clone().into(),
                name: i.name.clone().into(),
                origin: i.origin.label().into(),
                error: self.item_error(l.tab, i).unwrap_or_default().into(),
                in_use: self.in_use(l.tab, &i.key),
            })
            .collect();
        let g = l.win.global::<LibraryUi>();
        g.set_items(ModelRc::new(VecModel::from(rows)));
        g.set_selected(l.selected.clone().unwrap_or_default().into());
    }

    /// The item's load error, or for a shader its last compile error.
    fn item_error(&self, kind: Kind, item: &Item) -> Option<String> {
        item.error.clone().or_else(|| if kind == Kind::Shader { postfx::error(&item.key) } else { None })
    }

    fn in_use(&self, kind: Kind, key: &str) -> bool {
        let cfg = self.host.config.borrow();
        match kind {
            Kind::Shader => self.chain().iter().any(|k| k == key),
            Kind::Skin => cfg.skin == key,
            // an app's own theme, or the skin's for the apps that follow the window
            Kind::Theme => {
                cfg.tray_theme == key || self.registry.enabled().iter().any(|a| self.host.theme_for(a.borrow().manifest()).key == key)
            }
            Kind::Backdrop => crate::skin::get(&cfg.skin).backdrop == key,
            Kind::Mascot => false,
        }
    }

    /// The Terminal's shaders, in order.
    fn chain(&self) -> Vec<String> {
        let Some(app) = self.registry.find(TERMINAL) else { return Vec::new() };
        let a = app.borrow();
        self.host.config.borrow().list(a.manifest(), "shaders")
    }

    fn set_chain(&self, chain: Vec<String>) {
        let Some(app) = self.registry.find(TERMINAL) else { return };
        {
            let a = app.borrow();
            let value = toml::Value::Array(chain.into_iter().map(toml::Value::String).collect());
            self.host.set_setting(a.manifest(), "shaders", value);
        }
        app.borrow_mut().settings_changed(&self.ui);
    }

    /// The chosen item: what it is, its preview, its adjustments, the actions it allows.
    fn library_detail(&self, l: &mut Library) {
        let g = l.win.global::<LibraryUi>();
        let item = l.selected.as_ref().and_then(|k| library::items(l.tab).into_iter().find(|i| &i.key == k));
        let chain = self.chain();
        g.set_chain(ModelRc::new(VecModel::from(
            chain
                .iter()
                .map(|k| {
                    let found = shaders::find(k);
                    LibLink {
                        key: k.clone().into(),
                        name: found.as_ref().map_or_else(|| k.clone(), |s| s.name.clone()).into(),
                        bad: found.is_none() || postfx::error(k).is_some(),
                    }
                })
                .collect::<Vec<_>>(),
        )));
        let Some(item) = item else {
            g.set_name(SharedString::new());
            g.set_error(SharedString::new());
            g.set_params(ModelRc::default());
            g.set_swatches(ModelRc::default());
            g.set_use_label(SharedString::new());
            g.set_can_duplicate(false);
            g.set_can_open(false);
            g.set_can_remove(false);
            if let Some(fx) = &l.fx {
                fx.set(&l.win, Vec::new());
            }
            return;
        };
        let user = item.origin == Origin::User;
        g.set_name(item.name.clone().into());
        g.set_origin(item.origin.label().into());
        g.set_path(item.path.as_ref().map(|p| p.display().to_string()).unwrap_or_default().into());
        g.set_error(self.item_error(l.tab, &item).unwrap_or_default().into());
        g.set_can_open(item.path.is_some());
        g.set_can_remove(user);
        g.set_armed_remove(l.armed.is_some());
        g.set_can_duplicate(item.error.is_none() && (l.tab != Kind::Backdrop || user));
        g.set_credits(SharedString::new());
        g.set_description(SharedString::new());
        g.set_swatches(ModelRc::default());
        g.set_params(ModelRc::default());
        g.set_params_changed(false);
        g.set_use_label(SharedString::new());
        let rgb = |c: u32| slint::Color::from_rgb_u8((c >> 16) as u8, (c >> 8) as u8, c as u8);
        match l.tab {
            Kind::Shader => {
                let shader = shaders::find(&item.key);
                if let Some(s) = &shader {
                    let credits: Vec<&str> = [s.author.as_str(), s.license.as_str()].into_iter().filter(|t| !t.is_empty()).collect();
                    g.set_credits(credits.join(" · ").into());
                    g.set_description(s.description.clone().into());
                    let values = self.host.config.borrow().shader_params.get(&s.key).cloned().unwrap_or_default();
                    let params: Vec<LibParam> = s
                        .params
                        .iter()
                        .map(|p| {
                            let value = values.get(&p.name).and_then(value_f64).map_or(p.default, |v| v.clamp(p.min, p.max));
                            LibParam {
                                name: p.name.clone().into(),
                                label: p.label.clone().into(),
                                value: value as f32,
                                minimum: p.min as f32,
                                maximum: p.max as f32,
                                step: p.step as f32,
                                integer: p.integer,
                                changed: (value - p.default).abs() > f64::EPSILON,
                            }
                        })
                        .collect();
                    g.set_params_changed(params.iter().any(|p| p.changed));
                    g.set_params(ModelRc::new(VecModel::from(params)));
                    if let Some(fx) = &l.fx {
                        fx.set(&l.win, vec![Source { name: s.key.clone(), glsl: shaders::with_values(s, &values) }]);
                    }
                } else if let Some(fx) = &l.fx {
                    fx.set(&l.win, Vec::new());
                }
                if shader.is_some() {
                    let on = chain.contains(&item.key);
                    g.set_use_label(if on { tr!("Remove from the Terminal") } else { tr!("Add to the Terminal") }.into());
                }
            }
            Kind::Theme => {
                if let Some(t) = crate::mascot::themes::find(&item.key) {
                    let colors = [t.fur, t.visor, t.ear, t.blush, t.ok, t.warn, t.alarm, t.sleep, t.bg0, t.bg1, t.text, t.sub, t.muted];
                    g.set_swatches(ModelRc::new(VecModel::from(colors.map(rgb).to_vec())));
                }
            }
            Kind::Skin => {
                if let Some(s) = crate::skin::find(&item.key) {
                    let colors = [s.bg, s.panel, s.surface, s.edge, s.field, s.accent, s.accent2, s.ink, s.sub, s.dim, s.bad];
                    g.set_swatches(ModelRc::new(VecModel::from(colors.map(rgb).to_vec())));
                    if self.host.config.borrow().skin != item.key {
                        g.set_use_label(tr!("Use as the window skin").into());
                    }
                }
            }
            Kind::Backdrop => {
                if let Some(b) = crate::skin::backdrop::find(&item.key) {
                    let skin = crate::skin::current().unwrap_or_else(|| crate::skin::BUILTIN[0].clone());
                    let pixels = crate::skin::backdrop::render(b, &skin, 480.0, 160.0, 1.0);
                    g.set_picture(Image::from_rgb8(pixels));
                }
            }
            Kind::Mascot => {}
        }
    }

    fn library_select(&self, l: &mut Library, key: String) {
        if l.selected.as_deref() == Some(key.as_str()) {
            return;
        }
        l.selected = Some(key);
        l.armed = None;
        l.win.global::<LibraryUi>().set_selected(l.selected.clone().unwrap_or_default().into());
        l.win.global::<LibraryUi>().set_status(SharedString::new());
        self.library_detail(l);
    }

    fn library_step(&self, l: &mut Library, by: i32) {
        if l.listed.is_empty() {
            return;
        }
        let at = l.selected.as_ref().and_then(|s| l.listed.iter().position(|k| k == s));
        let next = match at {
            Some(i) => (i as i32 + by).clamp(0, l.listed.len() as i32 - 1) as usize,
            None => 0,
        };
        let key = l.listed[next].clone();
        self.library_select(l, key);
    }

    /// The main action of the chosen item: a shader in or out of the Terminal, a skin used.
    fn library_use(&self, l: &mut Library) {
        let Some(key) = l.selected.clone() else { return };
        match l.tab {
            Kind::Shader => {
                let mut chain = self.chain();
                match chain.iter().position(|k| *k == key) {
                    Some(i) => {
                        chain.remove(i);
                    }
                    None => chain.push(key),
                }
                self.set_chain(chain);
            }
            Kind::Skin => {
                self.choose_skin(crate::skin::index_of(&key));
                crate::skin::apply_current(&l.win);
            }
            _ => {}
        }
        self.library_list(l);
        self.library_detail(l);
    }

    fn library_chain_move(&self, l: &mut Library, i: usize, by: i32) {
        let mut chain = self.chain();
        let j = i as i32 + by;
        if i >= chain.len() || j < 0 || j as usize >= chain.len() {
            return;
        }
        chain.swap(i, j as usize);
        self.set_chain(chain);
        self.library_detail(l);
    }

    fn library_chain_remove(&self, l: &mut Library, i: usize) {
        let mut chain = self.chain();
        if i < chain.len() {
            chain.remove(i);
            self.set_chain(chain);
        }
        self.library_list(l);
        self.library_detail(l);
    }

    /// A new value for an adjustment of the chosen shader (`None`: back to the shader's).
    fn library_param(&self, l: &mut Library, name: Option<&str>, value: Option<f64>) {
        let Some(key) = l.selected.clone() else { return };
        {
            let mut cfg = self.host.config.borrow_mut();
            match name {
                // all of them back
                None => {
                    cfg.shader_params.remove(&key);
                }
                Some(name) => {
                    let table = cfg.shader_params.entry(key.clone()).or_default();
                    match value {
                        Some(v) => {
                            table.insert(name.into(), toml::Value::Float(v));
                        }
                        None => {
                            table.remove(name);
                        }
                    }
                    if table.is_empty() {
                        cfg.shader_params.remove(&key);
                    }
                }
            }
            cfg.save();
        }
        self.library_detail(l);
        // the Terminal compiles again too, once the slider settles
        if self.chain().contains(&key) {
            l.settle.start(TimerMode::SingleShot, SETTLE, || {
                with_hub(|h| {
                    if let Some(app) = h.registry.find(TERMINAL) {
                        app.borrow_mut().settings_changed(&h.ui);
                    }
                })
            });
        }
    }

    fn library_duplicate(&self, l: &mut Library) {
        let Some(key) = l.selected.clone() else { return };
        match library::duplicate(l.tab, &key) {
            Ok(new) => {
                self.library_say(l, tr!("Copied as \"{}\": open its file to edit it.", new), false);
                self.after_files_changed(l);
                self.library_select(l, new);
            }
            Err(e) => self.library_say(l, e, true),
        }
    }

    fn library_remove(&self, l: &mut Library) {
        let Some(key) = l.selected.clone() else { return };
        if !l.armed.is_some_and(|at| at.elapsed() < ARM) {
            l.armed = Some(Instant::now());
            l.win.global::<LibraryUi>().set_armed_remove(true);
            return;
        }
        l.armed = None;
        l.win.global::<LibraryUi>().set_armed_remove(false);
        match library::remove(l.tab, &key) {
            Ok(()) => {
                if l.tab == Kind::Shader {
                    let mut chain = self.chain();
                    if chain.contains(&key) {
                        chain.retain(|k| *k != key);
                        self.set_chain(chain);
                    }
                    self.host.config.borrow_mut().shader_params.remove(&key);
                }
                self.library_say(l, tr!("Removed.").into(), false);
                l.selected = None;
                self.after_files_changed(l);
            }
            Err(e) => self.library_say(l, e, true),
        }
    }

    /// Asks the system for files in a thread; the copy happens back here.
    fn library_import(&self, l: &Library) {
        let kind = l.tab;
        let (filter, extensions): (&str, &[&str]) = match kind {
            Kind::Shader => ("GLSL", &["glsl", "frag"]),
            _ => ("TOML", &["toml"]),
        };
        let spawned = std::thread::Builder::new().name("library-import".into()).spawn(move || {
            let Some(files) = rfd::FileDialog::new().set_title(tr!("Import into the Library")).add_filter(filter, extensions).pick_files() else {
                return;
            };
            let _ = slint::invoke_from_event_loop(move || {
                with_library(|h, l| match library::import(kind, &files) {
                    Ok(keys) => {
                        h.library_say(l, tr!("Imported: {}", keys.join(", ")), false);
                        h.after_files_changed(l);
                        if let Some(first) = keys.into_iter().next() {
                            h.library_select(l, first);
                        }
                    }
                    Err(e) => h.library_say(l, e, true),
                })
            });
        });
        if let Err(e) = spawned {
            log::warn!("library: no thread for the file dialog ({e})");
        }
    }

    fn library_open_file(&self, l: &Library) {
        let Some(path) = self.selected_path(l) else { return };
        if !crate::core::exec::open_file(&path) {
            log::warn!("could not open {}", path.display());
        }
    }

    fn library_open_folder(&self, l: &Library) {
        let dir = match l.tab {
            Kind::Mascot => crate::skin::user::dir().join(crate::mascot::pack::DIR),
            kind => kind.dir(),
        };
        if let Err(e) = std::fs::create_dir_all(&dir) {
            log::warn!("could not create {}: {e}", dir.display());
        } else if !crate::core::exec::open_dir(&dir) {
            log::warn!("could not open {}", dir.display());
        }
    }

    fn selected_path(&self, l: &Library) -> Option<PathBuf> {
        let key = l.selected.as_ref()?;
        library::items(l.tab).into_iter().find(|i| &i.key == key)?.path
    }

    /// Themes, skins and backdrops are read by the hub's reload; shaders by their stamp.
    fn after_files_changed(&self, l: &mut Library) {
        if l.tab != Kind::Shader {
            self.reload_looks();
            crate::skin::apply_current(&l.win);
            // already read: the next look at the folders has nothing new
            l.looks = Some(looks_stamp());
        }
        self.library_list(l);
        self.library_detail(l);
    }

    fn library_say(&self, l: &Library, text: String, bad: bool) {
        let g = l.win.global::<LibraryUi>();
        g.set_status(text.into());
        g.set_status_bad(bad);
    }
}

/// The theme, skin and backdrop folders together (`shaders::stamp` of each).
fn looks_stamp() -> u64 {
    [Kind::Theme, Kind::Skin, Kind::Backdrop].iter().fold(0u64, |acc, k| acc.rotate_left(7) ^ shaders::stamp(&k.dir()))
}

/// A float or integer TOML value as f64.
fn value_f64(v: &toml::Value) -> Option<f64> {
    v.as_float().or_else(|| v.as_integer().map(|i| i as f64))
}

/// The colors of the preview's sample screen (`SampleScreen` in ui/library.slint).
fn sample_colors() -> Colors {
    let c = |v: u32| [((v >> 16) & 0xff) as f32 / 255.0, ((v >> 8) & 0xff) as f32 / 255.0, (v & 0xff) as f32 / 255.0];
    let ansi = [
        0x000000, 0xff1744, 0x39ff14, 0xfff200, 0x7a5cff, 0xff2bd6, 0x3df2ff, 0xc9bee6, //
        0x5a4e78, 0xff5c7a, 0x6dff52, 0xfff666, 0x9d85ff, 0xff6be3, 0x7af7ff, 0xffffff,
    ]
    .map(c);
    Colors::new(c(0x0c0c14), c(0xc9bee6), c(0x3df2ff), ansi)
}

fn bind(win: &LibraryWindow) {
    let g = win.global::<LibraryUi>();
    g.on_tab_chosen(|key| {
        let Some(kind) = Kind::from_key(&key) else { return };
        with_library(|_, l| {
            l.tab = kind;
            l.filter.clear();
            l.selected = None;
        });
        // tells the Mascots app whether its tab is the one in view
        with_hub(|h| h.library_tab_changed());
    });
    g.on_select(|key| with_library(|h, l| h.library_select(l, key.to_string())));
    g.on_step(|by| with_library(|h, l| h.library_step(l, by)));
    g.on_filter_edited(|text| {
        with_library(|h, l| {
            l.filter = text.to_string();
            h.library_list(l);
            h.library_detail(l);
        })
    });
    g.on_use(|| with_library(|h, l| h.library_use(l)));
    g.on_chain_move(|i, by| with_library(|h, l| h.library_chain_move(l, i.max(0) as usize, by)));
    g.on_chain_remove(|i| with_library(|h, l| h.library_chain_remove(l, i.max(0) as usize)));
    g.on_param_edited(|name, v| with_library(|h, l| h.library_param(l, Some(&name), Some(f64::from(v)))));
    g.on_param_reset(|name| with_library(|h, l| h.library_param(l, Some(&name), None)));
    g.on_params_reset(|| with_library(|h, l| h.library_param(l, None, None)));
    g.on_duplicate(|| with_library(|h, l| h.library_duplicate(l)));
    g.on_open_file(|| with_library(|h, l| h.library_open_file(l)));
    g.on_open_folder(|| with_library(|h, l| h.library_open_folder(l)));
    g.on_remove(|| with_library(|h, l| h.library_remove(l)));
    g.on_import(|| with_library(|h, l| h.library_import(l)));
    g.on_close(|| with_hub(|h| h.library_close()));
    let w = win.as_weak();
    g.on_start_move(move || {
        if let Some(win) = w.upgrade() {
            crate::popup::drag_move(&win);
        }
    });
    let w = win.as_weak();
    g.on_start_resize(move |dir| {
        if let Some(win) = w.upgrade() {
            crate::popup::drag_resize(&win, &dir);
        }
    });
    win.window().on_close_requested(|| {
        with_hub(|h| h.library_close());
        slint::CloseRequestResponse::HideWindow
    });
}
