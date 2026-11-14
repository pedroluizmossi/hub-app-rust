//! Mascots mini-app: the person's mascots in one place. Both kinds (code mascots and PNG
//! sprites) are handled the same way here, with the hub's own readers:
//!
//! - the card counts them, shows the person's own ones and imports a folder or a `.zip`
//!   (`mascot::import`), chosen in the system's dialog (`rfd`, in a thread);
//! - the manager window lists every mascot and edits the chosen one's file
//!   (`mascot::edit`): name, the animation of each mood (PNG), the `[palette]` colors (PNG),
//!   the walking stops (`extras`); shows what it has (`mascot::check`); puts it to use (an
//!   app's mascot setting, a page, the tray) and removes the person's own ones.
//!
//! After any change the hub rereads the appearance folder (`hub::request_plugin_reload`);
//! the new mascot list arrives here as a new `sprites::generation`.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, ModelRc, VecModel};

use super::kit::HeaderMascot;
use crate::core::bus::Event;
use crate::core::manifest::Manifest;
use crate::core::miniapp::{Host, MiniApp, Report, mascot_targets};
use crate::mascot::anim::Reaction;
use crate::mascot::check;
use crate::mascot::sprites::{self, Sprite};
use crate::mascot::walkers::{self, Walker};
use crate::mascot::{Mood, edit, file, pack, render};
use crate::tr;
use crate::ui::{Anim, HubWindow, MascotItem, MascotsApp, MascotsManager, MascotsWindow, PaletteRow, StopRow};

/// The color roles a PNG color can take, in the order of the "role" list (after "keeps its
/// own color").
const ROLES: &[u8] = b"KPSHMCEWBTFDA";
/// Moods and reactions of the "Animations" list, in its order.
const MAPPED: [&str; 7] = ["sleep", "idle", "happy", "busy", "alarm", "celebrate", "surprised"];
/// Second click of "Remove": valid for this long.
const ARM: Duration = Duration::from_secs(6);
/// Most person's mascots on the card.
const GALLERY: usize = 10;

pub struct MascotsMini {
    host: Rc<Host>,
    manifest: Manifest,
    face: HeaderMascot,
    /// `sprites::generation` last shown (a new one: the list changed).
    generation: Option<u64>,
    status: String,
    status_bad: bool,
    busy: bool,
    manager: Option<Manager>,
}

/// The manager window and what it shows.
struct Manager {
    win: MascotsWindow,
    open: bool,
    /// The chosen mascot's key.
    selected: Option<String>,
    /// Preview: 0..5 the moods (calm, happy, busy, alarm, sleeping), 5 walking.
    mode: usize,
    walker: Option<Walker>,
    armed: Option<Instant>,
    /// What "Use in" lists, in order (the first entry is "Choose…").
    targets: Vec<Place>,
}

/// Where "Use in" puts a mascot.
#[derive(Clone)]
enum Place {
    Tray,
    /// Index and name (checked again when used: the pages may change meanwhile).
    Page(usize, String),
    App { app: String, setting: String },
}

/// Moods of the preview, in the list's order.
const PREVIEW: [Mood; 5] = [Mood::Idle, Mood::Happy, Mood::Busy, Mood::Alarm, Mood::Sleep];

impl MascotsMini {
    pub fn create(host: Rc<Host>, ui: &HubWindow) -> Rc<RefCell<Self>> {
        let manifest = Manifest::parse(include_str!("../../miniapps/mascots/manifest.toml")).expect("invalid Mascots manifest");
        let app = Rc::new(RefCell::new(MascotsMini {
            host,
            manifest,
            face: HeaderMascot::new(41),
            generation: None,
            status: String::new(),
            status_bad: false,
            busy: false,
            manager: None,
        }));
        Self::bind(&app, ui);
        app
    }

    fn bind(app: &Rc<RefCell<Self>>, ui: &HubWindow) {
        let g = ui.global::<MascotsApp>();
        let a = Rc::downgrade(app);
        g.on_import_folder(move || {
            if let Some(a) = a.upgrade() {
                a.borrow_mut().pick(false);
            }
        });
        let a = Rc::downgrade(app);
        g.on_import_zip(move || {
            if let Some(a) = a.upgrade() {
                a.borrow_mut().pick(true);
            }
        });
        let (a, w) = (Rc::downgrade(app), ui.as_weak());
        g.on_manage(move || {
            if let (Some(a), Some(ui)) = (a.upgrade(), w.upgrade()) {
                Self::open_manager(&a, &ui);
            }
        });
        let (a, w) = (Rc::downgrade(app), ui.as_weak());
        g.on_mascot_poked(move || {
            if let (Some(a), Some(ui)) = (a.upgrade(), w.upgrade()) {
                let mut a = a.borrow_mut();
                let calm = a.host.config.borrow().reduce_motion;
                if a.face.poke(Mood::Idle, calm) {
                    a.push_face(&ui);
                }
            }
        });
    }

    fn theme(&self) -> &'static crate::mascot::themes::Theme {
        self.host.theme_for(&self.manifest)
    }

    // ---- import ---------------------------------------------------------------------

    /// Asks the system for a folder (or a `.zip`) in a thread and imports it there; the
    /// result comes back through `on_report`.
    fn pick(&mut self, zip: bool) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.push_busy();
        let reporter = self.host.reporter(&self.manifest);
        let failed = reporter.clone();
        let appearance = crate::skin::user::dir();
        let spawned = std::thread::Builder::new().name("mascots-import".into()).spawn(move || {
            let chosen = if zip {
                rfd::FileDialog::new().set_title(tr!("Import mascots from a .zip")).add_filter("zip", &["zip", "ZIP"]).pick_file()
            } else {
                rfd::FileDialog::new().set_title(tr!("Import mascots from a folder")).pick_folder()
            };
            // cancelled: nothing to say
            let Some(path) = chosen else { return reporter.done() };
            match crate::mascot::import::import(&path, &appearance) {
                Ok(lines) => reporter.output(lines.join("\n")),
                Err(e) => reporter.error(e),
            }
        });
        if let Err(e) = spawned {
            failed.error(tr!("No thread for the import: {}", e));
        }
    }

    fn push_busy(&self) {
        if let Some(ui) = crate::hub::ui_handle() {
            ui.global::<MascotsApp>().set_busy(self.busy);
        }
        if let Some(m) = &self.manager {
            m.win.global::<MascotsManager>().set_busy(self.busy);
        }
    }

    // ---- card -----------------------------------------------------------------------

    fn push_face(&mut self, ui: &HubWindow) {
        let sprite = sprites::get(&self.mascot_key());
        self.face.refresh(sprite, self.theme(), Mood::Idle);
        let g = ui.global::<MascotsApp>();
        g.set_mascot(self.face.image());
        g.set_reaction(self.face.reaction());
    }

    /// Counts, kinds and the person's own mascots.
    fn push_card(&self, ui: &HubWindow, frame: u32) {
        let all = sprites::all();
        let own: Vec<&'static Sprite> = all.iter().copied().filter(|s| !builtin(s)).collect();
        let walking = all.iter().filter(|s| walkers::walks(s)).count();
        let png = own.iter().filter(|s| pack::art(s).is_some()).count();
        let g = ui.global::<MascotsApp>();
        g.set_counts(tr!("{} mascots · {} walk", all.len(), walking).into());
        g.set_kinds(tr!("{} of your own: {} in code · {} PNG sprites", own.len(), own.len() - png, png).into());
        let theme = self.theme();
        let gallery: Vec<slint::Image> = own.iter().rev().take(GALLERY).map(|s| render::image(s, theme, Mood::Idle, frame)).collect();
        g.set_gallery(ModelRc::new(VecModel::from(gallery)));
        g.set_status(self.status.as_str().into());
        g.set_status_bad(self.status_bad);
        g.set_busy(self.busy);
    }

    // ---- manager window ---------------------------------------------------------------

    fn open_manager(app: &Rc<RefCell<Self>>, ui: &HubWindow) {
        let mut me = app.borrow_mut();
        if me.manager.is_none() {
            match MascotsWindow::new() {
                Ok(win) => {
                    Self::bind_manager(&win, app, ui);
                    me.manager = Some(Manager { win, open: false, selected: None, mode: 0, walker: None, armed: None, targets: Vec::new() });
                    me.style_manager();
                }
                Err(e) => {
                    log::warn!("couldn't create the mascots window: {e}");
                    return;
                }
            }
        }
        if let Some(m) = me.manager.as_mut() {
            m.open = true;
        }
        me.fill_manager();
        let win = me.manager.as_ref().map(|m| m.win.clone_strong());
        drop(me);
        if let Some(win) = win {
            if let Err(e) = win.show() {
                log::warn!("couldn't show the mascots window: {e}");
            }
            crate::popup::focus(win.window());
        }
    }

    fn bind_manager(win: &MascotsWindow, app: &Rc<RefCell<Self>>, ui: &HubWindow) {
        let g = win.global::<MascotsManager>();
        // an action on the open window, with the app borrowed
        let on = |run: fn(&mut MascotsMini, i32)| {
            let a = Rc::downgrade(app);
            move |i: i32| {
                if let Some(a) = a.upgrade() {
                    run(&mut a.borrow_mut(), i);
                }
            }
        };
        g.on_select(on(|m, i| m.select(i)));
        g.on_preview_mode(on(|m, i| {
            if let Some(mg) = m.manager.as_mut() {
                mg.mode = i.max(0) as usize;
            }
            m.push_preview(0);
        }));
        g.on_stop_toggled(on(|m, i| m.toggle_stop(i)));
        g.on_use_in(on(|m, i| m.use_in(i)));
        let a = Rc::downgrade(app);
        g.on_map(move |which, k| {
            if let Some(a) = a.upgrade() {
                a.borrow_mut().map(which, k);
            }
        });
        let a = Rc::downgrade(app);
        g.on_palette_role(move |row, k| {
            if let Some(a) = a.upgrade() {
                a.borrow_mut().palette_role(row, k);
            }
        });
        let a = Rc::downgrade(app);
        g.on_rename(move |name| {
            if let Some(a) = a.upgrade() {
                a.borrow_mut().rename(&name);
            }
        });
        let a = Rc::downgrade(app);
        g.on_remove(move || {
            if let Some(a) = a.upgrade() {
                a.borrow_mut().remove();
            }
        });
        let a = Rc::downgrade(app);
        g.on_open_folder(move || {
            if let Some(a) = a.upgrade() {
                a.borrow().open_folder();
            }
        });
        let a = Rc::downgrade(app);
        g.on_import_folder(move || {
            if let Some(a) = a.upgrade() {
                a.borrow_mut().pick(false);
            }
        });
        let a = Rc::downgrade(app);
        g.on_import_zip(move || {
            if let Some(a) = a.upgrade() {
                a.borrow_mut().pick(true);
            }
        });
        let (a, w) = (Rc::downgrade(app), win.as_weak());
        g.on_close(move || {
            if let (Some(a), Some(win)) = (a.upgrade(), w.upgrade()) {
                let _ = win.hide();
                if let Some(m) = a.borrow_mut().manager.as_mut() {
                    m.open = false;
                }
            }
        });
        let w = win.as_weak();
        g.on_start_move(move || {
            if let Some(win) = w.upgrade() {
                crate::popup::drag_move(win.window());
            }
        });
        let w = win.as_weak();
        g.on_start_resize(move |dir| {
            if let Some(win) = w.upgrade() {
                crate::popup::drag_resize(win.window(), &dir);
            }
        });
        let a = Rc::downgrade(app);
        win.window().on_close_requested(move || {
            if let Some(a) = a.upgrade()
                && let Some(m) = a.borrow_mut().manager.as_mut()
            {
                m.open = false;
            }
            slint::CloseRequestResponse::HideWindow
        });
        let _ = ui;
    }

    /// The window follows the hub's look and the app's theme.
    fn style_manager(&self) {
        let Some(m) = &self.manager else { return };
        crate::skin::apply_current(&m.win);
        m.win.global::<Anim>().set_calm(true);
        m.win.global::<MascotsManager>().set_theme(render::colors(self.theme()));
    }

    /// The list and the chosen one's card, from scratch.
    fn fill_manager(&mut self) {
        let theme = self.theme();
        let Some(m) = self.manager.as_mut() else { return };
        let store = store_keys();
        let all = sprites::all();
        let items: Vec<MascotItem> = all
            .iter()
            .map(|s| MascotItem {
                key: s.key.into(),
                name: s.name().into(),
                kind: if pack::art(s).is_some() { "sprite" } else { "code" }.into(),
                origin: if builtin(s) {
                    tr!("built-in")
                } else if store.iter().any(|k| k == s.key) {
                    tr!("store")
                } else {
                    tr!("yours")
                }
                .into(),
                icon: render::image(s, theme, Mood::Idle, 1),
                walks: walkers::walks(s),
                themed: check::of_sprite(s).themed,
            })
            .collect();
        let g = m.win.global::<MascotsManager>();
        g.set_items(ModelRc::new(VecModel::from(items)));
        // the chosen one may have left (removed, folder changed)
        let index = m.selected.as_deref().and_then(|k| all.iter().position(|s| s.key == k));
        if index.is_none() {
            m.selected = None;
        }
        g.set_selected(index.map_or(-1, |i| i as i32));
        self.fill_card();
    }

    fn select(&mut self, i: i32) {
        let Some(s) = usize::try_from(i).ok().and_then(|i| sprites::all().get(i).copied()) else { return };
        if let Some(m) = self.manager.as_mut() {
            m.selected = Some(s.key.to_string());
            m.mode = 0;
            m.armed = None;
            m.win.global::<MascotsManager>().set_selected(i);
            m.win.global::<MascotsManager>().set_status("".into());
        }
        self.fill_card();
    }

    fn chosen(&self) -> Option<&'static Sprite> {
        sprites::find(self.manager.as_ref()?.selected.as_deref()?)
    }

    /// The chosen mascot's card: name, report, animations, colors, stops, places.
    fn fill_card(&mut self) {
        let theme = self.theme();
        let Some(s) = self.chosen() else {
            // none chosen (removed, or the folder changed): no card left on screen
            if let Some(m) = self.manager.as_mut() {
                m.walker = None;
                let g = m.win.global::<MascotsManager>();
                g.set_selected(-1);
                g.set_editable(false);
                g.set_can_map(false);
                g.set_can_palette(false);
                g.set_can_stops(false);
            }
            return;
        };
        let report = check::of_sprite(s);
        let art = pack::art(s);
        // a mascot from the store isn't changed here: an update would undo the change
        let from_store = store_keys().iter().any(|k| k == s.key);
        let Some(m) = self.manager.as_mut() else { return };
        let g = m.win.global::<MascotsManager>();
        g.set_name(s.name().into());
        g.set_key(s.key.into());
        g.set_editable(!builtin(s) && !from_store);
        g.set_from_store(from_store);
        g.set_report(report.text().into());
        g.set_armed_remove(false);
        m.walker = walkers::walker(s, theme);
        let mut modes: Vec<slint::SharedString> =
            [tr!("Calm"), tr!("Happy"), tr!("Busy"), tr!("Alarm"), tr!("Sleeping")].into_iter().map(Into::into).collect();
        if m.walker.is_some() {
            modes.push(tr!("Walking").into());
        }
        if m.mode >= modes.len() {
            m.mode = 0;
        }
        g.set_preview_modes(ModelRc::new(VecModel::from(modes)));
        g.set_preview_index(m.mode as i32);

        // animations of each mood and reaction (PNG sprite)
        let editable = !builtin(s) && !from_store;
        g.set_can_map(art.is_some() && editable);
        if let Some(art) = art {
            let names = art.declared_names();
            let mut options: Vec<slint::SharedString> = vec![tr!("(usual name)").into()];
            options.extend(names.iter().map(|n| n.as_str().into()));
            let (moods, reactions) = art.mapping();
            let current: Vec<String> = moods.into_iter().chain(reactions.into_iter().map(Option::unwrap_or_default)).collect();
            let mapping: Vec<i32> =
                current.iter().map(|n| names.iter().position(|x| x == n).map_or(0, |i| i as i32 + 1)).collect();
            g.set_anim_options(ModelRc::new(VecModel::from(options)));
            g.set_mapping(ModelRc::new(VecModel::from(mapping)));
        }

        // [palette] (PNG sprite): the listed colors first, then the loose ones
        g.set_can_palette(art.is_some() && editable);
        if let Some(art) = art {
            let mut rows: Vec<PaletteRow> = art
                .palette()
                .iter()
                .map(|(c, r)| palette_row(*c, ROLES.iter().position(|x| x == r).map_or(0, |i| i as i32 + 1)))
                .collect();
            rows.extend(art.loose_colors(24).into_iter().map(|c| palette_row(c, 0)));
            let roles: Vec<slint::SharedString> = std::iter::once(tr!("keeps its own color").to_string())
                .chain(ROLES.iter().map(|r| format!("{} · {}", *r as char, role_name(*r))))
                .map(Into::into)
                .collect();
            g.set_roles(ModelRc::new(VecModel::from(roles)));
            g.set_palette(ModelRc::new(VecModel::from(rows)));
        }

        // walking stops: the animations that aren't moving nor `idle`
        let stops: Vec<StopRow> = match (&report.walk, art) {
            (None, _) => Vec::new(),
            (Some(w), Some(art)) => art
                .declared_names()
                .iter()
                .filter(|n| !moving(n) && *n != "idle")
                .map(|n| StopRow { name: n.as_str().into(), on: w.stops.contains(n) })
                .collect(),
            (Some(_), None) => {
                let extras = file::extras(s.key);
                let mut names: Vec<String> =
                    crate::mascot::clips::of(s.key).map(|c| c.keys().cloned().collect()).unwrap_or_default();
                names.sort();
                names
                    .into_iter()
                    .filter(|n| !moving(n) && n != "idle")
                    .map(|n| StopRow { on: extras.contains(&n), name: n.into() })
                    .collect()
            }
        };
        g.set_can_stops(editable && !stops.is_empty());
        g.set_stops(ModelRc::new(VecModel::from(stops)));

        // places: the tray, the pages and the enabled apps' mascot settings
        let cfg = self.host.config.borrow();
        let mut labels: Vec<slint::SharedString> = vec![tr!("Choose…").into(), tr!("Tray icon").into()];
        let mut places = vec![Place::Tray, Place::Tray];
        for (i, p) in cfg.pages.iter().enumerate() {
            labels.push(tr!("Page: {}", p.name).into());
            places.push(Place::Page(i, p.name.clone()));
        }
        let targets = mascot_targets();
        for t in &targets {
            let several = targets.iter().filter(|x| x.app == t.app).count() > 1;
            labels.push(if several { format!("{} · {}", t.name, t.label) } else { t.name.clone() }.into());
            places.push(Place::App { app: t.app.clone(), setting: t.setting.clone() });
        }
        drop(cfg);
        m.targets = places;
        g.set_targets(ModelRc::new(VecModel::from(labels)));
        self.push_preview(0);
    }

    /// The preview image: the chosen mood, or walking.
    fn push_preview(&self, frame: u32) {
        let (Some(m), Some(s)) = (&self.manager, self.chosen()) else { return };
        let img = match (m.mode, &m.walker) {
            (5, Some(w)) => {
                let (right, _) = &w.frames[w.script.walk];
                right[(frame / 2) as usize % right.len().max(1)].clone()
            }
            (i, _) => render::image(s, self.theme(), PREVIEW[i.min(4)], frame),
        };
        m.win.global::<MascotsManager>().set_preview(img);
    }

    fn say(&self, text: String, bad: bool) {
        if let Some(m) = &self.manager {
            let g = m.win.global::<MascotsManager>();
            g.set_status(text.into());
            g.set_status_bad(bad);
        }
    }

    /// Runs a change on the chosen mascot's file and asks the hub to reread the mascots.
    fn change(&mut self, what: impl FnOnce(&edit::Target) -> Result<(), String>, done: &str) {
        let Some(s) = self.chosen() else { return };
        let result = edit::target(s, &crate::skin::user::dir())
            .and_then(|t| t.ok_or_else(|| tr!("a built-in mascot can't be changed").to_string()))
            .and_then(|t| what(&t));
        match result {
            Ok(()) => {
                self.say(done.to_string(), false);
                crate::hub::request_plugin_reload();
            }
            Err(e) => {
                self.say(e, true);
                // the controls go back to what the file says
                self.fill_card();
            }
        }
    }

    fn rename(&mut self, name: &str) {
        self.change(|t| edit::rename(t, name), tr!("Name saved."));
    }

    fn map(&mut self, which: i32, k: i32) {
        let Some(which) = usize::try_from(which).ok().and_then(|i| MAPPED.get(i).copied()) else { return };
        let Some(art) = self.chosen().and_then(pack::art) else { return };
        let anim = usize::try_from(k - 1).ok().and_then(|i| art.declared_names().get(i).cloned());
        self.change(|t| edit::set_mood(t, which, anim.as_deref()), tr!("Animation saved."));
    }

    fn palette_role(&mut self, row: i32, k: i32) {
        let Some(art) = self.chosen().and_then(pack::art) else { return };
        let mut colors: Vec<[u8; 3]> = art.palette().iter().map(|(c, _)| *c).collect();
        colors.extend(art.loose_colors(24));
        let Some(c) = usize::try_from(row).ok().and_then(|i| colors.get(i).copied()) else { return };
        let hex = format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2]);
        let role = usize::try_from(k - 1).ok().and_then(|i| ROLES.get(i)).map(|r| *r as char);
        self.change(|t| edit::set_palette(t, &hex, role), tr!("Color saved."));
    }

    fn toggle_stop(&mut self, i: i32) {
        let Some(m) = &self.manager else { return };
        let Some(s) = self.chosen() else { return };
        let g = m.win.global::<MascotsManager>();
        let rows: Vec<StopRow> = slint::Model::iter(&g.get_stops()).collect();
        let Some(row) = usize::try_from(i).ok().and_then(|i| rows.get(i)) else { return };
        // from what the file lists now (it may name animations the switches don't show):
        // only the clicked one changes; `idle` is set_extras' business
        let mut list: Vec<String> = check::of_sprite(s).walk.map(|w| w.stops).unwrap_or_default();
        list.retain(|n| n != "idle");
        if row.on {
            list.retain(|n| *n != row.name.as_str());
        } else if !list.iter().any(|n| *n == row.name.as_str()) {
            list.push(row.name.to_string());
        }
        self.change(|t| edit::set_extras(t, &list), tr!("Stops saved."));
    }

    fn use_in(&mut self, i: i32) {
        let Some(s) = self.chosen() else { return };
        let Some(place) = self.manager.as_ref().and_then(|m| usize::try_from(i).ok().filter(|i| *i > 0).and_then(|i| m.targets.get(i).cloned()))
        else {
            return;
        };
        {
            let mut cfg = self.host.config.borrow_mut();
            match &place {
                Place::Tray => cfg.tray_icon = s.key.to_string(),
                Place::Page(p, name) => {
                    // the pages may have changed since the list was made: only the same one
                    match cfg.pages.get_mut(*p).filter(|page| page.name == *name) {
                        Some(page) => page.icon = s.key.to_string(),
                        None => {
                            drop(cfg);
                            self.say(tr!("The pages changed: choose again.").to_string(), true);
                            self.fill_card();
                            return;
                        }
                    }
                }
                Place::App { app, setting } => cfg.set(app, setting, toml::Value::String(s.key.to_string())),
            }
            cfg.save();
        }
        self.host.settings_dirty.set(true);
        crate::hub::request_plugin_reload();
        self.say(tr!("{} is in use there now.", s.name()), false);
        // the list goes back to "Choose…"
        self.fill_card();
    }

    fn remove(&mut self) {
        let Some(s) = self.chosen() else { return };
        if builtin(s) {
            return;
        }
        if store_keys().iter().any(|k| k == s.key) {
            self.say(tr!("Installed by the store: remove it in the Store tab.").to_string(), true);
            return;
        }
        let Some(m) = self.manager.as_mut() else { return };
        if !m.armed.is_some_and(|at| at.elapsed() < ARM) {
            m.armed = Some(Instant::now());
            m.win.global::<MascotsManager>().set_armed_remove(true);
            self.say(tr!("Click again to remove {}. Whatever uses it goes back to the first mascot.", s.name()), true);
            return;
        }
        m.armed = None;
        let appearance = crate::skin::user::dir();
        let result = if pack::art(s).is_some() {
            std::fs::remove_dir_all(appearance.join(pack::DIR).join(s.key))
        } else {
            std::fs::remove_file(appearance.join(file::DIR).join(format!("{}.toml", s.key)))
        };
        match result {
            Ok(()) => {
                self.say(tr!("{} removed.", s.name()), false);
                if let Some(m) = self.manager.as_mut() {
                    m.selected = None;
                }
                crate::hub::request_plugin_reload();
            }
            Err(e) => self.say(e.to_string(), true),
        }
    }

    fn open_folder(&self) {
        let Some(s) = self.chosen() else { return };
        let appearance = crate::skin::user::dir();
        let dir: PathBuf = if pack::art(s).is_some() { appearance.join(pack::DIR).join(s.key) } else { appearance.join(file::DIR) };
        if !crate::core::exec::open_dir(&dir) {
            log::warn!("could not open {}", dir.display());
        }
    }
}

impl MiniApp for MascotsMini {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn enable(&mut self, ui: &HubWindow) {
        ui.global::<MascotsApp>().set_theme(render::colors(self.theme()));
        self.generation = None;
        self.push_face(ui);
        self.push_card(ui, 0);
    }

    fn disable(&mut self, _ui: &HubWindow) {
        if let Some(m) = self.manager.as_mut() {
            let _ = m.win.hide();
            m.open = false;
        }
    }

    fn on_event(&mut self, _ev: &Event, _ui: &HubWindow) {}

    fn on_report(&mut self, report: &Report, ui: &HubWindow) {
        self.busy = false;
        match report {
            Report::Done => {}
            Report::Output(text) => {
                self.status = text.clone();
                self.status_bad = false;
                self.face.react(Reaction::Celebrate);
                crate::hub::request_plugin_reload();
            }
            Report::Error(e) => {
                self.status = e.clone();
                self.status_bad = true;
            }
        }
        if !self.status.is_empty() {
            self.say(self.status.clone(), self.status_bad);
        }
        self.push_busy();
        self.push_face(ui);
        self.push_card(ui, 0);
    }

    fn tick(&mut self, frame: u32, ui: &HubWindow) {
        if self.face.refresh(sprites::get(&self.mascot_key()), self.theme(), Mood::Idle) {
            let g = ui.global::<MascotsApp>();
            g.set_mascot(self.face.image());
            g.set_reaction(self.face.reaction());
        }
        // the mascot list changed (import, edit, removal, Reload): everything again
        let generation = sprites::generation();
        if self.generation != Some(generation) {
            self.generation = Some(generation);
            if self.manager.as_ref().is_some_and(|m| m.open) {
                self.fill_manager();
            }
            self.push_card(ui, frame);
        } else if frame.is_multiple_of(2) {
            // the gallery and the preview move
            self.push_card(ui, frame);
        }
        if self.manager.as_ref().is_some_and(|m| m.open) {
            self.push_preview(frame);
            if let Some(m) = self.manager.as_mut()
                && m.armed.is_some_and(|at| at.elapsed() >= ARM)
            {
                m.armed = None;
                m.win.global::<MascotsManager>().set_armed_remove(false);
            }
        }
    }

    fn settings_changed(&mut self, ui: &HubWindow) {
        ui.global::<MascotsApp>().set_theme(render::colors(self.theme()));
        self.push_face(ui);
        self.push_card(ui, 0);
        self.style_manager();
        if self.manager.as_ref().is_some_and(|m| m.open) {
            self.fill_manager();
        }
    }

    fn mood(&self) -> Mood {
        Mood::Idle
    }

    fn mascot_key(&self) -> String {
        self.host.config.borrow().string(&self.manifest, "mascot")
    }

    fn summary(&self) -> String {
        tr!("Mascots: {}", sprites::all().len())
    }
}

/// A mascot built into the program (it has no file to change).
fn builtin(s: &Sprite) -> bool {
    sprites::SPRITES.iter().any(|b| std::ptr::eq(b, s))
}

fn moving(name: &str) -> bool {
    walkers::is_moving(name) || name == "jump"
}

fn palette_row(c: [u8; 3], role: i32) -> PaletteRow {
    PaletteRow {
        color: slint::Color::from_rgb_u8(c[0], c[1], c[2]),
        hex: format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2]).into(),
        role,
    }
}

/// What each color role is, for the list.
fn role_name(r: u8) -> &'static str {
    match r {
        b'K' => tr!("outline"),
        b'P' => tr!("main color"),
        b'S' => tr!("shadow"),
        b'H' => tr!("highlight"),
        b'M' => tr!("light detail"),
        b'C' => tr!("visor"),
        b'E' => tr!("eye"),
        b'W' => tr!("eye glint"),
        b'B' => tr!("blush"),
        b'T' => tr!("mouth"),
        b'F' => tr!("skin"),
        b'D' => tr!("hardware"),
        b'A' => tr!("LED (follows the mood)"),
        _ => "",
    }
}

/// Keys of the mascots the store installed (`store/<id>.json` in the appearance folder).
fn store_keys() -> Vec<String> {
    store_keys_in(&crate::skin::user::dir())
}

fn store_keys_in(appearance: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(appearance.join("store")) else { return Vec::new() };
    let mut keys = Vec::new();
    for e in entries.flatten() {
        let Ok(text) = std::fs::read_to_string(e.path()) else { continue };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
        for field in ["sprites", "packs"] {
            if let Some(list) = v.get(field).and_then(|l| l.as_array()) {
                keys.extend(list.iter().filter_map(|k| k.as_str().map(str::to_string)));
            }
        }
    }
    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_keys_come_from_the_receipts() {
        let d = std::env::temp_dir().join(format!("miniutils-mascots-app-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("store")).unwrap();
        std::fs::write(d.join("store/io.x.json"), r#"{"id":"io.x","sprites":["owl"],"packs":["knight"]}"#).unwrap();
        std::fs::write(d.join("store/broken.json"), "not json").unwrap();
        let mut keys = store_keys_in(&d);
        keys.sort();
        assert_eq!(keys, ["knight", "owl"]);
        assert!(store_keys_in(&d.join("nothing")).is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn every_role_has_a_name() {
        assert!(ROLES.iter().all(|r| !role_name(*r).is_empty()));
        assert!(builtin(sprites::get("neko")));
    }
}
