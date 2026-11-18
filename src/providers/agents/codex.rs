//! Codex: the `threads` table of `~/.codex/state_<n>.sqlite` (and `thread_spawn_edges` for
//! subagents) and, at the end of the session log (`rollout-*.jsonl`), the last
//! `task_started`/`task_complete`/`turn_aborted` and the last tool call.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{
    Agent, Details, LIMIT, RECENT, STALE, Source, State, Step, StepKind, Tool, base_name, describe, home_dir, iso_ms,
    last_steps, mtime_ms, one_line, open, push_step, read_last, read_tail, short_cmd, TIMELINE_TAIL,
};
use crate::tr;

/// Last turn marked in the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Turn {
    Started,
    Complete,
    Aborted,
}

/// What matters from the end of a log.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Tail {
    /// Last turn start or end and its time (ms).
    turn: Option<(Turn, i64)>,
    /// Last tool called in the current turn (with no turn mark: the last one in the chunk).
    action: String,
}

impl Tail {
    /// The end of a log that only grew since `prev` was read, when the new chunk has no
    /// turn mark (a long turn whose output filled the whole chunk): the turn is still the
    /// one `prev` saw, and a newer call replaces its action.
    fn carry(self, prev: &Tail) -> Tail {
        match (self.turn, prev.turn) {
            (None, Some(turn)) => {
                let action = match turn.0 {
                    Turn::Started if !self.action.is_empty() => self.action,
                    Turn::Started => prev.action.clone(),
                    _ => String::new(),
                };
                Tail { turn: Some(turn), action }
            }
            _ => self,
        }
    }
}

#[derive(Default)]
pub(super) struct Codex {
    /// Log tails already read: (size, mtime in ms) → what they had. Only re-read on change.
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
            .map_err(|_| tr!("unrecognized database format"))?;
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
            .map_err(|_| tr!("could not read the sessions"))?;
        let rows: Vec<_> = rows.flatten().collect();
        let mut agents = Vec::new();
        let mut used = Vec::new();
        for (id, cwd, title, rollout, updated, parent) in rows {
            let path = PathBuf::from(rollout);
            let (mtime, tail) = self.tail(&path);
            used.push(path.clone());
            let quiet = now - mtime.max(updated);
            let (state, since) = match tail.turn {
                // a turn started and it is still writing (or wrote recently): working
                Some((Turn::Started, at)) if quiet < STALE.as_millis() as i64 => (State::Working, at),
                Some((_, at)) => (State::Idle, at),
                None => (State::Idle, updated),
            };
            // the Codex app creates empty sessions when it opens; they only show up once they have a prompt
            if title.is_empty() && state == State::Idle {
                continue;
            }
            let detail = if matches!(tail.turn, Some((Turn::Aborted, _))) { tr!("interrupted") } else { "" };
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
                log: Some(path),
            });
        }
        self.tails.retain(|p, _| used.contains(p));
        Ok(agents)
    }
}

impl Codex {
    /// The log's mtime (ms) and what its end says. Only re-reads the file when it changes.
    fn tail(&mut self, path: &Path) -> (i64, Tail) {
        let Ok(meta) = std::fs::metadata(path) else { return (0, Tail::default()) };
        let key = (meta.len(), mtime_ms(&meta));
        if let Some((k, tail)) = self.tails.get(path)
            && *k == key {
                return (key.1, tail.clone());
            }
        let mut tail = read_tail(path, meta.len()).map(|text| parse_tail(&text)).unwrap_or_default();
        if tail.turn.is_none() {
            match self.tails.get(path) {
                // the file only grew: what the last reading saw still holds
                Some(((len, _), prev)) if *len <= key.0 => tail = tail.carry(prev),
                // first reading in the middle of a long turn: look further back, once
                _ if meta.len() > super::TAIL => {
                    if let Some(text) = read_last(path, meta.len(), TIMELINE_TAIL) {
                        tail = parse_tail(&text);
                    }
                }
                _ => {}
            }
        }
        self.tails.insert(path.to_path_buf(), (key, tail.clone()));
        (key.1, tail)
    }
}

/// The `state_<n>.sqlite` with the highest number (the number goes up when the format changes).
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

/// Last turn start or end in a log chunk (with its time, in ms) and the last tool called
/// after it.
///
/// Looks for the start of the event's `payload`, with unescaped quotes: inside a text (a
/// reply quoting these names) the quotes would come as `\"` and not match.
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
            // backwards: the first call seen is the last one of the current turn
            let action = if *turn == Turn::Started { action } else { String::new() };
            return Tail { turn: Some((*turn, at)), action };
        }
        if action.is_empty() && CALLS.iter().any(|c| line.contains(c)) {
            action = parse_call(line).unwrap_or_default();
        }
    }
    Tail { turn: None, action }
}

/// Timeline of a log chunk: the completed items (`item_completed`) of prompts, replies and
/// commands; old logs have `user_message`/`agent_message` instead.
pub(super) fn timeline(text: &str) -> Vec<Step> {
    const ITEM: &str = r#""type":"event_msg","payload":{"type":"item_completed""#;
    const OLD: [&str; 2] =
        [r#""type":"event_msg","payload":{"type":"user_message""#, r#""type":"event_msg","payload":{"type":"agent_message""#];
    let mut steps = Vec::new();
    for line in text.lines() {
        if !line.contains(ITEM) && !OLD.iter().any(|m| line.contains(m)) {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let at = iso_ms(&v["timestamp"]);
        let p = &v["payload"];
        if p["type"] != "item_completed" {
            let kind = if p["type"] == "user_message" { StepKind::User } else { StepKind::Reply };
            push_step(&mut steps, at, kind, p["message"].as_str().unwrap_or_default());
            continue;
        }
        let item = &p["item"];
        let text_of = |item: &Value| -> String {
            let parts = item["content"].as_array().map(Vec::as_slice).unwrap_or_default();
            parts.iter().filter_map(|c| c["text"].as_str()).collect::<Vec<_>>().join(" ")
        };
        match item["type"].as_str() {
            Some("UserMessage") => push_step(&mut steps, at, StepKind::User, &text_of(item)),
            Some("AgentMessage") => push_step(&mut steps, at, StepKind::Reply, &text_of(item)),
            Some("CommandExecution") => {
                // a file read: its name; anything else, only the script's program
                // (`command` is a string or a list: `["zsh", "-lc", "…"]`)
                let parsed = &item["parsed_cmd"][0];
                let summary = match (parsed["type"].as_str(), parsed["name"].as_str()) {
                    (Some("read"), Some(name)) => format!("read: {}", base_name(name)),
                    _ => {
                        let cmd = match &item["command"] {
                            Value::String(s) => short_cmd(s),
                            Value::Array(a) => a.last().and_then(Value::as_str).map(short_cmd).unwrap_or_default(),
                            _ => String::new(),
                        };
                        if cmd.is_empty() { "exec".into() } else { format!("exec: {cmd}") }
                    }
                };
                push_step(&mut steps, at, StepKind::Tool, &summary);
                if let Some(code) = item["exit_code"].as_i64().filter(|c| *c != 0) {
                    push_step(&mut steps, at, StepKind::Error, &tr!("the command exited with code {}", code));
                }
            }
            _ => {}
        }
    }
    last_steps(steps)
}

/// Summary of a tool call. In the current format the tool is `exec` and the input is code
/// (`tools.exec_command({cmd:"…"})`); in the old one, a function with JSON arguments.
fn parse_call(line: &str) -> Option<String> {
    let v: Value = serde_json::from_str(line).ok()?;
    let p = &v["payload"];
    let name = p["name"].as_str()?;
    if let Some(code) = p["input"].as_str() {
        let cmd = code.split("cmd:\"").nth(1).map(short_cmd).filter(|c| !c.is_empty());
        return Some(cmd.map_or_else(|| name.to_string(), |c| format!("{name}: {c}")));
    }
    let args: Value = p["arguments"].as_str().and_then(|a| serde_json::from_str(a).ok()).unwrap_or(Value::Null);
    // `command` also comes as a list (`["bash", "-lc", "…"]`): then only the function name
    Some(describe(name, &args))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_turn_and_last_tool() {
        let started = r#"{"timestamp":"2026-11-11T01:59:21.947Z","ordinal":1,"type":"event_msg","payload":{"type":"task_started","turn_id":"a"}}"#;
        let done = r#"{"timestamp":"2026-11-11T03:31:58.110Z","ordinal":9,"type":"event_msg","payload":{"type":"task_complete"}}"#;
        let aborted = r#"{"timestamp":"2026-11-11T02:02:04.948Z","ordinal":5,"type":"event_msg","payload":{"type":"turn_aborted"}}"#;
        // a text quoting the event has escaped quotes and doesn't count
        let quoted = r#"{"timestamp":"2026-11-11T04:00:00.000Z","type":"response_item","payload":{"type":"message","text":"{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\"}}"}}"#;
        let other = r#"{"timestamp":"2026-11-11T04:00:01.000Z","type":"event_msg","payload":{"type":"token_count"}}"#;
        // as in the log: `payload` starts with the type and the input is code, with escaped quotes
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

        // the action is the last call of the current turn, reduced to the program
        let (old, new) = (call("pwd && ls"), call("TOKEN=abc cargo test --all -- secret"));
        let working = parse_tail(&[started, old.as_str(), other, new.as_str(), other].join("\n"));
        assert_eq!(working.action, "exec: cargo test");
        // a closed turn has no action, and a call from the previous turn doesn't leak into the new one
        assert_eq!(parse_tail(&[started, new.as_str(), done].join("\n")).action, "");
        assert_eq!(parse_tail(&[started, new.as_str(), done, started].join("\n")).action, "");

        // a chunk without marks (long output after `task_started`) keeps the turn it grew from
        let prev = parse_tail(&[started, old.as_str()].join("\n"));
        let chunk = parse_tail(&[other, other].join("\n"));
        assert_eq!(chunk.turn, None);
        assert_eq!(chunk.clone().carry(&prev), prev);
        let newer = parse_tail(&[other, new.as_str(), other].join("\n")).carry(&prev);
        assert_eq!((newer.turn, newer.action.as_str()), (prev.turn, "exec: cargo test"));
        // a closed turn stays closed, with no action
        let closed = parse_tail(&[started, done].join("\n"));
        assert_eq!(parse_tail(new.as_str()).carry(&closed), Tail { turn: closed.turn, action: String::new() });
        // a chunk with its own mark wins
        assert_eq!(parse_tail(done).carry(&prev).turn.map(|t| t.0), Some(Turn::Complete));
    }

    #[test]
    fn timeline_steps() {
        let lines = [
            r#"{"timestamp":"2026-11-11T13:57:13.069Z","type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","content":[{"type":"text","text":"create the image"}]}}}"#,
            r#"{"timestamp":"2026-11-11T13:57:14.000Z","type":"event_msg","payload":{"type":"item_completed","item":{"type":"Reasoning","content":[]}}}"#,
            r#"{"timestamp":"2026-11-11T13:57:15.000Z","type":"event_msg","payload":{"type":"item_completed","item":{"type":"AgentMessage","content":[{"type":"Text","text":"Let me check."}]}}}"#,
            r#"{"timestamp":"2026-11-11T13:57:16.000Z","type":"event_msg","payload":{"type":"item_completed","item":{"type":"CommandExecution","command":"TOKEN=x cargo build --release","exit_code":101,"aggregated_output":"secret"}}}"#,
            r#"{"timestamp":"2026-11-11T13:57:17.000Z","type":"event_msg","payload":{"type":"item_completed","item":{"type":"CommandExecution","command":["bash","-lc","ls -la"],"exit_code":0}}}"#,
            r#"{"timestamp":"2026-11-11T13:57:17.500Z","type":"event_msg","payload":{"type":"item_completed","item":{"type":"CommandExecution","command":["zsh","-lc","cat /a/SKILL.md"],"parsed_cmd":[{"type":"read","cmd":"cat /a/SKILL.md","name":"SKILL.md"}],"exit_code":0}}}"#,
            r#"{"timestamp":"2026-11-11T13:57:18.000Z","type":"event_msg","payload":{"type":"agent_message","message":"old format"}}"#,
            r#"{"timestamp":"2026-11-11T13:57:19.000Z","type":"event_msg","payload":{"type":"token_count"}}"#,
        ]
        .join("\n");
        let got: Vec<(StepKind, String)> = timeline(&lines).into_iter().map(|s| (s.kind, s.text)).collect();
        assert_eq!(
            got,
            [
                (StepKind::User, "create the image".to_string()),
                (StepKind::Reply, "Let me check.".into()),
                (StepKind::Tool, "exec: cargo build".into()),
                (StepKind::Error, "the command exited with code 101".into()),
                (StepKind::Tool, "exec: ls".into()),
                (StepKind::Tool, "read: SKILL.md".into()),
                (StepKind::Reply, "old format".into()),
            ]
        );
    }
}
