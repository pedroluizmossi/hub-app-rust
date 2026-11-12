//! OpenCode: `session_v2` de `~/.local/share/opencode/opencode.db`, com `time_busy`,
//! `time_idle` e `parent_id` (subagentes); a última ferramenta vem de `session_message`.

use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde_json::Value;

use super::{
    Agent, Details, LIMIT, RECENT, STALE, Source, State, Step, StepKind, Tool, base_name, describe, last_steps, one_line, open,
    push_step,
};

pub(super) struct OpenCode;

impl Source for OpenCode {
    fn tool(&self) -> Tool {
        Tool::OpenCode
    }

    fn home(&self) -> Option<PathBuf> {
        crate::core::os::xdg_data_dir().map(|d| d.join("opencode"))
    }

    fn collect(&mut self, home: &Path, now: i64) -> Result<Vec<Agent>, String> {
        let db = home.join("opencode.db");
        if !db.is_file() {
            return Ok(Vec::new());
        }
        let conn = open(&db)?;
        let mut stmt = conn
            .prepare(
                "SELECT id, directory, COALESCE(title, ''), time_updated, time_busy, time_idle, \
                 COALESCE(idle_outcome, ''), parent_id \
                 FROM session_v2 WHERE time_archived IS NULL AND time_updated >= ?1 \
                 ORDER BY time_updated DESC LIMIT ?2",
            )
            .map_err(|_| "formato do banco não reconhecido")?;
        let rows = stmt
            .query_map((now - RECENT.as_millis() as i64, LIMIT as i64), |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, Option<i64>>(4)?,
                    r.get::<_, Option<i64>>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, Option<String>>(7)?,
                ))
            })
            .map_err(|_| "não consegui ler as sessões")?;
        let mut agents = Vec::new();
        for (id, cwd, title, updated, busy, idle, outcome, parent) in rows.flatten() {
            let log = Some(db.clone());
            let (state, since) = state(now, updated, busy, idle);
            let detail = match (state, outcome.as_str()) {
                (State::Idle, "failed") => "falhou",
                (State::Idle, "interrupted") => "interrompido",
                _ => "",
            };
            // só quem trabalha: a última mensagem do agente pode ser grande
            let action = if state == State::Working { last_action(&conn, &id) } else { String::new() };
            agents.push(Agent {
                tool: Tool::OpenCode,
                title: if title.is_empty() { base_name(&cwd) } else { one_line(&title) },
                id,
                cwd,
                state,
                since_ms: since,
                detail: detail.into(),
                action,
                parent,
                details: Details::default(),
                log,
            });
        }
        Ok(agents)
    }
}

/// Última ferramenta chamada na sessão ("" se a última mensagem não chamou nenhuma).
fn last_action(conn: &Connection, session: &str) -> String {
    conn.query_row(
        "SELECT data FROM session_message WHERE session_id = ?1 AND type = 'assistant' ORDER BY seq DESC LIMIT 1",
        [session],
        |r| r.get::<_, String>(0),
    )
    .ok()
    .and_then(|data| parse_message(&data))
    .unwrap_or_default()
}

/// Última parte `tool` de uma mensagem do agente (`content: [{type, name, state: {input}}]`).
fn parse_message(data: &str) -> Option<String> {
    let v: Value = serde_json::from_str(data).ok()?;
    let call = v["content"].as_array()?.iter().rev().find(|c| c["type"] == "tool")?;
    Some(describe(call["name"].as_str()?, &call["state"]["input"]))
}

/// Mensagens lidas do banco para a linha do tempo (cada uma vira um ou mais passos).
const MESSAGES: i64 = 60;

/// Linha do tempo da sessão: as últimas mensagens de `session_message`.
pub(super) fn timeline(db: &Path, session: &str) -> Vec<Step> {
    let Ok(conn) = open(db) else { return Vec::new() };
    let Ok(mut stmt) = conn.prepare(
        "SELECT type, data FROM session_message WHERE session_id = ?1 AND type IN ('user', 'assistant') \
         ORDER BY seq DESC LIMIT ?2",
    ) else {
        return Vec::new();
    };
    let Ok(rows) = stmt.query_map((session, MESSAGES), |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))) else {
        return Vec::new();
    };
    let mut rows: Vec<(String, String)> = rows.flatten().collect();
    rows.reverse();
    let mut steps = Vec::new();
    for (kind, data) in rows {
        message_steps(&mut steps, &kind, &data);
    }
    last_steps(steps)
}

/// Passos de uma mensagem: o pedido (`text` no topo) ou as partes da resposta (`content`:
/// texto e ferramentas; o raciocínio fica de fora).
fn message_steps(steps: &mut Vec<Step>, kind: &str, data: &str) {
    let Ok(v) = serde_json::from_str::<Value>(data) else { return };
    let at = v["time"]["created"].as_i64().unwrap_or(0);
    if kind == "user" {
        push_step(steps, at, StepKind::User, v["text"].as_str().unwrap_or_default());
        return;
    }
    for part in v["content"].as_array().map(Vec::as_slice).unwrap_or_default() {
        match part["type"].as_str() {
            Some("text") => push_step(steps, at, StepKind::Reply, part["text"].as_str().unwrap_or_default()),
            Some("tool") => {
                push_step(steps, at, StepKind::Tool, &describe(part["name"].as_str().unwrap_or("?"), &part["state"]["input"]));
                if part["state"]["status"] == "error" {
                    push_step(steps, at, StepKind::Error, "a ferramenta falhou");
                }
            }
            _ => {}
        }
    }
}

/// Trabalhando = ficou ocupado depois da última vez que parou (e ainda escreve).
fn state(now: i64, updated: i64, busy: Option<i64>, idle: Option<i64>) -> (State, i64) {
    match busy {
        Some(b) if idle.is_none_or(|i| b > i) && now - updated < STALE.as_millis() as i64 => (State::Working, b),
        _ => (State::Idle, idle.unwrap_or(updated)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estado() {
        let now = 10_000_000;
        // ocupado depois de parar: trabalhando desde então
        assert_eq!(state(now, now - 5, Some(900), Some(800)), (State::Working, 900));
        assert_eq!(state(now, now - 5, Some(900), None), (State::Working, 900));
        // parou depois de ficar ocupado
        assert_eq!(state(now, now - 5, Some(900), Some(950)), (State::Idle, 950));
        assert_eq!(state(now, 700, None, None), (State::Idle, 700));
        // "ocupado" sem escrever há mais de meia hora: caiu no meio
        assert_eq!(state(now, now - 1_900_000, Some(900), None).0, State::Idle);
    }

    #[test]
    fn ultima_ferramenta() {
        let msg = r#"{"time":{"created":1},"content":[{"type":"reasoning","text":"…"},
            {"type":"tool","name":"webfetch","state":{"status":"completed","input":{"format":"markdown","url":"https://opencode.ai/docs"}}},
            {"type":"tool","name":"read","state":{"status":"running","input":{"filePath":"/home/u/p/main.rs"}}}]}"#;
        assert_eq!(parse_message(msg).as_deref(), Some("read: main.rs"));
        assert_eq!(parse_message(r#"{"content":[{"type":"text","text":"oi"}]}"#), None);
    }

    #[test]
    fn passos_de_uma_mensagem() {
        let mut steps = Vec::new();
        message_steps(&mut steps, "user", r#"{"time":{"created":5},"text":"faça o deploy","files":[]}"#);
        message_steps(
            &mut steps,
            "assistant",
            r#"{"time":{"created":6},"content":[{"type":"reasoning","text":"…"},{"type":"text","text":"Feito."},
                {"type":"tool","name":"bash","state":{"status":"error","input":{"command":"kubectl apply -f x"}}}]}"#,
        );
        let got: Vec<(i64, StepKind, &str)> = steps.iter().map(|s| (s.at_ms, s.kind, s.text.as_str())).collect();
        assert_eq!(
            got,
            [
                (5, StepKind::User, "faça o deploy"),
                (6, StepKind::Reply, "Feito."),
                (6, StepKind::Tool, "bash: kubectl apply"),
                (6, StepKind::Error, "a ferramenta falhou"),
            ]
        );
    }
}
