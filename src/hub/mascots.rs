//! The Library's Mascots tab (`MascotsPanel` in ui/mascots.slint): every mascot, code ones
//! and PNG sprites alike, read with the hub's own readers.
//!
//! - lists them and edits the chosen one's file (`mascot::edit`): name, the animation of each
//!   mood (PNG), the `[palette]` colors (PNG), the walking stops (`extras`); shows what it has
//!   (`mascot::check`); puts it to use (an app's mascot setting, a page, the tray) and removes
//!   the person's own ones;
//! - imports a folder or a `.zip` (`mascot::import`), chosen in the system's dialog (`rfd`,
//!   in a thread).
//!
//! After any change the hub rereads the appearance folder (`hub::request_plugin_reload`);
//! the new mascot list arrives here as a new `sprites::generation`.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, ModelRc, VecModel};

use crate::core::miniapp::{Host, mascot_targets};
use crate::mascot::anim::{Animator, Reaction};
use crate::mascot::check;
use crate::mascot::sprites::{self, Sprite};
use crate::mascot::themes::Theme;
use crate::mascot::walkers::{self, Walker};
use crate::mascot::{Mood, edit, file, pack, render};
use crate::tr;
use crate::ui::{Anim, LibraryWindow, MascotItem, MascotsManager, PaletteRow, StopRow};

/// The color roles a PNG color can take, in the order of the "role" list (after "keeps its
/// own color").
const ROLES: &[u8] = b"KPSHMCEWBTFDA";
/// Moods and reactions of the "Animations" list, in its order.
const MAPPED: [&str; 7] = ["sleep", "idle", "happy", "busy", "alarm", "celebrate", "surprised"];
/// Second click of "Remove": valid for this long.
const ARM: Duration = Duration::from_secs(6);
/// Pace of the preview (its own timer: the hub may be hidden).
const TICK: Duration = Duration::from_millis(125);

/// The tab and what it shows.
pub(super) struct MascotsTab {
    host: Rc<Host>,
    /// The Library window, whose Mascots tab this is.
    win: LibraryWindow,
    open: bool,
    /// An import is running (its dialog or the copy).
    busy: bool,
    /// The chosen mascot's key.
    selected: Option<String>,
    /// What the preview list offers for the chosen one, in its order, and the choice.
    modes: Vec<Preview>,
    mode: usize,
    walker: Option<Walker>,
    armed: Option<Instant>,
    /// What "Use in" lists, in order (the first entry is "Choose…").
    targets: Vec<Place>,
    /// Drives the preview and the end of "Confirm removal" while the tab is shown: the
    /// hub's frame stops when its popup hides, and the Library stays on screen.
    timer: slint::Timer,
    ticks: u32,
    /// The preview draws like a header mascot: blinking, drawn animations, reactions.
    animator: Animator,
    cache: render::Cache,
    /// Walking frame shown last (the walking preview doesn't go through `cache`).
    walk_shown: Option<usize>,
    /// `sprites::generation` the list was made from.
    generation: Option<u64>,
    /// Text of the search field, and the keys it lets through, in the list's order.
    filter: String,
    listed: Vec<String>,
}

/// What the preview shows.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Preview {
    Mood(Mood),
    /// Plays the reaction again each time it ends.
    React(Reaction),
    Walking,
}

/// Where "Use in" puts a mascot.
#[derive(Clone)]
enum Place {
    Tray,
    /// Index and name (checked again when used: the pages may change meanwhile).
    Page(usize, String),
    App { app: String, setting: String },
}

/// The preview list: moods, reactions, then walking (only for a mascot that walks).
fn preview_modes(walks: bool) -> Vec<(Preview, &'static str)> {
    let mut modes = vec![
        (Preview::Mood(Mood::Idle), tr!("Calm")),
        (Preview::Mood(Mood::Happy), tr!("Happy")),
        (Preview::Mood(Mood::Busy), tr!("Busy")),
        (Preview::Mood(Mood::Alarm), tr!("Alarm")),
        (Preview::Mood(Mood::Sleep), tr!("Sleeping")),
        (Preview::React(Reaction::Celebrate), tr!("Celebrating")),
        (Preview::React(Reaction::Surprised), tr!("Surprised")),
    ];
    if walks {
        modes.push((Preview::Walking, tr!("Walking")));
    }
    modes
}

/// Whether `s` passes the search: by name, key or origin, ignoring case.
fn matches(filter: &str, name: &str, key: &str, origin: &str) -> bool {
    let f = filter.trim().to_lowercase();
    f.is_empty() || [name, key, origin].iter().any(|t| t.to_lowercase().contains(&f))
}

impl MascotsTab {
    /// The tab of a new Library window, bound to it.
    pub(super) fn create(host: Rc<Host>, win: &LibraryWindow) -> Rc<RefCell<Self>> {
        let tab = Rc::new(RefCell::new(MascotsTab {
            host,
            win: win.clone_strong(),
            open: false,
            busy: false,
            selected: None,
            modes: Vec::new(),
            mode: 0,
            walker: None,
            armed: None,
            targets: Vec::new(),
            timer: slint::Timer::default(),
            ticks: 0,
            animator: Animator::new(43),
            cache: render::Cache::default(),
            walk_shown: None,
            generation: None,
            filter: String::new(),
            listed: Vec::new(),
        }));
        Self::bind(win, &tab);
        tab.borrow().style();
        tab
    }

    /// The tab came into view (`true`) or left it, the window included: the preview runs
    /// only meanwhile.
    pub(super) fn shown(tab: &Rc<RefCell<Self>>, shown: bool) {
        let mut me = tab.borrow_mut();
        if !shown {
            me.open = false;
            me.armed = None;
            me.timer.stop();
            return;
        }
        me.open = true;
        let t = Rc::downgrade(tab);
        me.timer.start(slint::TimerMode::Repeated, TICK, move || {
            // already borrowed (a callback running): this tick is skipped
            if let Some(t) = t.upgrade()
                && let Ok(mut t) = t.try_borrow_mut()
            {
                t.tick();
            }
        });
        me.style();
        me.fill();
    }

    fn bind(win: &LibraryWindow, tab: &Rc<RefCell<Self>>) {
        let g = win.global::<MascotsManager>();
        // an action on the tab, borrowed
        let on = |run: fn(&mut MascotsTab, i32)| {
            let t = Rc::downgrade(tab);
            move |i: i32| {
                if let Some(t) = t.upgrade() {
                    run(&mut t.borrow_mut(), i);
                }
            }
        };
        let t = Rc::downgrade(tab);
        g.on_select(move |key| {
            if let Some(t) = t.upgrade() {
                t.borrow_mut().select(&key);
            }
        });
        g.on_step(on(|m, d| m.step(d)));
        let t = Rc::downgrade(tab);
        g.on_filter_edited(move |text| {
            if let Some(t) = t.upgrade() {
                let mut t = t.borrow_mut();
                t.filter = text.to_string();
                t.fill_list();
            }
        });
        g.on_preview_mode(on(|m, i| {
            m.mode = usize::try_from(i).unwrap_or(0).min(m.modes.len().saturating_sub(1));
            m.restart_preview();
            m.push_preview();
        }));
        g.on_stop_toggled(on(|m, i| m.toggle_stop(i)));
        g.on_use_in(on(|m, i| m.use_in(i)));
        let t = Rc::downgrade(tab);
        g.on_map(move |which, k| {
            if let Some(t) = t.upgrade() {
                t.borrow_mut().map(which, k);
            }
        });
        let t = Rc::downgrade(tab);
        g.on_palette_role(move |row, k| {
            if let Some(t) = t.upgrade() {
                t.borrow_mut().palette_role(row, k);
            }
        });
        let t = Rc::downgrade(tab);
        g.on_rename(move |name| {
            if let Some(t) = t.upgrade() {
                t.borrow_mut().rename(&name);
            }
        });
        let t = Rc::downgrade(tab);
        g.on_remove(move || {
            if let Some(t) = t.upgrade() {
                t.borrow_mut().remove();
            }
        });
        let t = Rc::downgrade(tab);
        g.on_open_folder(move || {
            if let Some(t) = t.upgrade() {
                t.borrow().open_folder();
            }
        });
        for zip in [false, true] {
            let t = Rc::downgrade(tab);
            let pick = move || {
                if let Some(t) = t.upgrade() {
                    t.borrow_mut().pick(zip);
                }
            };
            if zip {
                g.on_import_zip(pick);
            } else {
                g.on_import_folder(pick);
            }
        }
    }

    /// The colors the mascots are drawn in: the window skin's app theme.
    fn theme(&self) -> &'static Theme {
        crate::mascot::themes::get(crate::skin::get(&self.host.config.borrow().skin).app_theme)
    }

    /// The tab follows the hub's look.
    fn style(&self) {
        self.win.global::<Anim>().set_calm(true);
        self.win.global::<MascotsManager>().set_theme(render::colors(self.theme()));
    }

    // ---- import ---------------------------------------------------------------------

    /// Asks the system for a folder (or a `.zip`) in a thread and imports it there; the
    /// result comes back through `imported`.
    fn pick(&mut self, zip: bool) {
        if self.busy {
            return;
        }
        self.set_busy(true);
        let appearance = crate::skin::user::dir();
        let spawned = std::thread::Builder::new().name("mascots-import".into()).spawn(move || {
            let chosen = if zip {
                rfd::FileDialog::new().set_title(tr!("Import mascots from a .zip")).add_filter("zip", &["zip", "ZIP"]).pick_file()
            } else {
                rfd::FileDialog::new().set_title(tr!("Import mascots from a folder")).pick_folder()
            };
            // cancelled: nothing to say
            let result = chosen.map(|path| match crate::mascot::import::import(&path, &appearance) {
                // first line: the summary; then one line per mascot
                Ok(out) => {
                    let text = format!("{}\n{}", out.summary(), out.lines.join("\n"));
                    if out.count > 0 { Ok(text) } else { Err(text) }
                }
                Err(e) => Err(e.to_string()),
            });
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(tab) = super::library::mascots_tab() {
                    tab.borrow_mut().imported(result);
                }
            });
        });
        if let Err(e) = spawned {
            self.imported(Some(Err(tr!("No thread for the import: {}", e))));
        }
    }

    /// The end of an import (`None`: the dialog was cancelled).
    fn imported(&mut self, result: Option<Result<String, String>>) {
        self.set_busy(false);
        let Some(result) = result else { return };
        let (text, ok) = match result {
            Ok(text) => (text, true),
            Err(text) => (text, false),
        };
        log::info!("mascot import:\n{text}");
        self.say(text, !ok);
        // something came in
        if ok {
            crate::hub::request_plugin_reload();
        }
    }

    fn set_busy(&mut self, busy: bool) {
        self.busy = busy;
        self.win.global::<MascotsManager>().set_busy(busy);
    }

    // ---- list and card --------------------------------------------------------------

    /// The list and the chosen one's card, from scratch.
    fn fill(&mut self) {
        self.generation = Some(sprites::generation());
        // the chosen one may have left (removed, folder changed)
        if self.selected.as_deref().is_some_and(|k| sprites::find(k).is_none()) {
            self.selected = None;
        }
        self.fill_list();
        self.fill_card();
    }

    /// The list on the left: every mascot that passes the search.
    fn fill_list(&mut self) {
        let theme = self.theme();
        let store = store_keys();
        let mut listed = Vec::new();
        let items: Vec<MascotItem> = sprites::all()
            .into_iter()
            .filter_map(|s| {
                let origin = if builtin(s) {
                    tr!("built-in")
                } else if store.iter().any(|k| k == s.key) {
                    tr!("store")
                } else {
                    tr!("yours")
                };
                if !matches(&self.filter, s.name(), s.key, origin) {
                    return None;
                }
                listed.push(s.key.to_string());
                Some(MascotItem {
                    key: s.key.into(),
                    name: s.name().into(),
                    kind: if pack::art(s).is_some() { "sprite" } else { "code" }.into(),
                    origin: origin.into(),
                    icon: render::image(s, theme, Mood::Idle, 1),
                    walks: walkers::walks(s),
                    themed: check::of_sprite(s).themed,
                })
            })
            .collect();
        self.listed = listed;
        self.win.global::<MascotsManager>().set_items(ModelRc::new(VecModel::from(items)));
    }

    fn select(&mut self, key: &str) {
        if sprites::find(key).is_none() {
            return;
        }
        self.selected = Some(key.to_string());
        self.mode = 0;
        self.armed = None;
        self.restart_preview();
        self.win.global::<MascotsManager>().set_status("".into());
        self.fill_card();
    }

    /// Up/Down in the window: the previous or next mascot of the (searched) list.
    fn step(&mut self, delta: i32) {
        if self.listed.is_empty() {
            return;
        }
        let last = self.listed.len() as i32 - 1;
        let next = match self.selected.as_deref().and_then(|k| self.listed.iter().position(|x| x == k)) {
            Some(i) => (i as i32 + delta).clamp(0, last),
            None if delta < 0 => last,
            None => 0,
        };
        let key = self.listed[next as usize].clone();
        if self.selected.as_deref() != Some(key.as_str()) {
            self.select(&key);
        }
    }

    fn chosen(&self) -> Option<&'static Sprite> {
        sprites::find(self.selected.as_deref()?)
    }

    /// The chosen mascot's card: name, report, animations, colors, stops, places.
    fn fill_card(&mut self) {
        let theme = self.theme();
        let win = self.win.clone_strong();
        let g = win.global::<MascotsManager>();
        let Some(s) = self.chosen() else {
            // none chosen (removed, or the folder changed): no card left on screen
            self.walker = None;
            g.set_key("".into());
            g.set_editable(false);
            g.set_can_map(false);
            g.set_can_palette(false);
            g.set_can_stops(false);
            return;
        };
        let report = check::of_sprite(s);
        let art = pack::art(s);
        // a mascot from the store isn't changed here: an update would undo the change
        let from_store = store_keys().iter().any(|k| k == s.key);
        let editable = !builtin(s) && !from_store;
        g.set_name(s.name().into());
        g.set_key(s.key.into());
        g.set_editable(editable);
        g.set_from_store(from_store);
        g.set_report(report.text().into());
        g.set_armed_remove(false);
        self.walker = walkers::walker(s, theme);
        // the same preview stays chosen when it is still offered (after an edit, a reload)
        let before = self.modes.get(self.mode).copied();
        let offered = preview_modes(self.walker.is_some());
        self.modes = offered.iter().map(|(p, _)| *p).collect();
        self.mode = before.and_then(|b| self.modes.iter().position(|p| *p == b)).unwrap_or(0);
        self.restart_preview();
        let labels: Vec<slint::SharedString> = offered.iter().map(|(_, l)| (*l).into()).collect();
        g.set_preview_modes(ModelRc::new(VecModel::from(labels)));
        g.set_preview_index(self.mode as i32);

        // animations of each mood and reaction (PNG sprite)
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
        self.targets = places;
        g.set_targets(ModelRc::new(VecModel::from(labels)));
        self.push_preview();
    }

    /// The preview image: the chosen mood or reaction (drawn like a header mascot, with
    /// its drawn animations), or walking. Only handed to the window when it changed.
    fn push_preview(&mut self) {
        let theme = self.theme();
        let Some(s) = self.chosen() else { return };
        let now = Instant::now();
        let img = match (self.modes.get(self.mode).copied().unwrap_or(Preview::Mood(Mood::Idle)), &self.walker) {
            (Preview::Walking, Some(w)) => {
                let (right, _) = &w.frames[w.script.walk];
                let i = (self.ticks / 2) as usize % right.len().max(1);
                if self.walk_shown == Some(i) {
                    return;
                }
                self.walk_shown = Some(i);
                right.get(i).cloned().unwrap_or_default()
            }
            (preview, _) => {
                let mood = match preview {
                    Preview::Mood(mood) => mood,
                    _ => Mood::Idle,
                };
                if let Preview::React(r) = preview
                    && self.animator.current(now).is_none()
                {
                    // played to the end: again (a new animator has no cooldown)
                    self.animator = Animator::new(43);
                    self.animator.trigger(r, now);
                }
                let look = self.animator.look_with(s, mood, now);
                if !self.cache.update_look(s, theme, &look) {
                    return;
                }
                self.cache.image()
            }
        };
        self.win.global::<MascotsManager>().set_preview(img);
    }

    /// Another mascot or preview: drawn from the start (the next `push_preview` always
    /// hands over an image).
    fn restart_preview(&mut self) {
        self.animator = Animator::new(43);
        self.cache = render::Cache::default();
        self.walk_shown = None;
        if let Some(Preview::React(r)) = self.modes.get(self.mode) {
            self.animator.trigger(*r, Instant::now());
        }
    }

    /// One tick of the tab's own timer: the preview, the end of "Confirm removal" and the
    /// list, when the mascots changed (the hub's tick may not be running).
    fn tick(&mut self) {
        if !self.open {
            self.timer.stop();
            return;
        }
        self.ticks = self.ticks.wrapping_add(1);
        if self.armed.is_some_and(|at| at.elapsed() >= ARM) {
            self.armed = None;
            self.win.global::<MascotsManager>().set_armed_remove(false);
        }
        if self.generation != Some(sprites::generation()) {
            self.fill();
        }
        self.push_preview();
    }

    fn say(&self, text: String, bad: bool) {
        let g = self.win.global::<MascotsManager>();
        g.set_status(text.into());
        g.set_status_bad(bad);
    }

    // ---- changes --------------------------------------------------------------------

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
        let Some(s) = self.chosen() else { return };
        let rows: Vec<StopRow> = slint::Model::iter(&self.win.global::<MascotsManager>().get_stops()).collect();
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
        let Some(place) = usize::try_from(i).ok().filter(|i| *i > 0).and_then(|i| self.targets.get(i).cloned()) else {
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
        if !self.armed.is_some_and(|at| at.elapsed() < ARM) {
            self.armed = Some(Instant::now());
            self.win.global::<MascotsManager>().set_armed_remove(true);
            self.say(tr!("Click again to remove {}. Whatever uses it goes back to the first mascot.", s.name()), true);
            return;
        }
        self.armed = None;
        let appearance = crate::skin::user::dir();
        let result = if pack::art(s).is_some() {
            std::fs::remove_dir_all(appearance.join(pack::DIR).join(s.key))
        } else {
            std::fs::remove_file(appearance.join(file::DIR).join(format!("{}.toml", s.key)))
        };
        match result {
            Ok(()) => {
                self.say(tr!("{} removed.", s.name()), false);
                self.selected = None;
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
        let d = std::env::temp_dir().join(format!("miniutils-mascots-tab-{}", std::process::id()));
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
    fn search_matches_name_key_or_origin() {
        assert!(matches("", "Neko", "neko", "built-in"));
        assert!(matches("  NEK ", "Neko", "neko", "built-in"));
        assert!(matches("pink", "Pink Monster", "pink-monster", "yours"));
        assert!(matches("yours", "Owl", "owl", "yours"));
        assert!(!matches("cat", "Owl", "owl", "yours"));
    }

    #[test]
    fn preview_offers_walking_only_when_it_walks() {
        assert!(!preview_modes(false).iter().any(|(p, _)| *p == Preview::Walking));
        assert_eq!(preview_modes(true).last().map(|(p, _)| *p), Some(Preview::Walking));
        assert!(preview_modes(false).iter().any(|(p, _)| *p == Preview::React(Reaction::Surprised)));
    }

    #[test]
    fn every_role_has_a_name() {
        assert!(ROLES.iter().all(|r| !role_name(*r).is_empty()));
        assert!(builtin(sprites::get("neko")));
    }
}
