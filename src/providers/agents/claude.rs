//! Claude Code. The source that reports the most:
//! - `~/.claude/sessions/<pid>.json`, one per live process: `status` (`busy`/`idle`),
//!   `waitingFor` (what it expects from the user), name, folder.
//! - the transcript `projects/<folder>/<session>.jsonl`: conversation title, last prompt,
//!   model, effort, branch, tokens of each reply, turn durations, stop reason and the last
//!   tool called.
//! - `projects/<folder>/<session>/subagents/agent-*.meta.json`: type, description, model,
//!   depth and how each subagent ended (`finished`, `stoppedByUser`).
//!
//! The transcript only grows: each reading takes the new lines and updates what was already
//! known (`Log`), instead of re-reading the file, which easily exceeds tens of MB.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use serde_json::Value;

use super::{
    Agent, Details, LIMIT, RECENT, Source, State, Step, StepKind, Tokens, Tool, base_name, clip, describe, home_dir, iso_ms,
    last_steps, mtime_ms, one_line, push_step, read_last,
};
use crate::tr;

/// Maximum subagents read per session.
const SUBS: usize = 50;
/// Subagent with no end mark and no writes for this long: it is no longer running (the
/// session that spawned it closed or died). An active subagent writes on every model
/// call; only a very slow tool stays quiet longer than this.
const SUB_STALE: Duration = Duration::from_secs(10 * 60);
/// The last prompt is the user's text: keep only the beginning.
const PROMPT_MAX: usize = 200;

#[derive(Default)]
pub(super) struct Claude {
    /// Transcript of each session (the project folder name derives from the folder the
    /// session started in, which may not be the current one).
    transcripts: HashMap<String, PathBuf>,
    /// What has already been read from each transcript.
    logs: HashMap<PathBuf, Log>,
    /// Whether each unfinished subagent transcript ends interrupted, by its (size, mtime):
    /// the end is only read again when the file changes.
    interrupts: Interrupts,
}

/// Subagent transcript → ((size, mtime), ends interrupted).
type Interrupts = HashMap<PathBuf, ((u64, i64), bool)>;

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
            // the file is left behind when the process dies without cleaning up, and the pid may be reused
            if process_alive(pid, &proc_start) {
                agents.push(agent);
            }
        }
        agents.sort_by_key(|a| std::cmp::Reverse(a.since_ms));
        agents.truncate(LIMIT);
        self.transcripts.retain(|id, _| agents.iter().any(|a| a.id == *id));

        let mut subs = Vec::new();
        let mut used = Vec::new();
        let mut seen = Interrupts::new();
        for a in &mut agents {
            let Some(transcript) = self.transcript(home, &a.id, &a.cwd) else { continue };
            let log = self.logs.entry(transcript.clone()).or_default();
            log.read(&transcript);
            log.fill(a);
            a.log = Some(transcript.clone());
            let dir = transcript.with_extension("").join("subagents");
            subs.extend(subagents(&dir, a, now, &self.interrupts, &mut seen));
            used.push(transcript);
        }
        self.logs.retain(|p, _| used.contains(p));
        self.interrupts = seen;
        agents.extend(subs);
        Ok(agents)
    }
}

impl Claude {
    /// The session's `projects/<folder>/<session>.jsonl`: first by the name the current
    /// folder would give, then searching all of them.
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

/// Reads a `sessions/<pid>.json`: (pid, process start, session).
fn parse_session(text: &str) -> Option<(u32, String, Agent)> {
    let v: Value = serde_json::from_str(text).ok()?;
    let pid = u32::try_from(v["pid"].as_u64()?).ok()?;
    let text_of = |key: &str| v[key].as_str().unwrap_or_default().to_string();
    // spare, pre-warmed process: not anyone's session yet
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
        log: None,
    };
    Some((pid, text_of("procStart"), agent))
}

/// The process exists and is the same one that wrote the file (`procStart` is field 22 of
/// `/proc/<pid>/stat`). Without `procStart`, existing is enough.
#[cfg(not(windows))]
fn process_alive(pid: u32, proc_start: &str) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else { return false };
    proc_start.is_empty() || stat_start(&stat).is_none_or(|s| s == proc_start)
}

/// Windows has no `/proc` (nor a comparable `procStart`): the process existing is enough.
#[cfg(windows)]
fn process_alive(pid: u32, _proc_start: &str) -> bool {
    crate::core::os::alive(pid)
}

/// Field 22 (starttime) of a `/proc/<pid>/stat`. The program name (field 2) may contain
/// spaces and parentheses: count from the last `)`.
#[cfg(any(not(windows), test))]
fn stat_start(stat: &str) -> Option<&str> {
    stat.rsplit_once(')')?.1.split_whitespace().nth(19)
}

/// What is known about a transcript, updated on each reading with the new lines.
#[derive(Debug, Default, PartialEq, Eq)]
struct Log {
    /// How far the file has been read (always at the end of a line).
    offset: u64,
    /// Title Claude gave the conversation.
    title: String,
    last_prompt: String,
    model: String,
    effort: String,
    branch: String,
    stop_reason: String,
    /// Last tool called in the current turn (cleared when the turn closes).
    action: String,
    last_turn: Option<(i64, u32)>,
    /// Tokens of the replies already closed.
    tokens: Tokens,
    /// The reply in progress: (id, tokens). A reply spans several lines (one per block:
    /// reasoning, text, tool), all with the same id and the same usage; it only enters
    /// the sum when the next reply starts, so it isn't counted twice.
    open: Option<(String, Tokens)>,
}

impl Log {
    /// Reads the lines that arrived since last time.
    fn read(&mut self, path: &Path) {
        let Ok(mut file) = std::fs::File::open(path) else { return };
        let len = file.metadata().map_or(0, |m| m.len());
        if len < self.offset {
            // the file shrank (it was rewritten): start over
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
                // line still being written (no line break at the end): left for the next reading
                Ok(n) if n > 0 && line.last() == Some(&b'\n') => {
                    self.offset += n as u64;
                    self.feed(&String::from_utf8_lossy(&line));
                }
                _ => break,
            }
        }
    }

    /// One transcript line. Only the relevant ones get parsed: the others (attachments,
    /// request copies) may be huge.
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
                // the turn closed: its last tool isn't what the next turn is doing. Not on
                // `last-prompt`: Claude Code rewrites that line in the middle of a turn too.
                self.action.clear();
            }
            // `isSidechain`: a subagent's reply written to the main transcript
            Some("assistant") if v["isSidechain"] != Value::Bool(true) => {
                let m = &v["message"];
                // "<synthetic>": a message Claude Code built itself, with no model
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

    /// Hands the session what the transcript knows.
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
            wrote_ms: 0,
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

/// A session's subagents: an `agent-<id>.meta.json` and the transcript `agent-<id>.jsonl`
/// for each one.
/// `cache`: what the last collection found about interruptions; `seen` gets what this one
/// used, so files that went away leave the cache.
fn subagents(dir: &Path, parent: &Agent, now: i64, cache: &Interrupts, seen: &mut Interrupts) -> Vec<Agent> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut subs = Vec::new();
    for entry in entries.flatten() {
        let log = entry.path();
        if log.extension().is_none_or(|e| e != "jsonl") {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let mtime = mtime_ms(&meta);
        // too old: don't even open the description file
        if now - mtime > RECENT.as_millis() as i64 {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(log.with_extension("meta.json")) else { continue };
        let Some(sub) = parse_sub_meta(&text) else { continue };
        // a forced stop doesn't get `finished`: either the file says `stoppedByUser`, or
        // the transcript ends with the interruption notice
        let stopped = sub.stopped || (!sub.finished && interrupted(&log, (meta.len(), mtime), cache, seen));
        // no end mark and still writing: working since the file was created
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
                tr!("interrupted")
            } else {
                tr!("inactive")
            }
            .into(),
            action: String::new(),
            parent: Some(parent.id.clone()),
            details: Details { kind: sub.kind, model: sub.model, depth: sub.depth, wrote_ms: mtime, ..Details::default() },
            log: Some(log.clone()),
        });
    }
    subs.sort_by_key(|a| std::cmp::Reverse(a.since_ms));
    subs.truncate(SUBS);
    subs
}

/// Timeline of a transcript chunk: prompts, replies, tools and errors.
/// `sub`: a subagent transcript, where every line is `isSidechain` (in the main session's
/// transcript those lines belong to subagents and are left out).
pub(super) fn timeline(text: &str, sub: bool) -> Vec<Step> {
    let mut steps = Vec::new();
    for line in text.lines() {
        // only the relevant lines get parsed: attachments and results may be huge
        if !line.contains(r#""type":"user""#) && !line.contains(r#""type":"assistant""#) {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        // `isMeta`: text Claude Code injects itself (reminders, command output)
        if (!sub && v["isSidechain"] == Value::Bool(true)) || v["isMeta"] == Value::Bool(true) {
            continue;
        }
        let at = iso_ms(&v["timestamp"]);
        let content = &v["message"]["content"];
        let parts = content.as_array().map(Vec::as_slice).unwrap_or_default();
        match v["type"].as_str() {
            Some("user") => {
                let texts = content.as_str().into_iter().chain(parts.iter().filter(|p| p["type"] == "text").filter_map(|p| p["text"].as_str()));
                for t in texts {
                    if t.starts_with("[Request interrupted") {
                        push_step(&mut steps, at, StepKind::Error, tr!("interrupted by the user"));
                    } else if !t.trim_start().starts_with('<') {
                        // `<command-name>`, `<local-command-stdout>`…: markup, not a prompt
                        push_step(&mut steps, at, StepKind::User, t);
                    }
                }
                // the tool result may hold secrets: only whether it failed
                if parts.iter().any(|p| p["type"] == "tool_result" && p["is_error"] == Value::Bool(true)) {
                    push_step(&mut steps, at, StepKind::Error, tr!("the tool failed"));
                }
            }
            Some("assistant") => {
                for p in parts {
                    match p["type"].as_str() {
                        Some("text") => push_step(&mut steps, at, StepKind::Reply, p["text"].as_str().unwrap_or_default()),
                        Some("tool_use") => {
                            push_step(&mut steps, at, StepKind::Tool, &describe(p["name"].as_str().unwrap_or("?"), &p["input"]))
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    last_steps(steps)
}

#[derive(Debug, PartialEq, Eq)]
struct SubMeta {
    /// What the subagent does (the description given when spawning it; without one, the type).
    title: String,
    kind: String,
    model: String,
    depth: Option<u32>,
    finished: bool,
    /// Stopped by the user (or by the session that spawned it) before finishing.
    stopped: bool,
}

/// A subagent's transcript ends with Claude Code's interruption notice. Only the last line
/// matters and the notice is short, so only the last few KiB are read, and only when the
/// file changed since the last collection.
fn interrupted(log: &Path, stamp: (u64, i64), cache: &Interrupts, seen: &mut Interrupts) -> bool {
    const END: u64 = 8 * 1024;
    let hit = cache.get(log).filter(|(s, _)| *s == stamp).map(|(_, v)| *v);
    let value = hit.unwrap_or_else(|| read_last(log, stamp.0, END).is_some_and(|tail| ends_interrupted(&tail)));
    seen.insert(log.to_path_buf(), (stamp, value));
    value
}

/// The last line is the message Claude Code writes when a turn is interrupted. Unescaped
/// quotes: a text that merely quotes the notice (with `\"`) doesn't count.
fn ends_interrupted(tail: &str) -> bool {
    tail.lines().next_back().is_some_and(|last| last.contains(r#""text":"[Request interrupted by user"#))
}

/// Reads an `agent-<id>.meta.json`.
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
    fn session() {
        let text = r#"{"pid":3839,"sessionId":"abc","cwd":"/home/u/proj","startedAt":10,"procStart":"14457727",
            "kind":"interactive","name":"proj-39","status":"busy","statusUpdatedAt":99}"#;
        let (pid, start, a) = parse_session(text).unwrap();
        assert_eq!((pid, start.as_str()), (3839, "14457727"));
        assert_eq!((a.title.as_str(), a.state, a.since_ms), ("proj-39", State::Working, 99));

        let idle = parse_session(r#"{"pid":1,"cwd":"/home/u/proj/","status":"idle","startedAt":10}"#).unwrap().2;
        assert_eq!((idle.title.as_str(), idle.state, idle.since_ms), ("proj", State::Idle, 10));

        // waiting for the user beats the status
        let wait = parse_session(r#"{"pid":1,"cwd":"/x","status":"busy","waitingFor":"permission"}"#).unwrap().2;
        assert_eq!((wait.state, wait.detail.as_str()), (State::Waiting, "permission"));

        assert!(parse_session(r#"{"pid":1,"cwd":"/x","status":"idle","spare":true}"#).is_none());
        assert!(parse_session("garbage").is_none());
    }

    #[test]
    fn process_start_with_odd_name() {
        let stat = "42 (a b) c) S 1 42 42 0 -1 4194560 100 0 0 0 5 3 0 0 20 0 1 0 14457727 1000 200";
        assert_eq!(stat_start(stat), Some("14457727"));
    }

    /// A reply line of the transcript: one content block of reply `id`.
    fn answer(id: &str, out: u64, stop: &str, block: Value) -> String {
        json!({"type":"assistant","isSidechain":false,"effort":"medium","gitBranch":"master",
            "message":{"id":id,"model":"claude-x","stop_reason":stop,"content":[block],
                "usage":{"input_tokens":2,"output_tokens":out,"cache_read_input_tokens":100,"cache_creation_input_tokens":10}}})
        .to_string()
    }

    #[test]
    fn transcript_becomes_details() {
        let mut log = Log::default();
        let bash = json!({"type":"tool_use","name":"Bash","input":{"command":"cargo test --all","description":"Run the tests"}});
        let lines = [
            json!({"type":"ai-title","aiTitle":"Monitor agents"}).to_string(),
            json!({"type":"last-prompt","lastPrompt":"write\nthe  wrapper"}).to_string(),
            // the same reply on three lines (reasoning, text, tool): counted once
            answer("m1", 50, "tool_use", json!({"type":"thinking"})),
            answer("m1", 50, "tool_use", json!({"type":"text","text":"running it"})),
            answer("m1", 50, "tool_use", bash),
            json!({"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"ok"}]}}).to_string(),
            answer("m2", 7, "end_turn", json!({"type":"text","text":"done"})),
            json!({"type":"system","subtype":"turn_duration","durationMs":10449,"messageCount":316}).to_string(),
            // a subagent reply and a message without a model change nothing
            json!({"type":"assistant","isSidechain":true,"message":{"id":"s1","model":"other","usage":{"output_tokens":999}}}).to_string(),
            json!({"type":"assistant","message":{"model":"<synthetic>","content":[]}}).to_string(),
            "garbage quoting \"type\":\"assistant\"".to_string(),
        ];
        for line in &lines {
            log.feed(line);
        }

        let mut a = parse_session(r#"{"pid":1,"cwd":"/x","name":"x-39","status":"busy"}"#).unwrap().2;
        log.fill(&mut a);
        // the turn closed (`turn_duration`): its last tool isn't the current action
        assert_eq!((a.title.as_str(), a.action.as_str()), ("Monitor agents", ""));
        let d = &a.details;
        assert_eq!(d.last_prompt, "write the wrapper");
        assert_eq!((d.model.as_str(), d.effort.as_str(), d.branch.as_str()), ("claude-x", "medium", "master"));
        assert_eq!(d.tokens, Some(Tokens { input: 4, output: 57, cache_read: 200, cache_write: 20 }));
        assert_eq!((d.last_turn, d.stop_reason.as_str()), (Some((10449, 316)), "end_turn"));

        // idle: the last tool is no longer "what it is doing now"
        let mut idle = parse_session(r#"{"pid":1,"cwd":"/x","status":"idle"}"#).unwrap().2;
        log.fill(&mut idle);
        assert_eq!(idle.action, "");

        // a new turn that is only thinking so far: the previous turn's tool doesn't show
        let mut log = Log::default();
        log.feed(&answer("m1", 5, "tool_use", json!({"type":"tool_use","name":"Bash","input":{"description":"Run the tests"}})));
        log.feed(&json!({"type":"last-prompt","lastPrompt":"go"}).to_string());
        let mut busy = parse_session(r#"{"pid":1,"cwd":"/x","status":"busy"}"#).unwrap().2;
        log.fill(&mut busy);
        // `last-prompt` is rewritten mid-turn: it keeps the action
        assert_eq!(busy.action, "Bash: Run the tests");
        log.feed(&json!({"type":"system","subtype":"turn_duration","durationMs":5,"messageCount":2}).to_string());
        log.feed(&answer("m2", 5, "", json!({"type":"thinking"})));
        log.fill(&mut busy);
        assert_eq!(busy.action, "");
        log.feed(&answer("m2", 5, "tool_use", json!({"type":"tool_use","name":"Read","input":{"file_path":"/x/a.rs"}})));
        log.fill(&mut busy);
        assert_ne!(busy.action, "");

        // empty transcript: the session name stays and there are no details
        let mut plain = parse_session(r#"{"pid":1,"cwd":"/x","name":"x-39","status":"idle"}"#).unwrap().2;
        Log::default().fill(&mut plain);
        assert_eq!((plain.title.as_str(), &plain.details), ("x-39", &Details::default()));
    }

    #[test]
    fn reads_only_new_complete_lines() {
        let dir = std::env::temp_dir().join(format!("miniutils-claude-log-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.jsonl");
        let first = answer("m1", 5, "end_turn", json!({"type":"text","text":"a"}));
        let second = answer("m2", 7, "end_turn", json!({"type":"text","text":"b"}));
        let out = |log: &Log| log.open.as_ref().map_or(0, |(_, t)| t.output) + log.tokens.output;

        // the second line is still being written (no line break at the end)
        std::fs::write(&path, format!("{first}\n{}", &second[..30])).unwrap();
        let mut log = Log::default();
        log.read(&path);
        assert_eq!((out(&log), log.offset), (5, first.len() as u64 + 1));

        std::fs::write(&path, format!("{first}\n{second}\n")).unwrap();
        log.read(&path);
        assert_eq!(out(&log), 12);
        // nothing new, nothing changes
        log.read(&path);
        assert_eq!(out(&log), 12);

        // file rewritten smaller: start over
        std::fs::write(&path, format!("{second}\n")).unwrap();
        log.read(&path);
        assert_eq!(out(&log), 7);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn subagent_description() {
        let meta = r#"{"agentType":"web-fetch","description":"Read the page","spawnDepth":1,"pickedEffortModel":"claude-y","finished":true}"#;
        let sub = parse_sub_meta(meta).unwrap();
        assert_eq!((sub.title.as_str(), sub.kind.as_str(), sub.model.as_str()), ("Read the page", "web-fetch", "claude-y"));
        assert_eq!((sub.depth, sub.finished), (Some(1), true));
        assert!(!sub.stopped);
        let bare = parse_sub_meta(r#"{"agentType":"Explore"}"#).unwrap();
        assert_eq!((bare.title.as_str(), bare.depth, bare.finished, bare.stopped), ("Explore", None, false, false));
        assert_eq!(parse_sub_meta("{}"), None);

        // forced stop: no `finished`, with `stoppedByUser`; the actual model beats the picked one
        let killed = parse_sub_meta(r#"{"agentType":"Explore","stoppedByUser":true,"model":"claude-z","pickedEffortModel":"claude-y"}"#).unwrap();
        assert_eq!((killed.finished, killed.stopped, killed.model.as_str()), (false, true, "claude-z"));
    }

    #[test]
    fn transcript_ending_interrupted() {
        let result = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"ok"}]}}"#;
        let stop = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"[Request interrupted by user for tool use]"}]}}"#;
        // a result that merely quotes the notice has escaped quotes
        let quoted = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"{\"text\":\"[Request interrupted by user]\"}"}]}}"#;
        assert!(ends_interrupted(&[result, stop].join("\n")));
        assert!(ends_interrupted(&format!("{stop}\n")));
        // interrupted earlier but continued afterwards: doesn't count
        assert!(!ends_interrupted(&[stop, result].join("\n")));
        assert!(!ends_interrupted(&[result, quoted].join("\n")));
        assert!(!ends_interrupted(""));
    }

    #[test]
    fn interruption_read_only_when_the_file_changes() {
        let dir = std::env::temp_dir().join(format!("miniutils-claude-sub-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("agent-a.jsonl");
        let stop = r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"[Request interrupted by user]"}]}}"#;
        std::fs::write(&path, format!("{stop}\n")).unwrap();
        let stamp = (std::fs::metadata(&path).unwrap().len(), 1);
        let mut seen = Interrupts::new();
        assert!(interrupted(&path, stamp, &Interrupts::new(), &mut seen));
        // same size and time: the cached answer, without reading (the file is gone)
        std::fs::remove_file(&path).unwrap();
        let cache = std::mem::take(&mut seen);
        assert!(interrupted(&path, stamp, &cache, &mut seen));
        assert_eq!(seen.len(), 1);
        // changed: read again (now missing, so not interrupted)
        assert!(!interrupted(&path, (stamp.0 + 1, 2), &cache, &mut seen));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn timeline_steps() {
        let lines = [
            r#"{"type":"user","isSidechain":false,"timestamp":"2026-11-11T10:00:00.000Z","message":{"role":"user","content":"run the\ntests"}}"#,
            r#"{"type":"user","isMeta":true,"timestamp":"2026-11-11T10:00:00.000Z","message":{"role":"user","content":"reminder"}}"#,
            r#"{"type":"user","timestamp":"2026-11-11T10:00:00.000Z","message":{"role":"user","content":"<command-name>/clear</command-name>"}}"#,
            r#"{"type":"assistant","timestamp":"2026-11-11T10:00:01.000Z","message":{"content":[{"type":"thinking","thinking":"..."},{"type":"text","text":"Running it."},{"type":"tool_use","name":"Bash","input":{"command":"cargo test --all"}}]}}"#,
            r#"{"type":"user","timestamp":"2026-11-11T10:00:02.000Z","message":{"role":"user","content":[{"type":"tool_result","content":"secret","is_error":true}]}}"#,
            r#"{"type":"user","timestamp":"2026-11-11T10:00:02.000Z","message":{"role":"user","content":[{"type":"tool_result","content":"ok"}]}}"#,
            r#"{"type":"assistant","isSidechain":true,"timestamp":"2026-11-11T10:00:03.000Z","message":{"content":[{"type":"text","text":"from a subagent"}]}}"#,
            r#"{"type":"user","timestamp":"2026-11-11T10:00:04.000Z","message":{"role":"user","content":[{"type":"text","text":"[Request interrupted by user]"}]}}"#,
            r#"{"type":"ai-title","aiTitle":"x"}"#,
        ]
        .join("\n");
        let got: Vec<(StepKind, String)> = timeline(&lines, false).into_iter().map(|s| (s.kind, s.text)).collect();
        assert_eq!(
            got,
            [
                (StepKind::User, "run the tests".to_string()),
                (StepKind::Reply, "Running it.".into()),
                (StepKind::Tool, "Bash: cargo test".into()),
                (StepKind::Error, "the tool failed".into()),
                (StepKind::Error, "interrupted by the user".into()),
            ]
        );
        // the tool result doesn't show up, not even on error
        assert!(!lines.is_empty() && got.iter().all(|(_, t)| !t.contains("secret")));
        // in a subagent transcript, the `isSidechain` lines are its own
        assert!(timeline(&lines, true).iter().any(|s| s.text == "from a subagent"));
        assert_eq!(timeline(&lines, false)[0].at_ms, 1_794_391_200_000);
    }
}
