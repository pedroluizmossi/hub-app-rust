//! Dados dos apps: uma pasta por app, que o hub cuida (cria, versiona, copia antes de
//! migrar, mede, exporta e apaga). Vale para todos os tipos: plugin com script, app
//! externo e app embutido.
//!
//! Onde: `<dados>/miniutils/app-data/<id>/` (`~/.local/share/miniutils/app-data` no
//! Linux, `%APPDATA%\miniutils\app-data` no Windows), ou `MINIUTILS_DADOS`. Fica fora da
//! pasta dos apps: uma atualização troca a pasta do app inteira, e os dados ficam.
//!
//! O que tem dentro depende do que o app declara no `[data]` do manifesto (`Kind`):
//! - `kv`: `store.json`, que o host grava por `store_set` (plugin com script);
//! - `sqlite`: `data.sqlite`, o banco que o host abre por `db_exec`/`db_query` (plugin com
//!   script; ver `apps/plugin/db.rs`);
//! - `files`: o que o app quiser (app externo, que recebe a pasta; app embutido).
//!
//! Mais `.miniutils.json`, do hub: a versão dos dados (`version` do `[data]`). Quando o app
//! sobe a versão, o hub copia a pasta para `.copias/<id>/` antes de o app migrar, e a
//! devolve se a migração falhar (`prepare`, `restore`).
//!
//! ```toml
//! [data]
//! kinds = ["sqlite", "kv"]   # o que o app guarda (sem [data]: kv no script, files no app externo)
//! version = 2                # formato dos dados; subir chama a migração
//! max_mb = 20                # cota do banco (1 a 1024; padrão 10)
//! ```

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Arquivo do hub com a versão dos dados, dentro da pasta do app.
pub const META: &str = ".miniutils.json";
/// Os dados de `store_set` (plugin com script).
pub const KV_FILE: &str = "store.json";
/// O banco de `db_exec`/`db_query` (plugin com script).
pub const DB_FILE: &str = "data.sqlite";
/// Cópias de antes da migração, uma por app.
const BACKUPS: &str = ".copias";
/// Para de medir uma pasta depois de tantos arquivos (o tamanho mostrado fica "mais de").
const MAX_ENTRIES: usize = 50_000;

/// O que um app guarda.
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

    /// Nome para a tela.
    pub fn label(self) -> &'static str {
        match self {
            Kind::Kv => "chave-valor",
            Kind::Sqlite => "banco SQLite",
            Kind::Files => "arquivos",
        }
    }
}

/// A seção `[data]` do manifesto.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DataSpec {
    #[serde(default)]
    pub kinds: Vec<Kind>,
    /// Versão do formato dos dados (1 a 1.000.000).
    #[serde(default = "first_version")]
    pub version: u32,
    /// Cota do banco SQLite, em MB (1 a 1024).
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
    /// Sem `[data]`: o padrão do tipo de app.
    pub fn with(kinds: &[Kind]) -> DataSpec {
        DataSpec { kinds: kinds.to_vec(), version: first_version(), max_mb: default_max_mb() }
    }

    pub fn has(&self, k: Kind) -> bool {
        self.kinds.contains(&k)
    }

    pub fn check(&self) -> Result<(), String> {
        if !(1..=1_000_000).contains(&self.version) {
            return Err(format!("[data] version = {}: use de 1 a 1000000", self.version));
        }
        if !(1..=1024).contains(&self.max_mb) {
            return Err(format!("[data] max_mb = {}: use de 1 a 1024", self.max_mb));
        }
        if let Some(k) = self.kinds.iter().enumerate().find(|(i, k)| self.kinds[..*i].contains(k)).map(|(_, k)| k) {
            return Err(format!("[data] kinds: \"{}\" repetido", k.key()));
        }
        Ok(())
    }

    /// "banco SQLite e chave-valor" (vazio se não guarda nada).
    pub fn describe(&self) -> String {
        let names: Vec<&str> = self.kinds.iter().map(|k| k.label()).collect();
        match names.as_slice() {
            [] => String::new(),
            [one] => one.to_string(),
            [rest @ .., last] => format!("{} e {last}", rest.join(", ")),
        }
    }

    /// O banco cabe em quantos bytes.
    pub fn max_bytes(&self) -> u64 {
        u64::from(self.max_mb) << 20
    }
}

// ---- onde ---------------------------------------------------------------------------

/// A pasta com os dados de todos os apps: `MINIUTILS_DADOS`, ou `<dados>/miniutils/app-data`.
pub fn root() -> PathBuf {
    match std::env::var_os("MINIUTILS_DADOS").filter(|v| !v.is_empty()) {
        Some(d) => PathBuf::from(d),
        None => dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("miniutils").join("app-data"),
    }
}

/// A pasta dos dados de um app.
pub fn dir(id: &str) -> PathBuf {
    root().join(id)
}

// ---- versão e migração --------------------------------------------------------------

#[derive(Debug, Default, serde::Serialize, Deserialize)]
struct Meta {
    version: u32,
}

/// Tem algo além do arquivo do hub?
pub fn has_data(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|mut it| it.any(|e| e.is_ok_and(|e| e.file_name() != META)))
}

/// A versão dos dados na pasta: a do `.miniutils.json`; sem ele, 1 se houver dados (de
/// antes de existir versão) e `None` se a pasta estiver vazia.
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
    write_atomic(&dir.join(META), &json)
}

/// O que o app tem de fazer antes de começar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prep {
    /// Os dados já estão na versão do app (ou não há dados).
    Ready,
    /// Migrar de `from` para `to`; a cópia de antes está em `.copias/<id>/`.
    Migrate { from: u32, to: u32 },
}

/// Confere a versão dos dados contra a do app. Versão nova: copia a pasta antes (a cópia
/// de uma tentativa anterior, da mesma versão, é mantida: ela é a de antes de tudo).
/// Dados de uma versão mais nova que a do app: erro, e o app não começa.
pub fn prepare(root: &Path, id: &str, version: u32) -> Result<Prep, String> {
    let dir = root.join(id);
    match stored_version(&dir) {
        None => {
            set_version(&dir, version)?;
            Ok(Prep::Ready)
        }
        Some(v) if v == version => Ok(Prep::Ready),
        Some(v) if v > version => Err(format!(
            "os dados são da versão {v}, mais nova que a deste app ({version}): atualize o app, ou apague os dados nas Configurações"
        )),
        Some(v) => {
            // dados de antes da versão: grava a 1 para a cópia saber de onde é
            set_version(&dir, v)?;
            let copy = root.join(BACKUPS).join(id);
            if stored_version(&copy) != Some(v) {
                remove(&copy)?;
                copy_dir(&dir, &copy)?;
            }
            Ok(Prep::Migrate { from: v, to: version })
        }
    }
}

/// Devolve a cópia de antes da migração (a migração falhou).
pub fn restore(root: &Path, id: &str) -> Result<(), String> {
    let (dir, copy) = (root.join(id), root.join(BACKUPS).join(id));
    if !copy.is_dir() {
        return Err("não há cópia de antes da migração".into());
    }
    remove(&dir)?;
    copy_dir(&copy, &dir)
}

// ---- tamanho, exportar e apagar -----------------------------------------------------

/// Bytes na pasta (sem seguir links). `true` se parou no limite de arquivos.
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

/// "1,2 MB" (vírgula decimal).
pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 { format!("{bytes} B") } else { format!("{v:.1} {}", UNITS[u]).replace('.', ",") }
}

/// Apaga os dados de um app (e a cópia de antes da migração).
pub fn delete(root: &Path, id: &str) -> Result<(), String> {
    remove(&root.join(id))?;
    remove(&root.join(BACKUPS).join(id))
}

/// Copia os dados de um app para `<para>/<id>-<data e hora>/`. O banco SQLite sai por
/// `VACUUM INTO` (uma cópia inteira mesmo com o app gravando); o resto, como está.
pub fn export(root: &Path, id: &str, to: &Path) -> Result<PathBuf, String> {
    let dir = root.join(id);
    if !dir.is_dir() {
        return Err("este app não tem dados".into());
    }
    let stamp = chrono::Local::now().format("%Y-%m-%d-%H%M%S");
    let dest = to.join(format!("{id}-{stamp}"));
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

/// Pasta para onde vão as exportações: `Downloads/miniutils-dados` (ou a pasta pessoal).
pub fn export_dir() -> PathBuf {
    dirs::download_dir().or_else(dirs::home_dir).unwrap_or_else(|| PathBuf::from(".")).join("miniutils-dados")
}

fn vacuum_into(db: &Path, to: &Path) -> Result<(), String> {
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = rusqlite::Connection::open_with_flags(db, flags).map_err(|e| format!("{}: {e}", db.display()))?;
    let _ = conn.busy_timeout(std::time::Duration::from_secs(5));
    conn.execute("VACUUM INTO ?1", [to.to_string_lossy()]).map_err(|e| format!("{}: {e}", db.display()))?;
    Ok(())
}

/// Ids com dados guardados (pastas de `root` com algo além da versão; sem as do hub).
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

// ---- pastas de antes ----------------------------------------------------------------

/// Traz os dados do lugar antigo: `plugins/.data/<id>.json` (script) e
/// `plugins/.data/<id>/` (app externo) e, com `todo`, o `todo.json` da pasta de config. Sem
/// nada para trazer, não faz nada.
///
/// Com `MINIUTILS_DADOS` (instância de desenvolvimento, teste) não move nada: os arquivos
/// antigos são os de verdade, e a pasta nova pode ser apagada depois. O To-do só vem quando
/// o hub abre (`todo`): o hub que estiver aberto ainda grava no lugar antigo.
pub fn migrate_legacy(todo: bool) {
    if std::env::var_os("MINIUTILS_DADOS").is_some() {
        return;
    }
    let root = root();
    let n = migrate_plugin_data(&crate::core::plugins::dir().join(".data"), &root);
    if n > 0 {
        log::info!("dados de {n} app(s) trazidos de plugins/.data para {}", root.display());
    }
    let Some(cfg) = dirs::config_dir().filter(|_| todo) else { return };
    let old = cfg.join("miniutils").join("todo.json");
    let new = root.join("cyber.todo").join("todo.json");
    if !old.is_file() {
        return;
    }
    if new.exists() {
        // os dois existem: fica o novo, e o antigo não é apagado
        log::warn!("To-do: {} ficou de fora ({} já existe)", old.display(), new.display());
        return;
    }
    match std::fs::create_dir_all(new.parent().unwrap_or(&root)).map_err(|e| e.to_string()).and_then(|_| move_path(&old, &new)) {
        Ok(()) => log::info!("To-do: {} → {}", old.display(), new.display()),
        Err(e) => log::warn!("To-do: não consegui trazer {}: {e}", old.display()),
    }
}

/// Move o conteúdo de `legacy` para `root`; devolve quantos apps vieram.
fn migrate_plugin_data(legacy: &Path, root: &Path) -> usize {
    let Ok(rd) = std::fs::read_dir(legacy) else { return 0 };
    let mut moved = 0;
    for e in rd.flatten() {
        let (path, name) = (e.path(), e.file_name().to_string_lossy().into_owned());
        let result = if path.is_dir() {
            // app externo: a pasta inteira (o que já existir no destino fica)
            let dest = root.join(&name);
            std::fs::create_dir_all(&dest).map_err(|e| e.to_string()).and_then(|_| {
                for f in std::fs::read_dir(&path).map_err(|e| e.to_string())?.flatten() {
                    let to = dest.join(f.file_name());
                    if !to.exists() {
                        move_path(&f.path(), &to)?;
                    }
                }
                Ok(())
            })
        } else if let Some(id) = name.strip_suffix(".json") {
            let to = root.join(id).join(KV_FILE);
            if to.exists() {
                Ok(())
            } else {
                std::fs::create_dir_all(root.join(id)).map_err(|e| e.to_string()).and_then(|_| move_path(&path, &to))
            }
        } else {
            continue;
        };
        match result {
            Ok(()) => moved += 1,
            Err(e) => log::warn!("dados de {name}: não consegui trazer de {}: {e}", legacy.display()),
        }
    }
    // só some se ficou vazia (o que não deu para trazer continua lá)
    let _ = remove_empty_dirs(legacy);
    moved
}

fn remove_empty_dirs(dir: &Path) -> std::io::Result<()> {
    for e in std::fs::read_dir(dir)?.flatten() {
        if e.path().is_dir() {
            let _ = remove_empty_dirs(&e.path());
        }
    }
    std::fs::remove_dir(dir)
}

// ---- arquivos -----------------------------------------------------------------------

/// Arquivo novo por inteiro: grava ao lado e troca (nunca fica pela metade).
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes).and_then(|_| std::fs::rename(&tmp, path)).map_err(|e| format!("{}: {e}", path.display()))
}

fn remove(path: &Path) -> Result<(), String> {
    match std::fs::remove_dir_all(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!("{}: {e}", path.display())),
        _ => Ok(()),
    }
}

/// `rename`, ou copiar e apagar quando a origem está em outro disco.
fn move_path(from: &Path, to: &Path) -> Result<(), String> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    if from.is_dir() {
        copy_dir(from, to)?;
        remove(from)
    } else {
        std::fs::copy(from, to).map_err(|e| format!("{}: {e}", to.display()))?;
        std::fs::remove_file(from).map_err(|e| format!("{}: {e}", from.display()))
    }
}

fn copy_dir(from: &Path, to: &Path) -> Result<(), String> {
    copy_tree(from, to, &[])
}

/// Copia a árvore (arquivos comuns e pastas; links ficam de fora), menos `skip`.
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

    fn temp(nome: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("miniutils-appdata-{nome}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn secao_data_do_manifesto() {
        let s: DataSpec = toml::from_str("kinds = [\"sqlite\", \"kv\"]\nversion = 3").unwrap();
        assert_eq!(s.kinds, [Kind::Sqlite, Kind::Kv]);
        assert_eq!((s.version, s.max_mb), (3, 10));
        assert_eq!(s.describe(), "banco SQLite e chave-valor");
        assert!(s.check().is_ok());
        assert!(toml::from_str::<DataSpec>("kinds = [\"nuvem\"]").is_err());
        assert!(toml::from_str::<DataSpec>("tamanho = 1").is_err());
        let rep: DataSpec = toml::from_str("kinds = [\"kv\", \"kv\"]").unwrap();
        assert!(rep.check().unwrap_err().contains("repetido"));
        let grande: DataSpec = toml::from_str("max_mb = 5000").unwrap();
        assert!(grande.check().is_err());
        assert_eq!(DataSpec::with(&[]).describe(), "");
    }

    #[test]
    fn versao_copia_e_devolve() {
        let root = temp("versao");
        let id = "io.x.app";
        // sem dados: grava a versão e pronto
        assert_eq!(prepare(&root, id, 2).unwrap(), Prep::Ready);
        assert_eq!(stored_version(&root.join(id)), Some(2));
        assert!(!has_data(&root.join(id)));
        assert_eq!(prepare(&root, id, 2).unwrap(), Prep::Ready);
        // dados de antes da versão contam como 1
        let outro = "io.x.antigo";
        std::fs::create_dir_all(root.join(outro)).unwrap();
        std::fs::write(root.join(outro).join("a.txt"), "v1").unwrap();
        assert_eq!(prepare(&root, outro, 3).unwrap(), Prep::Migrate { from: 1, to: 3 });
        // a migração mexe nos dados e falha: volta a cópia
        std::fs::write(root.join(outro).join("a.txt"), "estragado").unwrap();
        // tentar de novo não troca a cópia de antes de tudo
        assert_eq!(prepare(&root, outro, 3).unwrap(), Prep::Migrate { from: 1, to: 3 });
        restore(&root, outro).unwrap();
        assert_eq!(std::fs::read_to_string(root.join(outro).join("a.txt")).unwrap(), "v1");
        // deu certo: versão nova
        set_version(&root.join(outro), 3).unwrap();
        assert_eq!(prepare(&root, outro, 3).unwrap(), Prep::Ready);
        // app mais velho que os dados: não começa
        assert!(prepare(&root, outro, 2).unwrap_err().contains("mais nova"));
        // só a versão não conta como dados guardados
        assert_eq!(ids(&root), [outro]);
        delete(&root, outro).unwrap();
        assert!(!root.join(outro).exists() && !root.join(BACKUPS).join(outro).exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn exporta_o_banco_inteiro_e_mede() {
        let root = temp("exporta");
        let dir = root.join("io.x.db");
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub").join("nota.txt"), "oi").unwrap();
        {
            let c = rusqlite::Connection::open(dir.join(DB_FILE)).unwrap();
            c.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE t(x); INSERT INTO t VALUES (42);").unwrap();
            // conexão aberta (o app rodando): o VACUUM INTO ainda leva tudo
            let out = export(&root, "io.x.db", &root.join("saida")).unwrap();
            let lido: i64 = rusqlite::Connection::open(out.join(DB_FILE)).unwrap().query_row("SELECT x FROM t", [], |r| r.get(0)).unwrap();
            assert_eq!(lido, 42);
            assert_eq!(std::fs::read_to_string(out.join("sub").join("nota.txt")).unwrap(), "oi");
            assert!(!out.join(format!("{DB_FILE}-wal")).exists());
        }
        let (bytes, cortou) = size(&dir);
        assert!(bytes > 2 && !cortou);
        assert!(export(&root, "io.x.nada", &root).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn traz_os_dados_do_lugar_antigo() {
        let base = temp("antigo");
        let (legacy, root) = (base.join(".data"), base.join("app-data"));
        std::fs::create_dir_all(legacy.join("io.x.ext")).unwrap();
        std::fs::write(legacy.join("io.x.ext").join("pastas.txt"), "~/p").unwrap();
        std::fs::write(legacy.join("io.x.script.json"), "{\"n\":1}").unwrap();
        // já existe no destino: não sobrescreve
        std::fs::create_dir_all(root.join("io.x.ja")).unwrap();
        std::fs::write(root.join("io.x.ja").join(KV_FILE), "novo").unwrap();
        std::fs::write(legacy.join("io.x.ja.json"), "velho").unwrap();
        assert_eq!(migrate_plugin_data(&legacy, &root), 3);
        assert_eq!(std::fs::read_to_string(root.join("io.x.ext").join("pastas.txt")).unwrap(), "~/p");
        assert_eq!(std::fs::read_to_string(root.join("io.x.script").join(KV_FILE)).unwrap(), "{\"n\":1}");
        assert_eq!(std::fs::read_to_string(root.join("io.x.ja").join(KV_FILE)).unwrap(), "novo");
        // o que não veio continua lá
        assert!(legacy.join("io.x.ja.json").exists());
        assert_eq!(migrate_plugin_data(&base.join("nao-existe"), &root), 0);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn tamanho_legivel() {
        assert_eq!(human(0), "0 B");
        assert_eq!(human(1536), "1,5 KB");
        assert_eq!(human(3 << 20), "3,0 MB");
    }
}
