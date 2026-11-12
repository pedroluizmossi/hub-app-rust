//! O banco SQLite de um plugin com script (`[data] kinds = ["sqlite"]`): `data.sqlite` na
//! pasta de dados do app (`core/appdata.rs`), aberto pelo host na primeira chamada.
//!
//! API do script:
//! ```text
//! db_batch(sql)               várias instruções de uma vez, sem parâmetros (criar tabelas)
//! db_exec(sql, [params])      uma instrução; devolve quantas linhas mudou
//! db_query(sql, [params])     uma consulta; devolve as linhas, cada uma num mapa coluna → valor
//! ```
//! Parâmetros posicionais (`?`): `()` vira NULL, bool vira 0/1, números e textos como são,
//! lista ou mapa vira texto JSON (leia de volta com `parse_json`). Na volta, INTEGER vira
//! inteiro, REAL vira número, TEXT vira texto, NULL vira `()` e BLOB, uma lista de bytes.
//!
//! O script não sai da pasta dele nem passa da cota:
//! - `ATTACH`/`DETACH` (abrir outro arquivo) e `PRAGMA` fora de uma lista de leitura e de
//!   `user_version` são recusados; extensões não carregam;
//! - o tamanho máximo é o `max_mb` do `[data]` (o SQLite recusa gravar além: `max_page_count`);
//! - cada instrução tem até `TIMEOUT` e cada consulta devolve até `MAX_ROWS` linhas;
//! - uma transação (`BEGIN`) aberta no fim da chamada do script é desfeita.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use rusqlite::limits::Limit;
use rusqlite::types::{Value as SqlValue, ValueRef};
use rusqlite::{Connection, ErrorCode};

use super::script::dyn_to_json;

/// Prazo de cada instrução (nos testes, curto: o teste da consulta sem fim espera por ele).
const TIMEOUT: Duration = if cfg!(test) { Duration::from_millis(400) } else { Duration::from_secs(5) };
/// Linhas que uma consulta pode devolver.
pub(super) const MAX_ROWS: usize = 10_000;
/// Maior texto ou blob num valor, e maior instrução SQL.
const MAX_VALUE: i32 = 4 << 20;
const MAX_SQL: i32 = 1 << 20;
/// PRAGMAs que o script pode usar (ler a estrutura; `user_version` para a migração).
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

/// Onde fica o banco e a cota.
#[derive(Clone, Debug)]
pub(super) struct DbSpec {
    pub path: PathBuf,
    pub max_bytes: u64,
}

/// O banco do plugin, aberto na primeira chamada.
#[derive(Default)]
pub(super) struct Db {
    /// `None` = o app não declarou `sqlite`.
    spec: Option<DbSpec>,
    conn: Option<Connection>,
    /// Prazo da instrução em curso, em ms desde `epoch` (o progress handler confere).
    deadline: Arc<AtomicU64>,
    epoch: Option<Instant>,
}

impl Db {
    pub(super) fn new(spec: Option<DbSpec>) -> Db {
        Db { spec, ..Default::default() }
    }

    fn conn(&mut self) -> Result<&Connection, String> {
        if self.conn.is_none() {
            let spec = self.spec.as_ref().ok_or("este app não declarou banco: ponha [data] kinds = [\"sqlite\"] no manifest.toml")?;
            let epoch = *self.epoch.get_or_insert_with(Instant::now);
            self.conn = Some(open(spec, epoch, self.deadline.clone()).map_err(|e| format!("banco: {e}"))?);
        }
        let epoch = self.epoch.unwrap_or_else(Instant::now);
        self.deadline.store((epoch.elapsed() + TIMEOUT).as_millis() as u64, Ordering::SeqCst);
        Ok(self.conn.as_ref().expect("aberto acima"))
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
                return Err(format!("a consulta passa de {MAX_ROWS} linhas: use LIMIT"));
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

    /// Fim de uma chamada do script: transação esquecida aberta é desfeita (senão travaria
    /// o banco até a próxima). Devolve o aviso, se houve.
    pub(super) fn end_call(&mut self) -> Option<String> {
        let conn = self.conn.as_ref()?;
        if conn.is_autocommit() {
            return None;
        }
        let _ = conn.execute_batch("ROLLBACK");
        Some("banco: a chamada terminou com uma transação aberta (BEGIN sem COMMIT); ela foi desfeita".into())
    }

    /// Fecha o arquivo (antes de devolver a cópia de uma migração que falhou).
    pub(super) fn close(&mut self) {
        self.conn = None;
    }

    fn max_bytes(&self) -> u64 {
        self.spec.as_ref().map_or(0, |s| s.max_bytes)
    }
}

/// Abre o banco com os limites. As PRAGMAs do host vêm antes do autorizador, que as
/// recusaria ao script.
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

/// O erro do SQLite num texto que diz o que fazer.
fn explain(e: rusqlite::Error, max_bytes: u64) -> String {
    match e.sqlite_error_code() {
        Some(ErrorCode::DiskFull) => {
            format!("banco: passou da cota de {} MB ([data] max_mb no manifest.toml)", max_bytes >> 20)
        }
        Some(ErrorCode::OperationInterrupted) => format!("banco: a instrução passou de {} s", TIMEOUT.as_secs()),
        Some(ErrorCode::AuthorizationForStatementDenied) => {
            "banco: instrução não permitida (ATTACH, DETACH e PRAGMA que muda o banco ficam com o host)".into()
        }
        _ => format!("banco: {e}"),
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
    Err(format!("banco: parâmetro de tipo {} não vale (use número, texto, bool, () ou lista/mapa)", v.type_name()))
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

    fn banco(nome: &str, max_bytes: u64) -> (Db, PathBuf) {
        let dir = std::env::temp_dir().join(format!("miniutils-db-{nome}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        (Db::new(Some(DbSpec { path: dir.join("data.sqlite"), max_bytes })), dir)
    }

    #[test]
    fn grava_le_e_converte() {
        let (mut db, dir) = banco("le", 10 << 20);
        db.batch("CREATE TABLE t(id INTEGER PRIMARY KEY, nome TEXT, peso REAL, extra TEXT, nada)").unwrap();
        let params: rhai::Array = vec!["ana".into(), (1.5 as rhai::FLOAT).into(), rhai::Array::from(vec![1.into()]).into(), ().into()];
        assert_eq!(db.exec("INSERT INTO t(nome, peso, extra, nada) VALUES (?, ?, ?, ?)", &params).unwrap(), 1);
        let rows = db.query("SELECT * FROM t WHERE nome = ?", &vec!["ana".into()]).unwrap();
        let m = rows[0].clone().cast::<rhai::Map>();
        assert_eq!(m["id"].as_int().unwrap(), 1);
        assert_eq!(m["peso"].as_float().unwrap(), 1.5);
        assert_eq!(m["extra"].clone().into_string().unwrap(), "[1]");
        assert!(m["nada"].is_unit());
        // transação esquecida aberta é desfeita no fim da chamada
        db.exec("BEGIN", &vec![]).unwrap();
        db.exec("DELETE FROM t", &vec![]).unwrap();
        assert!(db.end_call().is_some());
        assert_eq!(db.query("SELECT count(*) AS n FROM t", &vec![]).unwrap().len(), 1);
        assert!(db.end_call().is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn nao_sai_da_pasta_nem_da_cota() {
        let (mut db, dir) = banco("limites", 64 << 10);
        db.batch("CREATE TABLE t(x)").unwrap();
        let fora = dir.join("outro.sqlite");
        let attach = db.exec(&format!("ATTACH '{}' AS o", fora.display()), &vec![]);
        assert!(attach.is_err(), "ATTACH abriria outro arquivo");
        assert!(db.exec(&format!("VACUUM INTO '{}'", fora.display()), &vec![]).is_err());
        assert!(!fora.exists());
        assert!(db.exec("PRAGMA max_page_count = 100000", &vec![]).unwrap_err().contains("não permitida"));
        assert!(db.batch("PRAGMA journal_mode = DELETE").is_err());
        // a versão do esquema e a leitura da estrutura podem
        db.exec("PRAGMA user_version = 3", &vec![]).unwrap();
        assert_eq!(db.query("PRAGMA user_version", &vec![]).unwrap()[0].clone().cast::<rhai::Map>()["user_version"].as_int().unwrap(), 3);
        assert_eq!(db.query("PRAGMA table_info(t)", &vec![]).unwrap().len(), 1);
        // passar da cota de 64 KiB
        let grande: rhai::Dynamic = "x".repeat(20_000).into();
        let mut erro = String::new();
        for _ in 0..10 {
            if let Err(e) = db.exec("INSERT INTO t VALUES (?)", &vec![grande.clone()]) {
                erro = e;
                break;
            }
        }
        assert!(erro.contains("cota"), "{erro}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn consulta_longa_ou_grande_para() {
        let (mut db, dir) = banco("longa", 10 << 20);
        let lenta = "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c) SELECT count(*) FROM c";
        let t = Instant::now();
        assert!(db.query(lenta, &vec![]).unwrap_err().contains("passou de"));
        assert!(t.elapsed() < TIMEOUT + Duration::from_secs(3));
        let muitas = format!("WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c LIMIT {}) SELECT x FROM c", MAX_ROWS + 1);
        assert!(db.query(&muitas, &vec![]).unwrap_err().contains("LIMIT"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn sem_declarar_nao_tem_banco() {
        let mut db = Db::new(None);
        assert!(db.batch("CREATE TABLE t(x)").unwrap_err().contains("[data]"));
    }
}
