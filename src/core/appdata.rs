//! App data: one folder per app, managed by the hub (created, versioned, copied before a
//! migration, measured, exported and deleted). It applies to every kind of app: plugin
//! with a script, external app and built-in app.
//!
//! Where: `<data>/miniutils/app-data/<id>/` (`~/.local/share/miniutils/app-data` on
//! Linux, `%APPDATA%\miniutils\app-data` on Windows), or `MINIUTILS_DATA`. It is outside
//! the apps folder: an update replaces the whole app folder, and the data stays.
//!
//! What is inside depends on what the app declares in the manifest's `[data]` (`Kind`):
//! - `kv`: `store.json`, which the host writes through `store_set` (plugin with a script);
//! - `sqlite`: `data.sqlite`, the database the host opens for `db_exec`/`db_query` (plugin
//!   with a script; see `apps/plugin/db.rs`);
//! - `files`: whatever the app wants (external app, which gets the folder; built-in app).
//!
//! Plus the hub's `.miniutils.json`: the data version (`version` of `[data]`). When the app
//! bumps the version, the hub copies the folder to `.backups/<id>/` before the app migrates,
//! and puts it back if the migration fails (`prepare`, `restore`).
//!
//! ```toml
//! [data]
//! kinds = ["sqlite", "kv"]   # what the app stores (without [data]: kv for scripts, files for external apps)
//! version = 2                # data format; bumping it runs the migration
//! max_mb = 20                # database quota (1 to 1024; default 10)
//! ```

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::tr;

/// The hub's file with the data version, inside the app folder.
pub const META: &str = ".miniutils.json";
/// The data of `store_set` (plugin with a script).
pub const KV_FILE: &str = "store.json";
/// The database of `db_exec`/`db_query` (plugin with a script).
pub const DB_FILE: &str = "data.sqlite";
/// Copies from before a migration, one per app (folder name kept: it is already on disk).
const BACKUPS: &str = ".backups";
/// Stops measuring a folder after this many files (the size shown becomes "more than").
const MAX_ENTRIES: usize = 50_000;

/// What an app stores.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Kv,
    Sqlite,
    Files,
}

impl Kind {
    pub fn key(self) -> &'static str {
        match self {
            Kind::Kv => "kv",
            Kind::Sqlite => "sqlite",
            Kind::Files => "files",
        }
    }

    /// Name for the screen.
    pub fn label(self) -> &'static str {
        match self {
            Kind::Kv => tr!("key-value"),
            Kind::Sqlite => tr!("SQLite database"),
            Kind::Files => tr!("files"),
        }
    }
}

/// The manifest's `[data]` section.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DataSpec {
    #[serde(default)]
    pub kinds: Vec<Kind>,
    /// Version of the data format (1 to 1,000,000).
    #[serde(default = "first_version")]
    pub version: u32,
    /// Quota of the SQLite database, in MB (1 to 1024).
    #[serde(default = "default_max_mb")]
    pub max_mb: u32,
}

fn first_version() -> u32 {
    1
}

fn default_max_mb() -> u32 {
    10
}

impl DataSpec {
    /// Without `[data]`: the default for the kind of app.
    pub fn with(kinds: &[Kind]) -> DataSpec {
        DataSpec { kinds: kinds.to_vec(), version: first_version(), max_mb: default_max_mb() }
    }

    pub fn has(&self, k: Kind) -> bool {
        self.kinds.contains(&k)
    }

    pub fn check(&self) -> Result<(), String> {
        // shown to the user (Settings › System, `--plugins`): translated
        if !(1..=1_000_000).contains(&self.version) {
            return Err(tr!("[data] version = {}: use 1 to 1000000", self.version));
        }
        if !(1..=1024).contains(&self.max_mb) {
            return Err(tr!("[data] max_mb = {}: use 1 to 1024", self.max_mb));
        }
        if let Some(k) = self.kinds.iter().enumerate().find(|(i, k)| self.kinds[..*i].contains(k)).map(|(_, k)| k) {
            return Err(tr!("[data] kinds: \"{}\" repeated", k.key()));
        }
        Ok(())
    }

    /// "SQLite database and key-value" (empty if it stores nothing).
    pub fn describe(&self) -> String {
        let names: Vec<&str> = self.kinds.iter().map(|k| k.label()).collect();
        match names.as_slice() {
            [] => String::new(),
            [one] => one.to_string(),
            [rest @ .., last] => tr!("{} and {}", rest.join(", "), last),
        }
    }

    /// How many bytes the database may take.
    pub fn max_bytes(&self) -> u64 {
        u64::from(self.max_mb) << 20
    }
}

// ---- where --------------------------------------------------------------------------

/// The folder with every app's data: `MINIUTILS_DATA`, or `<data>/miniutils/app-data`.
pub fn root() -> PathBuf {
    match std::env::var_os("MINIUTILS_DATA").filter(|v| !v.is_empty()) {
        Some(d) => PathBuf::from(d),
        None => dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("miniutils").join("app-data"),
    }
}

/// The folder of the pre-migration copies.
fn backups(root: &Path) -> PathBuf {
    root.join(BACKUPS)
}

/// The data folder of an app.
pub fn dir(id: &str) -> PathBuf {
    root().join(id)
}

// ---- version and migration ----------------------------------------------------------

#[derive(Debug, Default, serde::Serialize, Deserialize)]
struct Meta {
    version: u32,
}

/// Is there anything besides the hub's file?
pub fn has_data(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|mut it| it.any(|e| e.is_ok_and(|e| e.file_name() != META)))
}

/// The data version in the folder: the one in `.miniutils.json`; without it, 1 if there is
/// data (from before versions existed) and `None` if the folder is empty.
pub fn stored_version(dir: &Path) -> Option<u32> {
    let meta = std::fs::read(dir.join(META)).ok().and_then(|b| serde_json::from_slice::<Meta>(&b).ok());
    match meta {
        Some(m) => Some(m.version),
        None => has_data(dir).then_some(1),
    }
}

pub fn set_version(dir: &Path, version: u32) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let json = serde_json::to_vec(&Meta { version }).unwrap_or_default();
    let path = dir.join(META);
    crate::core::os::write_atomic(&path, &json).map_err(|e| format!("{}: {e}", path.display()))
}

/// What the app has to do before starting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prep {
    /// The data is already at the app's version (or there is no data).
    Ready,
    /// Migrate from `from` to `to`; the copy from before is in `.backups/<id>/`.
    Migrate { from: u32, to: u32 },
}

/// Checks the data version against the app's. New version: copies the folder first (a copy
/// from an earlier attempt, of the same version, is kept: it is the one from before it all).
/// Data from a version newer than the app's: error, and the app does not start.
pub fn prepare(root: &Path, id: &str, version: u32) -> Result<Prep, String> {
    let dir = root.join(id);
    match stored_version(&dir) {
        None => {
            set_version(&dir, version)?;
            Ok(Prep::Ready)
        }
        Some(v) if v == version => Ok(Prep::Ready),
        Some(v) if v > version => Err(tr!(
            "the data is from version {}, newer than this app's ({}): update the app, or delete the data in Settings",
            v,
            version
        )),
        Some(v) => {
            // data from before versions: write 1 so the copy knows where it comes from
            set_version(&dir, v)?;
            let copy = backups(root).join(id);
            if stored_version(&copy) != Some(v) {
                remove(&copy)?;
                copy_dir(&dir, &copy)?;
            }
            Ok(Prep::Migrate { from: v, to: version })
        }
    }
}

/// Puts back the copy from before the migration (the migration failed).
pub fn restore(root: &Path, id: &str) -> Result<(), String> {
    let (dir, copy) = (root.join(id), backups(root).join(id));
    if !copy.is_dir() {
        return Err(tr!("there is no copy from before the migration").into());
    }
    remove(&dir)?;
    copy_dir(&copy, &dir)
}

// ---- size, export and delete --------------------------------------------------------

/// Bytes in the folder (without following links). `true` if it stopped at the file limit.
pub fn size(dir: &Path) -> (u64, bool) {
    let (mut total, mut seen) = (0u64, 0usize);
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            seen += 1;
            if seen > MAX_ENTRIES {
                return (total, true);
            }
            let Ok(meta) = e.path().symlink_metadata() else { continue };
            if meta.is_dir() {
                stack.push(e.path());
            } else if meta.is_file() {
                total += meta.len();
            }
        }
    }
    (total, false)
}

/// "1.2 MB" ("1,2 MB" in Portuguese: decimal comma).
pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = bytes as f64;
    let mut u = 0;
    // compare the value as shown (one decimal): 1048575 bytes is "1.0 MB", not "1024.0 KB"
    while (v * 10.0).round() >= 10240.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        return format!("{bytes} B");
    }
    let text = format!("{v:.1} {}", UNITS[u]);
    if crate::core::i18n::current() == crate::core::i18n::Language::PtBr { text.replace('.', ",") } else { text }
}

/// Deletes an app's data (and the copy from before the migration).
pub fn delete(root: &Path, id: &str) -> Result<(), String> {
    remove(&root.join(id))?;
    remove(&backups(root).join(id))
}

/// Copies an app's data to `<to>/<id>-<date and time>/`. The SQLite database goes through
/// `VACUUM INTO` (a whole copy even while the app is writing); the rest, as it is.
pub fn export(root: &Path, id: &str, to: &Path) -> Result<PathBuf, String> {
    let dir = root.join(id);
    if !dir.is_dir() {
        return Err(tr!("this app has no data").into());
    }
    let stamp = chrono::Local::now().format("%Y-%m-%d-%H%M%S");
    // a second export in the same second gets its own folder (VACUUM INTO refuses an
    // existing file)
    let base = format!("{id}-{stamp}");
    let mut dest = to.join(&base);
    let mut n = 2;
    while dest.exists() {
        dest = to.join(format!("{base}-{n}"));
        n += 1;
    }
    std::fs::create_dir_all(&dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    let db = dir.join(DB_FILE);
    let skip: Vec<PathBuf> = if db.is_file() {
        vacuum_into(&db, &dest.join(DB_FILE))?;
        ["", "-wal", "-shm", "-journal"].iter().map(|s| dir.join(format!("{DB_FILE}{s}"))).collect()
    } else {
        Vec::new()
    };
    copy_tree(&dir, &dest, &skip)?;
    Ok(dest)
}

/// Folder the exports go to: `Downloads/miniutils-data` (or the home folder).
pub fn export_dir() -> PathBuf {
    dirs::download_dir().or_else(dirs::home_dir).unwrap_or_else(|| PathBuf::from(".")).join("miniutils-data")
}

fn vacuum_into(db: &Path, to: &Path) -> Result<(), String> {
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = rusqlite::Connection::open_with_flags(db, flags).map_err(|e| format!("{}: {e}", db.display()))?;
    let _ = conn.busy_timeout(std::time::Duration::from_secs(5));
    conn.execute("VACUUM INTO ?1", [to.to_string_lossy()]).map_err(|e| format!("{}: {e}", db.display()))?;
    Ok(())
}

/// Ids with stored data (folders in `root` with something besides the version; not the hub's).
pub fn ids(root: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(root)
        .map(|rd| {
            rd.flatten()
                .filter(|e| has_data(&e.path()))
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| !n.starts_with('.'))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

// ---- files --------------------------------------------------------------------------

fn remove(path: &Path) -> Result<(), String> {
    match std::fs::remove_dir_all(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!("{}: {e}", path.display())),
        _ => Ok(()),
    }
}

fn copy_dir(from: &Path, to: &Path) -> Result<(), String> {
    copy_tree(from, to, &[])
}

/// Copies the tree (regular files and folders; links are left out), except `skip`.
fn copy_tree(from: &Path, to: &Path, skip: &[PathBuf]) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| format!("{}: {e}", to.display()))?;
    for e in std::fs::read_dir(from).map_err(|e| format!("{}: {e}", from.display()))?.flatten() {
        let path = e.path();
        if skip.contains(&path) {
            continue;
        }
        let Ok(meta) = path.symlink_metadata() else { continue };
        let dest = to.join(e.file_name());
        if meta.is_dir() {
            copy_tree(&path, &dest, skip)?;
        } else if meta.is_file() {
            std::fs::copy(&path, &dest).map_err(|e| format!("{}: {e}", path.display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;


    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("miniutils-appdata-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn manifest_data_section() {
        let s: DataSpec = toml::from_str("kinds = [\"sqlite\", \"kv\"]\nversion = 3").unwrap();
        assert_eq!(s.kinds, [Kind::Sqlite, Kind::Kv]);
        assert_eq!((s.version, s.max_mb), (3, 10));
        assert_eq!(s.describe(), "SQLite database and key-value");
        assert!(s.check().is_ok());
        assert!(toml::from_str::<DataSpec>("kinds = [\"cloud\"]").is_err());
        assert!(toml::from_str::<DataSpec>("size = 1").is_err());
        let rep: DataSpec = toml::from_str("kinds = [\"kv\", \"kv\"]").unwrap();
        assert!(rep.check().unwrap_err().contains("repeated"));
        let big: DataSpec = toml::from_str("max_mb = 5000").unwrap();
        assert!(big.check().is_err());
        assert_eq!(DataSpec::with(&[]).describe(), "");
    }

    #[test]
    fn version_copies_and_restores() {
        let root = temp("version");
        let id = "io.x.app";
        // no data: writes the version and that's it
        assert_eq!(prepare(&root, id, 2).unwrap(), Prep::Ready);
        assert_eq!(stored_version(&root.join(id)), Some(2));
        assert!(!has_data(&root.join(id)));
        assert_eq!(prepare(&root, id, 2).unwrap(), Prep::Ready);
        // data from before versions counts as 1
        let other = "io.x.old";
        std::fs::create_dir_all(root.join(other)).unwrap();
        std::fs::write(root.join(other).join("a.txt"), "v1").unwrap();
        assert_eq!(prepare(&root, other, 3).unwrap(), Prep::Migrate { from: 1, to: 3 });
        // the migration touches the data and fails: the copy comes back
        std::fs::write(root.join(other).join("a.txt"), "broken").unwrap();
        // trying again does not replace the copy from before it all
        assert_eq!(prepare(&root, other, 3).unwrap(), Prep::Migrate { from: 1, to: 3 });
        restore(&root, other).unwrap();
        assert_eq!(std::fs::read_to_string(root.join(other).join("a.txt")).unwrap(), "v1");
        // it worked: new version
        set_version(&root.join(other), 3).unwrap();
        assert_eq!(prepare(&root, other, 3).unwrap(), Prep::Ready);
        // app older than the data: does not start
        assert!(prepare(&root, other, 2).unwrap_err().contains("newer"));
        // the version alone does not count as stored data
        assert_eq!(ids(&root), [other]);
        delete(&root, other).unwrap();
        assert!(!root.join(other).exists() && !root.join(BACKUPS).join(other).exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn exports_the_whole_database_and_measures() {
        let root = temp("export");
        let dir = root.join("io.x.db");
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub").join("note.txt"), "hi").unwrap();
        {
            let c = rusqlite::Connection::open(dir.join(DB_FILE)).unwrap();
            c.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE t(x); INSERT INTO t VALUES (42);").unwrap();
            // connection open (the app running): VACUUM INTO still takes everything
            let out = export(&root, "io.x.db", &root.join("out")).unwrap();
            let read: i64 = rusqlite::Connection::open(out.join(DB_FILE)).unwrap().query_row("SELECT x FROM t", [], |r| r.get(0)).unwrap();
            assert_eq!(read, 42);
            assert_eq!(std::fs::read_to_string(out.join("sub").join("note.txt")).unwrap(), "hi");
            assert!(!out.join(format!("{DB_FILE}-wal")).exists());
            // again right away (same second): a folder of its own, not an error
            let again = export(&root, "io.x.db", &root.join("out")).unwrap();
            assert_ne!(again, out);
            assert!(again.join(DB_FILE).is_file());
        }
        let (bytes, cut) = size(&dir);
        assert!(bytes > 2 && !cut);
        assert!(export(&root, "io.x.none", &root).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn readable_size() {
        assert_eq!(human(0), "0 B");
        assert_eq!(human(1536), "1.5 KB");
        assert_eq!(human(3 << 20), "3.0 MB");
        // just under a unit: rounds up into the next one
        assert_eq!(human(1_048_575), "1.0 MB");
        assert_eq!(human(1023), "1023 B");
        assert_eq!(human(1024), "1.0 KB");
    }
}
