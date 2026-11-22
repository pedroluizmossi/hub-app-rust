//! User configuration in `~/.config/miniutils/config.toml`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::layout::{self, Rect};
use super::manifest::{Manifest, SettingKind};
use crate::tr;

/// App name (default display name; lowercased for the config folder, socket and binary).
pub const APP_NAME: &str = "MiniUtils";

/// Current display name, read from other threads (tray and notifications).
static DISPLAY_NAME: std::sync::RwLock<String> = std::sync::RwLock::new(String::new());

/// Name the user chose (`name` in the config), or `MiniUtils`.
pub fn display_name() -> String {
    let n = DISPLAY_NAME.read().map(|n| n.clone()).unwrap_or_default();
    if n.trim().is_empty() { APP_NAME.into() } else { n }
}

/// Range of `text_scale` (the sizes Appearance offers go from one to the other).
pub const TEXT_SCALE_MIN: u32 = 90;
pub const TEXT_SCALE_MAX: u32 = 125;

/// Current version of the file format. When the structure changes, bump the number and
/// add the matching migration in `migrate`.
pub const CURRENT_VERSION: u32 = 4;
/// Oldest file format that still migrates; an older file is set aside like a broken one.
const OLDEST_VERSION: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// File format version (missing = 0, the old format).
    pub version: u32,
    /// Name shown on the window, the tray and notifications (the user can change it).
    pub name: String,
    /// Pages of the panel (the sidebar), each with its own grid and apps.
    pub pages: Vec<Page>,
    /// Window skin (see `src/skin`).
    pub skin: String,
    /// Accent color on top of the skin: `""` (the skin's own) or one of `skin::ACCENTS`.
    pub accent: String,
    /// Interface text size, in % of the design (`TEXT_SCALE_MIN` to `TEXT_SCALE_MAX`).
    pub text_scale: u32,
    /// Animated background (rain, stars, scanline); "Reduce motion" also turns it off.
    pub ambient: bool,
    /// Reduce motion: mascots without jumps or shakes, and no reactions or easter eggs.
    pub reduce_motion: bool,
    /// Mascots stay put when the pointer is over their card (on by default; works on its
    /// own, without "Reduce motion", which keeps them still anyway).
    pub still_on_hover: bool,
    /// How much the mascots chat with each other: `"off"`, `"some"` or `"lots"`.
    pub chatter: String,
    /// Interface language: `"auto"` (the system's), `"en"` or `"pt-BR"` (`core::i18n`).
    pub language: String,
    /// Where the pages go: `"side"` (bar on the left), `"right"`, `"bottom"` or `"top"`
    /// (tabs in the header). An unknown value means `"side"` (`hub::appearance::NAVS`).
    pub nav: String,
    /// Mascot of the tray icon: `"auto"` (the one of the app in the worst state) or a mascot.
    pub tray_icon: String,
    /// Theme of the tray icon: `"auto"` (the one of the app shown) or a theme.
    pub tray_theme: String,
    /// Position and size of each app in its page's grid, in cells: `[layout."cyber.docker"]`.
    pub layout: BTreeMap<String, Rect>,
    /// Window size in logical pixels (remembered between launches).
    pub window: Option<(u32, u32)>,
    /// Where the window opens: `"tray"` (next to the tray icon), `"center"`, a corner
    /// (`"top-left"`, `"top-right"`, `"bottom-left"`, `"bottom-right"`) or `"last"` (where it
    /// was left, `window_pos`). An unknown value means `"tray"` (`popup::Place`).
    pub window_place: String,
    /// Last position of the window (top-left corner, physical pixels), for `"last"`.
    pub window_pos: Option<(i32, i32)>,
    /// Desktop app: the window shows up in the taskbar and the window switcher, is not
    /// above the others and does not hide when it loses focus (the tray icon stays).
    pub desktop_app: bool,
    /// How windows are drawn: `"software"` (on the CPU, light) or `"opengl"` (femtovg on the
    /// GPU: the terminal's shaders). Read once at start (`popup::select_backend`); an unknown
    /// value means `"software"`.
    pub renderer: String,
    /// Enabled mini-apps, in tab order.
    pub enabled: Vec<String>,
    /// Settings of each mini-app: `[apps."cyber.docker"]`.
    pub apps: BTreeMap<String, toml::Table>,
    /// Plugin store index (`registry.json`: path or https://). Empty = the official store
    /// (`store::OFFICIAL_INDEX`).
    pub store_index: String,
    /// Public key (ed25519, hex) that signs the store index. Empty = the official one when
    /// the index is the official one; for another index, no check (accepted only if local).
    pub store_key: String,
    /// How to save, depending on the file that was read (not written to the TOML).
    #[serde(skip)]
    state: SaveState,
}

/// A page of the panel: shown in the sidebar, with its own grid.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Page {
    /// Name shown in the sidebar.
    pub name: String,
    /// Mascot of the sidebar icon (key of `mascot::sprites`).
    pub icon: String,
    /// Grid columns.
    pub columns: u32,
    /// Grid rows.
    pub rows: u32,
    /// Apps of this page, enabled or not. An app is on one page only.
    pub apps: Vec<String>,
}

impl Default for Page {
    fn default() -> Self {
        Page { name: tr!("General").into(), icon: "neko".into(), columns: 2, rows: 1, apps: Vec::new() }
    }
}

/// What to do on save, depending on the file that was read.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
enum SaveState {
    #[default]
    Normal,
    /// File from a newer MiniUtils: never overwritten.
    ReadOnly,
    /// Broken TOML: the original is moved to `.broken` before saving anything new.
    Broken,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            version: CURRENT_VERSION,
            name: APP_NAME.into(),
            pages: vec![Page {
                apps: vec!["cyber.docker".into(), "cyber.vitals".into(), "cyber.cd".into()],
                ..Page::default()
            }],
            skin: "neon".into(),
            accent: String::new(),
            text_scale: 100,
            ambient: true,
            reduce_motion: false,
            still_on_hover: true,
            chatter: "some".into(),
            language: "auto".into(),
            nav: "side".into(),
            tray_icon: "auto".into(),
            tray_theme: "auto".into(),
            window: None,
            window_place: "tray".into(),
            window_pos: None,
            desktop_app: false,
            renderer: "software".into(),
            layout: BTreeMap::new(),
            enabled: vec!["cyber.docker".into(), "cyber.vitals".into(), "cyber.cd".into()],
            apps: BTreeMap::new(),
            store_index: String::new(),
            store_key: String::new(),
            state: SaveState::Normal,
        }
    }
}

impl Config {
    /// Page the app is on, if any.
    pub fn page_of(&self, id: &str) -> Option<usize> {
        self.pages.iter().position(|p| p.apps.iter().any(|a| a == id))
    }

    /// Page asked for by `--tab`: by the page name or by an app on it, with the full id
    /// (`cyber.docker`) or just its last part (`docker`). Case-insensitive; a page name
    /// wins over an app with the same name.
    pub fn find_page(&self, tab: &str) -> Option<usize> {
        let tab = tab.trim();
        let same = |s: &str| s.trim().eq_ignore_ascii_case(tab);
        self.pages.iter().position(|p| same(&p.name)).or_else(|| {
            self.pages.iter().position(|p| {
                p.apps.iter().any(|id| same(id) || id.rsplit_once('.').is_some_and(|(_, short)| same(short)))
            })
        })
    }

    /// Enabled apps of the page, in page order.
    pub fn page_apps(&self, page: usize) -> Vec<String> {
        self.pages.get(page).map_or_else(Vec::new, |p| p.apps.iter().filter(|a| self.is_enabled(a)).cloned().collect())
    }

    /// Rectangles of the page's enabled apps, in `page_apps` order.
    pub fn rects(&self, page: usize) -> Vec<Rect> {
        self.page_apps(page).iter().map(|id| self.layout.get(id).copied().unwrap_or(Rect::new(0, 0, 1, 1))).collect()
    }

    /// Puts the app at the end of page `to` (taking it out of where it was). The position is
    /// chosen by the next `arrange`: it stays where it was if it fits, otherwise it goes to
    /// the first free spot.
    pub fn move_to_page(&mut self, id: &str, to: usize) {
        if to >= self.pages.len() || self.page_of(id) == Some(to) {
            return;
        }
        for p in &mut self.pages {
            p.apps.retain(|a| a != id);
        }
        self.pages[to].apps.push(id.to_string());
    }

    /// New page at the end, with a mascot not in use yet. Returns its index.
    pub fn add_page(&mut self) -> usize {
        let n = self.pages.len() + 1;
        let icon = crate::mascot::sprites::SPRITES
            .iter()
            .map(|s| s.key)
            .find(|k| !self.pages.iter().any(|p| p.icon == *k))
            .unwrap_or("neko");
        self.pages.push(Page { name: tr!("Page {}", n), icon: icon.into(), ..Page::default() });
        self.pages.len() - 1
    }

    /// Deletes the page; its apps go to the previous one (or the first). The last page stays.
    pub fn remove_page(&mut self, i: usize) -> bool {
        if self.pages.len() <= 1 || i >= self.pages.len() {
            return false;
        }
        let page = self.pages.remove(i);
        let dest = i.saturating_sub(1).min(self.pages.len() - 1);
        self.pages[dest].apps.extend(page.apps);
        true
    }

    /// Makes sure there is at least one page and that every enabled app is on exactly one.
    fn ensure_pages(&mut self) {
        if self.pages.is_empty() {
            self.pages.push(Page::default());
        }
        let mut seen = std::collections::HashSet::new();
        for p in &mut self.pages {
            p.apps.retain(|a| seen.insert(a.clone()));
        }
        let orphans: Vec<String> = self.enabled.iter().filter(|id| !seen.contains(*id)).cloned().collect();
        self.pages[0].apps.extend(orphans);
    }

    /// Makes sure every enabled app has a page and a spot on the grid, and that nothing
    /// overlaps. Each app stays where it is if it fits; otherwise it goes to the first free
    /// spot and, with none left, the grid grows. Returns `true` if anything changed.
    pub fn arrange(&mut self) -> bool {
        let before = (self.pages.clone(), self.layout.clone());
        self.ensure_pages();
        for i in 0..self.pages.len() {
            let ids = self.page_apps(i);
            let p = &mut self.pages[i];
            p.columns = p.columns.clamp(1, layout::MAX_COLS);
            p.rows = p.rows.clamp(1, layout::MAX_ROWS);
            let (cols, mut rows) = (p.columns, p.rows);
            let mut placed: Vec<Rect> = Vec::new();
            for id in &ids {
                // no saved spot: one cell, in the first free spot
                let want = self.layout.get(id).copied().unwrap_or(Rect::new(cols, 0, 1, 1));
                let (r, used) = layout::add(&placed, want, cols, rows);
                rows = used;
                placed.push(r);
            }
            for (id, r) in ids.iter().zip(placed) {
                self.layout.insert(id.clone(), r);
            }
            self.pages[i].rows = rows;
        }
        before != (self.pages.clone(), self.layout.clone())
    }

    pub fn path() -> PathBuf {
        dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("miniutils/config.toml")
    }

    /// Reads the configuration; if missing or broken, uses the defaults.
    pub fn load() -> Self {
        let path = Self::path();
        let mut cfg = Self::load_from(&path);
        // keep the `name` line in the file, so the user finds where to change the name
        let no_name = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| t.parse::<toml::Table>().ok())
            .is_some_and(|t| !t.contains_key("name"));
        if no_name && cfg.state == SaveState::Normal {
            cfg.save_to(&path);
        }
        cfg.publish_name();
        cfg
    }

    /// Makes the display name visible to the tray and notifications.
    pub fn publish_name(&self) {
        if let Ok(mut n) = DISPLAY_NAME.write() {
            *n = self.name.trim().to_string();
        }
    }

    fn load_from(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Config::default();
        };
        // Empty is not an old config (without `version`), it is a write that did not finish:
        // migrating it would keep an empty backup and save the defaults as if chosen.
        if text.trim().is_empty() {
            return Config::default();
        }
        let mut table: toml::Table = match text.parse() {
            Ok(t) => t,
            Err(e) => return Self::broken(&e.to_string()),
        };
        let version = match table.get("version") {
            None => 0,
            Some(v) => match v.as_integer().and_then(|i| u32::try_from(i).ok()) {
                Some(n) => n,
                None => return Self::broken("invalid version"),
            },
        };

        // File from a newer MiniUtils: read what we can, but don't touch it.
        if version > CURRENT_VERSION {
            log::warn!(
                "{} is version {version}, newer than this build's {CURRENT_VERSION}; changes will not be saved",
                path.display()
            );
            let cfg = deserialize(table).unwrap_or_else(|e| {
                log::warn!("invalid config ({e}); using the defaults");
                Config::default()
            });
            let mut cfg = Config { state: SaveState::ReadOnly, ..cfg };
            cfg.normalize();
            return cfg;
        }

        if version < OLDEST_VERSION {
            return Self::broken(&format!("version {version} is too old to migrate"));
        }
        let old = version < CURRENT_VERSION;
        if old {
            migrate(&mut table, version);
        }
        match deserialize(table) {
            Ok(mut cfg) => {
                cfg.normalize();
                // Save the new version only after keeping the original.
                if old && backup(path, version) {
                    cfg.save_to(path);
                }
                cfg
            }
            Err(e) => Self::broken(&e),
        }
    }

    /// Brings hand-edited values back into what the app understands: `text_scale` within its
    /// range (plugin views read it as is), an unknown `language` or `window_place` to the
    /// default. (`chatter` and `nav` already treat an unknown value as the default where
    /// they are read.)
    fn normalize(&mut self) {
        self.text_scale = self.text_scale.clamp(TEXT_SCALE_MIN, TEXT_SCALE_MAX);
        if !crate::core::i18n::Language::KEYS.contains(&self.language.as_str()) {
            log::debug!("unknown language {:?} in the config; using auto", self.language);
            self.language = "auto".into();
        }
        if crate::popup::Place::index_of(&self.window_place).is_none() {
            log::debug!("unknown window_place {:?} in the config; using tray", self.window_place);
            self.window_place = "tray".into();
        }
    }

    /// Message for the user when the file that was read can't be used as is: from a newer
    /// MiniUtils (nothing is saved) or unreadable (defaults in use). Shown once at start.
    pub fn load_notice(&self) -> Option<&'static str> {
        match self.state {
            SaveState::Normal => None,
            SaveState::ReadOnly => Some(tr!("config.toml is from a newer MiniUtils: changes made here won't be saved.")),
            SaveState::Broken => Some(tr!("config.toml could not be read, so the defaults are in use. It is set aside as config.toml.broken on the next save.")),
        }
    }

    /// Default configuration for a file that could not be read; the original is kept.
    fn broken(reason: &str) -> Self {
        log::warn!("invalid config ({reason}); using the defaults");
        Config { state: SaveState::Broken, ..Config::default() }
    }

    /// Saves the configuration. The text is built here and a writer thread does the disk
    /// work: the `fsync` does not block the interface (edit mode saves on every key press).
    /// Call `flush_saves` before exiting.
    pub fn save(&mut self) {
        let path = Self::path();
        if let Some(text) = self.text_to_save(&path) {
            writer::submit(path, text);
        }
    }

    fn save_to(&mut self, path: &Path) {
        if let Some(text) = self.text_to_save(path) {
            write_logged(path, &text);
        }
    }

    /// The TOML to write to `path`, or `None` if it must not be written (read-only, or the
    /// broken file could not be moved aside).
    fn text_to_save(&mut self, path: &Path) -> Option<String> {
        match self.state {
            SaveState::ReadOnly => return None,
            SaveState::Broken => {
                if path.exists() {
                    let dest = free_name(&with_suffix(path, ".broken"));
                    if let Err(e) = std::fs::rename(path, &dest) {
                        log::warn!("could not move {} to {}: {e}", path.display(), dest.display());
                        return None;
                    }
                }
                self.state = SaveState::Normal;
            }
            SaveState::Normal => {}
        }
        match toml::to_string_pretty(self) {
            Ok(text) => Some(text),
            Err(e) => {
                log::error!("could not serialize the config: {e}");
                None
            }
        }
    }

    pub fn is_enabled(&self, id: &str) -> bool {
        self.enabled.iter().any(|e| e == id)
    }

    /// Value of an app setting, or the manifest default. A saved value of the wrong type
    /// (`alarm = "90"` edited by hand) is ignored in favor of the default.
    pub fn value(&self, m: &Manifest, key: &str) -> toml::Value {
        let def = m.setting(key);
        let saved = self.apps.get(&m.id).and_then(|t| t.get(key));
        if let Some(v) = saved {
            let typed = match def.map(|d| d.kind) {
                None => true,
                Some(SettingKind::Bool) => v.is_bool(),
                Some(SettingKind::Int) => v.is_integer(),
                Some(SettingKind::Choice | SettingKind::Text | SettingKind::Secret) => v.is_str(),
            };
            if typed {
                return v.clone();
            }
            log::debug!("{}: setting {key} = {v} has the wrong type; using the default", m.id);
        }
        def.map(|s| s.default.clone()).unwrap_or(toml::Value::Boolean(false))
    }

    pub fn bool(&self, m: &Manifest, key: &str) -> bool {
        self.value(m, key).as_bool().unwrap_or(false)
    }

    /// An int setting, within the manifest's `min` and `max`.
    pub fn int(&self, m: &Manifest, key: &str) -> i64 {
        let v = self.value(m, key).as_integer().unwrap_or(0);
        let def = m.setting(key);
        let v = def.and_then(|d| d.min).map_or(v, |min| v.max(min));
        def.and_then(|d| d.max).map_or(v, |max| v.min(max))
    }

    pub fn string(&self, m: &Manifest, key: &str) -> String {
        self.value(m, key).as_str().unwrap_or_default().to_string()
    }

    pub fn set(&mut self, app: &str, key: &str, value: toml::Value) {
        self.apps.entry(app.to_string()).or_default().insert(key.to_string(), value);
    }
}

/// Waits for the writer thread to finish what is pending (call before exiting).
pub fn flush_saves() {
    writer::flush();
}

fn write_logged(path: &Path, text: &str) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = crate::core::os::write_atomic_private(path, text.as_bytes()) {
        log::error!("could not save {}: {e}", path.display());
    }
}

/// Single thread that writes the configuration. It keeps only the newest request: many
/// saves in a row (arrow keys in edit mode) become one trip to the disk.
mod writer {
    use std::path::PathBuf;
    use std::sync::{Condvar, Mutex, Once};

    struct Queue {
        job: Option<(PathBuf, String)>,
        busy: bool,
    }

    static QUEUE: Mutex<Queue> = Mutex::new(Queue { job: None, busy: false });
    static CHANGED: Condvar = Condvar::new();
    static START: Once = Once::new();
    static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    pub fn submit(path: PathBuf, text: String) {
        START.call_once(|| {
            let ok = std::thread::Builder::new().name("miniutils-config".into()).spawn(run).is_ok();
            STARTED.store(ok, std::sync::atomic::Ordering::Relaxed);
        });
        if !STARTED.load(std::sync::atomic::Ordering::Relaxed) {
            // no thread: write right here
            super::write_logged(&path, &text);
            return;
        }
        let mut q = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
        q.job = Some((path, text));
        CHANGED.notify_all();
    }

    pub fn flush() {
        let mut q = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
        while q.job.is_some() || q.busy {
            q = CHANGED.wait(q).unwrap_or_else(|e| e.into_inner());
        }
    }

    fn run() {
        loop {
            let (path, text) = {
                let mut q = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
                loop {
                    if let Some(job) = q.job.take() {
                        q.busy = true;
                        break job;
                    }
                    q = CHANGED.wait(q).unwrap_or_else(|e| e.into_inner());
                }
            };
            super::write_logged(&path, &text);
            let mut q = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
            q.busy = false;
            CHANGED.notify_all();
        }
    }
}

/// Deserializes the (already migrated) TOML table into the configuration.
fn deserialize(table: toml::Table) -> Result<Config, String> {
    toml::Value::Table(table).try_into::<Config>().map_err(|e| e.to_string())
}

/// Brings the table from version `from` up to `CURRENT_VERSION`, step by step.
fn migrate(t: &mut toml::Table, from: u32) {
    for v in from..CURRENT_VERSION {
        match v {
            2 => migrate_2_to_3(t),
            3 => migrate_3_to_4(t),
            _ => {}
        }
    }
    t.insert("version".to_string(), toml::Value::Integer(CURRENT_VERSION.into()));
}

/// v2 -> v3: the CD Player mini-app comes enabled (in the first free spot of the grid).
fn migrate_2_to_3(t: &mut toml::Table) {
    let Some(list) = t.get_mut("enabled").and_then(|v| v.as_array_mut()) else { return };
    if !list.iter().any(|v| v.as_str() == Some("cyber.cd")) {
        list.push(toml::Value::String("cyber.cd".to_string()));
    }
}

/// v3 -> v4: the single grid becomes the "General" page (sidebar), with the previous
/// columns, rows and enabled apps. Positions in `layout` still apply.
fn migrate_3_to_4(t: &mut toml::Table) {
    let mut page = toml::Table::new();
    page.insert("name".into(), toml::Value::String(tr!("General").into()));
    page.insert("icon".into(), toml::Value::String("neko".into()));
    page.insert("columns".into(), t.remove("columns").unwrap_or(toml::Value::Integer(2)));
    page.insert("rows".into(), t.remove("rows").unwrap_or(toml::Value::Integer(1)));
    let apps = t.get("enabled").cloned().unwrap_or_else(|| toml::Value::Array(Vec::new()));
    page.insert("apps".into(), apps);
    t.insert("pages".into(), toml::Value::Array(vec![toml::Value::Table(page)]));
}

/// Copies the file, still at format `version`, to `config.toml.v<version>.bak` (`.1`, `.2`…
/// if taken): each migration keeps its own original. Returns `false` on failure.
fn backup(path: &Path, version: u32) -> bool {
    let bak = free_name(&with_suffix(path, &format!(".v{version}.bak")));
    match std::fs::copy(path, &bak) {
        Ok(_) => true,
        Err(e) => {
            log::warn!("could not create {}: {e}", bak.display());
            false
        }
    }
}

/// Path with an extra suffix, such as `config.toml.bak`.
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// First free name: `base`, then `base.1`, `base.2`…
fn free_name(base: &Path) -> PathBuf {
    let mut p = base.to_path_buf();
    let mut n = 1;
    while p.exists() {
        p = with_suffix(base, &format!(".{n}"));
        n += 1;
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Temporary directory of its own for each test; deleted at the end.
    struct Tmp(PathBuf);

    impl Tmp {
        fn new(name: &str) -> Self {
            let d = std::env::temp_dir().join(format!("miniutils-test-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            Tmp(d)
        }

        fn file(&self) -> PathBuf {
            self.0.join("config.toml")
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn empty_file_is_not_migrated_and_saving_leaves_nothing_behind() {
        let tmp = Tmp::new("empty");
        let path = tmp.file();
        // an old interrupted write left the file empty
        std::fs::write(&path, "  \n").unwrap();
        let mut cfg = Config::load_from(&path);
        assert!(cfg.state == SaveState::Normal);
        // no backup of an empty file, and no "migrated" rewrite
        assert_eq!(std::fs::read_dir(&tmp.0).unwrap().count(), 1);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "  \n");

        cfg.name = "My hub".into();
        cfg.save_to(&path);
        // the temporary file became the file; nothing is left next to it
        assert_eq!(std::fs::read_dir(&tmp.0).unwrap().count(), 1);
        assert_eq!(Config::load_from(&path).name, "My hub");

        // when it cannot write (the target is a directory), what exists stays as it is
        let taken = tmp.0.join("taken");
        std::fs::create_dir_all(taken.join("inside")).unwrap();
        assert!(crate::core::os::write_atomic(&taken, b"x").is_err());
        assert!(taken.join("inside").is_dir());
        assert!(!with_suffix(&taken, ".tmp").exists());
    }

    #[test]
    fn too_old_version_is_set_aside_and_defaults_are_used() {
        let t = Tmp::new("v1");
        let v1 = "version = 1\ncolumns = 3\nenabled = [\"cyber.docker\"]\n";
        std::fs::write(t.file(), v1).unwrap();

        let mut cfg = Config::load_from(&t.file());
        assert!(cfg.state == SaveState::Broken);
        assert_eq!(cfg.enabled, Config::default().enabled);
        // the original stays until the first save, which moves it aside
        assert_eq!(std::fs::read_to_string(t.file()).unwrap(), v1);
        cfg.save_to(&t.file());
        assert_eq!(std::fs::read_to_string(with_suffix(&t.file(), ".broken")).unwrap(), v1);
    }

    #[test]
    fn v2_enables_the_cd_player_only_once() {
        let t = Tmp::new("v2-cd");
        std::fs::write(t.file(), "version = 2\ncolumns = 2\nenabled = [\"cyber.docker\"]\n").unwrap();

        let cfg = Config::load_from(&t.file());
        assert_eq!(cfg.version, CURRENT_VERSION);
        assert_eq!(cfg.enabled, vec!["cyber.docker".to_string(), "cyber.cd".to_string()]);
        assert!(with_suffix(&t.file(), ".v2.bak").exists());

        // already migrated: reading again does not duplicate the app
        let cfg = Config::load_from(&t.file());
        assert_eq!(cfg.enabled.iter().filter(|e| *e == "cyber.cd").count(), 1);
    }

    #[test]
    fn arrange_finds_a_spot_for_a_re_enabled_app() {
        let page = Page { columns: 2, rows: 1, apps: vec!["a".into(), "b".into()], ..Page::default() };
        let mut cfg = Config { pages: vec![page], enabled: vec!["a".into()], ..Config::default() };
        cfg.layout.insert("a".into(), Rect::new(0, 0, 2, 1));
        cfg.layout.insert("b".into(), Rect::new(0, 0, 1, 1));
        cfg.enabled.push("b".into());
        assert!(cfg.arrange());
        assert_eq!(cfg.layout["a"], Rect::new(0, 0, 2, 1));
        assert_eq!(cfg.layout["b"], Rect::new(0, 1, 1, 1));
        assert_eq!(cfg.pages[0].rows, 2);
        assert!(!cfg.arrange());
    }

    #[test]
    fn v3_becomes_the_general_page() {
        let t = Tmp::new("v3");
        std::fs::write(
            t.file(),
            "version = 3\ncolumns = 3\nrows = 2\nenabled = [\"cyber.docker\", \"cyber.cd\"]\n\
             [layout.\"cyber.docker\"]\nx = 1\ny = 1\nw = 2\nh = 1\n",
        )
        .unwrap();

        let cfg = Config::load_from(&t.file());
        assert_eq!(cfg.version, CURRENT_VERSION);
        assert_eq!(cfg.pages.len(), 1);
        let p = &cfg.pages[0];
        assert_eq!((p.name.as_str(), p.columns, p.rows), ("General", 3, 2));
        assert_eq!(p.apps, vec!["cyber.docker".to_string(), "cyber.cd".to_string()]);
        assert_eq!(cfg.layout["cyber.docker"], Rect::new(1, 1, 2, 1));
        let new = std::fs::read_to_string(t.file()).unwrap();
        let table: toml::Table = new.parse().unwrap();
        assert!(!table.contains_key("columns") && !table.contains_key("rows"));
    }

    #[test]
    fn existing_page_names_are_kept() {
        let t = Tmp::new("page-name");
        std::fs::write(t.file(), format!("version = {CURRENT_VERSION}\n[[pages]]\nname = \"Geral\"\n")).unwrap();
        assert_eq!(Config::load_from(&t.file()).pages[0].name, "Geral");
    }

    #[test]
    fn enabled_app_without_a_page_goes_to_the_first() {
        let mut cfg = Config { pages: vec![Page::default(), Page::default()], enabled: vec!["a".into()], ..Config::default() };
        cfg.arrange();
        assert_eq!(cfg.page_of("a"), Some(0));
        assert_eq!(cfg.page_apps(0), vec!["a".to_string()]);
    }

    #[test]
    fn find_page_by_name_or_by_app() {
        let page = |name: &str, apps: &[&str]| Page {
            name: name.into(),
            apps: apps.iter().map(|a| a.to_string()).collect(),
            ..Page::default()
        };
        let cfg = Config {
            pages: vec![page("General", &["cyber.vitals"]), page("AI", &["cyber.docker", "io.github.x.uptime"]), page("Docker", &[])],
            ..Config::default()
        };
        assert_eq!(cfg.find_page("general"), Some(0));
        assert_eq!(cfg.find_page(" AI "), Some(1));
        assert_eq!(cfg.find_page("cyber.vitals"), Some(0));
        assert_eq!(cfg.find_page("Vitals"), Some(0));
        assert_eq!(cfg.find_page("uptime"), Some(1));
        // the page called "Docker" wins over the cyber.docker app
        assert_eq!(cfg.find_page("docker"), Some(2));
        assert_eq!(cfg.find_page("cyber"), None);
        assert_eq!(cfg.find_page("nothing"), None);
    }

    #[test]
    fn move_and_delete_pages() {
        let mut cfg = Config { pages: vec![Page::default()], enabled: vec!["a".into(), "b".into()], ..Config::default() };
        cfg.arrange();
        let p = cfg.add_page();
        assert_eq!(p, 1);
        assert_eq!(cfg.pages[1].name, "Page 2");
        assert_ne!(cfg.pages[1].icon, cfg.pages[0].icon);
        cfg.move_to_page("b", 1);
        cfg.arrange();
        assert_eq!(cfg.page_apps(0), vec!["a".to_string()]);
        assert_eq!(cfg.page_apps(1), vec!["b".to_string()]);
        // a moved app stays where it was, if it fits the new grid
        assert_eq!(cfg.rects(1), vec![Rect::new(1, 0, 1, 1)]);

        assert!(cfg.remove_page(1));
        assert_eq!(cfg.page_of("b"), Some(0));
        // the last page stays
        assert!(!cfg.remove_page(0));
    }

    #[test]
    fn current_version_loads_without_migrating_or_backing_up() {
        let t = Tmp::new("current");
        std::fs::write(t.file(), format!("version = {CURRENT_VERSION}\nskin = \"matrix\"\nenabled = []\n")).unwrap();

        let cfg = Config::load_from(&t.file());
        assert_eq!(cfg.skin, "matrix");
        assert!(cfg.enabled.is_empty());
        assert_eq!(std::fs::read_dir(&t.0).unwrap().count(), 1, "no backup");
    }

    #[test]
    fn each_migration_keeps_its_own_original() {
        let t = Tmp::new("two-migrations");
        let v2 = "version = 2\ncolumns = 2\nenabled = [\"cyber.docker\"]\n";
        std::fs::write(t.file(), v2).unwrap();
        Config::load_from(&t.file());
        // later, a v3 file (e.g. written by an older build) is migrated again
        let v3 = "version = 3\ncolumns = 3\nenabled = [\"cyber.vitals\"]\n";
        std::fs::write(t.file(), v3).unwrap();
        Config::load_from(&t.file());
        assert_eq!(std::fs::read_to_string(with_suffix(&t.file(), ".v2.bak")).unwrap(), v2);
        assert_eq!(std::fs::read_to_string(with_suffix(&t.file(), ".v3.bak")).unwrap(), v3);
        // the same version migrated twice: the first copy is not overwritten
        std::fs::write(t.file(), v2.replace("docker", "ports")).unwrap();
        Config::load_from(&t.file());
        assert_eq!(std::fs::read_to_string(with_suffix(&t.file(), ".v2.bak")).unwrap(), v2);
        assert!(with_suffix(&t.file(), ".v2.bak.1").exists());
    }

    #[test]
    fn hand_edited_values_are_brought_back_in_range() {
        let t = Tmp::new("normalize");
        std::fs::write(
            t.file(),
            format!("version = {CURRENT_VERSION}\ntext_scale = 400\nlanguage = \"klingon\"\nwindow_place = \"moon\"\n"),
        )
        .unwrap();
        let cfg = Config::load_from(&t.file());
        assert_eq!(cfg.text_scale, TEXT_SCALE_MAX);
        assert_eq!(cfg.language, "auto");
        assert_eq!(cfg.window_place, "tray");
        assert!(cfg.load_notice().is_none());
        std::fs::write(t.file(), format!("version = {CURRENT_VERSION}\ntext_scale = 10\nlanguage = \"pt-BR\"\n")).unwrap();
        let cfg = Config::load_from(&t.file());
        assert_eq!((cfg.text_scale, cfg.language.as_str()), (TEXT_SCALE_MIN, "pt-BR"));
    }

    #[test]
    fn app_setting_of_the_wrong_type_uses_the_default() {
        let m = Manifest::parse(
            "id='a.b'\nname='x'\napi=1\n\
             [[settings]]\nkey='alarm'\nlabel='A'\ntype='int'\ndefault=90\nmin=50\nmax=100\n\
             [[settings]]\nkey='on'\nlabel='O'\ntype='bool'\ndefault=true\n",
        )
        .unwrap();
        let mut cfg = Config::default();
        cfg.set("a.b", "alarm", toml::Value::String("70".into()));
        cfg.set("a.b", "on", toml::Value::Integer(0));
        assert_eq!(cfg.int(&m, "alarm"), 90);
        assert!(cfg.bool(&m, "on"));
        // right type but out of range: clamped to the manifest's min/max
        cfg.set("a.b", "alarm", toml::Value::Integer(400));
        assert_eq!(cfg.int(&m, "alarm"), 100);
        cfg.set("a.b", "alarm", toml::Value::Integer(-3));
        assert_eq!(cfg.int(&m, "alarm"), 50);
        cfg.set("a.b", "alarm", toml::Value::Integer(70));
        assert_eq!(cfg.int(&m, "alarm"), 70);
    }

    #[test]
    fn future_version_is_read_only() {
        let t = Tmp::new("future");
        let original = format!("version = {}\nskin = \"matrix\"\nnovelty = true\n", CURRENT_VERSION + 1);
        std::fs::write(t.file(), &original).unwrap();

        let mut cfg = Config::load_from(&t.file());
        assert_eq!(cfg.skin, "matrix");
        cfg.skin = "neon".into();
        assert!(cfg.load_notice().is_some());
        cfg.save_to(&t.file());
        assert_eq!(std::fs::read_to_string(t.file()).unwrap(), original);
        assert_eq!(std::fs::read_dir(&t.0).unwrap().count(), 1, "no backup");
    }

    #[test]
    fn broken_toml_is_kept_on_save() {
        let t = Tmp::new("broken");
        let broken = "skin = [\n";
        std::fs::write(t.file(), broken).unwrap();

        let mut cfg = Config::load_from(&t.file());
        assert_eq!(cfg.skin, Config::default().skin);
        assert!(cfg.load_notice().is_some());
        // nothing is written until the first save
        assert_eq!(std::fs::read_to_string(t.file()).unwrap(), broken);

        cfg.skin = "matrix".into();
        cfg.save_to(&t.file());
        assert_eq!(std::fs::read_to_string(with_suffix(&t.file(), ".broken")).unwrap(), broken);
        let new = Config::load_from(&t.file());
        assert_eq!(new.skin, "matrix");
        assert_eq!(new.version, CURRENT_VERSION);
    }

    #[test]
    fn save_without_a_file_creates_it_with_the_version() {
        let t = Tmp::new("new");
        let mut cfg = Config::load_from(&t.file());
        cfg.save_to(&t.file());
        let text = std::fs::read_to_string(t.file()).unwrap();
        assert!(text.contains(&format!("version = {CURRENT_VERSION}")));
        assert!(!with_suffix(&t.file(), ".broken").exists());
    }

    /// The config can hold `secret` settings (tokens): only the user reads it.
    #[cfg(unix)]
    #[test]
    fn saved_config_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let t = Tmp::new("private");
        // an existing file with a looser mode is replaced by a private one
        std::fs::write(t.file(), "version = 1\n").unwrap();
        std::fs::set_permissions(t.file(), std::fs::Permissions::from_mode(0o644)).unwrap();
        let mut cfg = Config::load_from(&t.file());
        cfg.set("a.b", "token", toml::Value::String("cix_x".into()));
        cfg.save_to(&t.file());
        let mode = std::fs::metadata(t.file()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
