//! The SQLite database of a scripted plugin (`[data] kinds = ["sqlite"]`): `data.sqlite` in
//! the app's data folder (`core/appdata.rs`), opened by the host on the first call.
//!
//! Script API:
//! ```text
//! db_batch(sql)               several statements at once, no parameters (create tables)
//! db_exec(sql, [params])      one statement; returns how many rows it changed
//! db_query(sql, [params])     one query; returns the rows, each as a column → value map
//! ```
//! Positional parameters (`?`): `()` becomes NULL, bool becomes 0/1, numbers and texts stay
//! as they are, a list or map becomes JSON text (read it back with `parse_json`). Coming
//! back, INTEGER becomes an integer, REAL a number, TEXT a text, NULL `()` and BLOB a list
//! of bytes.
//!
//! The script doesn't leave its folder or go over its quota:
//! - `ATTACH`/`DETACH` (opening another file) and `PRAGMA` outside a read-only list and
//!   `user_version` are refused; extensions don't load;
//! - the maximum size is the `max_mb` of `[data]` (SQLite refuses to write past it:
//!   `max_page_count`);
//! - each statement gets up to `TIMEOUT` and each query returns up to `MAX_ROWS` rows;
//! - a transaction (`BEGIN`) left open at the end of the script's call is rolled back.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use rusqlite::limits::Limit;
use rusqlite::types::{Value as SqlValue, ValueRef};
use rusqlite::{Connection, ErrorCode};

use super::script::dyn_to_json;
use crate::tr;

/// Time limit of each statement (short in tests: the endless-query test waits for it).
const TIMEOUT: Duration = if cfg!(test) { Duration::from_millis(400) } else { Duration::from_secs(5) };
/// Rows a query may return.
pub(super) const MAX_ROWS: usize = 10_000;
/// Largest text or blob in a value, and largest SQL statement.
const MAX_VALUE: i32 = 4 << 20;
const MAX_SQL: i32 = 1 << 20;
/// PRAGMAs the script may use (reading the structure; `user_version` for migrations).
const PRAGMAS: &[&str] = &[
    "user_version",
    "table_info",
    "table_xinfo",
    "table_list",
    "index_list",
    "index_info",
    "index_xinfo",
    "foreign_key_list",
    "foreign_key_check",
    "integrity_check",
    "quick_check",
    "optimize",
];

/// Where the database lives and its quota.
#[derive(Clone, Debug)]
pub(super) struct DbSpec {
    pub path: PathBuf,
    pub max_bytes: u64,
}

/// The plugin's database, opened on the first call.
#[derive(Default)]
pub(super) struct Db {
    /// `None` = the app didn't declare `sqlite`.
    spec: Option<DbSpec>,
    conn: Option<Connection>,
    /// Deadline of the running statement, in ms since `epoch` (the progress handler checks it).
    deadline: Arc<AtomicU64>,
    epoch: Option<Instant>,
}

impl Db {
    pub(super) fn new(spec: Option<DbSpec>) -> Db {
        Db { spec, ..Default::default() }
    }

    fn conn(&mut self) -> Result<&Connection, String> {
        if self.conn.is_none() {
            let spec = self.spec.as_ref().ok_or(tr!("this app declared no database: add [data] kinds = [\"sqlite\"] to manifest.toml"))?;
            let epoch = *self.epoch.get_or_insert_with(Instant::now);
            self.conn = Some(open(spec, epoch, self.deadline.clone()).map_err(|e| tr!("database: {}", e))?);
        }
        let epoch = self.epoch.unwrap_or_else(Instant::now);
        self.deadline.store((epoch.elapsed() + TIMEOUT).as_millis() as u64, Ordering::SeqCst);
        Ok(self.conn.as_ref().expect("opened above"))
    }

    pub(super) fn batch(&mut self, sql: &str) -> Result<(), String> {
        let max = self.max_bytes();
        self.conn()?.execute_batch(sql).map_err(|e| explain(e, max))
    }

    pub(super) fn exec(&mut self, sql: &str, params: &rhai::Array) -> Result<i64, String> {
        let max = self.max_bytes();
        let values = to_params(params)?;
        let conn = self.conn()?;
        let mut st = conn.prepare(sql).map_err(|e| explain(e, max))?;
        st.execute(rusqlite::params_from_iter(values)).map(|n| n as i64).map_err(|e| explain(e, max))
    }

    pub(super) fn query(&mut self, sql: &str, params: &rhai::Array) -> Result<rhai::Array, String> {
        let max = self.max_bytes();
        let values = to_params(params)?;
        let conn = self.conn()?;
        let mut st = conn.prepare(sql).map_err(|e| explain(e, max))?;
        let names: Vec<String> = st.column_names().iter().map(|s| s.to_string()).collect();
        let mut rows = st.query(rusqlite::params_from_iter(values)).map_err(|e| explain(e, max))?;
        let mut out = rhai::Array::new();
        while let Some(row) = rows.next().map_err(|e| explain(e, max))? {
            if out.len() == MAX_ROWS {
                return Err(tr!("the query returns more than {} rows: use LIMIT", MAX_ROWS));
            }
            let mut m = rhai::Map::new();
            for (i, name) in names.iter().enumerate() {
                let v = row.get_ref(i).map_err(|e| e.to_string())?;
                m.insert(name.as_str().into(), from_sql(v));
            }
            out.push(m.into());
        }
        Ok(out)
    }

    /// End of a script call: a transaction left open is rolled back (otherwise it would lock
    /// the database until the next one). Returns the warning, if any.
    pub(super) fn end_call(&mut self) -> Option<String> {
        let conn = self.conn.as_ref()?;
        if conn.is_autocommit() {
            return None;
        }
        let _ = conn.execute_batch("ROLLBACK");
        Some(tr!("database: the call ended with an open transaction (BEGIN without COMMIT); it was rolled back").into())
    }

    /// Closes the file (before restoring the copy after a failed migration).
    pub(super) fn close(&mut self) {
        self.conn = None;
    }

    fn max_bytes(&self) -> u64 {
        self.spec.as_ref().map_or(0, |s| s.max_bytes)
    }
}

/// Opens the database with the limits. The host's PRAGMAs come before the authorizer,
/// which would refuse them to the script.
fn open(spec: &DbSpec, epoch: Instant, deadline: Arc<AtomicU64>) -> rusqlite::Result<Connection> {
    if let Some(dir) = spec.path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let conn = Connection::open(&spec.path)?;
    conn.busy_timeout(Duration::from_secs(2))?;
    conn.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", true)?;
    let page: i64 = conn.pragma_query_value(None, "page_size", |r| r.get(0))?;
    let pages = (spec.max_bytes / page.max(512) as u64).max(16);
    conn.query_row(&format!("PRAGMA max_page_count = {pages}"), [], |_| Ok(()))?;
    conn.set_limit(Limit::SQLITE_LIMIT_ATTACHED, 0)?;
    conn.set_limit(Limit::SQLITE_LIMIT_LENGTH, MAX_VALUE)?;
    conn.set_limit(Limit::SQLITE_LIMIT_SQL_LENGTH, MAX_SQL)?;
    conn.authorizer(Some(|ctx: AuthContext<'_>| match ctx.action {
        AuthAction::Attach { .. } | AuthAction::Detach { .. } => Authorization::Deny,
        AuthAction::Pragma { pragma_name, .. } if !PRAGMAS.contains(&pragma_name.to_ascii_lowercase().as_str()) => {
            Authorization::Deny
        }
        _ => Authorization::Allow,
    }));
    conn.progress_handler(
        1000,
        Some(move || epoch.elapsed().as_millis() as u64 > deadline.load(Ordering::SeqCst)),
    );
    Ok(conn)
}

/// The SQLite error as text that says what to do.
fn explain(e: rusqlite::Error, max_bytes: u64) -> String {
    match e.sqlite_error_code() {
        Some(ErrorCode::DiskFull) => {
            tr!("database: over the {} MB quota ([data] max_mb in manifest.toml)", max_bytes >> 20)
        }
        Some(ErrorCode::OperationInterrupted) => tr!("database: the statement took longer than {} s", TIMEOUT.as_secs()),
        Some(ErrorCode::AuthorizationForStatementDenied) => {
            tr!("database: statement not allowed (ATTACH, DETACH and PRAGMAs that change the database are reserved for the host)").into()
        }
        _ => tr!("database: {}", e),
    }
}

fn to_params(params: &rhai::Array) -> Result<Vec<SqlValue>, String> {
    params.iter().map(to_sql).collect()
}

fn to_sql(v: &rhai::Dynamic) -> Result<SqlValue, String> {
    if v.is_unit() {
        return Ok(SqlValue::Null);
    }
    if let Some(b) = v.clone().try_cast::<bool>() {
        return Ok(SqlValue::Integer(b as i64));
    }
    if let Some(i) = v.clone().try_cast::<rhai::INT>() {
        return Ok(SqlValue::Integer(i));
    }
    if let Some(f) = v.clone().try_cast::<rhai::FLOAT>() {
        return Ok(SqlValue::Real(f));
    }
    if v.is_string() || v.is_char() {
        return Ok(SqlValue::Text(v.to_string()));
    }
    if v.is_array() || v.is_map() {
        return serde_json::to_string(&dyn_to_json(v)).map(SqlValue::Text).map_err(|e| e.to_string());
    }
    Err(tr!("database: a parameter of type {} isn't allowed (use a number, text, bool, () or a list/map)", v.type_name()))
}

fn from_sql(v: ValueRef<'_>) -> rhai::Dynamic {
    match v {
        ValueRef::Null => rhai::Dynamic::UNIT,
        ValueRef::Integer(i) => i.into(),
        ValueRef::Real(f) => f.into(),
        ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned().into(),
        ValueRef::Blob(b) => b.iter().map(|x| rhai::Dynamic::from(*x as rhai::INT)).collect::<rhai::Array>().into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn database(name: &str, max_bytes: u64) -> (Db, PathBuf) {
        let dir = std::env::temp_dir().join(format!("miniutils-db-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        (Db::new(Some(DbSpec { path: dir.join("data.sqlite"), max_bytes })), dir)
    }

    #[test]
    fn writes_reads_and_converts() {
        let (mut db, dir) = database("read", 10 << 20);
        db.batch("CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT, weight REAL, extra TEXT, empty)").unwrap();
        let params: rhai::Array = vec!["ana".into(), (1.5 as rhai::FLOAT).into(), rhai::Array::from(vec![1.into()]).into(), ().into()];
        assert_eq!(db.exec("INSERT INTO t(name, weight, extra, empty) VALUES (?, ?, ?, ?)", &params).unwrap(), 1);
        let rows = db.query("SELECT * FROM t WHERE name = ?", &vec!["ana".into()]).unwrap();
        let m = rows[0].clone().cast::<rhai::Map>();
        assert_eq!(m["id"].as_int().unwrap(), 1);
        assert_eq!(m["weight"].as_float().unwrap(), 1.5);
        assert_eq!(m["extra"].clone().into_string().unwrap(), "[1]");
        assert!(m["empty"].is_unit());
        // a transaction left open is rolled back at the end of the call
        db.exec("BEGIN", &vec![]).unwrap();
        db.exec("DELETE FROM t", &vec![]).unwrap();
        assert!(db.end_call().is_some());
        assert_eq!(db.query("SELECT count(*) AS n FROM t", &vec![]).unwrap().len(), 1);
        assert!(db.end_call().is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn stays_inside_its_folder_and_quota() {
        let (mut db, dir) = database("limits", 64 << 10);
        db.batch("CREATE TABLE t(x)").unwrap();
        let outside = dir.join("other.sqlite");
        let attach = db.exec(&format!("ATTACH '{}' AS o", outside.display()), &vec![]);
        assert!(attach.is_err(), "ATTACH would open another file");
        assert!(db.exec(&format!("VACUUM INTO '{}'", outside.display()), &vec![]).is_err());
        assert!(!outside.exists());
        assert!(db.exec("PRAGMA max_page_count = 100000", &vec![]).unwrap_err().contains("not allowed"));
        assert!(db.batch("PRAGMA journal_mode = DELETE").is_err());
        // the schema version and reading the structure are allowed
        db.exec("PRAGMA user_version = 3", &vec![]).unwrap();
        assert_eq!(db.query("PRAGMA user_version", &vec![]).unwrap()[0].clone().cast::<rhai::Map>()["user_version"].as_int().unwrap(), 3);
        assert_eq!(db.query("PRAGMA table_info(t)", &vec![]).unwrap().len(), 1);
        // going over the 64 KiB quota
        let big: rhai::Dynamic = "x".repeat(20_000).into();
        let mut error = String::new();
        for _ in 0..10 {
            if let Err(e) = db.exec("INSERT INTO t VALUES (?)", &vec![big.clone()]) {
                error = e;
                break;
            }
        }
        assert!(error.contains("quota"), "{error}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn long_or_large_query_stops() {
        let (mut db, dir) = database("long", 10 << 20);
        let slow = "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c) SELECT count(*) FROM c";
        let t = Instant::now();
        assert!(db.query(slow, &vec![]).unwrap_err().contains("longer than"));
        assert!(t.elapsed() < TIMEOUT + Duration::from_secs(3));
        let many = format!("WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c LIMIT {}) SELECT x FROM c", MAX_ROWS + 1);
        assert!(db.query(&many, &vec![]).unwrap_err().contains("LIMIT"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn undeclared_has_no_database() {
        let mut db = Db::new(None);
        assert!(db.batch("CREATE TABLE t(x)").unwrap_err().contains("[data]"));
    }
}
