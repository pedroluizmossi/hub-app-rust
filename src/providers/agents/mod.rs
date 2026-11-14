//! Agents running in the AI coding tools.
//!
//! Each tool has its own source (`claude.rs`, `codex.rs`, `opencode.rs`), which knows where
//! it keeps its state and what can be taken from there. They all implement `Source` and
//! return `Agent`: the basics (state, title, folder) are required; the rest goes in
//! `Details`, with optional fields each source fills as far as the tool reports. The screen
//! shows whatever comes: a tool with less information just has fewer lines.
//!
//! Sources only read what the tools already write to disk; they change nothing there.
//! Those formats are internal and change between versions: each source fails on its own
//! (it becomes a short error on screen) without taking the others down.
//!
//! Only a short summary of the current action comes out (tool name, file, program): never
//! the full command or the content, which may hold secrets.
//!
//! A session's timeline (`Timeline`) is only read for the session the user opened in the
//! detail window (`watch`): prompts and replies, clipped, and tool calls in the same short
//! summary. It doesn't go to plugins (`serde(skip)`).

mod claude;
mod codex;
mod opencode;

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags};
use serde_json::Value;

use crate::tr;

/// Sessions stopped for longer than this don't even leave the provider (the app filters
/// further by its option).
const RECENT: Duration = Duration::from_secs(24 * 3600);
/// "Working" with no write for this long: the tool died mid-turn.
const STALE: Duration = Duration::from_secs(30 * 60);
/// How much of the end of a log is read to find the last turn and the last tool.
const TAIL: u64 = 256 * 1024;
/// Maximum sessions per tool (subagents count in Codex and OpenCode).
const LIMIT: usize = 40;
/// How much of the end of the log the timeline reads: more than `TAIL`, because tool
/// results and images take most of it (only one session is read, and only when it changes).
const TIMELINE_TAIL: u64 = 2 * 1024 * 1024;
/// Steps kept in the timeline (the most recent ones).
const STEPS: usize = 80;
/// Prompts and replies in the timeline: only the beginning.
const STEP_TEXT: usize = 400;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "lowercase")]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// In the middle of a turn.
    Working,
    /// Stopped waiting for the user (permission, question). Only Claude reports it.
    Waiting,
    /// Finished the turn; waiting for the next prompt.
    Idle,
}

/// Tokens spent in a session, summed over every reply.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

/// What a tool knows beyond the basics about a session. All optional ("" or `None` = the
/// tool doesn't report it, or its source doesn't read it yet).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Details {
    /// The user's last prompt (their text: only shown when they open the row).
    pub last_prompt: String,
    pub model: String,
    /// Reasoning effort of the last reply ("high", "medium"…).
    pub effort: String,
    /// Git branch in the session's folder.
    pub branch: String,
    pub tokens: Option<Tokens>,
    /// Last closed turn: duration in ms and how many messages the conversation had.
    pub last_turn: Option<(i64, u32)>,
    /// Why the last reply stopped ("end_turn", "tool_use", "max_tokens"…).
    pub stop_reason: String,
    /// Subagent: its type ("Explore", "web-fetch"…).
    pub kind: String,
    /// Subagent: how many spawns away from the user's session it is (1 = directly from it).
    pub depth: Option<u32>,
    /// Last write to the conversation log (ms since 1970; 0 = unknown). Claude writes on
    /// every tool call and every result: writing steadily, it is calling tools; quiet, it is
    /// thinking (or in a slow tool).
    pub wrote_ms: i64,
}

/// An agent session.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Agent {
    pub tool: Tool,
    /// Session id in the tool (stable while it exists).
    pub id: String,
    pub title: String,
    /// Folder it runs in.
    pub cwd: String,
    pub state: State,
    /// Since when it is in this state (ms since 1970; 0 = unknown).
    pub since_ms: i64,
    /// What it expects from the user, or how the last turn ended ("" = nothing to say).
    pub detail: String,
    /// Summary of the last tool used in the current turn ("" = unknown or stopped).
    pub action: String,
    /// Subagent: id of the session that spawned it (same tool).
    pub parent: Option<String>,
    pub details: Details,
    /// Where the tool writes the conversation (transcript, log or database): the timeline
    /// reads from there. Local path, not sent to plugins.
    #[serde(skip)]
    pub log: Option<PathBuf>,
}

/// What a timeline step is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepKind {
    /// User prompt.
    User,
    /// Text of the agent's reply.
    Reply,
    /// Tool call (only the summary, as in `Agent::action`).
    Tool,
    /// A tool that failed or an interrupted turn.
    Error,
}

/// One step of the conversation: when (ms since 1970; 0 = unknown), what, and the text,
/// already clipped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub at_ms: i64,
    pub kind: StepKind,
    pub text: String,
}

/// The last steps of the session open in the detail window, oldest to newest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Timeline {
    pub tool: Tool,
    pub id: String,
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct AgentsSnapshot {
    pub agents: Vec<Agent>,
    /// Tools installed (with a state folder) on this machine.
    pub found: Vec<Tool>,
    /// Sources that couldn't be read.
    pub errors: Vec<(Tool, String)>,
    /// Timeline of the watched session (`watch`), if it is in the list.
    #[serde(skip)]
    pub timeline: Option<Timeline>,
}

/// Session open in the detail window: the provider reads the timeline of that one only.
static WATCH: Mutex<Option<(Tool, String)>> = Mutex::new(None);

/// Starts watching the session (or none, with `None`).
pub fn watch(key: Option<(Tool, String)>) {
    if let Ok(mut w) = WATCH.lock() {
        *w = key;
    }
}

/// Some session is open in the detail window (the provider reads at the open-window pace).
pub fn watching() -> bool {
    WATCH.lock().is_ok_and(|w| w.is_some())
}

fn watched() -> Option<(Tool, String)> {
    WATCH.lock().ok().and_then(|w| w.clone())
}

/// A tool's source: where it keeps its state and how to read the sessions from there.
///
/// To add a tool: a module with a type implementing this, a case in `Tool` and a line in
/// `AgentsProvider::default`. Whatever it doesn't know stays empty in `Details`; nothing
/// else is needed for it to show up on screen.
trait Source: Send {
    fn tool(&self) -> Tool;
    /// The tool's state folder (if it exists, the tool is installed).
    fn home(&self) -> Option<PathBuf>;
    /// Open or recent sessions. `now` in ms since 1970.
    fn collect(&mut self, home: &Path, now: i64) -> Result<Vec<Agent>, String>;
}

pub struct AgentsProvider {
    sources: Vec<Box<dyn Source>>,
    /// Last timeline read: from which file, its (size, mtime) and the steps. Only re-read
    /// when the file changes.
    timeline: Option<(PathBuf, (u64, i64), Vec<Step>)>,
}

impl Default for AgentsProvider {
    fn default() -> Self {
        AgentsProvider {
            sources: vec![
                Box::new(claude::Claude::default()),
                Box::new(codex::Codex::default()),
                Box::new(opencode::OpenCode),
            ],
            timeline: None,
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
        snap.timeline = self.timeline_of(&snap.agents);
        snap
    }

    /// Timeline of the watched session, if it is in `agents`.
    fn timeline_of(&mut self, agents: &[Agent]) -> Option<Timeline> {
        let Some((tool, id)) = watched() else {
            self.timeline = None;
            return None;
        };
        let a = agents.iter().find(|a| a.tool == tool && a.id == id)?;
        let path = a.log.clone()?;
        let steps = match tool {
            // database: the query is small and the main file doesn't even change on every write (WAL)
            Tool::OpenCode => opencode::timeline(&path, &id),
            Tool::Claude | Tool::Codex => {
                let meta = std::fs::metadata(&path).ok()?;
                let stamp = (meta.len(), mtime_ms(&meta));
                match &self.timeline {
                    Some((p, s, steps)) if *p == path && *s == stamp => steps.clone(),
                    _ => {
                        let text = read_last(&path, meta.len(), TIMELINE_TAIL)?;
                        // start cut in the middle of a line: that line is left out
                        let text =
                            if meta.len() > TIMELINE_TAIL { text.split_once('\n').map_or("", |(_, r)| r) } else { &text };
                        let steps = if tool == Tool::Claude {
                            claude::timeline(text, a.parent.is_some())
                        } else {
                            codex::timeline(text)
                        };
                        self.timeline = Some((path.clone(), stamp, steps.clone()));
                        steps
                    }
                }
            }
        };
        Some(Timeline { tool, id, steps })
    }
}

/// Adds a step with the text on a single line and clipped; empty text is skipped.
fn push_step(steps: &mut Vec<Step>, at_ms: i64, kind: StepKind, text: &str) {
    let text = clip(text, STEP_TEXT);
    if text.is_empty() {
        return;
    }
    steps.push(Step { at_ms, kind, text });
}

/// Only the `STEPS` most recent steps.
fn last_steps(mut steps: Vec<Step>) -> Vec<Step> {
    if steps.len() > STEPS {
        steps.drain(..steps.len() - STEPS);
    }
    steps
}

/// ISO 8601 time (`2026-11-11T13:57:13.069Z`) in ms since 1970; 0 if unreadable.
fn iso_ms(v: &Value) -> i64 {
    v.as_str().and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()).map_or(0, |t| t.timestamp_millis())
}

/// State folder: the tool's environment variable, or `rel` in the home folder.
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

/// Text on a single line (titles come from the first prompt, which may have line breaks).
fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A single line, clipped to `max` characters, with an ellipsis.
pub fn clip(s: &str, max: usize) -> String {
    let s = one_line(s);
    if s.chars().count() <= max { s } else { s.chars().take(max.saturating_sub(1)).chain(['…']).collect() }
}

/// Opens another tool's database read-only, without waiting long for a lock.
fn open(path: &Path) -> Result<Connection, String> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .map_err(|_| tr!("could not open the database"))?;
    conn.busy_timeout(Duration::from_millis(300)).map_err(|_| tr!("could not open the database"))?;
    Ok(conn)
}

/// The last `TAIL` bytes of a file of `len` bytes.
fn read_tail(path: &Path, len: u64) -> Option<String> {
    read_last(path, len, TAIL)
}

/// The last `n` bytes of a file of `len` bytes.
fn read_last(path: &Path, len: u64, n: u64) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    file.seek(SeekFrom::Start(len.saturating_sub(n))).ok()?;
    let mut buf = Vec::new();
    file.take(n).read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// A shell command reduced to the program and, if it is a plain word, the subcommand:
/// `FOO=1 cargo test --all` → `cargo test`. The rest (arguments, URLs, tokens) is left out.
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

/// Summary of a tool call from its name and arguments: the description the agent itself
/// gave, the file, the program.
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

/// `https://example.com/a?b` → `example.com`.
fn host(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    clip(rest.split(['/', '?', '#']).next().unwrap_or_default(), 40)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_summary_drops_arguments() {
        assert_eq!(short_cmd("cargo test --all"), "cargo test");
        assert_eq!(short_cmd("A=1 B=2 /usr/bin/git push origin main"), "git push");
        assert_eq!(short_cmd("curl -H 'Authorization: Bearer x' https://a.b"), "curl");
        assert_eq!(short_cmd("cat /etc/passwd"), "cat");
        assert_eq!(short_cmd("pwd && ls"), "pwd");
        assert_eq!(short_cmd("$(evil) x"), "");
        assert_eq!(short_cmd(""), "");
    }

    #[test]
    fn call_summary() {
        assert_eq!(describe("TaskStop", &serde_json::json!({})), "TaskStop");
        assert_eq!(describe("WebFetch", &serde_json::json!({"url":"https://a.b/c?token=x"})), "WebFetch: a.b");
        assert_eq!(describe("bash", &serde_json::json!({"command":"npm run build"})), "bash: npm run");
        assert_eq!(describe("Edit", &serde_json::json!({"file_path":"/home/u/p/src/bus.rs","old_string":"secret"})), "Edit: bus.rs");
        assert_eq!(clip("a  long\ntext", 9), "a long t…");
    }

    #[test]
    fn steps_are_clipped_and_only_the_last_kept() {
        let mut steps = Vec::new();
        push_step(&mut steps, 1, StepKind::User, "  \n ");
        assert!(steps.is_empty(), "empty text does not become a step");
        for i in 0..STEPS + 5 {
            push_step(&mut steps, i as i64, StepKind::Reply, "hi");
        }
        let steps = last_steps(steps);
        assert_eq!(steps.len(), STEPS);
        assert_eq!(steps[0].at_ms, 5);
        assert_eq!(iso_ms(&Value::from("2026-11-11T13:57:13.069Z")), 1_794_405_433_069);
    }
}
