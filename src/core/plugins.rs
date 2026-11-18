//! Community plugins (proof of concept): mini-apps installed from a repository.
//!
//! There are three kinds of plugin. The first two have no native code:
//! - **simple**: the `manifest.toml` says which command to run and how to show it (below);
//!   the host runs it and shows it on a generic screen;
//! - **with its own view**: `[ui]` points to a `view.slint` (interpreted at run time) and a
//!   `main.rhai` (logic, in a sandboxed interpreter). See `apps/plugin/`.
//!
//! The third, the **external app** (`[app]`), has **full access**: its logic is its own
//! executable (any language), which the host starts when the app is enabled and talks to
//! through JSON on standard input and output (protocol in `apps/external.rs`). It runs with
//! the user's permissions, so it declares no `[permissions]`; the store announces it as
//! full access and checks the binary's hash (see `store.rs`).
//!
//! ```toml
//! uses = ["sys"]               # providers forwarded to the app ("event" message)
//! [app]
//! view = "view.slint"          # the view, as in a plugin with [ui]
//! linux = "bin/my-app"         # the executable for each system, in bin/ (one is enough)
//! windows = "bin/my-app.exe"
//! every = 10                   # "tick" message every N seconds (optional; 1 to 3600)
//! ```
//!
//! The `manifest.toml` has the same format as the built-in mini-apps, with extra sections:
//!
//! ```toml
//! id = "io.github.someone.uptime"  # unique; `cyber.*` belongs to the built-in apps
//! name = "Uptime"
//! icon = "coruja"
//! api = 1                        # 2 for text/secret settings
//! [permissions]
//! exec = ["uptime"]                # only these programs may run (no shell)
//! [run]
//! command = ["uptime", "-p"]       # what to run
//! every = 30                       # every how many seconds (5 to 3600)
//! [display]
//! title = "Up for"
//! [[mood]]                         # mood from the output: the first matching rule wins
//! contains = "week"
//! mood = "happy"
//! ```
//!
//! With no matching rule, the mood is "idle"; empty output is "sleep"; a failing command is
//! "alarm".
//!
//! A plugin with its own view, instead of `[run]`:
//!
//! ```toml
//! uses = ["sys", "procs"] # providers the script gets in fn on_event(topic, data)
//! [permissions]
//! exec = ["git"]
//! net = ["api.example.com"]   # domains http_get() may query (https only)
//! [ui]
//! view = "view.slint"     # the view: exports `component Main`
//! script = "main.rhai"    # fn start(), fn tick(), fn action(name, arg)…
//! every = 10              # tick every N seconds (optional; 1 to 3600)
//! ```
//!
//! Files of a plugin (`files`): those in `TOP_FILES` at the root, plus `ui/*.slint` (views
//! imported by `view.slint`), `assets/*.png|jpg|svg` (images, through `@image-url`) and
//! `i18n/*.po` (translations of the view and manifest texts, see `core::i18n`). No
//! links or other folders; binaries only in `bin/` of an external app. The data lives
//! outside the plugin folder, in the app's data folder (`core/appdata.rs`), so it survives
//! an update. What the app stores goes in `[data]`:
//!
//! ```toml
//! [data]
//! kinds = ["sqlite", "kv"]   # script: kv (store_set) and/or sqlite (db_exec); external app: files
//! version = 1                # data format; bumping it calls fn migrate(from, to)
//! max_mb = 10                # database quota
//! ```
//!
//! Without `[data]`, a script gets `kv` and an external app `files`; a simple plugin stores
//! nothing.
//!
//! Installed plugins live in `dir()`: `~/.local/share/miniutils/plugins/<id>/` on Linux,
//! `%APPDATA%\miniutils\plugins\<id>\` on Windows. The store installs there (see `store.rs`)
//! and anyone may drop an app there by hand (a local app, without `installed.json`); `scan`
//! says which folders did not become an app and why. With `miniutils --plugin-dev <folder>`
//! (repeatable), the plugins under development come along (`set_dev`).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::Deserialize;

use super::appdata::{DataSpec, Kind};
use super::manifest::{Manifest, SettingDef, SettingKind};
use crate::mascot::sprites;
use crate::{tr, tr_noop};

/// Files at the root of a plugin.
const TOP_FILES: &[&str] =
    &["manifest.toml", "view.slint", "main.rhai", "README.md", "LICENSE", "LICENSE.md", "LICENSE.txt"];
/// Subfolders and the extensions accepted in each.
const SUBDIRS: &[(&str, &[&str])] =
    &[("ui", &["slint"]), ("assets", &["png", "jpg", "jpeg", "svg"]), ("i18n", &["po"])];
/// Limits: per file, in total and in number of files.
pub const MAX_FILE: u64 = 1 << 20;
const MAX_TOTAL: u64 = 8 << 20;
const MAX_FILES: usize = 64;
/// Executables of an external app (`bin/`): per file. Not counted in the total above.
pub const MAX_BIN: u64 = 256 << 20;
/// The folder with an external app's executables.
const BIN_DIR: &str = "bin";

/// A plugin ready for the host to run.
#[derive(Debug, Clone)]
pub struct Plugin {
    /// Folder it was read from (empty when it came from the manifest text only).
    pub dir: PathBuf,
    pub manifest: Manifest,
    /// Simple plugin: the command the host runs.
    pub run: Option<Run>,
    /// Plugin with its own view and script.
    pub ui: Option<Ui>,
    /// External app, with full access.
    pub app: Option<AppSpec>,
    pub display: Display,
    pub moods: Vec<MoodRule>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Run {
    pub command: Vec<String>,
    pub every: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ui {
    pub view: String,
    pub script: String,
    /// Interval of the script's `tick()`, in seconds (none = never).
    pub every: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppSpec {
    pub view: String,
    /// Executable on Linux (`bin/<name>`, relative to the app folder).
    #[serde(default)]
    pub linux: Option<String>,
    /// Executable on Windows (`bin/<name>.exe`).
    #[serde(default)]
    pub windows: Option<String>,
    /// Interval of the `tick` message, in seconds (none = never).
    pub every: Option<u64>,
}

impl AppSpec {
    /// This system's executable (relative to the app folder), if the app has one.
    pub fn binary(&self) -> Option<&str> {
        if cfg!(windows) { self.windows.as_deref() } else { self.linux.as_deref() }
    }

    /// The declared executables, each with its system ("linux", "windows").
    pub fn binaries(&self) -> Vec<(&'static str, &str)> {
        [("linux", &self.linux), ("windows", &self.windows)]
            .into_iter()
            .filter_map(|(os, b)| b.as_deref().map(|b| (os, b)))
            .collect()
    }
}

/// The system this binary runs on, as the manifest and the index call it.
pub fn this_os() -> &'static str {
    if cfg!(windows) { "windows" } else { "linux" }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Display {
    /// Title above the output (default: the plugin name).
    #[serde(default)]
    pub title: String,
    /// How many lines of output to show (1 to 6; default 3).
    #[serde(default)]
    pub lines: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MoodRule {
    pub contains: String,
    pub mood: String,
}

/// The sections only plugins have.
#[derive(Deserialize)]
struct Extra {
    run: Option<Run>,
    ui: Option<Ui>,
    app: Option<AppSpec>,
    #[serde(default)]
    display: Display,
    #[serde(default)]
    mood: Vec<MoodRule>,
}

/// A program name for the PATH lookup (`ls`, `docker`), not a path on either system: no
/// `/`, `\`, drive letter (`C:`) or leading dot (`..`, hidden files).
fn plain_program(p: &str) -> bool {
    !p.trim().is_empty() && !p.contains(['/', '\\', ':']) && !p.starts_with('.')
}

impl Plugin {
    /// Reads and validates a plugin manifest. The errors are shown to the user (Settings ›
    /// System, `--plugins`, `--plugin-dev`), so they are translated.
    pub fn parse(text: &str) -> Result<Plugin, String> {
        let mut manifest = Manifest::parse(text).map_err(|e| e.to_string())?;
        let extra: Extra = toml::from_str(text).map_err(|e| e.to_string())?;
        validate_id(&manifest.id)?;
        if let Some(p) = manifest.permissions.exec.iter().find(|p| !plain_program(p)) {
            return Err(tr!("[permissions] exec: use the program name, not a path: {}", p));
        }
        if let Some(h) = manifest.permissions.net.iter().find(|h| !valid_host(h)) {
            return Err(tr!("[permissions] net: \"{}\" is not a domain (e.g. api.example.com, without https:// or a path)", h));
        }
        match [extra.run.is_some(), extra.ui.is_some(), extra.app.is_some()].iter().filter(|s| **s).count() {
            0 => {
                return Err(tr!("missing the [run] (simple plugin), [ui] (own view) or [app] (full-access app) section").into());
            }
            1 => {}
            _ => return Err(tr!("use only one of the [run], [ui] or [app] sections").into()),
        }
        if let Some(app) = &extra.app {
            // the executable runs with the user's permissions: an allowlist would not tell the truth
            if !manifest.permissions.exec.is_empty() || !manifest.permissions.net.is_empty() {
                return Err(tr!("[app] has full access: do not declare [permissions] (the store announces it as full access)").into());
            }
            if app.view != "view.slint" {
                return Err(tr!("[app] use view = \"view.slint\"").into());
            }
            if app.every.is_some_and(|e| !(1..=3600).contains(&e)) {
                return Err(tr!("[app] every: use 1 to 3600 seconds").into());
            }
            if app.binaries().is_empty() {
                return Err(tr!("[app] missing the executable: linux = \"bin/<name>\" and/or windows = \"bin/<name>.exe\"").into());
            }
            if let Some((os, b)) = app.binaries().into_iter().find(|(_, b)| !valid_bin(b)) {
                return Err(tr!("[app] {} = \"{}\": the executable goes in bin/ (e.g. bin/my-app)", os, b));
            }
        }
        // providers and network reach the script (or the app): a simple plugin has nobody to use them
        if extra.run.is_some() && !manifest.uses.is_empty() {
            return Err(tr!("uses only applies to a plugin with [ui] or [app] (the script gets it in fn on_event)").into());
        }
        if extra.ui.is_none() && !manifest.permissions.net.is_empty() {
            return Err(tr!("[permissions] net only applies to a plugin with [ui] (the script uses http_get)").into());
        }
        if let Some(run) = &extra.run {
            let program = run.command.first().ok_or(tr!("[run] command is empty"))?;
            if !plain_program(program) {
                return Err(tr!("[run] use the program name, not a path: {}", program));
            }
            if !manifest.permissions.exec.contains(program) {
                return Err(tr!("[run] runs \"{}\", which is not in [permissions] exec", program));
            }
            if !(5..=3600).contains(&run.every) {
                return Err(tr!("[run] every = {}: use 5 to 3600 seconds", run.every));
            }
        }
        if let Some(ui) = &extra.ui {
            if ui.view != "view.slint" || ui.script != "main.rhai" {
                return Err(tr!("[ui] use view = \"view.slint\" and script = \"main.rhai\"").into());
            }
            if ui.every.is_some_and(|e| !(1..=3600).contains(&e)) {
                return Err(tr!("[ui] every: use 1 to 3600 seconds").into());
            }
        }
        if let Some(d) = &manifest.data {
            let bad = |k: &Kind| match (&extra.run, &extra.ui) {
                (Some(_), _) => true,
                (_, Some(_)) => *k == Kind::Files,
                _ => *k != Kind::Files,
            };
            if let Some(k) = d.kinds.iter().find(|k| bad(k)) {
                return Err(match (&extra.run, &extra.ui) {
                    (Some(_), _) => tr!("[data]: a simple plugin stores no data (use [ui] or [app])").to_string(),
                    (_, Some(_)) => tr!("[data] kinds: \"{}\" does not apply to a script (use \"kv\" and/or \"sqlite\")", k.key()),
                    _ => tr!("[data] kinds: \"{}\" does not apply to an external app (it writes whatever it wants: use \"files\")", k.key()),
                });
            }
        }
        if let Some(n) = extra.display.lines
            && !(1..=6).contains(&n) {
                return Err(tr!("[display] lines = {}: use 1 to 6", n));
            }
        for r in &extra.mood {
            if !["sleep", "idle", "happy", "busy", "alarm"].contains(&r.mood.as_str()) {
                return Err(tr!("[[mood]] unknown mood: {}", r.mood));
            }
        }
        if sprites::find(&manifest.icon).is_none() {
            return Err(tr!("icon = \"{}\": there is no such mascot", manifest.icon));
        }
        add_standard_settings(&mut manifest);
        Ok(Plugin {
            dir: PathBuf::new(),
            manifest,
            run: extra.run,
            ui: extra.ui,
            app: extra.app,
            display: extra.display,
            moods: extra.mood,
        })
    }

    /// Reads the plugin in folder `dir`.
    pub fn load(dir: &Path) -> Result<Plugin, String> {
        let text = std::fs::read_to_string(dir.join("manifest.toml")).map_err(|e| format!("manifest.toml: {e}"))?;
        let mut p = Plugin::parse(&text)?;
        p.dir = dir.to_path_buf();
        // texts of the manifest in the interface language (the app's own i18n/<lang>.po)
        let catalog = crate::core::i18n::Catalog::of_app(dir, crate::core::i18n::current());
        catalog.localize_manifest(&mut p.manifest);
        if let Some(t) = catalog.get(&p.display.title) {
            p.display.title = t.to_string();
        }
        Ok(p)
    }

    /// What the plugin may do, to show before installing ("runs: ps · network: api.x.com ·
    /// reads: sys").
    pub fn grants(&self) -> String {
        let uses: Vec<String> = self.manifest.uses.iter().map(|t| t.name().to_string()).collect();
        if self.app.is_some() {
            return describe_full_access(&uses);
        }
        describe_grants(&self.manifest.permissions.exec, &self.manifest.permissions.net, &uses, self.data().has(Kind::Sqlite))
    }

    /// What the app stores: the manifest's `[data]` or, without it, the default for its kind
    /// (`kv` for a script, `files` for an external app, nothing for a simple plugin).
    pub fn data(&self) -> DataSpec {
        if let Some(d) = &self.manifest.data {
            return d.clone();
        }
        DataSpec::with(match (&self.ui, &self.app) {
            (Some(_), _) => &[Kind::Kv],
            (_, Some(_)) => &[Kind::Files],
            _ => &[],
        })
    }

    /// External app: runs its own executable with the user's permissions.
    pub fn full_access(&self) -> bool {
        self.app.is_some()
    }

    /// This system's executable (full path), for an external app.
    pub fn binary_path(&self) -> Option<PathBuf> {
        self.app.as_ref()?.binary().map(|b| self.dir.join(b))
    }

    pub fn title(&self) -> &str {
        if self.display.title.is_empty() { &self.manifest.name } else { &self.display.title }
    }

    pub fn lines(&self) -> usize {
        self.display.lines.unwrap_or(3)
    }
}

/// Plugin id: lowercase letters, digits, `.`, `_` and `-`, with at least one dot (e.g.
/// `io.github.someone.uptime`). `cyber.*` is reserved for the built-in apps.
pub fn validate_id(id: &str) -> Result<(), String> {
    let ok_chars = id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'));
    let parts_ok = id.split('.').count() >= 2 && id.split('.').all(|p| !p.is_empty());
    if !ok_chars || !parts_ok || id.len() > 80 {
        return Err(tr!("invalid id: \"{}\" (e.g. io.github.someone.my-app)", id));
    }
    if id.starts_with("cyber.") {
        return Err(tr!("id \"{}\": the cyber. prefix belongs to the built-in apps", id));
    }
    Ok(())
}

/// Domain for `[permissions] net`: lowercase letters, digits, `-` and `.`, with at least
/// one dot (no scheme, port or path).
pub fn valid_host(h: &str) -> bool {
    let chars = h.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.');
    let labels = h.split('.').all(|l| !l.is_empty() && !l.starts_with('-') && !l.ends_with('-'));
    chars && labels && h.contains('.') && h.len() <= 253
}

/// Text of the permissions: "runs: ps · network: api.x.com · reads: sys · stores: SQLite
/// database" (or "runs no programs and does not use the network" when it asks for nothing).
pub fn describe_grants(exec: &[String], net: &[String], uses: &[String], sqlite: bool) -> String {
    let mut parts = Vec::new();
    if !exec.is_empty() {
        parts.push(tr!("runs: {}", exec.join(", ")));
    }
    if !net.is_empty() {
        parts.push(tr!("network: {}", net.join(", ")));
    }
    if !uses.is_empty() {
        parts.push(tr!("reads: {}", uses.join(", ")));
    }
    if sqlite {
        parts.push(crate::core::i18n::tr(GRANT_SQLITE).into());
    }
    if parts.is_empty() { tr!("runs no programs and does not use the network").into() } else { parts.join(" · ") }
}

/// The database in the permission list (and in an update's new permissions). Translate it
/// with `i18n::tr` where it is shown.
pub const GRANT_SQLITE: &str = tr_noop!("stores: SQLite database");

/// Text of an external app's permissions: "FULL ACCESS: … · reads: sys".
pub fn describe_full_access(uses: &[String]) -> String {
    let mut out = String::from(tr!("FULL ACCESS: runs its own program with your user's permissions"));
    if !uses.is_empty() {
        out.push_str(" · ");
        out.push_str(&tr!("reads: {}", uses.join(", ")));
    }
    out
}

/// Executable of an external app: `bin/<name>`, with a plain name (no subfolder or `..`).
fn valid_bin(path: &str) -> bool {
    path.strip_prefix("bin/").is_some_and(safe_name)
}

/// Plain file name: letters, digits, `-`, `_` and `.`, not starting with a dot.
fn safe_name(name: &str) -> bool {
    !name.is_empty() && !name.starts_with('.') && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Does the manifest in `dir` declare an external app (`[app]`)?
fn declares_app(dir: &Path) -> bool {
    std::fs::read_to_string(dir.join("manifest.toml"))
        .ok()
        .and_then(|t| toml::from_str::<Extra>(&t).ok())
        .is_some_and(|e| e.app.is_some())
}

/// The files of the plugin in `dir` (relative paths), checked: regular files only (no
/// links), within the limits. Files of other types are ignored.
pub fn files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut total = 0u64;
    let mut take = |rel: PathBuf| -> Result<(), String> {
        let meta = std::fs::symlink_metadata(dir.join(&rel)).map_err(|e| format!("{}: {e}", rel.display()))?;
        if !meta.file_type().is_file() {
            return Err(tr!("{} is not a regular file", rel.display()));
        }
        if meta.len() > MAX_FILE {
            return Err(tr!("{} is over 1 MiB", rel.display()));
        }
        total += meta.len();
        out.push(rel);
        Ok(())
    };
    for name in TOP_FILES {
        if std::fs::symlink_metadata(dir.join(name)).is_ok() {
            take(PathBuf::from(name))?;
        }
    }
    for (sub, exts) in SUBDIRS {
        let Ok(meta) = std::fs::symlink_metadata(dir.join(sub)) else { continue };
        if !meta.is_dir() {
            return Err(tr!("{}/ is not a folder", sub));
        }
        let mut names: Vec<String> = std::fs::read_dir(dir.join(sub))
            .map_err(|e| format!("{sub}/: {e}"))?
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        for name in names {
            let ext = Path::new(&name).extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
            if safe_name(&name) && exts.contains(&ext.as_str()) {
                take(Path::new(sub).join(&name))?;
            }
        }
    }
    if declares_app(dir) {
        out.extend(bin_files(dir)?);
    }
    if out.len() > MAX_FILES {
        return Err(tr!("too many files ({}; the limit is {})", out.len(), MAX_FILES));
    }
    if total > MAX_TOTAL {
        return Err(tr!("the files are over 8 MiB together").into());
    }
    Ok(out)
}

/// The executables of an external app (`bin/`), checked: regular files with plain names,
/// up to `MAX_BIN` each. In dev mode a link is allowed (to the app project's `target/`).
fn bin_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let bin = dir.join(BIN_DIR);
    let Ok(meta) = std::fs::symlink_metadata(&bin) else { return Ok(Vec::new()) };
    if !meta.is_dir() {
        return Err(tr!("{}/ is not a folder", BIN_DIR));
    }
    let dev = is_dev(dir);
    let mut names: Vec<String> = std::fs::read_dir(&bin)
        .map_err(|e| format!("{BIN_DIR}/: {e}"))?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| safe_name(n))
        .collect();
    names.sort();
    let mut out = Vec::new();
    for name in names {
        let rel = Path::new(BIN_DIR).join(&name);
        let path = dir.join(&rel);
        let meta = if dev { std::fs::metadata(&path) } else { std::fs::symlink_metadata(&path) }
            .map_err(|e| format!("{}: {e}", rel.display()))?;
        if !meta.file_type().is_file() {
            return Err(tr!("{} is not a regular file", rel.display()));
        }
        if meta.len() > MAX_BIN {
            return Err(tr!("{} is over {} MiB", rel.display(), MAX_BIN >> 20));
        }
        out.push(rel);
    }
    Ok(out)
}

/// Is it a file inside `bin/` (an external app's executable)?
pub fn is_bin(rel: &Path) -> bool {
    rel.starts_with(BIN_DIR)
}

/// Plugins under development (`miniutils --plugin-dev <folder>`, once per folder).
static DEV: OnceLock<Vec<PathBuf>> = OnceLock::new();

/// Sets the development folders (canonical paths; repeated ones count once).
pub fn set_dev(mut dirs: Vec<PathBuf>) {
    let mut seen = std::collections::HashSet::new();
    dirs.retain(|d| seen.insert(d.clone()));
    let _ = DEV.set(dirs);
}

pub fn dev_dirs() -> &'static [PathBuf] {
    DEV.get().map_or(&[], Vec::as_slice)
}

/// Is `dir` one of the development folders?
pub fn is_dev(dir: &Path) -> bool {
    dev_dirs().iter().any(|d| d == dir)
}

/// The installed plugins and, in dev mode, the plugins in the development folders (each
/// replaces an installed one with the same id).
pub fn load_all(root: &Path) -> Vec<Plugin> {
    with_dev(load_installed(root), dev_dirs())
}

fn with_dev(mut list: Vec<Plugin>, dev: &[PathBuf]) -> Vec<Plugin> {
    let mut dev_ids: Vec<String> = Vec::new();
    for dir in dev {
        match Plugin::load(dir) {
            // two development folders with the same id: the first one on the command line wins
            Ok(p) if dev_ids.contains(&p.manifest.id) => {
                log::warn!("plugin-dev: {}: {} is already loaded from another folder", dir.display(), p.manifest.id)
            }
            Ok(p) => {
                list.retain(|o| o.manifest.id != p.manifest.id);
                dev_ids.push(p.manifest.id.clone());
                list.push(p);
            }
            Err(e) => log::warn!("plugin-dev: {}: {e}", dir.display()),
        }
    }
    list
}

/// Every app has a theme and a mascot; a plugin that declares neither gets the defaults.
fn add_standard_settings(m: &mut Manifest) {
    if m.setting("theme").is_none() {
        // the options (the themes) are filled in by the screen: `SettingDef::live_options`
        m.settings.push(SettingDef {
            key: "theme".into(),
            label: tr!("Theme").into(),
            kind: SettingKind::Choice,
            default: toml::Value::String("auto".into()),
            min: None,
            max: None,
            step: None,
            options: vec!["auto".into()],
            max_len: None,
        });
    }
    if m.setting("mascot").is_none() {
        let mut options = vec![m.icon.clone()];
        options.extend(sprites::all().iter().map(|s| s.key.to_string()).filter(|k| *k != m.icon));
        m.settings.push(SettingDef {
            key: "mascot".into(),
            label: tr!("Mascot").into(),
            kind: SettingKind::Choice,
            default: toml::Value::String(m.icon.clone()),
            min: None,
            max: None,
            step: None,
            options,
            max_len: None,
        });
    }
}

/// Folder of the installed plugins: `MINIUTILS_PLUGINS`, or `~/.local/share/miniutils/plugins`.
pub fn dir() -> PathBuf {
    match std::env::var_os("MINIUTILS_PLUGINS").filter(|v| !v.is_empty()) {
        Some(d) => PathBuf::from(d),
        None => dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("miniutils/plugins"),
    }
}

/// Valid plugins installed in `dir`, in id order. Invalid ones are left out (with the
/// reason in the log): a broken plugin does not keep the hub from opening.
pub fn load_installed(dir: &Path) -> Vec<Plugin> {
    let (list, skipped) = scan(dir);
    for s in skipped {
        log::warn!("plugin in {} skipped: {}", dir.join(&s.folder).display(), s.reason);
    }
    list
}

/// A folder of `dir` that did not become an app, and why (shown in Settings › System and in
/// `--plugins`).
#[derive(Debug, Clone, PartialEq)]
pub struct Skipped {
    pub folder: String,
    pub reason: String,
}

/// Reads `dir`: the valid plugins (in folder order) and the rejected folders. Logs
/// nothing; the caller decides where to show it.
pub fn scan(dir: &Path) -> (Vec<Plugin>, Vec<Skipped>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return (Vec::new(), Vec::new()) };
    let mut dirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    dirs.sort();
    let (mut out, mut skipped) = (Vec::new(), Vec::new());
    for d in dirs {
        let folder = d.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        // temporary folders of an install in progress, and `.data` (plugin data)
        if folder.starts_with('.') {
            continue;
        }
        if !d.join("manifest.toml").is_file() {
            skipped.push(Skipped { folder, reason: tr!("has no manifest.toml").into() });
            continue;
        }
        match Plugin::load(&d) {
            // the folder is the id, so there are never two plugins with the same id
            Ok(p) if folder == p.manifest.id => out.push(p),
            Ok(p) => skipped.push(Skipped {
                reason: tr!("the id in manifest.toml is \"{}\": the folder must have that name", p.manifest.id),
                folder,
            }),
            Err(e) => skipped.push(Skipped { folder, reason: e }),
        }
    }
    (out, skipped)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub const EXAMPLE: &str = include_str!("../../examples/plugin-uptime/manifest.toml");

    #[test]
    fn example_is_valid() {
        let p = Plugin::parse(EXAMPLE).unwrap();
        assert_eq!(p.run.as_ref().unwrap().command[0], "uptime");
        assert!(p.manifest.setting("theme").is_some());
        assert!(p.manifest.setting("mascot").is_some());
    }

    #[test]
    fn rejects_dangerous_or_malformed_plugins() {
        let base = "name='x'\napi=1\n";
        let case = |extra: &str| Plugin::parse(&format!("{base}{extra}")).unwrap_err();
        assert!(case("id='cyber.x'\n[permissions]\nexec=['ls']\n[run]\ncommand=['ls']\nevery=10").contains("cyber."));
        assert!(case("id='a.b'\n[run]\ncommand=['ls']\nevery=10").contains("not in [permissions]"));
        assert!(case("id='a.b'\n[permissions]\nexec=['/bin/ls']\n[run]\ncommand=['/bin/ls']\nevery=10").contains("path"));
        // Windows paths are paths too (checked on every system)
        assert!(case("id='a.b'\n[permissions]\nexec=['C:\\x\\y.exe']\n[run]\ncommand=['ls']\nevery=10").contains("path"));
        assert!(case("id='a.b'\n[permissions]\nexec=['..\\x']\n[run]\ncommand=['ls']\nevery=10").contains("path"));
        assert!(case("id='a.b'\n[permissions]\nexec=['ls', '']\n[run]\ncommand=['ls']\nevery=10").contains("path"));
        assert!(case("id='a.b'\n[permissions]\nexec=['ls']\n[run]\ncommand=['ls']\nevery=1").contains("every"));
        assert!(case("id='a.b'\n[permissions]\nexec=['ls']").contains("[run]"));
        assert!(case("id='a.b'\n[ui]\nview='../x.slint'\nscript='main.rhai'").contains("view.slint"));
        assert!(case("id='A b'\n[permissions]\nexec=['ls']\n[run]\ncommand=['ls']\nevery=10").contains("invalid id"));
        // providers and network only with a view and script
        assert!(case("id='a.b'\nuses=['sys']\n[permissions]\nexec=['ls']\n[run]\ncommand=['ls']\nevery=10").contains("uses"));
        assert!(case("id='a.b'\n[permissions]\nexec=['ls']\nnet=['a.com']\n[run]\ncommand=['ls']\nevery=10").contains("net"));
        let ui = "[ui]\nview='view.slint'\nscript='main.rhai'";
        assert!(case(&format!("id='a.b'\n[permissions]\nnet=['https://a.com']\n{ui}")).contains("domain"));
        assert!(case(&format!("id='a.b'\n[permissions]\nnet=['a.com/x']\n{ui}")).contains("domain"));
        let ok = Plugin::parse(&format!("{base}id='a.b'\nuses=['sys']\n[permissions]\nnet=['api.a.com']\n{ui}")).unwrap();
        assert_eq!(ok.grants(), "network: api.a.com · reads: sys");
    }

    #[test]
    fn external_app_valid_and_rejected() {
        let ok = Plugin::parse(include_str!("../../examples/app-hello/manifest.toml")).unwrap();
        let app = ok.app.as_ref().unwrap();
        assert!(ok.full_access());
        assert_eq!(app.binaries(), [("linux", "bin/app-hello"), ("windows", "bin/app-hello.exe")]);
        assert!(ok.grants().starts_with("FULL ACCESS"));
        assert!(ok.grants().ends_with("reads: sys"));
        let base = "name='x'\napi=1\nid='a.b'\n";
        let case = |extra: &str| Plugin::parse(&format!("{base}{extra}")).unwrap_err();
        let app = "[app]\nview='view.slint'\nlinux='bin/x'";
        assert!(case(&format!("[permissions]\nexec=['ls']\n{app}")).contains("full access"));
        assert!(case(&format!("[permissions]\nnet=['a.com']\n{app}")).contains("full access"));
        assert!(case("[app]\nview='view.slint'").contains("executable"));
        assert!(case("[app]\nview='view.slint'\nlinux='/usr/bin/x'").contains("bin/"));
        assert!(case("[app]\nview='view.slint'\nlinux='bin/../x'").contains("bin/"));
        assert!(case("[app]\nview='view.slint'\nlinux='bin/sub/x'").contains("bin/"));
        assert!(case(&format!("{app}\nevery=0")).contains("every"));
        assert!(case(&format!("{app}\n[ui]\nview='view.slint'\nscript='main.rhai'")).contains("only one"));
    }

    #[test]
    fn plugin_files_only_the_allowed_ones() {
        let dir = std::env::temp_dir().join(format!("miniutils-files-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("ui")).unwrap();
        std::fs::create_dir_all(dir.join("assets")).unwrap();
        std::fs::create_dir_all(dir.join("i18n")).unwrap();
        std::fs::create_dir_all(dir.join("other")).unwrap();
        for f in ["manifest.toml", "view.slint", "main.rhai", "ui/bar.slint", "ui/notes.txt", "assets/logo.png", "assets/.hidden.png", "i18n/pt-BR.po", "i18n/notes.txt", "other/x.slint", "build.rs"] {
            std::fs::write(dir.join(f), "x").unwrap();
        }
        // the folder separator depends on the system: compare with `/`
        let got: Vec<String> = files(&dir).unwrap().iter().map(|p| p.display().to_string().replace('\\', "/")).collect();
        assert_eq!(got, ["manifest.toml", "view.slint", "main.rhai", "ui/bar.slint", "assets/logo.png", "i18n/pt-BR.po"]);
        // a link with an allowed name: rejected (it does not follow outside the folder). Unix
        // only: creating a link on Windows needs a privilege
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc/hostname", dir.join("ui/outside.slint")).unwrap();
            assert!(files(&dir).unwrap_err().contains("ui/outside.slint"));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bin_only_counts_for_external_apps() {
        let dir = std::env::temp_dir().join(format!("miniutils-bin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join("bin/my-app"), "x").unwrap();
        std::fs::write(dir.join("bin/.hidden"), "x").unwrap();
        let manifest = |section: &str| std::fs::write(dir.join("manifest.toml"), format!("id='a.b'\nname='x'\napi=1\n{section}")).unwrap();
        let names = || -> Vec<String> { files(&dir).unwrap().iter().map(|p| p.display().to_string().replace('\\', "/")).collect() };
        // plugin with a script: bin/ is left out
        manifest("[ui]\nview='view.slint'\nscript='main.rhai'");
        assert_eq!(names(), ["manifest.toml"]);
        manifest("[app]\nview='view.slint'\nlinux='bin/my-app'");
        assert_eq!(names(), ["manifest.toml", "bin/my-app"]);
        assert!(is_bin(Path::new("bin/my-app")) && !is_bin(Path::new("binary.slint")));
        // outside dev mode, a link in bin/ is rejected
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/bin/sh", dir.join("bin/sh")).unwrap();
            assert!(files(&dir).unwrap_err().contains("bin/sh"));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_explains_the_rejected_folders() {
        let dir = std::env::temp_dir().join(format!("miniutils-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let folder = |name: &str, manifest: Option<&str>| {
            std::fs::create_dir_all(dir.join(name)).unwrap();
            if let Some(m) = manifest {
                std::fs::write(dir.join(name).join("manifest.toml"), m).unwrap();
            }
        };
        let id = Plugin::parse(EXAMPLE).unwrap().manifest.id;
        folder(&id, Some(EXAMPLE));
        folder("uptime", Some(EXAMPLE));
        folder("empty", None);
        folder("malformed", Some("id='a.b'\nname='x'\napi=1\n"));
        folder(".data", None);
        let (ok, rejected) = scan(&dir);
        assert_eq!(ok.len(), 1);
        assert_eq!(ok[0].manifest.id, id);
        let reason = |p: &str| rejected.iter().find(|s| s.folder == p).map(|s| s.reason.clone()).unwrap();
        assert!(reason("uptime").contains("the folder must have that name"));
        assert_eq!(reason("empty"), "has no manifest.toml");
        assert!(!reason("malformed").is_empty());
        assert_eq!(rejected.len(), 3, ".data is left out: {rejected:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn several_dev_folders() {
        let root = std::env::temp_dir().join(format!("miniutils-devs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let plugin = |name: &str, id: &str| {
            let dir = root.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            let text = include_str!("../../examples/plugin-uptime/manifest.toml");
            let original = Plugin::parse(text).unwrap().manifest.id;
            std::fs::write(dir.join("manifest.toml"), text.replace(&original, id)).unwrap();
            dir
        };
        let a = plugin("a", "io.test.a");
        let b = plugin("b", "io.test.b");
        let same_as_a = plugin("c", "io.test.a");
        let broken = root.join("broken");
        std::fs::create_dir_all(&broken).unwrap();
        let installed = vec![Plugin::load(&plugin("installed-b", "io.test.b")).unwrap()];
        let list = with_dev(installed, &[a.clone(), b.clone(), same_as_a, broken]);
        let got: Vec<(&str, &Path)> = list.iter().map(|p| (p.manifest.id.as_str(), p.dir.as_path())).collect();
        // every dev folder comes in; one replaces the installed app with its id; the same id
        // twice keeps the first folder; a broken folder is only logged
        assert_eq!(got, [("io.test.a", a.as_path()), ("io.test.b", b.as_path())]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
