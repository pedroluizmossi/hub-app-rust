//! OpenCode: `session_v2` of `~/.local/share/opencode/opencode.db`, with `time_busy`,
//! `time_idle` and `parent_id` (subagents); the last tool comes from `session_message`.

use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde_json::Value;

use super::{
    Agent, Details, LIMIT, RECENT, STALE, Source, State, Step, StepKind, Tool, base_name, describe, last_steps, one_line, open,
    push_step,
};
use crate::tr;

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
            .map_err(|_| tr!("unrecognized database format"))?;
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
            .map_err(|_| tr!("could not read the sessions"))?;
        let mut agents = Vec::new();
        for (id, cwd, title, updated, busy, idle, outcome, parent) in rows.flatten() {
            let log = Some(db.clone());
            let (state, since) = state(now, updated, busy, idle);
            let detail = match (state, outcome.as_str()) {
                (State::Idle, "failed") => tr!("failed"),
                (State::Idle, "interrupted") => tr!("interrupted"),
                _ => "",
            };
            // only working ones: the agent's last message may be large
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

/// Last tool called in the session ("" if the last message called none).
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

/// Last `tool` part of an agent message (`content: [{type, name, state: {input}}]`).
fn parse_message(data: &str) -> Option<String> {
    let v: Value = serde_json::from_str(data).ok()?;
    let call = v["content"].as_array()?.iter().rev().find(|c| c["type"] == "tool")?;
    Some(describe(call["name"].as_str()?, &call["state"]["input"]))
}

/// Messages read from the database for the timeline (each becomes one or more steps).
const MESSAGES: i64 = 60;

/// The session's timeline: the last messages of `session_message`.
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

/// Steps of a message: the prompt (`text` at the top) or the parts of the reply (`content`:
/// text and tools; reasoning is left out).
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
                    push_step(steps, at, StepKind::Error, tr!("the tool failed"));
                }
            }
            _ => {}
        }
    }
}

/// Working = became busy after the last time it stopped (and still writes).
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
    fn state_from_times() {
        let now = 10_000_000;
        // busy after stopping: working since then
        assert_eq!(state(now, now - 5, Some(900), Some(800)), (State::Working, 900));
        assert_eq!(state(now, now - 5, Some(900), None), (State::Working, 900));
        // stopped after being busy
        assert_eq!(state(now, now - 5, Some(900), Some(950)), (State::Idle, 950));
        assert_eq!(state(now, 700, None, None), (State::Idle, 700));
        // "busy" without writing for over half an hour: it died midway
        assert_eq!(state(now, now - 1_900_000, Some(900), None).0, State::Idle);
    }

    #[test]
    fn last_tool() {
        let msg = r#"{"time":{"created":1},"content":[{"type":"reasoning","text":"…"},
            {"type":"tool","name":"webfetch","state":{"status":"completed","input":{"format":"markdown","url":"https://opencode.ai/docs"}}},
            {"type":"tool","name":"read","state":{"status":"running","input":{"filePath":"/home/u/p/main.rs"}}}]}"#;
        assert_eq!(parse_message(msg).as_deref(), Some("read: main.rs"));
        assert_eq!(parse_message(r#"{"content":[{"type":"text","text":"hi"}]}"#), None);
    }

    #[test]
    fn steps_of_a_message() {
        let mut steps = Vec::new();
        message_steps(&mut steps, "user", r#"{"time":{"created":5},"text":"deploy it","files":[]}"#);
        message_steps(
            &mut steps,
            "assistant",
            r#"{"time":{"created":6},"content":[{"type":"reasoning","text":"…"},{"type":"text","text":"Done."},
                {"type":"tool","name":"bash","state":{"status":"error","input":{"command":"kubectl apply -f x"}}}]}"#,
        );
        let got: Vec<(i64, StepKind, &str)> = steps.iter().map(|s| (s.at_ms, s.kind, s.text.as_str())).collect();
        assert_eq!(
            got,
            [
                (5, StepKind::User, "deploy it"),
                (6, StepKind::Reply, "Done."),
                (6, StepKind::Tool, "bash: kubectl apply"),
                (6, StepKind::Error, "the tool failed"),
            ]
        );
    }
}
