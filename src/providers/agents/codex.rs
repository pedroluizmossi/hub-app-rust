//! Codex: a tabela `threads` de `~/.codex/state_<n>.sqlite` (e `thread_spawn_edges` para
//! os subagentes) e, no fim do log da sessão (`rollout-*.jsonl`), o último
//! `task_started`/`task_complete`/`turn_aborted` e a última chamada de ferramenta.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{
    Agent, Details, LIMIT, RECENT, STALE, Source, State, Tool, base_name, describe, home_dir, mtime_ms, one_line, open,
    read_tail, short_cmd,
};

/// Último turno marcado no log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Turn {
    Started,
    Complete,
    Aborted,
}

/// O que interessa do fim de um log.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Tail {
    /// Último início ou fim de turno e a hora dele (ms).
    turn: Option<(Turn, i64)>,
    /// Última ferramenta chamada no turno em curso.
    action: String,
}

#[derive(Default)]
pub(super) struct Codex {
    /// Fim de log já lido: (tamanho, mtime em ms) → o que tinha. Só relê quando muda.
    tails: HashMap<PathBuf, ((u64, i64), Tail)>,
}

impl Source for Codex {
    fn tool(&self) -> Tool {
        Tool::Codex
    }

    fn home(&self) -> Option<PathBuf> {
        home_dir("CODEX_HOME", ".codex")
    }

    fn collect(&mut self, home: &Path, now: i64) -> Result<Vec<Agent>, String> {
        let Some(db) = find_db(home) else { return Ok(Vec::new()) };
        let conn = open(&db)?;
        let mut stmt = conn
            .prepare(
                "SELECT t.id, t.cwd, t.title, t.rollout_path, COALESCE(t.updated_at_ms, t.updated_at * 1000), \
                 (SELECT e.parent_thread_id FROM thread_spawn_edges e WHERE e.child_thread_id = t.id) \
                 FROM threads t WHERE t.archived = 0 AND COALESCE(t.updated_at_ms, t.updated_at * 1000) >= ?1 \
                 ORDER BY 5 DESC LIMIT ?2",
            )
            .map_err(|_| "formato do banco não reconhecido")?;
        let rows = stmt
            .query_map((now - RECENT.as_millis() as i64, LIMIT as i64), |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, Option<String>>(5)?,
                ))
            })
            .map_err(|_| "não consegui ler as sessões")?;
        let rows: Vec<_> = rows.flatten().collect();
        let mut agents = Vec::new();
        let mut used = Vec::new();
        for (id, cwd, title, rollout, updated, parent) in rows {
            let path = PathBuf::from(rollout);
            let (mtime, tail) = self.tail(&path);
            used.push(path);
            let quiet = now - mtime.max(updated);
            let (state, since) = match tail.turn {
                // começou um turno e ainda escreve (ou escreveu há pouco): trabalhando
                Some((Turn::Started, at)) if quiet < STALE.as_millis() as i64 => (State::Working, at),
                Some((_, at)) => (State::Idle, at),
                None => (State::Idle, updated),
            };
            // o app do Codex cria sessões vazias ao abrir; só aparecem quando têm um pedido
            if title.is_empty() && state == State::Idle {
                continue;
            }
            let detail = if matches!(tail.turn, Some((Turn::Aborted, _))) { "interrompido" } else { "" };
            agents.push(Agent {
                tool: Tool::Codex,
                id,
                title: if title.is_empty() { base_name(&cwd) } else { one_line(&title) },
                cwd,
                state,
                since_ms: since,
                detail: detail.into(),
                action: if state == State::Working { tail.action } else { String::new() },
                parent,
                details: Details::default(),
            });
        }
        self.tails.retain(|p, _| used.contains(p));
        Ok(agents)
    }
}

impl Codex {
    /// mtime do log (ms) e o que o fim dele diz. Só relê o arquivo quando ele muda.
    fn tail(&mut self, path: &Path) -> (i64, Tail) {
        let Ok(meta) = std::fs::metadata(path) else { return (0, Tail::default()) };
        let key = (meta.len(), mtime_ms(&meta));
        if let Some((k, tail)) = self.tails.get(path) {
            if *k == key {
                return (key.1, tail.clone());
            }
        }
        let tail = read_tail(path, meta.len()).map(|text| parse_tail(&text)).unwrap_or_default();
        self.tails.insert(path.to_path_buf(), (key, tail.clone()));
        (key.1, tail)
    }
}

/// `state_<n>.sqlite` de número mais alto (o número sobe quando o formato muda).
fn find_db(home: &Path) -> Option<PathBuf> {
    std::fs::read_dir(home)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let n: u32 = name.strip_prefix("state_")?.strip_suffix(".sqlite")?.parse().ok()?;
            Some((n, e.path()))
        })
        .max_by_key(|(n, _)| *n)
        .map(|(_, p)| p)
}

/// Último início ou fim de turno num trecho do log (com a hora, em ms) e a última
/// ferramenta chamada depois dele.
///
/// Procura o começo do `payload` do evento, com as aspas sem escape: dentro de um texto
/// (uma resposta citando esses nomes) as aspas viriam como `\"` e não casam.
fn parse_tail(tail: &str) -> Tail {
    const MARKS: [(&str, Turn); 3] = [
        (r#""type":"event_msg","payload":{"type":"task_started""#, Turn::Started),
        (r#""type":"event_msg","payload":{"type":"task_complete""#, Turn::Complete),
        (r#""type":"event_msg","payload":{"type":"turn_aborted""#, Turn::Aborted),
    ];
    const CALLS: [&str; 2] = [
        r#""type":"response_item","payload":{"type":"custom_tool_call""#,
        r#""type":"response_item","payload":{"type":"function_call""#,
    ];
    let mut action = String::new();
    for line in tail.lines().rev() {
        if let Some((_, turn)) = MARKS.iter().find(|(mark, _)| line.contains(mark)) {
            let stamp = line.split(r#""timestamp":""#).nth(1).and_then(|r| r.split('"').next());
            let at = stamp.and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()).map_or(0, |t| t.timestamp_millis());
            // de trás para frente: a primeira chamada vista é a última do turno em curso
            let action = if *turn == Turn::Started { action } else { String::new() };
            return Tail { turn: Some((*turn, at)), action };
        }
        if action.is_empty() && CALLS.iter().any(|c| line.contains(c)) {
            action = parse_call(line).unwrap_or_default();
        }
    }
    Tail::default()
}

/// Resumo de uma chamada de ferramenta. No formato atual a ferramenta é `exec` e a
/// entrada é código (`tools.exec_command({cmd:"…"})`); no antigo, uma função com os
/// argumentos em JSON.
fn parse_call(line: &str) -> Option<String> {
    let v: Value = serde_json::from_str(line).ok()?;
    let p = &v["payload"];
    let name = p["name"].as_str()?;
    if let Some(code) = p["input"].as_str() {
        let cmd = code.split("cmd:\"").nth(1).map(short_cmd).filter(|c| !c.is_empty());
        return Some(cmd.map_or_else(|| name.to_string(), |c| format!("{name}: {c}")));
    }
    let args: Value = p["arguments"].as_str().and_then(|a| serde_json::from_str(a).ok()).unwrap_or(Value::Null);
    // `command` também vem como lista (`["bash", "-lc", "…"]`): aí só o nome da função
    Some(describe(name, &args))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ultimo_turno_e_ultima_ferramenta() {
        let started = r#"{"timestamp":"2026-11-11T01:59:21.947Z","ordinal":1,"type":"event_msg","payload":{"type":"task_started","turn_id":"a"}}"#;
        let done = r#"{"timestamp":"2026-11-11T03:31:58.110Z","ordinal":9,"type":"event_msg","payload":{"type":"task_complete"}}"#;
        let aborted = r#"{"timestamp":"2026-11-11T02:02:04.948Z","ordinal":5,"type":"event_msg","payload":{"type":"turn_aborted"}}"#;
        // um texto que cita o evento tem as aspas com escape e não conta
        let quoted = r#"{"timestamp":"2026-11-11T04:00:00.000Z","type":"response_item","payload":{"type":"message","text":"{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\"}}"}}"#;
        let other = r#"{"timestamp":"2026-11-11T04:00:01.000Z","type":"event_msg","payload":{"type":"token_count"}}"#;
        // como no log: `payload` começa pelo tipo e a entrada é código, com as aspas escapadas
        let call = |cmd: &str| {
            format!(
                r#"{{"timestamp":"2026-11-11T04:00:02.000Z","ordinal":3,"type":"response_item","payload":{{"type":"custom_tool_call","id":"ctc_1","status":"completed","name":"exec","input":"const r = await tools.exec_command({{cmd:\"{cmd}\",timeout:5}});"}}}}"#
            )
        };
        let turn = |lines: &[&str]| parse_tail(&lines.join("\n")).turn.map(|t| t.0);

        let tail = parse_tail(&[started, other, done, quoted, other].join("\n"));
        assert_eq!(tail, Tail { turn: Some((Turn::Complete, 1_794_367_918_110)), action: String::new() });
        assert_eq!(turn(&[done, started, other]), Some(Turn::Started));
        assert_eq!(turn(&[started, aborted]), Some(Turn::Aborted));
        assert_eq!(turn(&[quoted, other]), None);

        // a ação é a última chamada do turno em curso, reduzida ao programa
        let (old, new) = (call("pwd && ls"), call("TOKEN=abc cargo test --all -- segredo"));
        let working = parse_tail(&[started, old.as_str(), other, new.as_str(), other].join("\n"));
        assert_eq!(working.action, "exec: cargo test");
        // turno fechado não tem ação, e chamada do turno anterior não vaza para o novo
        assert_eq!(parse_tail(&[started, new.as_str(), done].join("\n")).action, "");
        assert_eq!(parse_tail(&[started, new.as_str(), done, started].join("\n")).action, "");
    }
}
