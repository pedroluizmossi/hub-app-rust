//! Plugin store: index, installation, update and removal.
//!
//! Follows Omarchy's marketplace model: an index (`registry.json`) lists the plugins, each
//! in a git repository. Deliberate differences:
//! - the index pins the reviewed **commit**, and installation checks that exactly that
//!   commit arrived (Omarchy clones HEAD, which may have changed after the review);
//! - the index is **signed** (ed25519, `registry.json.sig` next to it): with the public key
//!   configured, a tampered index is refused. Without a key, only a local index is accepted;
//! - the plugin does not run native code: the host runs the manifest and, for plugins with
//!   a view, interprets the `.slint` and the Rhai script sandboxed (see `apps/plugin/`); only
//!   the files `plugins::files` accepts are copied;
//! - the exception is the **external app** (`[app]`, full access): the index must announce
//!   it (`full_access`) and pin, per OS, where the executable comes from and its SHA-256
//!   (`binaries`). The executable does not come from the repository: it is downloaded from
//!   the address in the index (preferably built by the store itself from the reviewed
//!   commit) and only goes in if the hash matches. The confirmation warns, prominently, that
//!   the app has full access;
//! - the permissions the index announces (programs, network domains and providers read)
//!   must match the manifest's, and are shown before installing (on an update, the new ones
//!   are highlighted).
//!
//! Command line in [`USAGE`]; the window uses the same logic (Store tab, `hub/store.rs`).

use std::collections::BTreeMap;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use ring::signature::{self, KeyPair};
use serde::{Deserialize, Serialize};

use crate::core::appdata::{self, Kind};
use crate::core::config::Config;
use crate::core::exec;
use crate::core::i18n;
use crate::core::plugins::{self, Plugin};
use crate::tr;

mod entry;
mod mascots;

pub use mascots::{MascotEntry, MascotStatus};

/// The store part of `--help`, in the current language (`println!("{}", store::USAGE)`).
pub const USAGE: Usage = Usage;

/// See [`USAGE`].
pub struct Usage;

impl std::fmt::Display for Usage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&usage())
    }
}

/// Width of the command column in [`usage`].
const USAGE_COLUMN: usize = 48;

/// One command of the help: the command, then its description aligned in a column
/// (a description with `\n` continues on the next lines, in the same column).
fn usage_line(out: &mut String, cmd: &str, desc: &str) {
    out.push_str("  ");
    out.push_str(cmd);
    let mut lines = desc.lines();
    if cmd.chars().count() + 2 < USAGE_COLUMN {
        out.push_str(&" ".repeat(USAGE_COLUMN - 2 - cmd.chars().count()));
        out.push_str(lines.next().unwrap_or(""));
    }
    for l in lines {
        out.push('\n');
        out.push_str(&" ".repeat(USAGE_COLUMN));
        out.push_str(l);
    }
    out.push('\n');
}

/// The store commands for `--help` (and the answer to a malformed store command).
pub fn usage() -> String {
    let mut out = String::new();
    out.push_str(tr!("store (plugins and mascots):"));
    out.push('\n');
    usage_line(&mut out, "miniutils --store [index]", tr!("lists the plugins and mascot packs and the\nstate of each"));
    usage_line(&mut out, "miniutils --install <id> [index] [--yes]", tr!("installs (asks first, without --yes)"));
    usage_line(&mut out, "miniutils --update <id> [index] [--yes]", tr!("updates to the index's commit"));
    usage_line(
        &mut out,
        "miniutils --remove <id> [--delete-data | --keep-data]",
        tr!("removes an installed plugin or mascot pack;\nif it has data, asks whether to delete it (or\nfollows the option)"),
    );
    usage_line(&mut out, "miniutils --plugins", tr!("lists the installed ones (from the store and local)"));
    usage_line(&mut out, "miniutils --reload", tr!("the running MiniUtils rereads the apps and themes"));
    out.push_str(tr!("  --key <hex>     public key that signs the index (default: store_key from the config)"));
    out.push('\n');
    out.push_str(tr!("Without an index, uses the config's store_index or, if it is empty, the official store\n(github.com/pedroluizmossi/miniutils-store). The index is a registry.json (path or\nhttps://), with the signature in registry.json.sig."));
    out.push_str("\n\n");
    out.push_str(tr!("index maintainers:"));
    out.push('\n');
    usage_line(&mut out, "miniutils --store-key <file>", tr!("generates a key pair (prints the public one)"));
    usage_line(&mut out, "miniutils --store-sign <registry.json> <file>", tr!("writes registry.json.sig"));
    usage_line(
        &mut out,
        "miniutils --store-entry <registry.json> <repo> <tag> [--path p] [--app-version v] [--license l]",
        tr!("adds or updates the app of the repository\n(https://… or owner/repo) at that tag: reads the\nmanifest at the commit and, for an external app,\nchecks the executables of the tag's Release"),
    );
    usage_line(
        &mut out,
        "miniutils --install-from <repo> <tag> [--path p]",
        tr!("installs straight from the author's repository at a tag,\nwithout a store (not reviewed: you accept the risk)"),
    );
    out.push('\n');
    out.push_str(tr!("The running miniutils rereads the plugins by itself after installing, updating or removing.\nLocal app (no store): create the folder <id>/ inside the apps folder (miniutils --dirs)\nand run miniutils --reload, or use Settings › System › Reload."));
    out
}

/// Deadline for `git clone`/`checkout`.
const GIT_TIMEOUT: Duration = Duration::from_secs(120);

/// The official store: used when the config does not name another (`store_index` empty).
/// The index and the key are those of https://github.com/pedroluizmossi/miniutils-store.
pub const OFFICIAL_INDEX: &str = "https://raw.githubusercontent.com/pedroluizmossi/miniutils-store/main/registry.json";
pub const OFFICIAL_KEY: &str = "070dba03dd19902eeed1d02573272be1fa3fc263f4a8eda3cf10b3beca98464e";

/// Where the index comes from and which key checks it.
#[derive(Debug, Clone, Default)]
pub struct Source {
    pub index: String,
    /// ed25519 public key (hex). Empty = no check (local index only).
    pub key: String,
}

impl Source {
    /// The index and key from the config; without an index, the official store.
    pub fn from_config(cfg: &Config) -> Source {
        let index = cfg.store_index.trim();
        Source { index: if index.is_empty() { OFFICIAL_INDEX } else { index }.to_string(), key: cfg.store_key.trim().to_string() }
            .with_official_key()
    }

    /// The official index with no key given uses the official key.
    fn with_official_key(mut self) -> Source {
        if self.key.is_empty() && self.index == OFFICIAL_INDEX {
            self.key = OFFICIAL_KEY.to_string();
        }
        self
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Index {
    pub schema: u32,
    pub plugins: Vec<Entry>,
    /// Packs of mascots (drawings only, see `store/mascots.rs`). Optional: an index
    /// without it is still valid.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mascots: Vec<MascotEntry>,
}

/// Everything in the index and its state here.
#[derive(Debug, Clone)]
pub struct Catalog {
    pub plugins: Vec<Status>,
    pub mascots: Vec<MascotStatus>,
    pub trust: Trust,
}

/// An index entry. Fields are in the order `registry.json` writes them, and empty ones are
/// not written: `--store-entry` rewrites the index without touching the other entries.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Entry {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    pub license: String,
    pub version: String,
    /// Git repository: `https://…` or a local folder (for tests).
    pub repo: String,
    /// Plugin subfolder in the repository (a repo may hold several).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub path: String,
    /// Reviewed commit (full SHA, 40 characters).
    pub commit: String,
    /// Programs the plugin runs (must match the manifest).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permissions: Vec<String>,
    /// Domains the plugin queries (`[permissions] net`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub net: Vec<String>,
    /// Providers the plugin reads (`uses`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uses: Vec<String>,
    /// External app (`[app]`): runs its own executable with the user's permissions.
    #[serde(default, skip_serializing_if = "is_false")]
    pub full_access: bool,
    /// Stores data in a SQLite database (`[data] kinds` with "sqlite"; must match the manifest).
    #[serde(default, skip_serializing_if = "is_false")]
    pub sqlite: bool,
    /// External app executable, per OS ("linux", "windows").
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub binaries: BTreeMap<String, Binary>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Where to download an external app executable and the hash it must have.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Binary {
    /// `https://…` (or an absolute path, for tests with a local index).
    pub url: String,
    /// SHA-256 of the file, in hex (64 characters).
    pub sha256: String,
}

impl Entry {
    /// "runs: ps · network: api.x.com · reads: sys" (or "FULL ACCESS: …").
    pub fn grants_text(&self) -> String {
        if self.full_access {
            return plugins::describe_full_access(&self.uses);
        }
        plugins::describe_grants(&self.permissions, &self.net, &self.uses, self.sqlite)
    }
}

/// What is given up with full access, in the list of new permissions of an update.
fn full_access_grant() -> &'static str {
    tr!("FULL ACCESS")
}

/// Each permission with its kind in front ("ps", "network: api.x.com", "reads: sys",
/// "stores: SQLite database", "FULL ACCESS"), to compare versions.
fn grant_list(exec: &[String], net: &[String], uses: &[String], sqlite: bool, full: bool) -> Vec<String> {
    full.then(|| full_access_grant().to_string())
        .into_iter()
        .chain(exec.iter().cloned())
        .chain(net.iter().map(|h| tr!("network: {}", h)))
        .chain(uses.iter().map(|t| tr!("reads: {}", t)))
        .chain(sqlite.then(|| i18n::tr(plugins::GRANT_SQLITE).to_string()))
        .collect()
}

/// What is written next to the installed plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Installed {
    pub id: String,
    pub version: String,
    pub repo: String,
    pub commit: String,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub net: Vec<String>,
    #[serde(default)]
    pub uses: Vec<String>,
    #[serde(default)]
    pub full_access: bool,
    #[serde(default)]
    pub sqlite: bool,
    /// Subfolder in the repository (empty: the root); `--install-from` needs it to update.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub path: String,
    /// Installed with `--install-from`: no store reviewed it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub unreviewed: bool,
}

/// How the index was checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trust {
    /// Valid signature with the configured key.
    Signed,
    /// No key configured: local index, accepted without a check.
    Unsigned,
    /// Installed straight from the author's repository with `--install-from`; no store
    /// reviewed or signed it.
    Unreviewed,
}

/// A plugin from the index and its state here.
#[derive(Debug, Clone)]
pub struct Status {
    pub entry: Entry,
    pub installed: Option<Installed>,
}

impl Status {
    /// Installed at a commit other than the one the index pins now.
    pub fn has_update(&self) -> bool {
        self.installed.as_ref().is_some_and(|i| !i.commit.eq_ignore_ascii_case(&self.entry.commit))
    }

    /// Permissions the index's version asks for that the installed one did not ("who",
    /// "network: api.x.com", "reads: agents").
    pub fn new_permissions(&self) -> Vec<String> {
        let had = self
            .installed
            .as_ref()
            .map(|i| grant_list(&i.permissions, &i.net, &i.uses, i.sqlite, i.full_access))
            .unwrap_or_default();
        let e = &self.entry;
        grant_list(&e.permissions, &e.net, &e.uses, e.sqlite, e.full_access).into_iter().filter(|p| !had.contains(p)).collect()
    }
}

// ---- command line -------------------------------------------------------------------

/// Handles the store commands. `None` if `args` is not a store command; otherwise the exit
/// code.
pub fn run_cli(args: &[String]) -> Option<i32> {
    let cmd = args.first()?.as_str();
    if !matches!(
        cmd,
        "--store"
            | "--install"
            | "--update"
            | "--remove"
            | "--plugins"
            | "--reload"
            | "--store-key"
            | "--store-sign"
            | "--store-entry"
            | "--install-from"
    ) {
        return None;
    }
    // `--key <hex>` and `--yes` may come in any position
    let mut key = None;
    let mut yes = false;
    let mut accept_risk = false;
    let mut purge: Option<bool> = None;
    let mut entry_opts = entry::Opts::default();
    let mut pos: Vec<&str> = Vec::new();
    let mut it = args[1..].iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--key" => key = it.next().cloned(),
            "--path" => entry_opts.path = it.next().cloned(),
            "--app-version" => entry_opts.version = it.next().cloned(),
            "--license" => entry_opts.license = it.next().cloned(),
            "--yes" => yes = true,
            "--i-accept-the-risk" => accept_risk = true,
            "--delete-data" => purge = Some(true),
            "--keep-data" => purge = Some(false),
            _ => pos.push(a),
        }
    }
    let cfg = Config::load();
    let source = |i: usize| -> Result<Source, String> {
        let mut s = Source::from_config(&cfg);
        if let Some(index) = pos.get(i) {
            s.index = index.to_string();
            // index given on the command line: the config's key belongs to another index
            if s.index != Source::from_config(&cfg).index {
                s.key.clear();
            }
        }
        if let Some(k) = &key {
            s.key = k.clone();
        }
        Ok(s.with_official_key())
    };
    let root = plugins::dir();
    let (mut always, mut prompt) = (|_: &str| true, ask_install);
    let confirm: &mut dyn FnMut(&str) -> bool = if yes { &mut always } else { &mut prompt };
    let id = pos.first().copied();
    let result = match (cmd, id) {
        ("--store", _) => source(0).and_then(|s| list(&s, &root)),
        ("--install", Some(id)) | ("--update", Some(id)) => source(1).and_then(|s| install_cli(&s, id, &root, confirm)).inspect(|_| {
            // the running hub rereads apps and appearance (where the mascots go)
            crate::instance::notify_plugins_changed();
        }),
        ("--install-from", Some(repo)) => match pos.get(1) {
            Some(tag) => install_from_cli(repo, tag, &entry_opts, yes, accept_risk, &root).inspect(|_| {
                crate::instance::notify_plugins_changed();
            }),
            None => Err(usage()),
        },
        ("--remove", Some(id)) => remove_cli(id, &root, purge, !yes).inspect(|_| {
            crate::instance::notify_plugins_changed();
        }),
        ("--plugins", _) => Ok(installed_list(&root)),
        ("--reload", _) => Ok(if crate::instance::notify_plugins_changed() {
            tr!(
                "the running MiniUtils reread the apps ({}) and the themes, skins and backdrops ({})\n{}",
                root.display(),
                crate::skin::user::dir().display(),
                installed_list(&root)
            )
        } else {
            tr!("no MiniUtils running: the apps in {} load when it opens", root.display())
        }),
        ("--store-key", Some(path)) => keygen(Path::new(path)),
        ("--store-sign", Some(index)) => match pos.get(1) {
            Some(k) => sign(Path::new(index), Path::new(k)),
            None => Err(usage()),
        },
        ("--store-entry", Some(index)) => match (pos.get(1), pos.get(2)) {
            (Some(repo), Some(tag)) => entry::run(Path::new(index), repo, tag, &entry_opts),
            _ => Err(usage()),
        },
        _ => Err(usage()),
    };
    Some(match result {
        Ok(msg) => {
            println!("{msg}");
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    })
}

fn list(src: &Source, root: &Path) -> Result<String, String> {
    let catalog = statuses(src, root, &crate::skin::user::dir())?;
    let mut out = tr!(
        "{} plugin(s) in {} ({}):",
        catalog.plugins.len(),
        src.index,
        if catalog.trust == Trust::Signed { tr!("signature checked") } else { tr!("local index, signature not checked") }
    );
    out.push('\n');
    for s in &catalog.plugins {
        let e = &s.entry;
        let state = match &s.installed {
            None => tr!("not installed").to_string(),
            Some(i) if s.has_update() => tr!("installed {} · update to {}", i.version, e.version),
            Some(i) => tr!("installed {}", i.version),
        };
        out.push_str(&format!(
            "\n  {}  {} {}  ({}, {})  [{state}]\n    {}\n    {}\n",
            e.id,
            e.name,
            e.version,
            if e.author.is_empty() { tr!("no author") } else { &e.author },
            e.license,
            e.description,
            e.grants_text()
        ));
    }
    if !catalog.mascots.is_empty() {
        out.push('\n');
        out.push_str(&tr!("{} mascot pack(s) (drawings only, no code):", catalog.mascots.len()));
        out.push('\n');
    }
    for s in &catalog.mascots {
        let e = &s.entry;
        let state = match &s.installed {
            None => tr!("not installed").to_string(),
            Some(i) if s.has_update() => tr!("installed {} · update to {}", i.version, e.version),
            Some(i) => format!("{} · {}", tr!("installed {}", i.version), i.contents()),
        };
        out.push_str(&format!(
            "\n  {}  {} {}  ({}, {})  [{state}]\n    {}\n",
            e.id,
            e.name,
            e.version,
            if e.author.is_empty() { tr!("no author") } else { &e.author },
            e.license,
            e.description,
        ));
    }
    Ok(out)
}

/// `--install-from <repo> <tag>`: builds the entry from the author's repository and installs
/// it with no store in between. Without `--yes` the user types the app id to accept.
fn install_from_cli(repo: &str, tag: &str, opts: &entry::Opts, yes: bool, accept_risk: bool, root: &Path) -> Result<String, String> {
    if yes && !accept_risk {
        return Err(tr!("--install-from with --yes also needs --i-accept-the-risk").into());
    }
    let entry = entry::from_repo(repo, tag, opts)?;
    let id = entry.id.clone();
    let mut always = |_: &str| true;
    let mut typed = |summary: &str| ask_typed_id(summary, &id);
    let confirm: &mut dyn FnMut(&str) -> bool = if yes { &mut always } else { &mut typed };
    let dir = install_entry(&entry, &Trust::Unreviewed, root, confirm)?;
    Ok(tr!("installed in {} (not reviewed by any store)", dir.display()))
}

/// Shows the summary and accepts only if the user types the app id exactly.
fn ask_typed_id(summary: &str, id: &str) -> bool {
    print!("{summary}\n{} ", tr!("Type the app id ({}) to install it:", id));
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    line.trim() == id
}

/// Asks on the terminal whether to install.
fn ask_install(summary: &str) -> bool {
    ask(summary, tr!("Install?"))
}

/// Shows the summary and asks (yes/no; the default is no).
fn ask(summary: &str, question: &str) -> bool {
    print!("{summary}\n{question} {} ", tr!("[y/N]"));
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    // Portuguese answers too, whatever the language
    matches!(line.trim().to_lowercase().as_str(), "s" | "sim" | "y" | "yes")
}

// ---- index and signature ------------------------------------------------------------

/// Largest index (or Release file) `fetch` accepts.
const FETCH_MAX: u64 = 4 << 20;

/// Downloads (https) or reads (local path) a small file: the index, its signature, a
/// Release's `registry-entry.json`. A response over `FETCH_MAX` is refused while it
/// arrives, not after it is all in memory.
fn fetch(src: &str) -> Result<Vec<u8>, String> {
    if src.starts_with("https://") {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(20))
            .user_agent(concat!("miniutils/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| e.to_string())?;
        let resp = client.get(src).send().and_then(|r| r.error_for_status()).map_err(|e| format!("{src}: {e}"))?;
        if resp.content_length().is_some_and(|n| n > FETCH_MAX) {
            return Err(tr!("{}: index too large", src));
        }
        let mut bytes = Vec::new();
        resp.take(FETCH_MAX + 1).read_to_end(&mut bytes).map_err(|e| format!("{src}: {e}"))?;
        if bytes.len() as u64 > FETCH_MAX {
            return Err(tr!("{}: index too large", src));
        }
        Ok(bytes)
    } else {
        std::fs::read(src).map_err(|e| format!("{src}: {e}"))
    }
}

/// Reads the index and checks the signature (`<index>.sig`) with the source's key.
pub fn load_index(src: &Source) -> Result<(Index, Trust), String> {
    // remote without a key: refused before downloading
    if src.key.trim().is_empty() && src.index.starts_with("https://") {
        return Err(tr!("{}: remote index with no key to check the signature (configure the store key)", src.index));
    }
    let bytes = fetch(&src.index)?;
    let trust = if src.key.trim().is_empty() {
        Trust::Unsigned
    } else {
        let key = from_hex(src.key.trim()).filter(|k| k.len() == 32).ok_or(tr!("invalid store key (64 hex characters)"))?;
        let sig_text = fetch(&format!("{}.sig", src.index)).map_err(|e| tr!("no index signature: {}", e))?;
        let sig = from_hex(String::from_utf8_lossy(&sig_text).trim()).ok_or(tr!("unreadable index signature"))?;
        signature::UnparsedPublicKey::new(&signature::ED25519, &key)
            .verify(&bytes, &sig)
            .map_err(|_| tr!("{}: the signature does not match (tampered index or wrong key)", src.index))?;
        Trust::Signed
    };
    let index: Index = serde_json::from_slice(&bytes).map_err(|e| tr!("{}: invalid index: {}", src.index, e))?;
    if index.schema != 1 {
        return Err(tr!("{}: index is version {} (this miniutils understands 1)", src.index, index.schema));
    }
    Ok((index, trust))
}

/// Generates an ed25519 key pair: the private key goes to `path` (readable by the owner
/// only), the public one is printed, to put in the config of those who use the store.
fn keygen(path: &Path) -> Result<String, String> {
    if path.exists() {
        return Err(tr!("{} already exists: I don't overwrite a key", path.display()));
    }
    let rng = ring::rand::SystemRandom::new();
    let pkcs8 = signature::Ed25519KeyPair::generate_pkcs8(&rng).map_err(|_| tr!("could not generate the key"))?;
    let pair = signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).map_err(|_| tr!("generated key is invalid"))?;
    let mut f = crate::core::os::owner_only(std::fs::OpenOptions::new().write(true).create_new(true))
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    f.write_all(to_hex(pkcs8.as_ref()).as_bytes()).map_err(|e| e.to_string())?;
    Ok(to_hex(pair.public_key().as_ref()))
}

/// Signs the index with the private key in `key_path`; writes `<index>.sig`.
fn sign(index: &Path, key_path: &Path) -> Result<String, String> {
    let text = std::fs::read_to_string(key_path).map_err(|e| format!("{}: {e}", key_path.display()))?;
    let pkcs8 = from_hex(text.trim()).ok_or(tr!("unreadable key file"))?;
    let pair = signature::Ed25519KeyPair::from_pkcs8(&pkcs8).map_err(|_| tr!("invalid key file"))?;
    let bytes = std::fs::read(index).map_err(|e| format!("{}: {e}", index.display()))?;
    // check before signing: no point signing an index that does not open
    serde_json::from_slice::<Index>(&bytes).map_err(|e| tr!("{}: invalid index: {}", index.display(), e))?;
    let sig_path = PathBuf::from(format!("{}.sig", index.display()));
    std::fs::write(&sig_path, to_hex(pair.sign(&bytes).as_ref())).map_err(|e| e.to_string())?;
    Ok(tr!("{}\npublic key: {}", sig_path.display(), to_hex(pair.public_key().as_ref())))
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

// ---- state, installation, removal --------------------------------------------------

/// What is installed in `root/<id>/`, if anything.
pub fn installed(root: &Path, id: &str) -> Option<Installed> {
    let text = std::fs::read_to_string(root.join(id).join("installed.json")).ok()?;
    serde_json::from_str(&text).ok()
}

/// Each plugin and mascot pack in the index and whether it is installed (plugins in `root`,
/// mascots in the appearance folder `appearance`).
pub fn statuses(src: &Source, root: &Path, appearance: &Path) -> Result<Catalog, String> {
    let (index, trust) = load_index(src)?;
    let plugins = index
        .plugins
        .into_iter()
        .map(|entry| {
            let installed = installed(root, &entry.id);
            Status { entry, installed }
        })
        .collect();
    let mascots = index
        .mascots
        .into_iter()
        .map(|entry| {
            let installed = mascots::installed(appearance, &entry.id);
            MascotStatus { entry, installed }
        })
        .collect();
    Ok(Catalog { plugins, mascots, trust })
}

/// Installs (or updates) mascot pack `id` from the index into the appearance folder.
/// Returns a line on what went in.
pub fn install_mascot(src: &Source, id: &str, appearance: &Path, confirm: &mut dyn FnMut(&str) -> bool) -> Result<String, String> {
    let (index, trust) = load_index(src)?;
    let entry = index.mascots.iter().find(|e| e.id == id).ok_or_else(|| tr!("\"{}\" is not in the index", id))?;
    mascots::install(entry, &trust, appearance, confirm)
}

/// Removes what mascot pack `id` installed in the appearance folder.
pub fn remove_mascot(id: &str, appearance: &Path) -> Result<String, String> {
    mascots::remove(appearance, id)
}

/// `--install`/`--update` from the command line: a plugin, or a mascot pack if the index
/// lists `id` among the mascots.
fn install_cli(src: &Source, id: &str, root: &Path, confirm: &mut dyn FnMut(&str) -> bool) -> Result<String, String> {
    let (index, _) = load_index(src)?;
    if !index.plugins.iter().any(|e| e.id == id) && index.mascots.iter().any(|e| e.id == id) {
        return install_mascot(src, id, &crate::skin::user::dir(), confirm);
    }
    if !index.plugins.iter().any(|e| e.id == id)
        && let Some(i) = installed(root, id).filter(|i| i.unreviewed)
    {
        let hint = if i.path.is_empty() { String::new() } else { format!(" --path {}", i.path) };
        return Err(tr!(
            "{} was installed with --install-from, outside the store: update it with miniutils --install-from {} <tag>{}",
            id,
            i.repo,
            hint
        ));
    }
    install(src, id, root, confirm).map(|dir| tr!("installed in {}", dir.display()))
}

/// Installs (or updates) plugin `id` from the index into `root/<id>/`. `confirm` gets the
/// summary (name, author, commit, permissions) and decides. Returns the installed folder.
pub fn install(src: &Source, id: &str, root: &Path, confirm: &mut dyn FnMut(&str) -> bool) -> Result<PathBuf, String> {
    let (index, trust) = load_index(src)?;
    let entry = index.plugins.iter().find(|e| e.id == id).ok_or_else(|| tr!("\"{}\" is not in the index", id))?;
    install_entry(entry, &trust, root, confirm)
}

/// Installs (or updates) `entry` into `root/<id>/` with the given `trust`: the part of
/// [`install`] after the entry is found (also used by `--install-from`).
pub fn install_entry(entry: &Entry, trust: &Trust, root: &Path, confirm: &mut dyn FnMut(&str) -> bool) -> Result<PathBuf, String> {
    let id = entry.id.as_str();
    plugins::validate_id(&entry.id)?;
    check_entry(entry)?;
    let previous = installed(root, id);
    if previous.as_ref().is_some_and(|p| p.commit.eq_ignore_ascii_case(&entry.commit)) {
        return Err(tr!("{} is already installed at this version ({})", id, entry.version));
    }

    // download to a temporary place next to the destination (the final `rename` does not
    // cross disks)
    std::fs::create_dir_all(root).map_err(|e| format!("{}: {e}", root.display()))?;
    let tmp = root.join(format!(".{id}.{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let result = fetch_and_install(entry, trust, previous.as_ref(), root, &tmp, confirm);
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

/// Checks where an entry (plugin or mascot) comes from, before downloading anything: the
/// pinned commit, an https repository, a path inside it and a declared license.
fn check_source(id: &str, repo: &str, path: &str, commit: &str, license: &str) -> Result<(), String> {
    if commit.len() != 40 || !commit.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(tr!("{}: the index must pin the commit (40-character SHA)", id));
    }
    // only https or a local folder: no ext::, ssh with a command, file:// with tricks
    let local = Path::new(repo).is_absolute();
    if !(repo.starts_with("https://") || local) {
        return Err(tr!("{}: the repository must be https:// (or a local folder, for tests)", id));
    }
    if repo_subpath(path).is_none() {
        return Err(tr!("{}: invalid path in the index: {}", id, path));
    }
    if license.trim().is_empty() {
        return Err(tr!("{}: the index does not declare the license", id));
    }
    Ok(())
}

/// A folder inside the repository, as `a/b` (`\\` becomes `/`; "" is the root), or `None`
/// if it could leave the clone: absolute, with a Windows drive (`C:`) or `..`/`.` parts.
/// A name that only contains dots (`my..app`) is fine.
pub(crate) fn repo_subpath(path: &str) -> Option<String> {
    let path = path.replace('\\', "/");
    if path.starts_with('/') || path.contains(':') {
        return None;
    }
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    if parts.iter().any(|p| matches!(*p, "." | "..")) {
        return None;
    }
    Some(parts.join("/"))
}

/// Clones `repo` into `into` (without file contents yet: checking out the commit fetches
/// only its own) and checks out exactly `commit` (no hooks, no submodules: the clone runs
/// nothing from the repository).
fn checkout(id: &str, repo: &str, commit: &str, into: &Path) -> Result<(), String> {
    let into_s = into.to_string_lossy().to_string();
    git(&["clone", "--quiet", "--no-checkout", "--filter=blob:none", "-c", "core.hooksPath=/dev/null", "--", repo, &into_s])?;
    git(&["-C", &into_s, "-c", "core.hooksPath=/dev/null", "checkout", "--quiet", "--detach", commit])?;
    let head = git(&["-C", &into_s, "rev-parse", "HEAD"])?;
    if !head.trim().eq_ignore_ascii_case(commit) {
        return Err(tr!("{}: the repository delivered commit {}, not the index's {}", id, head.trim(), commit));
    }
    Ok(())
}

/// Checks what the index says before downloading anything.
fn check_entry(e: &Entry) -> Result<(), String> {
    check_source(&e.id, &e.repo, &e.path, &e.commit, &e.license)?;
    if !e.full_access && !e.binaries.is_empty() {
        return Err(tr!("{}: only an app with full_access brings an executable (binaries)", e.id));
    }
    for (os, b) in &e.binaries {
        if !["linux", "windows"].contains(&os.as_str()) {
            return Err(tr!("{}: binaries.{}: use linux or windows", e.id, os));
        }
        if !(b.url.starts_with("https://") || Path::new(&b.url).is_absolute()) {
            return Err(tr!("{}: binaries.{}: the address must be https:// (or a local path, for tests)", e.id, os));
        }
        if b.sha256.len() != 64 || !b.sha256.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(tr!("{}: binaries.{}: sha256 must have 64 hex characters", e.id, os));
        }
    }
    Ok(())
}

/// Downloads (or copies, for a local path) the executable to `to`, checking size and
/// SHA-256. Wrong hash: the file is deleted and installation stops.
fn download_verified(b: &Binary, to: &Path) -> Result<(), String> {
    let mut reader: Box<dyn Read> = if b.url.starts_with("https://") {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(300))
            .build()
            .map_err(|e| e.to_string())?;
        let resp = client.get(&b.url).send().and_then(|r| r.error_for_status()).map_err(|e| format!("{}: {e}", b.url))?;
        Box::new(resp)
    } else {
        Box::new(std::fs::File::open(&b.url).map_err(|e| format!("{}: {e}", b.url))?)
    };
    let mut file = std::fs::File::create(to).map_err(|e| format!("{}: {e}", to.display()))?;
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buf = vec![0u8; 64 << 10];
    let mut total = 0u64;
    let result = loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break Ok(()),
            Ok(n) => n,
            Err(e) => break Err(format!("{}: {e}", b.url)),
        };
        total += n as u64;
        if total > plugins::MAX_BIN {
            break Err(tr!("{}: the executable is over {} MiB", b.url, plugins::MAX_BIN >> 20));
        }
        digest.update(&buf[..n]);
        if let Err(e) = file.write_all(&buf[..n]) {
            break Err(format!("{}: {e}", to.display()));
        }
    };
    let got = to_hex(digest.finish().as_ref());
    let result = result.and_then(|_| {
        if got.eq_ignore_ascii_case(&b.sha256) {
            Ok(())
        } else {
            Err(tr!("the downloaded executable has SHA-256 {}, not the index's {}", got, b.sha256))
        }
    });
    drop(file);
    if result.is_err() {
        let _ = std::fs::remove_file(to);
    }
    result?;
    // executable for the owner (and readable by others), like an installed binary
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(to, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn git(args: &[&str]) -> Result<String, String> {
    exec::run_timeout("git", args, GIT_TIMEOUT).map_err(|e| format!("git {}: {e}", args.first().unwrap_or(&"")))
}

fn fetch_and_install(
    e: &Entry,
    trust: &Trust,
    previous: Option<&Installed>,
    root: &Path,
    tmp: &Path,
    confirm: &mut dyn FnMut(&str) -> bool,
) -> Result<PathBuf, String> {
    let checkout = tmp.join("repo");
    self::checkout(&e.id, &e.repo, &e.commit, &checkout)?;

    let src = if e.path.is_empty() { checkout.clone() } else { checkout.join(&e.path) };
    let text = std::fs::read_to_string(src.join("manifest.toml")).map_err(|err| tr!("{}: no manifest.toml ({})", e.id, err))?;
    let plugin = Plugin::parse(&text).map_err(|err| tr!("{}: invalid manifest: {}", e.id, err))?;
    check_matches(e, &plugin)?;
    if plugin.ui.is_some() {
        // the view and the script must also open before installing
        crate::apps::plugin::check_files(&src).map_err(|err| format!("{}: {err}", e.id))?;
    }
    // external app: the view opens, and there is an executable for this OS
    let binary = match &plugin.app {
        Some(app) => {
            crate::apps::plugin::check_view(&src).map_err(|err| format!("{}: {err}", e.id))?;
            let os = plugins::this_os();
            let (Some(name), Some(b)) = (app.binary(), e.binaries.get(os)) else {
                return Err(tr!("{}: this app has no version for {}", e.id, os));
            };
            Some((name.to_string(), b.clone()))
        }
        None => None,
    };

    let new_perms: Vec<String> = match previous {
        Some(p) => {
            let had = grant_list(&p.permissions, &p.net, &p.uses, p.sqlite, p.full_access);
            grant_list(&e.permissions, &e.net, &e.uses, e.sqlite, e.full_access).into_iter().filter(|x| !had.contains(x)).collect()
        }
        None => Vec::new(),
    };
    let what = match (&plugin.run, &plugin.ui, &binary) {
        (_, _, Some((name, b))) => tr!("executable {}, sha256 {}…", name, &b.sha256[..12]),
        (_, Some(ui), _) => tr!("own view and script ({}, {})", ui.view, ui.script),
        (Some(run), None, _) => tr!("every {} s: {}", run.every, run.command.join(" ")),
        (None, None, None) => String::new(),
    };
    let danger = match (plugin.full_access(), trust) {
        (false, _) => "",
        // no store in between: only the author is left to trust
        (true, Trust::Unreviewed) => tr!("  WARNING: FULL ACCESS. This app runs its own program with your user's permissions:\n  it reads and writes your files, uses the network and runs other programs. Install it\n  only if you trust the author.\n"),
        (true, _) => tr!("  WARNING: FULL ACCESS. This app runs its own program with your user's permissions:\n  it reads and writes your files, uses the network and runs other programs. Install it\n  only if you trust the author and the store.\n"),
    };
    let mut summary = format!("\n{} {}  ({})\n  {}\n  ", plugin.manifest.name, e.version, e.id, plugin.manifest.description);
    summary += &tr!("author: {}   license: {}", if e.author.is_empty() { "?" } else { &e.author }, e.license);
    summary += "\n  ";
    summary += &tr!("repository: {}", e.repo);
    summary += "\n  commit: ";
    summary += &e.commit;
    summary += "\n  ";
    summary += &tr!(
        "index: {}",
        match trust {
            Trust::Signed => tr!("signature checked"),
            Trust::Unsigned => tr!("local, signature not checked"),
            Trust::Unreviewed => tr!("not reviewed by any store: straight from the author's repository"),
        }
    );
    summary += "\n  ";
    if let Some(p) = previous {
        summary += &tr!("update from {} to {}", p.version, e.version);
        summary += "\n  ";
    }
    summary += &format!("{}  ({what})\n", plugin.grants());
    if !new_perms.is_empty() {
        summary += "  ";
        summary += &tr!("WARNING, new permission: {}", new_perms.join(", "));
        summary += "\n";
    }
    if *trust == Trust::Unreviewed {
        summary += tr!("  WARNING: NOT REVIEWED. No store checked this code; only its author vouches for it.\n");
    }
    summary += danger;
    if !confirm(&summary) {
        return Err(tr!("installation cancelled").into());
    }

    // only the allowed files, and only real files (no links)
    let staged = tmp.join("plugin");
    std::fs::create_dir_all(&staged).map_err(|err| err.to_string())?;
    for rel in plugins::files(&src).map_err(|err| format!("{}: {err}", e.id))? {
        // the executable comes from the index's address, checked by its hash, never from
        // the repository
        if plugins::is_bin(&rel) {
            continue;
        }
        let to = staged.join(&rel);
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
        }
        std::fs::copy(src.join(&rel), &to).map_err(|err| format!("{}: {err}", rel.display()))?;
    }
    if let Some((name, b)) = &binary {
        let to = staged.join(name);
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
        }
        download_verified(b, &to).map_err(|err| format!("{}: {err}", e.id))?;
    }
    let installed = Installed {
        id: e.id.clone(),
        version: e.version.clone(),
        repo: e.repo.clone(),
        commit: e.commit.clone(),
        permissions: plugin.manifest.permissions.exec.clone(),
        net: plugin.manifest.permissions.net.clone(),
        uses: plugin.manifest.uses.iter().map(|t| t.name().to_string()).collect(),
        full_access: plugin.full_access(),
        sqlite: plugin.data().has(Kind::Sqlite),
        path: e.path.clone(),
        unreviewed: *trust == Trust::Unreviewed,
    };
    let meta = serde_json::to_string_pretty(&installed).map_err(|err| err.to_string())?;
    std::fs::write(staged.join("installed.json"), meta).map_err(|err| err.to_string())?;

    // swap: the old version only leaves when the new one is ready next to it
    let dest = root.join(&e.id);
    let old = tmp.join("old");
    if dest.exists() {
        // on Windows a running executable locks the folder: the hub stops the app before
        // updating from the window, but the command line has no way to
        let hint = if plugin.full_access() { tr!(" (turn the app off in the hub and try again)") } else { "" };
        std::fs::rename(&dest, &old).map_err(|err| format!("{}: {err}{hint}", dest.display()))?;
    }
    if let Err(err) = std::fs::rename(&staged, &dest) {
        let _ = std::fs::rename(&old, &dest);
        return Err(format!("{}: {err}", dest.display()));
    }
    Ok(dest)
}

/// What the index announces must be what the manifest asks for.
fn check_matches(e: &Entry, p: &Plugin) -> Result<(), String> {
    if p.manifest.id != e.id {
        return Err(tr!("the manifest says id \"{}\", the index \"{}\"", p.manifest.id, e.id));
    }
    let sorted = |v: &[String]| {
        let mut v = v.to_vec();
        v.sort();
        v.dedup();
        v
    };
    if e.full_access != p.full_access() {
        return Err(if p.full_access() {
            tr!("{}: the manifest is of an app with full access ([app]), but the index does not announce full_access", e.id)
        } else {
            tr!("{}: the index announces full_access, but the manifest is not of an external app ([app])", e.id)
        });
    }
    // the index may publish fewer OSes than the manifest (installation says which one is
    // missing), never one the manifest does not declare
    if let Some(app) = &p.app {
        let declared: Vec<&str> = app.binaries().into_iter().map(|(os, _)| os).collect();
        if let Some(os) = e.binaries.keys().find(|os| !declared.contains(&os.as_str())) {
            return Err(tr!("{}: the index brings an executable for {}, which the manifest does not declare", e.id, os));
        }
    }
    if e.sqlite != p.data().has(Kind::Sqlite) {
        return Err(if e.sqlite {
            tr!("{}: the index announces sqlite, but the manifest asks for no database ([data] kinds)", e.id)
        } else {
            tr!("{}: the manifest stores data in a database ([data] kinds = [\"sqlite\"]), but the index does not announce sqlite", e.id)
        });
    }
    let uses: Vec<String> = p.manifest.uses.iter().map(|t| t.name().to_string()).collect();
    let pairs = [
        (tr!("to run"), &e.permissions, &p.manifest.permissions.exec),
        (tr!("to query (net)"), &e.net, &p.manifest.permissions.net),
        (tr!("to read (uses)"), &e.uses, &uses),
    ];
    for (what, want, got) in pairs {
        let (want, got) = (sorted(want), sorted(got));
        if want != got {
            return Err(tr!("{}: the index announces {} {}, but the manifest asks for {}", e.id, what, format!("{want:?}"), format!("{got:?}")));
        }
    }
    Ok(())
}

/// Removes plugin `id` from `root`. The data (the app's folder in `data_root`, see
/// `core/appdata.rs`) is kept for a reinstall, unless `purge`.
pub fn remove(id: &str, root: &Path, data_root: &Path, purge: bool) -> Result<String, String> {
    plugins::validate_id(id)?;
    let dir = root.join(id);
    if !dir.join("manifest.toml").is_file() {
        return Err(tr!("\"{}\" is not installed", id));
    }
    std::fs::remove_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let kept = !purge && appdata::has_data(&data_root.join(id));
    // delete; or, with no data, only the folder that kept the version
    if !kept {
        appdata::delete(data_root, id).map_err(|e| tr!("{} removed, but the data stayed: {}", id, e))?;
    }
    Ok(match (purge, kept) {
        (true, _) => tr!("{} removed, with its data", id),
        (false, true) => tr!("{} removed; its data is kept for when you reinstall", id),
        (false, false) => tr!("{} removed", id),
    })
}

/// `--remove`: with stored data and no `--delete-data`/`--keep-data`, asks.
fn remove_cli(id: &str, root: &Path, choice: Option<bool>, confirm_tty: bool) -> Result<String, String> {
    // not a plugin here, but a mascot pack from the store: only drawings, no data to ask about
    let appearance = crate::skin::user::dir();
    if !root.join(id).join("manifest.toml").is_file() && mascots::installed(&appearance, id).is_some() {
        return remove_mascot(id, &appearance);
    }
    let data_root = appdata::root();
    let data = data_root.join(id);
    let purge = match choice {
        Some(p) => p,
        None if appdata::has_data(&data) && root.join(id).join("manifest.toml").is_file() => {
            let (bytes, _) = appdata::size(&data);
            confirm_tty
                && ask(
                    &tr!("{} has {} of data in {}.", id, appdata::human(bytes), data.display()),
                    tr!("Delete the data too? (no: it is kept for when you reinstall)"),
                )
        }
        None => false,
    };
    remove(id, root, &data_root, purge)
}

fn installed_list(root: &Path) -> String {
    let (list, skipped) = plugins::scan(root);
    if list.is_empty() && skipped.is_empty() {
        return tr!("no plugins installed in {}", root.display());
    }
    let mut out = tr!("installed in {}:", root.display());
    out.push('\n');
    for p in list {
        let origin = match installed(root, &p.manifest.id) {
            Some(m) if m.unreviewed => {
                tr!("{}  (not reviewed, commit {})", m.version, m.commit.chars().take(12).collect::<String>())
            }
            Some(m) => tr!("{}  (store, commit {})", m.version, m.commit.chars().take(12).collect::<String>()),
            None => tr!("(local)").to_string(),
        };
        out.push_str(&format!("  {}  {} {origin}\n", p.manifest.id, p.manifest.name));
    }
    for s in skipped {
        out.push_str(&format!("  {}\n", tr!("skipped: {}/  {}", s.folder, s.reason)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::plugins::tests::EXAMPLE;

    #[test]
    fn repository_subpath_stays_inside_the_clone() {
        for bad in ["C:/x", "C:\\x", "/x", "\\\\server\\share", "a/../b", "..", "./a", "a/./b"] {
            assert_eq!(repo_subpath(bad), None, "{bad}");
        }
        assert_eq!(repo_subpath("my..app").as_deref(), Some("my..app"));
        assert_eq!(repo_subpath("apps\\notes/").as_deref(), Some("apps/notes"));
        assert_eq!(repo_subpath("").as_deref(), Some(""));
    }

    /// A real git repository with the example plugin and an index pointing to it.
    struct Store {
        dir: PathBuf,
        commit: String,
    }

    impl Store {
        fn new(name: &str) -> Option<Store> {
            if !exec::which("git") {
                return None;
            }
            let dir = std::env::temp_dir().join(format!("miniutils-store-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            let repo = dir.join("repo");
            std::fs::create_dir_all(&repo).unwrap();
            std::fs::write(repo.join("manifest.toml"), EXAMPLE).unwrap();
            std::fs::write(repo.join("README.md"), "# example").unwrap();
            std::fs::write(repo.join("install.sh"), "rm -rf ~").unwrap(); // must not be copied
            let commit = Store::commit(&repo, "v1");
            Some(Store { dir, commit })
        }

        fn commit(repo: &Path, msg: &str) -> String {
            let r = repo.to_string_lossy().to_string();
            if !repo.join(".git").exists() {
                git(&["-C", &r, "init", "--quiet"]).unwrap();
            }
            git(&["-C", &r, "add", "."]).unwrap();
            git(&["-C", &r, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "--quiet", "-m", msg]).unwrap();
            git(&["-C", &r, "rev-parse", "HEAD"]).unwrap().trim().to_string()
        }

        fn index(&self, commit: &str, version: &str, permissions: &str) -> Source {
            let path = self.dir.join("registry.json");
            // The path goes as an escaped JSON string: on Windows it has `\`.
            let repo = serde_json::to_string(&self.dir.join("repo").to_string_lossy()).unwrap();
            let json = format!(
                r#"{{"schema":1,"plugins":[{{"id":"io.github.exemplo.uptime","name":"Uptime","license":"MIT","version":"{version}","repo":{repo},"commit":"{commit}","permissions":[{permissions}]}}]}}"#
            );
            std::fs::write(&path, json).unwrap();
            Source { index: path.to_string_lossy().to_string(), key: String::new() }
        }
    }

    impl Drop for Store {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// A flag that is not a store command is left to the rest of `main`.
    #[test]
    fn other_flags_are_not_store_commands() {
        assert_eq!(run_cli(&["--hidden".to_string()]), None);
    }

    /// `--store-entry`: the new version from the tag changes only the entry's commit and
    /// version, the same commit again is refused, and the rewritten index installs.
    #[test]
    fn store_entry_from_tag_updates_index_and_installs() {
        let Some(store) = Store::new("tagged") else { return };
        let src = store.index(&store.commit, "0.1.0", r#""uptime""#);
        let index = PathBuf::from(&src.index);
        let repo = store.dir.join("repo");
        let r = repo.to_string_lossy().to_string();
        std::fs::write(repo.join("README.md"), "# example 2").unwrap();
        let new_commit = Store::commit(&repo, "v2");
        git(&["-C", &r, "tag", "v0.2.0"]).unwrap();

        let report = entry::run(&index, &r, "v0.2.0", &entry::Opts::default()).unwrap();
        assert!(report.contains("id: io.github.exemplo.uptime"), "{report}");
        assert!(report.contains("0.1.0 → 0.2.0"), "{report}");
        let (read, _) = load_index(&src).unwrap();
        assert_eq!(read.plugins.len(), 1);
        let e = &read.plugins[0];
        assert_eq!((e.version.as_str(), e.commit.as_str(), e.license.as_str()), ("0.2.0", new_commit.as_str(), "MIT"));
        assert_eq!(e.permissions, ["uptime"]);

        let err = entry::run(&index, &r, "v0.2.0", &entry::Opts::default()).unwrap_err();
        assert!(err.contains("already in the index"), "{err}");
        let err = entry::run(&index, &r, "missing-v9.0.0", &entry::Opts::default()).unwrap_err();
        assert!(err.contains("no such tag"), "{err}");

        let root = store.dir.join("plugins");
        install(&src, "io.github.exemplo.uptime", &root, &mut |_| true).unwrap();
        assert_eq!(installed(&root, "io.github.exemplo.uptime").unwrap().commit, new_commit);
    }

    #[test]
    fn installs_pinned_commit_and_only_allowed_files() {
        let Some(store) = Store::new("ok") else { return };
        let src = store.index(&store.commit, "0.1.0", r#""uptime""#);
        let root = store.dir.join("plugins");
        let mut summary = String::new();
        let dest = install(&src, "io.github.exemplo.uptime", &root, &mut |s| {
            summary = s.to_string();
            true
        })
        .unwrap();
        let grants = plugins::describe_grants(&["uptime".to_string()], &[], &[], false);
        assert!(summary.contains(&grants), "{summary}");
        assert!(dest.join("manifest.toml").is_file());
        assert!(dest.join("README.md").is_file());
        assert!(!dest.join("install.sh").exists(), "a file outside the list does not go in");
        assert_eq!(installed(&root, "io.github.exemplo.uptime").unwrap().commit, store.commit);
        let all = plugins::load_installed(&root);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].manifest.name, "Uptime");
        assert!(install(&src, "io.github.exemplo.uptime", &root, &mut |_| true).unwrap_err().contains("already installed"));
        let data = root.join("test-data");
        std::fs::create_dir_all(data.join("io.github.exemplo.uptime")).unwrap();
        std::fs::write(data.join("io.github.exemplo.uptime").join("store.json"), "{}").unwrap();
        // without "delete the data": the data folder stays for a reinstall
        assert!(remove("io.github.exemplo.uptime", &root, &data, false).unwrap().contains("kept"));
        assert!(data.join("io.github.exemplo.uptime").join("store.json").exists());
        assert!(plugins::load_installed(&root).is_empty());
    }

    #[test]
    fn updates_and_warns_of_new_permission() {
        let Some(store) = Store::new("upd") else { return };
        let root = store.dir.join("plugins");
        let id = "io.github.exemplo.uptime";
        install(&store.index(&store.commit, "0.1.0", r#""uptime""#), id, &root, &mut |_| true).unwrap();
        // the new version asks for one more program
        let repo = store.dir.join("repo");
        let manifest = EXAMPLE.replace(r#"exec = ["uptime"]"#, r#"exec = ["uptime", "who"]"#);
        std::fs::write(repo.join("manifest.toml"), manifest).unwrap();
        let commit2 = Store::commit(&repo, "v2");
        let src = store.index(&commit2, "0.2.0", r#""uptime", "who""#);
        let st = statuses(&src, &root, &store.dir.join("appearance")).unwrap().plugins;
        assert!(st[0].has_update());
        assert_eq!(st[0].new_permissions(), ["who"]);
        let mut summary = String::new();
        install(&src, id, &root, &mut |s| {
            summary = s.to_string();
            true
        })
        .unwrap();
        assert!(summary.contains("update from 0.1.0 to 0.2.0"), "{summary}");
        assert!(summary.contains("new permission: who"), "{summary}");
        assert_eq!(installed(&root, id).unwrap().version, "0.2.0");
    }

    #[test]
    fn refuses_mismatched_commit_or_permission_and_respects_no() {
        let Some(store) = Store::new("bad") else { return };
        let root = store.dir.join("plugins");
        let id = "io.github.exemplo.uptime";
        // a commit that does not exist in the repository
        let other = store.index(&"a".repeat(40), "0.1.0", r#""uptime""#);
        assert!(install(&other, id, &root, &mut |_| true).is_err());
        // an index hiding a permission
        let lie = store.index(&store.commit, "0.1.0", "");
        assert!(install(&lie, id, &root, &mut |_| true).unwrap_err().contains("announces"));
        // short commit (not pinned)
        let short = store.index("abc123", "0.1.0", r#""uptime""#);
        assert!(install(&short, id, &root, &mut |_| true).unwrap_err().contains("40-character"));
        // the user said no
        let ok = store.index(&store.commit, "0.1.0", r#""uptime""#);
        assert!(install(&ok, id, &root, &mut |_| false).unwrap_err().contains("cancelled"));
        assert!(plugins::load_installed(&root).is_empty());
    }

    /// An index with an external app: the repository brings manifest and view; the
    /// executable comes from a separate file, pinned by the SHA-256 in the index.
    #[test]
    fn external_app_downloads_the_binary_and_checks_the_hash() {
        if !exec::which("git") {
            return;
        }
        let dir = std::env::temp_dir().join(format!("miniutils-store-app-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let repo = dir.join("repo");
        std::fs::create_dir_all(repo.join("bin")).unwrap();
        let example = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/app-hello");
        for f in ["manifest.toml", "view.slint", "README.md"] {
            std::fs::copy(example.join(f), repo.join(f)).unwrap();
        }
        // a binary in the repository is not used: the index's one is
        std::fs::write(repo.join("bin/app-hello"), "from the repository").unwrap();
        let commit = Store::commit(&repo, "app");
        let binary = dir.join("app-hello-published");
        std::fs::write(&binary, "binary built by the store").unwrap();
        let hash = to_hex(ring::digest::digest(&ring::digest::SHA256, b"binary built by the store").as_ref());
        let index = |full: bool, sha: &str| -> Source {
            let path = dir.join("registry.json");
            let url = serde_json::to_string(&binary.to_string_lossy()).unwrap();
            let repo = serde_json::to_string(&repo.to_string_lossy()).unwrap();
            let bins = format!(r#"{{"linux":{{"url":{url},"sha256":"{sha}"}},"windows":{{"url":{url},"sha256":"{sha}"}}}}"#);
            let extra = if full { format!(r#","full_access":true,"binaries":{bins}"#) } else { String::new() };
            let json = format!(
                r#"{{"schema":1,"plugins":[{{"id":"io.github.pedroluizmossi.app-hello","name":"Hello","license":"MIT","version":"0.1.0","repo":{repo},"commit":"{commit}","uses":["sys"]{extra}}}]}}"#
            );
            std::fs::write(&path, json).unwrap();
            Source { index: path.to_string_lossy().to_string(), key: String::new() }
        };
        let root = dir.join("plugins");
        let id = "io.github.pedroluizmossi.app-hello";
        // an index hiding the full access
        assert!(install(&index(false, &hash), id, &root, &mut |_| true).unwrap_err().contains("full_access"));
        // a hash that does not match: nothing installed
        let wrong = "0".repeat(64);
        assert!(install(&index(true, &wrong), id, &root, &mut |_| true).unwrap_err().contains("SHA-256"));
        assert!(plugins::load_installed(&root).is_empty());
        // right: the warning shows and the installed binary is the index's
        let mut summary = String::new();
        let dest = install(&index(true, &hash), id, &root, &mut |s| {
            summary = s.to_string();
            true
        })
        .unwrap();
        assert!(summary.contains("FULL ACCESS"), "{summary}");
        let name = if cfg!(windows) { "bin/app-hello.exe" } else { "bin/app-hello" };
        assert_eq!(std::fs::read_to_string(dest.join(name)).unwrap(), "binary built by the store");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(dest.join(name)).unwrap().permissions().mode() & 0o777, 0o755);
        }
        let inst = installed(&root, id).unwrap();
        assert!(inst.full_access);
        let p = &plugins::load_installed(&root)[0];
        assert!(p.full_access() && p.binary_path().unwrap().is_file());
        // removing with "delete the data" also takes the app's data folder
        let data = dir.join("data");
        std::fs::create_dir_all(data.join(id)).unwrap();
        std::fs::write(data.join(id).join("x"), "1").unwrap();
        remove(id, &root, &data, true).unwrap();
        assert!(!data.join(id).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn full_access_in_an_update_is_a_new_permission() {
        let entry = |full: bool| Entry {
            id: "a.b".into(),
            name: "x".into(),
            description: String::new(),
            author: String::new(),
            license: "MIT".into(),
            version: "2".into(),
            repo: "https://x".into(),
            commit: "a".repeat(40),
            path: String::new(),
            permissions: vec![],
            net: vec![],
            uses: vec!["sys".into()],
            full_access: full,
            sqlite: false,
            binaries: BTreeMap::new(),
        };
        let before = Installed {
            id: "a.b".into(),
            version: "1".into(),
            repo: "https://x".into(),
            commit: "b".repeat(40),
            permissions: vec![],
            net: vec![],
            uses: vec!["sys".into()],
            full_access: false,
            sqlite: false,
            path: String::new(),
            unreviewed: false,
        };
        let st = Status { entry: entry(true), installed: Some(before.clone()) };
        assert_eq!(st.new_permissions(), [full_access_grant()]);
        // starting to store in a database also shows as a new permission
        let mut with_db = entry(false);
        with_db.sqlite = true;
        let sqlite = i18n::tr(plugins::GRANT_SQLITE);
        assert!(with_db.grants_text().ends_with(sqlite));
        let st2 = Status { entry: with_db, installed: Some(before) };
        assert_eq!(st2.new_permissions(), [sqlite]);
        assert!(st.entry.grants_text().starts_with(&plugins::describe_full_access(&[])));
        // a binary in an index that does not announce full access
        let mut e = entry(false);
        e.binaries.insert("linux".into(), Binary { url: "https://x/b".into(), sha256: "0".repeat(64) });
        assert!(check_entry(&e).unwrap_err().contains("full_access"));
        let mut e = entry(true);
        e.binaries.insert("linux".into(), Binary { url: "http://x/b".into(), sha256: "0".repeat(64) });
        assert!(check_entry(&e).unwrap_err().contains("https"));
    }

    #[test]
    fn no_index_in_config_means_the_official_store() {
        let mut cfg = Config::default();
        let s = Source::from_config(&cfg);
        assert_eq!((s.index.as_str(), s.key.as_str()), (OFFICIAL_INDEX, OFFICIAL_KEY));
        // another store: the official key does not go along
        cfg.store_index = "/tmp/other/registry.json".into();
        assert_eq!(Source::from_config(&cfg).key, "");
        cfg.store_key = "ab".repeat(32);
        assert_eq!(Source::from_config(&cfg).key, "ab".repeat(32));
    }

    /// A PNG sheet of `frames` 16×16 frames side by side, with something drawn in each.
    fn sheet(frames: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_fn(16 * frames, 16, |x, y| {
            if (4..12).contains(&(x % 16)) && (4..12).contains(&y) { image::Rgba([200, 80, 40, 255]) } else { image::Rgba([0, 0, 0, 0]) }
        });
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    /// A mascot pack: installs only the drawings (a sprite and a PNG mascot), refuses a key
    /// that already exists, updates by swapping only its own files, and removes them.
    #[test]
    fn mascot_pack_installs_updates_and_removes_only_its_drawings() {
        if !exec::which("git") {
            return;
        }
        let dir = std::env::temp_dir().join(format!("miniutils-store-mascots-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let repo = dir.join("repo");
        let sprite = include_str!("../examples/appearance/sprites/jellyfish.toml");
        std::fs::create_dir_all(repo.join("sprites")).unwrap();
        std::fs::create_dir_all(repo.join("mascots/knight")).unwrap();
        std::fs::write(repo.join("sprites/owl.toml"), sprite).unwrap();
        std::fs::write(repo.join("sprites/notes.txt"), "not a sprite").unwrap();
        std::fs::write(repo.join("mascots/knight/Knight_Idle_2.png"), sheet(2)).unwrap();
        std::fs::write(repo.join("mascots/knight/Knight_Run_2.png"), sheet(2)).unwrap();
        std::fs::write(repo.join("install.sh"), "rm -rf ~").unwrap(); // must not be copied
        let v1 = Store::commit(&repo, "v1");
        let index = |id: &str, commit: &str, version: &str| -> Source {
            let path = dir.join(format!("{id}.json"));
            let r = serde_json::to_string(&repo.to_string_lossy()).unwrap();
            let json = format!(
                r#"{{"schema":1,"plugins":[],"mascots":[{{"id":"{id}","name":"Critters","license":"CC0-1.0","version":"{version}","repo":{r},"commit":"{commit}"}}]}}"#
            );
            std::fs::write(&path, json).unwrap();
            Source { index: path.to_string_lossy().to_string(), key: String::new() }
        };
        let appearance = dir.join("appearance");
        let id = "io.github.exemplo.critters";
        let src = index(id, &v1, "1.0.0");

        // the person's own code mascot `knight`: the pack's PNG `knight` would push it out
        std::fs::create_dir_all(appearance.join("sprites")).unwrap();
        std::fs::write(appearance.join("sprites/knight.toml"), "mine").unwrap();
        let err = install_mascot(&src, id, &appearance, &mut |_| true).unwrap_err();
        assert!(err.contains("sprites/knight.toml"), "{err}");
        assert!(!appearance.join("mascots/knight").exists());
        std::fs::remove_file(appearance.join("sprites/knight.toml")).unwrap();

        let mut summary = String::new();
        let msg = install_mascot(&src, id, &appearance, &mut |s| {
            summary = s.to_string();
            true
        })
        .unwrap();
        assert!(msg.contains("owl") && msg.contains("knight"), "{msg}");
        assert!(summary.contains("only drawings"), "{summary}");
        assert!(appearance.join("sprites/owl.toml").is_file());
        assert!(appearance.join("mascots/knight/Knight_Idle_2.png").is_file());
        assert!(!appearance.join("sprites/notes.txt").exists());
        assert!(!appearance.join("install.sh").exists());
        // what the hub reads from the folder
        let (sprites, skipped) = crate::mascot::file::load(&appearance.join("sprites"), &[]);
        assert!(skipped.is_empty() && sprites.len() == 1, "{skipped:?}");
        let catalog = statuses(&src, &dir.join("plugins"), &appearance).unwrap();
        let r = catalog.mascots[0].installed.as_ref().unwrap();
        assert_eq!((r.sprites.as_slice(), r.packs.as_slice()), (["owl".to_string()].as_slice(), ["knight".to_string()].as_slice()));
        assert!(install_mascot(&src, id, &appearance, &mut |_| true).unwrap_err().contains("already installed"));

        // another entry with the same keys: refused, nothing touched
        let other = index("io.github.exemplo.copycat", &v1, "1.0.0");
        let err = install_mascot(&other, "io.github.exemplo.copycat", &appearance, &mut |_| true).unwrap_err();
        assert!(err.contains("already exists"), "{err}");

        // the update renames the sprite: the old file goes, the new one comes
        std::fs::rename(repo.join("sprites/owl.toml"), repo.join("sprites/cat.toml")).unwrap();
        let v2 = Store::commit(&repo, "v2");
        let src = index(id, &v2, "1.1.0");
        install_mascot(&src, id, &appearance, &mut |_| true).unwrap();
        assert!(!appearance.join("sprites/owl.toml").exists());
        assert!(appearance.join("sprites/cat.toml").is_file());

        // a broken sprite in the next version: refused before touching the folder
        std::fs::write(repo.join("sprites/cat.toml"), "grid = 1").unwrap();
        let v3 = Store::commit(&repo, "v3");
        let src = index(id, &v3, "1.2.0");
        assert!(install_mascot(&src, id, &appearance, &mut |_| true).unwrap_err().contains("cat.toml"));
        assert!(appearance.join("sprites/cat.toml").is_file());

        let msg = remove_mascot(id, &appearance).unwrap();
        assert!(msg.contains("removed"), "{msg}");
        assert!(!appearance.join("sprites/cat.toml").exists());
        assert!(!appearance.join("mascots/knight").exists());
        assert!(remove_mascot(id, &appearance).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn index_signature() {
        let dir = std::env::temp_dir().join(format!("miniutils-sign-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let index = dir.join("registry.json");
        std::fs::write(&index, r#"{"schema":1,"plugins":[]}"#).unwrap();
        let key_file = dir.join("key");
        let public = keygen(&key_file).unwrap();
        assert!(keygen(&key_file).is_err(), "does not overwrite the key");
        sign(&index, &key_file).unwrap();
        let src = Source { index: index.to_string_lossy().to_string(), key: public.clone() };
        assert_eq!(load_index(&src).unwrap().1, Trust::Signed);
        // tampered with after signing
        std::fs::write(&index, r#"{"schema":1,"plugins":[ ]}"#).unwrap();
        assert!(load_index(&src).unwrap_err().contains("does not match"));
        // another store's key
        let other = Source { key: "11".repeat(32), ..src.clone() };
        assert!(load_index(&other).is_err());
        // remote without a key: refused before downloading
        let remote = Source { index: "https://example.invalid/registry.json".into(), key: String::new() };
        assert!(load_index(&remote).unwrap_err().contains("no key"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// `install_entry` with `Trust::Unreviewed` warns in the summary and records it.
    #[test]
    fn unreviewed_install_warns_and_is_recorded() {
        let Some(store) = Store::new("unreviewed") else { return };
        let src = store.index(&store.commit, "0.1.0", r#""uptime""#);
        let (index, _) = load_index(&src).unwrap();
        let mut entry = index.plugins[0].clone();
        entry.path = String::new();
        let root = store.dir.join("plugins");
        let mut summary = String::new();
        install_entry(&entry, &Trust::Unreviewed, &root, &mut |s| {
            summary = s.to_string();
            true
        })
        .unwrap();
        assert!(summary.contains("NOT REVIEWED"), "{summary}");
        assert!(summary.contains("not reviewed by any store"), "{summary}");
        let i = installed(&root, "io.github.exemplo.uptime").unwrap();
        assert!(i.unreviewed);
        assert_eq!(i.path, "");
        // a store install is not marked
        install(&src, "io.github.exemplo.uptime", &store.dir.join("plugins2"), &mut |_| true).unwrap();
        assert!(!installed(&store.dir.join("plugins2"), "io.github.exemplo.uptime").unwrap().unreviewed);
    }

    /// An `installed.json` written before `path` and `unreviewed` existed still parses.
    #[test]
    fn old_installed_json_still_parses() {
        let json = r#"{"id":"a.b","version":"1","repo":"https://x","commit":"abc","permissions":[],"net":[],"uses":[],"full_access":false,"sqlite":false}"#;
        let i: Installed = serde_json::from_str(json).unwrap();
        assert!(!i.unreviewed);
        assert!(i.path.is_empty());
        let out = serde_json::to_string(&i).unwrap();
        assert!(!out.contains("unreviewed") && !out.contains("\"path\""), "{out}");
    }

    /// The help lists the English commands, aligned.
    #[test]
    fn usage_lists_the_commands() {
        let u = USAGE.to_string();
        for cmd in ["--store", "--install", "--update", "--remove", "--reload", "--store-key", "--store-sign", "--store-entry", "--install-from", "--key"] {
            assert!(u.contains(cmd), "{cmd}: {u}");
        }
        assert!(u.contains("  miniutils --plugins                           lists"), "{u}");
    }
}
