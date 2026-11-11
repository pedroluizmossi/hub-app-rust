//! Claude Code. É a fonte que mais informa:
//! - `~/.claude/sessions/<pid>.json`, um por processo vivo: `status` (`busy`/`idle`),
//!   `waitingFor` (o que espera do usuário), nome, pasta.
//! - o transcript `projects/<pasta>/<sessão>.jsonl`: título da conversa, último pedido,
//!   modelo, esforço, branch, tokens de cada resposta, duração dos turnos, motivo da
//!   parada e a última ferramenta chamada.
//! - `projects/<pasta>/<sessão>/subagents/agent-*.meta.json`: tipo, descrição, modelo,
//!   profundidade e como acabou (`finished`, `stoppedByUser`) cada subagente.
//!
//! O transcript só cresce: cada leitura pega as linhas novas e atualiza o que já sabia
//! (`Log`), em vez de reler o arquivo, que passa fácil de dezenas de MB.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use serde_json::Value;

use super::{
    Agent, Details, LIMIT, RECENT, Source, State, Tokens, Tool, base_name, clip, describe, home_dir, mtime_ms, one_line, read_tail,
};

/// Máximo de subagentes lidos por sessão.
const SUBS: usize = 50;
/// Subagente sem marca de fim e sem escrever há tanto tempo: não está mais rodando (a
/// sessão que o disparou fechou ou caiu). Um subagente ativo grava a cada chamada ao
/// modelo; só uma ferramenta muito demorada passa disso calada.
const SUB_STALE: Duration = Duration::from_secs(10 * 60);
/// O último pedido é texto do usuário: guarda só o começo.
const PROMPT_MAX: usize = 200;

#[derive(Default)]
pub(super) struct Claude {
    /// Transcript de cada sessão (o nome da pasta do projeto é derivado da pasta em que a
    /// sessão começou, que pode não ser a atual).
    transcripts: HashMap<String, PathBuf>,
    /// O que já foi lido de cada transcript.
    logs: HashMap<PathBuf, Log>,
}

impl Source for Claude {
    fn tool(&self) -> Tool {
        Tool::Claude
    }

    fn home(&self) -> Option<PathBuf> {
        home_dir("CLAUDE_CONFIG_DIR", ".claude")
    }

    fn collect(&mut self, home: &Path, now: i64) -> Result<Vec<Agent>, String> {
        let Ok(entries) = std::fs::read_dir(home.join("sessions")) else { return Ok(Vec::new()) };
        let mut agents = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let Some((pid, proc_start, agent)) = parse_session(&text) else { continue };
            // o arquivo sobra quando o processo morre sem limpar, e o pid pode ser reusado
            if process_alive(pid, &proc_start) {
                agents.push(agent);
            }
        }
        agents.sort_by(|a, b| b.since_ms.cmp(&a.since_ms));
        agents.truncate(LIMIT);
        self.transcripts.retain(|id, _| agents.iter().any(|a| a.id == *id));

        let mut subs = Vec::new();
        let mut used = Vec::new();
        for a in &mut agents {
            let Some(transcript) = self.transcript(home, &a.id, &a.cwd) else { continue };
            let log = self.logs.entry(transcript.clone()).or_default();
            log.read(&transcript);
            log.fill(a);
            subs.extend(subagents(&transcript.with_extension("").join("subagents"), a, now));
            used.push(transcript);
        }
        self.logs.retain(|p, _| used.contains(p));
        agents.extend(subs);
        Ok(agents)
    }
}

impl Claude {
    /// `projects/<pasta>/<sessão>.jsonl` da sessão: primeiro pelo nome que a pasta atual
    /// daria, depois procurando em todas.
    fn transcript(&mut self, home: &Path, id: &str, cwd: &str) -> Option<PathBuf> {
        if id.is_empty() || id.contains(['/', '.']) {
            return None;
        }
        if let Some(p) = self.transcripts.get(id) {
            return Some(p.clone());
        }
        let projects = home.join("projects");
        let file = format!("{id}.jsonl");
        let slug: String = cwd.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
        let guess = projects.join(slug).join(&file);
        let found = if guess.is_file() {
            guess
        } else {
            std::fs::read_dir(&projects).ok()?.flatten().map(|e| e.path().join(&file)).find(|p| p.is_file())?
        };
        self.transcripts.insert(id.to_string(), found.clone());
        Some(found)
    }
}

/// Lê um `sessions/<pid>.json`: (pid, início do processo, sessão).
fn parse_session(text: &str) -> Option<(u32, String, Agent)> {
    let v: Value = serde_json::from_str(text).ok()?;
    let pid = u32::try_from(v["pid"].as_u64()?).ok()?;
    let text_of = |key: &str| v[key].as_str().unwrap_or_default().to_string();
    // processo de reserva, pré-aquecido: ainda não é uma sessão de ninguém
    if !v["spare"].is_null() && v["spare"] != Value::Bool(false) {
        return None;
    }
    let cwd = text_of("cwd");
    let waiting = text_of("waitingFor");
    let status = text_of("status");
    let state = if !waiting.is_empty() || status == "waiting" {
        State::Waiting
    } else if status == "busy" {
        State::Working
    } else {
        State::Idle
    };
    let name = text_of("name");
    let agent = Agent {
        tool: Tool::Claude,
        id: text_of("sessionId"),
        title: if name.is_empty() { base_name(&cwd) } else { name },
        cwd,
        state,
        since_ms: v["statusUpdatedAt"].as_i64().or_else(|| v["startedAt"].as_i64()).unwrap_or(0),
        detail: one_line(&waiting),
        action: String::new(),
        parent: None,
        details: Details::default(),
    };
    Some((pid, text_of("procStart"), agent))
}

/// O processo existe e é o mesmo que gravou o arquivo (`procStart` é o campo 22 do
/// `/proc/<pid>/stat`). Sem `procStart`, basta existir.
fn process_alive(pid: u32, proc_start: &str) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else { return false };
    proc_start.is_empty() || stat_start(&stat).is_none_or(|s| s == proc_start)
}

/// Campo 22 (starttime) de um `/proc/<pid>/stat`. O nome do programa (campo 2) pode ter
/// espaços e parênteses: conta a partir do último `)`.
fn stat_start(stat: &str) -> Option<&str> {
    stat.rsplit_once(')')?.1.split_whitespace().nth(19)
}

/// O que já se sabe de um transcript, atualizado a cada leitura com as linhas novas.
#[derive(Debug, Default, PartialEq, Eq)]
struct Log {
    /// Até onde o arquivo já foi lido (sempre no fim de uma linha).
    offset: u64,
    /// Título que o Claude deu à conversa.
    title: String,
    last_prompt: String,
    model: String,
    effort: String,
    branch: String,
    stop_reason: String,
    /// Última ferramenta chamada.
    action: String,
    last_turn: Option<(i64, u32)>,
    /// Tokens das respostas já fechadas.
    tokens: Tokens,
    /// A resposta em curso: (id, tokens). Uma resposta ocupa várias linhas (uma por
    /// bloco: raciocínio, texto, ferramenta), todas com o mesmo id e o mesmo uso; só
    /// entra na soma quando a resposta seguinte começa, para não contar repetido.
    open: Option<(String, Tokens)>,
}

impl Log {
    /// Lê as linhas que chegaram desde a última vez.
    fn read(&mut self, path: &Path) {
        let Ok(mut file) = std::fs::File::open(path) else { return };
        let len = file.metadata().map_or(0, |m| m.len());
        if len < self.offset {
            // o arquivo encolheu (foi reescrito): começa de novo
            *self = Log::default();
        }
        if len == self.offset || file.seek(SeekFrom::Start(self.offset)).is_err() {
            return;
        }
        let mut reader = BufReader::new(file);
        let mut line = Vec::new();
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                // linha ainda sendo escrita (sem a quebra no fim): fica para a próxima leitura
                Ok(n) if n > 0 && line.last() == Some(&b'\n') => {
                    self.offset += n as u64;
                    self.feed(&String::from_utf8_lossy(&line));
                }
                _ => break,
            }
        }
    }

    /// Uma linha do transcript. Só as que interessam passam pelo parse: as outras
    /// (anexos, cópias de requisição) podem ser enormes.
    fn feed(&mut self, line: &str) {
        const KINDS: [&str; 4] =
            [r#""type":"assistant""#, r#""type":"ai-title""#, r#""type":"last-prompt""#, r#""subtype":"turn_duration""#];
        if !KINDS.iter().any(|k| line.contains(k)) {
            return;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else { return };
        let set = |slot: &mut String, value: &Value| {
            if let Some(s) = value.as_str().filter(|s| !s.is_empty()) {
                *slot = s.to_string();
            }
        };
        match v["type"].as_str() {
            Some("ai-title") => set(&mut self.title, &v["aiTitle"]),
            Some("last-prompt") => {
                if let Some(p) = v["lastPrompt"].as_str() {
                    self.last_prompt = clip(p, PROMPT_MAX);
                }
            }
            Some("system") if v["subtype"] == "turn_duration" => {
                if let Some(ms) = v["durationMs"].as_i64() {
                    self.last_turn = Some((ms, v["messageCount"].as_u64().unwrap_or(0) as u32));
                }
            }
            // `isSidechain`: resposta de subagente gravada no transcript principal
            Some("assistant") if v["isSidechain"] != Value::Bool(true) => {
                let m = &v["message"];
                // "<synthetic>": mensagem que o próprio Claude Code montou, sem modelo
                if m["model"].as_str().is_some_and(|s| !s.starts_with('<')) {
                    set(&mut self.model, &m["model"]);
                }
                set(&mut self.effort, &v["effort"]);
                set(&mut self.branch, &v["gitBranch"]);
                set(&mut self.stop_reason, &m["stop_reason"]);
                if let Some(id) = m["id"].as_str() {
                    let n = |key: &str| m["usage"][key].as_u64().unwrap_or(0);
                    let used = Tokens {
                        input: n("input_tokens"),
                        output: n("output_tokens"),
                        cache_read: n("cache_read_input_tokens"),
                        cache_write: n("cache_creation_input_tokens"),
                    };
                    match &mut self.open {
                        Some((open, tokens)) if open == id => *tokens = used,
                        _ => {
                            if let Some((_, done)) = self.open.replace((id.to_string(), used)) {
                                self.tokens = sum(self.tokens, done);
                            }
                        }
                    }
                }
                let call = m["content"].as_array().and_then(|c| c.iter().rev().find(|c| c["type"] == "tool_use"));
                if let Some(name) = call.and_then(|c| c["name"].as_str()) {
                    self.action = describe(name, &call.unwrap()["input"]);
                }
            }
            _ => {}
        }
    }

    /// Passa para a sessão o que o transcript sabe.
    fn fill(&self, a: &mut Agent) {
        if !self.title.is_empty() {
            a.title = self.title.clone();
        }
        if a.state != State::Idle {
            a.action = self.action.clone();
        }
        let total = self.open.as_ref().map_or(self.tokens, |(_, t)| sum(self.tokens, *t));
        a.details = Details {
            last_prompt: self.last_prompt.clone(),
            model: self.model.clone(),
            effort: self.effort.clone(),
            branch: self.branch.clone(),
            tokens: (total != Tokens::default()).then_some(total),
            last_turn: self.last_turn,
            stop_reason: self.stop_reason.clone(),
            kind: String::new(),
            depth: None,
        };
    }
}

fn sum(a: Tokens, b: Tokens) -> Tokens {
    Tokens {
        input: a.input + b.input,
        output: a.output + b.output,
        cache_read: a.cache_read + b.cache_read,
        cache_write: a.cache_write + b.cache_write,
    }
}

/// Subagentes de uma sessão: um `agent-<id>.meta.json` e o transcript `agent-<id>.jsonl`
/// de cada um.
fn subagents(dir: &Path, parent: &Agent, now: i64) -> Vec<Agent> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut subs = Vec::new();
    for entry in entries.flatten() {
        let log = entry.path();
        if log.extension().is_none_or(|e| e != "jsonl") {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let mtime = mtime_ms(&meta);
        // antigo demais: nem abre o arquivo de descrição
        if now - mtime > RECENT.as_millis() as i64 {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(log.with_extension("meta.json")) else { continue };
        let Some(sub) = parse_sub_meta(&text) else { continue };
        // parado à força não ganha `finished`: ou o arquivo diz `stoppedByUser`, ou o
        // transcript acaba no aviso de interrupção
        let stopped = sub.stopped || (!sub.finished && interrupted(&log, meta.len()));
        // sem marca de fim e ainda escrevendo: trabalhando desde que o arquivo nasceu
        let working = !sub.finished && !stopped && now - mtime < SUB_STALE.as_millis() as i64;
        let born = meta.created().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis() as i64);
        subs.push(Agent {
            tool: Tool::Claude,
            id: log.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
            title: sub.title,
            cwd: parent.cwd.clone(),
            state: if working { State::Working } else { State::Idle },
            since_ms: if working { born } else { mtime },
            detail: if sub.finished || working {
                ""
            } else if stopped {
                "interrompido"
            } else {
                "sem atividade"
            }
            .into(),
            action: String::new(),
            parent: Some(parent.id.clone()),
            details: Details { kind: sub.kind, model: sub.model, depth: sub.depth, ..Details::default() },
        });
    }
    subs.sort_by(|a, b| b.since_ms.cmp(&a.since_ms));
    subs.truncate(SUBS);
    subs
}

#[derive(Debug, PartialEq, Eq)]
struct SubMeta {
    /// O que o subagente faz (a descrição dada ao dispará-lo; sem ela, o tipo).
    title: String,
    kind: String,
    model: String,
    depth: Option<u32>,
    finished: bool,
    /// Parado pelo usuário (ou pela sessão que o disparou) antes de terminar.
    stopped: bool,
}

/// O transcript de um subagente acaba no aviso de interrupção do Claude Code.
fn interrupted(log: &Path, len: u64) -> bool {
    read_tail(log, len).is_some_and(|tail| ends_interrupted(&tail))
}

/// A última linha é a mensagem que o Claude Code grava ao interromper um turno. Aspas
/// sem escape: um texto que só cite o aviso (com `\"`) não conta.
fn ends_interrupted(tail: &str) -> bool {
    tail.lines().next_back().is_some_and(|last| last.contains(r#""text":"[Request interrupted by user"#))
}

/// Lê um `agent-<id>.meta.json`.
fn parse_sub_meta(text: &str) -> Option<SubMeta> {
    let v: Value = serde_json::from_str(text).ok()?;
    let text_of = |key: &str| v[key].as_str().unwrap_or_default().to_string();
    let kind = text_of("agentType");
    let description = text_of("description");
    let title = if description.is_empty() { kind.clone() } else { description };
    if title.is_empty() {
        return None;
    }
    Some(SubMeta {
        title: clip(&title, 60),
        kind,
        model: Some(text_of("model")).filter(|m| !m.is_empty()).unwrap_or_else(|| text_of("pickedEffortModel")),
        depth: v["spawnDepth"].as_u64().map(|d| d as u32),
        finished: v["finished"] == Value::Bool(true),
        stopped: v["stoppedByUser"] == Value::Bool(true),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sessao() {
        let text = r#"{"pid":3839,"sessionId":"abc","cwd":"/home/u/proj","startedAt":10,"procStart":"14457727",
            "kind":"interactive","name":"proj-39","status":"busy","statusUpdatedAt":99}"#;
        let (pid, start, a) = parse_session(text).unwrap();
        assert_eq!((pid, start.as_str()), (3839, "14457727"));
        assert_eq!((a.title.as_str(), a.state, a.since_ms), ("proj-39", State::Working, 99));

        let idle = parse_session(r#"{"pid":1,"cwd":"/home/u/proj/","status":"idle","startedAt":10}"#).unwrap().2;
        assert_eq!((idle.title.as_str(), idle.state, idle.since_ms), ("proj", State::Idle, 10));

        // esperando o usuário vale mais que o status
        let wait = parse_session(r#"{"pid":1,"cwd":"/x","status":"busy","waitingFor":"permission"}"#).unwrap().2;
        assert_eq!((wait.state, wait.detail.as_str()), (State::Waiting, "permission"));

        assert!(parse_session(r#"{"pid":1,"cwd":"/x","status":"idle","spare":true}"#).is_none());
        assert!(parse_session("lixo").is_none());
    }

    #[test]
    fn inicio_do_processo_com_nome_estranho() {
        let stat = "42 (a b) c) S 1 42 42 0 -1 4194560 100 0 0 0 5 3 0 0 20 0 1 0 14457727 1000 200";
        assert_eq!(stat_start(stat), Some("14457727"));
    }

    /// Uma linha de resposta do transcript: um bloco de conteúdo da resposta `id`.
    fn answer(id: &str, out: u64, stop: &str, block: Value) -> String {
        json!({"type":"assistant","isSidechain":false,"effort":"medium","gitBranch":"master",
            "message":{"id":id,"model":"claude-x","stop_reason":stop,"content":[block],
                "usage":{"input_tokens":2,"output_tokens":out,"cache_read_input_tokens":100,"cache_creation_input_tokens":10}}})
        .to_string()
    }

    #[test]
    fn transcript_vira_detalhes() {
        let mut log = Log::default();
        let bash = json!({"type":"tool_use","name":"Bash","input":{"command":"cargo test --all","description":"Rodar os testes"}});
        let lines = [
            json!({"type":"ai-title","aiTitle":"Monitorar agentes"}).to_string(),
            json!({"type":"last-prompt","lastPrompt":"faça\no  wrapper"}).to_string(),
            // a mesma resposta em três linhas (raciocínio, texto, ferramenta): conta uma vez
            answer("m1", 50, "tool_use", json!({"type":"thinking"})),
            answer("m1", 50, "tool_use", json!({"type":"text","text":"vou rodar"})),
            answer("m1", 50, "tool_use", bash),
            json!({"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"ok"}]}}).to_string(),
            answer("m2", 7, "end_turn", json!({"type":"text","text":"pronto"})),
            json!({"type":"system","subtype":"turn_duration","durationMs":10449,"messageCount":316}).to_string(),
            // resposta de subagente e mensagem sem modelo não mudam nada
            json!({"type":"assistant","isSidechain":true,"message":{"id":"s1","model":"outro","usage":{"output_tokens":999}}}).to_string(),
            json!({"type":"assistant","message":{"model":"<synthetic>","content":[]}}).to_string(),
            "lixo que cita \"type\":\"assistant\"".to_string(),
        ];
        for line in &lines {
            log.feed(line);
        }

        let mut a = parse_session(r#"{"pid":1,"cwd":"/x","name":"x-39","status":"busy"}"#).unwrap().2;
        log.fill(&mut a);
        assert_eq!((a.title.as_str(), a.action.as_str()), ("Monitorar agentes", "Bash: Rodar os testes"));
        let d = &a.details;
        assert_eq!(d.last_prompt, "faça o wrapper");
        assert_eq!((d.model.as_str(), d.effort.as_str(), d.branch.as_str()), ("claude-x", "medium", "master"));
        assert_eq!(d.tokens, Some(Tokens { input: 4, output: 57, cache_read: 200, cache_write: 20 }));
        assert_eq!((d.last_turn, d.stop_reason.as_str()), (Some((10449, 316)), "end_turn"));

        // parada: a última ferramenta deixa de ser "o que faz agora"
        let mut idle = parse_session(r#"{"pid":1,"cwd":"/x","status":"idle"}"#).unwrap().2;
        log.fill(&mut idle);
        assert_eq!(idle.action, "");

        // transcript vazio: fica o nome da sessão e nenhum detalhe
        let mut plain = parse_session(r#"{"pid":1,"cwd":"/x","name":"x-39","status":"idle"}"#).unwrap().2;
        Log::default().fill(&mut plain);
        assert_eq!((plain.title.as_str(), &plain.details), ("x-39", &Details::default()));
    }

    #[test]
    fn le_so_as_linhas_novas_e_completas() {
        let dir = std::env::temp_dir().join(format!("miniutils-claude-log-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.jsonl");
        let first = answer("m1", 5, "end_turn", json!({"type":"text","text":"a"}));
        let second = answer("m2", 7, "end_turn", json!({"type":"text","text":"b"}));
        let out = |log: &Log| log.open.as_ref().map_or(0, |(_, t)| t.output) + log.tokens.output;

        // a segunda linha ainda está sendo escrita (sem a quebra no fim)
        std::fs::write(&path, format!("{first}\n{}", &second[..30])).unwrap();
        let mut log = Log::default();
        log.read(&path);
        assert_eq!((out(&log), log.offset), (5, first.len() as u64 + 1));

        std::fs::write(&path, format!("{first}\n{second}\n")).unwrap();
        log.read(&path);
        assert_eq!(out(&log), 12);
        // sem nada novo, nada muda
        log.read(&path);
        assert_eq!(out(&log), 12);

        // arquivo reescrito menor: recomeça
        std::fs::write(&path, format!("{second}\n")).unwrap();
        log.read(&path);
        assert_eq!(out(&log), 7);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn descricao_do_subagente() {
        let meta = r#"{"agentType":"web-fetch","description":"Ler a página","spawnDepth":1,"pickedEffortModel":"claude-y","finished":true}"#;
        let sub = parse_sub_meta(meta).unwrap();
        assert_eq!((sub.title.as_str(), sub.kind.as_str(), sub.model.as_str()), ("Ler a página", "web-fetch", "claude-y"));
        assert_eq!((sub.depth, sub.finished), (Some(1), true));
        assert!(!sub.stopped);
        let bare = parse_sub_meta(r#"{"agentType":"Explore"}"#).unwrap();
        assert_eq!((bare.title.as_str(), bare.depth, bare.finished, bare.stopped), ("Explore", None, false, false));
        assert_eq!(parse_sub_meta("{}"), None);

        // parado à força: sem `finished`, com `stoppedByUser`; o modelo efetivo vale mais que o escolhido
        let killed = parse_sub_meta(r#"{"agentType":"Explore","stoppedByUser":true,"model":"claude-z","pickedEffortModel":"claude-y"}"#).unwrap();
        assert_eq!((killed.finished, killed.stopped, killed.model.as_str()), (false, true, "claude-z"));
    }

    #[test]
    fn transcript_que_acaba_interrompido() {
        let result = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"ok"}]}}"#;
        let stop = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"[Request interrupted by user for tool use]"}]}}"#;
        // um resultado que só cita o aviso tem as aspas com escape
        let quoted = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"{\"text\":\"[Request interrupted by user]\"}"}]}}"#;
        assert!(ends_interrupted(&[result, stop].join("\n")));
        assert!(ends_interrupted(&format!("{stop}\n")));
        // interrompido antes, mas continuou depois: não conta
        assert!(!ends_interrupted(&[stop, result].join("\n")));
        assert!(!ends_interrupted(&[result, quoted].join("\n")));
        assert!(!ends_interrupted(""));
    }
}
