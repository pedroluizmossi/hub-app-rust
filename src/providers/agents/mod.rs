//! Agentes rodando nos codificadores de IA.
//!
//! Cada ferramenta tem a sua fonte (`claude.rs`, `codex.rs`, `opencode.rs`), que sabe onde
//! ela guarda o estado e o que dá para tirar de lá. Todas implementam `Source` e devolvem
//! `Agent`: o básico (estado, título, pasta) é obrigatório; o resto fica em `Details`, com
//! campos opcionais que cada fonte preenche até onde a ferramenta informa. A tela mostra o
//! que vier: uma ferramenta com menos informação só tem menos linhas.
//!
//! As fontes só leem o que as ferramentas já gravam no disco; não mudam nada nelas. Esses
//! formatos são internos e mudam entre versões: cada fonte falha sozinha (vira um erro
//! curto na tela) sem derrubar as outras.
//!
//! Da ação em curso só sai um resumo curto (nome da ferramenta, arquivo, programa): nunca
//! o comando inteiro nem o conteúdo, que podem ter segredos.

mod claude;
mod codex;
mod opencode;

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags};
use serde_json::Value;

/// Sessões paradas há mais que isto nem saem do provedor (o app filtra mais pela opção).
const RECENT: Duration = Duration::from_secs(24 * 3600);
/// "Trabalhando" sem nenhuma escrita há tanto tempo: a ferramenta caiu no meio do turno.
const STALE: Duration = Duration::from_secs(30 * 60);
/// Quanto do fim de um log é lido para achar o último turno e a última ferramenta.
const TAIL: u64 = 256 * 1024;
/// Máximo de sessões por ferramenta (subagentes entram na conta no Codex e no OpenCode).
const LIMIT: usize = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Tool {
    Claude,
    Codex,
    OpenCode,
}

impl Tool {
    pub fn name(self) -> &'static str {
        match self {
            Tool::Claude => "Claude",
            Tool::Codex => "Codex",
            Tool::OpenCode => "OpenCode",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum State {
    /// No meio de um turno.
    Working,
    /// Parado esperando o usuário (permissão, pergunta). Só o Claude informa.
    Waiting,
    /// Terminou o turno; aguarda o próximo pedido.
    Idle,
}

/// Tokens gastos numa sessão, somados de todas as respostas.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

/// O que uma ferramenta sabe a mais sobre uma sessão. Tudo opcional ("" ou `None` = a
/// ferramenta não informa, ou a fonte dela ainda não lê).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Details {
    /// Último pedido do usuário (texto dele: só aparece quando ele abre a linha).
    pub last_prompt: String,
    pub model: String,
    /// Esforço de raciocínio da última resposta ("high", "medium"…).
    pub effort: String,
    /// Branch do git na pasta da sessão.
    pub branch: String,
    pub tokens: Option<Tokens>,
    /// Último turno fechado: duração em ms e quantas mensagens a conversa tinha.
    pub last_turn: Option<(i64, u32)>,
    /// Por que a última resposta parou ("end_turn", "tool_use", "max_tokens"…).
    pub stop_reason: String,
    /// Subagente: o tipo dele ("Explore", "web-fetch"…).
    pub kind: String,
    /// Subagente: a quantos disparos está da sessão do usuário (1 = direto dela).
    pub depth: Option<u32>,
}

/// Uma sessão de agente.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agent {
    pub tool: Tool,
    /// Id da sessão na ferramenta (estável enquanto ela existir).
    pub id: String,
    pub title: String,
    /// Pasta em que roda.
    pub cwd: String,
    pub state: State,
    /// Desde quando está nesse estado (ms desde 1970; 0 = não sei).
    pub since_ms: i64,
    /// O que espera do usuário, ou como o último turno acabou ("" = nada a dizer).
    pub detail: String,
    /// Resumo da última ferramenta usada no turno em curso ("" = não sei ou parado).
    pub action: String,
    /// Subagente: o id da sessão que o disparou (da mesma ferramenta).
    pub parent: Option<String>,
    pub details: Details,
}

#[derive(Debug, Clone, Default)]
pub struct AgentsSnapshot {
    pub agents: Vec<Agent>,
    /// Ferramentas instaladas (com pasta de estado) nesta máquina.
    pub found: Vec<Tool>,
    /// Fontes que não deu para ler.
    pub errors: Vec<(Tool, String)>,
}

/// A fonte de uma ferramenta: onde ela guarda o estado e como ler as sessões de lá.
///
/// Para acrescentar uma ferramenta: um módulo com um tipo que implementa isto, um caso em
/// `Tool` e uma linha em `AgentsProvider::default`. O que ela não souber fica vazio em
/// `Details`; não precisa de mais nada para aparecer na tela.
trait Source: Send {
    fn tool(&self) -> Tool;
    /// Pasta de estado da ferramenta (se existe, ela está instalada).
    fn home(&self) -> Option<PathBuf>;
    /// As sessões abertas ou recentes. `now` em ms desde 1970.
    fn collect(&mut self, home: &Path, now: i64) -> Result<Vec<Agent>, String>;
}

pub struct AgentsProvider {
    sources: Vec<Box<dyn Source>>,
}

impl Default for AgentsProvider {
    fn default() -> Self {
        AgentsProvider {
            sources: vec![
                Box::new(claude::Claude::default()),
                Box::new(codex::Codex::default()),
                Box::new(opencode::OpenCode),
            ],
        }
    }
}

impl AgentsProvider {
    pub fn collect(&mut self) -> AgentsSnapshot {
        let mut snap = AgentsSnapshot::default();
        let now = now_ms();
        for source in &mut self.sources {
            let Some(home) = source.home().filter(|h| h.is_dir()) else { continue };
            snap.found.push(source.tool());
            match source.collect(&home, now) {
                Ok(list) => snap.agents.extend(list),
                Err(e) => snap.errors.push((source.tool(), e)),
            }
        }
        snap
    }
}

/// Pasta de estado: a variável de ambiente da ferramenta, ou `rel` na pasta pessoal.
fn home_dir(env: &str, rel: &str) -> Option<PathBuf> {
    std::env::var_os(env).map(PathBuf::from).or_else(|| dirs::home_dir().map(|h| h.join(rel)))
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

fn mtime_ms(meta: &std::fs::Metadata) -> i64 {
    meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis() as i64)
}

fn base_name(path: &str) -> String {
    path.trim_end_matches('/').rsplit('/').next().unwrap_or(path).to_string()
}

/// Texto de uma linha só (títulos vêm do primeiro pedido, que pode ter quebras).
fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Uma linha só, cortada em `max` letras, com reticências.
pub fn clip(s: &str, max: usize) -> String {
    let s = one_line(s);
    if s.chars().count() <= max { s } else { s.chars().take(max.saturating_sub(1)).chain(['…']).collect() }
}

/// Abre um banco de outra ferramenta só para leitura, sem esperar muito por um bloqueio.
fn open(path: &Path) -> Result<Connection, String> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .map_err(|_| "não consegui abrir o banco")?;
    conn.busy_timeout(Duration::from_millis(300)).map_err(|_| "não consegui abrir o banco")?;
    Ok(conn)
}

/// Os últimos `TAIL` bytes de um arquivo de `len` bytes.
fn read_tail(path: &Path, len: u64) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    file.seek(SeekFrom::Start(len.saturating_sub(TAIL))).ok()?;
    let mut buf = Vec::new();
    file.take(TAIL).read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// Um comando de shell reduzido ao programa e, se for uma palavra simples, ao subcomando:
/// `FOO=1 cargo test --all` → `cargo test`. O resto (argumentos, URLs, tokens) fica de fora.
fn short_cmd(cmd: &str) -> String {
    let plain = |w: &str| {
        w.len() <= 24 && !w.starts_with('-') && w.chars().all(|c| c.is_ascii_alphanumeric() || "_./:-".contains(c))
    };
    let mut words = cmd.split_whitespace().skip_while(|w| w.contains('='));
    let Some(program) = words.next().map(base_name).filter(|p| plain(p)) else { return String::new() };
    match words.next().filter(|w| plain(w) && !w.contains('/')) {
        Some(arg) => format!("{program} {arg}"),
        None => program,
    }
}

/// Resumo de uma chamada de ferramenta a partir do nome e dos argumentos: a descrição que
/// o próprio agente deu, o arquivo, o programa.
fn describe(name: &str, input: &Value) -> String {
    let text = |key: &str| input[key].as_str().filter(|s| !s.trim().is_empty());
    let arg = text("description")
        .map(|d| clip(d, 60))
        .or_else(|| ["file_path", "filePath", "path", "notebook_path"].iter().find_map(|k| text(k)).map(base_name))
        .or_else(|| text("command").or_else(|| text("cmd")).map(short_cmd))
        .or_else(|| text("pattern").or_else(|| text("skill")).map(|p| clip(p, 30)))
        .or_else(|| text("url").map(host))
        .unwrap_or_default();
    if arg.is_empty() { name.to_string() } else { format!("{name}: {arg}") }
}

/// `https://exemplo.com/a?b` → `exemplo.com`.
fn host(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    clip(rest.split(['/', '?', '#']).next().unwrap_or_default(), 40)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resumo_do_comando_nao_leva_argumentos() {
        assert_eq!(short_cmd("cargo test --all"), "cargo test");
        assert_eq!(short_cmd("A=1 B=2 /usr/bin/git push origin main"), "git push");
        assert_eq!(short_cmd("curl -H 'Authorization: Bearer x' https://a.b"), "curl");
        assert_eq!(short_cmd("cat /etc/passwd"), "cat");
        assert_eq!(short_cmd("pwd && ls"), "pwd");
        assert_eq!(short_cmd("$(evil) x"), "");
        assert_eq!(short_cmd(""), "");
    }

    #[test]
    fn resumo_da_chamada() {
        assert_eq!(describe("TaskStop", &serde_json::json!({})), "TaskStop");
        assert_eq!(describe("WebFetch", &serde_json::json!({"url":"https://a.b/c?token=x"})), "WebFetch: a.b");
        assert_eq!(describe("bash", &serde_json::json!({"command":"npm run build"})), "bash: npm run");
        assert_eq!(describe("Edit", &serde_json::json!({"file_path":"/home/u/p/src/bus.rs","old_string":"segredo"})), "Edit: bus.rs");
        assert_eq!(clip("um  texto\ncomprido", 9), "um texto…");
    }
}
