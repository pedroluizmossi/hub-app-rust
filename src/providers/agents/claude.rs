//! Claude Code. The source that reports the most:
//! - `~/.claude/sessions/<pid>.json`, one per live process: `status` (`busy`/`idle`),
//!   `waitingFor` (what it expects from the user), name, folder.
//! - the transcript `projects/<folder>/<session>.jsonl`: conversation title, last prompt,
//!   model, effort, branch, tokens of each reply, turn durations, stop reason and the last
//!   tool called.
//! - `projects/<folder>/<session>/subagents/agent-*.meta.json`: type, description, model,
//!   depth and how each subagent ended (`finished`, `stoppedByUser`).
//! - `projects/<folder>/<session>/subagents/workflows/<run>/`: the agents of a workflow run
//!   (same files as a subagent) and its `journal.jsonl`; `<session>/workflows/<run>.json`
//!   appears when the run ends. Each run becomes one entry under the session, and its agents
//!   hang from the run.
//!
//! The transcript only grows: each reading takes the new lines and updates what was already
//! known (`Log`), instead of re-reading the file, which easily exceeds tens of MB.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use serde_json::Value;

use super::{
    Agent, Details, LIMIT, Phase, WORKFLOW, RECENT, Source, State, Step, StepKind, Tokens, Tool, base_name, clip, describe, home_dir, iso_ms,
    last_steps, mtime_ms, one_line, push_step, read_last,
};
use crate::tr;

/// Maximum subagents read per session (not counting workflow agents).
const SUBS: usize = 50;
/// Maximum workflow runs read per session.
const RUNS: usize = 10;
/// Maximum agents read per workflow run: a run easily starts dozens (a deep research
/// started over a hundred), and they all count in its progress.
const RUN_AGENTS: usize = 250;
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
    /// What was read about each workflow run, by its folder.
    runs: Runs,
    /// What has already been read from each workflow agent's transcript.
    run_logs: HashMap<PathBuf, Log>,
}

/// Subagent transcript → ((size, mtime), ends interrupted).
type Interrupts = HashMap<PathBuf, ((u64, i64), bool)>;
/// Workflow run folder → what was read about it.
type Runs = HashMap<PathBuf, Run>;

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
        let mut runs = Runs::new();
        let mut run_logs = Vec::new();
        for a in &mut agents {
            let Some(transcript) = self.transcript(home, &a.id, &a.cwd) else { continue };
            let log = self.logs.entry(transcript.clone()).or_default();
            log.read(&transcript);
            log.fill(a);
            a.log = Some(transcript.clone());
            let session = transcript.with_extension("");
            subs.extend(subagents(&session.join("subagents"), a, now, SUBS, &HashSet::new(), &self.interrupts, &mut seen));
            for mut run in workflows(&session, a, now, &mut self.runs, &mut runs) {
                let mut agents = subagents(&run.dir, &run.agent, now, RUN_AGENTS, &run.journal.ended, &self.interrupts, &mut seen);
                fill_run_agents(&mut agents, &run.journal, &mut self.run_logs, &mut run_logs);
                sum_run(&mut run, &agents);
                subs.push(run.agent);
                subs.extend(agents);
            }
            used.push(transcript);
        }
        self.logs.retain(|p, _| used.contains(p));
        self.interrupts = seen;
        self.runs = runs;
        self.run_logs.retain(|p, _| run_logs.contains(p));
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
    /// A subagent's own transcript, where every reply is `isSidechain`.
    sidechain: bool,
}

impl Log {
    /// Reads the lines that arrived since last time.
    fn read(&mut self, path: &Path) {
        let Ok(mut file) = std::fs::File::open(path) else { return };
        let len = file.metadata().map_or(0, |m| m.len());
        if len < self.offset {
            // the file shrank (it was rewritten): start over
            *self = Log { sidechain: self.sidechain, ..Log::default() };
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
            Some("assistant") if self.sidechain || v["isSidechain"] != Value::Bool(true) => {
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
            ..Details::default()
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

/// A session's subagents (or a workflow run's agents): an `agent-<id>.meta.json` and the
/// transcript `agent-<id>.jsonl` for each one, the `max` most recent.
/// `ended`: ids of agents known to have returned although their file has no `finished`
/// (a workflow's journal says so).
/// `cache`: what the last collection found about interruptions; `seen` gets what this one
/// used, so files that went away leave the cache.
fn subagents(
    dir: &Path,
    parent: &Agent,
    now: i64,
    max: usize,
    ended: &HashSet<String>,
    cache: &Interrupts,
    seen: &mut Interrupts,
) -> Vec<Agent> {
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
        let Some(mut sub) = parse_sub_meta(&text) else { continue };
        let id = log.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        sub.finished |= id.strip_prefix("agent-").is_some_and(|i| ended.contains(i));
        // a forced stop doesn't get `finished`: either the file says `stoppedByUser`, or
        // the transcript ends with the interruption notice
        let stopped = sub.stopped || (!sub.finished && interrupted(&log, (meta.len(), mtime), cache, seen));
        // no end mark and still writing: working since the file was created
        let working = !sub.finished && !stopped && now - mtime < SUB_STALE.as_millis() as i64;
        let born = meta.created().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis() as i64);
        subs.push(Agent {
            tool: Tool::Claude,
            id,
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
    subs.truncate(max);
    subs
}

/// A workflow run found in a session, with the agent that stands for it (its state comes
/// from its agents afterwards, in `sum_run`).
struct RunDir {
    dir: PathBuf,
    agent: Agent,
    journal: Journal,
    /// How the run ended ("" = no end file yet).
    status: String,
    /// The phases the script declares, in order (empty = it declares none, or they
    /// couldn't be read).
    phases: Vec<String>,
}

/// What was read about a workflow run: the journal by its (size, mtime), so it is only
/// parsed again when it grows; the script once; the end file once it appears.
#[derive(Debug, Default, Clone)]
struct Run {
    stamp: (u64, i64),
    journal: Journal,
    script: Option<Script>,
    end: Option<RunEnd>,
}

/// The run's script, `workflows/scripts/<name>-<run>.js`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Script {
    name: String,
    /// Titles in `meta.phases`.
    phases: Vec<String>,
}

/// A run's `journal.jsonl`: one line when each agent starts (with its label and phase) and
/// one when it returns.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Journal {
    /// The agents in the order they started.
    agents: Vec<RunAgent>,
    ended: HashSet<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct RunAgent {
    id: String,
    label: String,
    phase: String,
}

impl Journal {
    /// Phase of the last agent started.
    fn phase(&self) -> &str {
        self.agents.last().map_or("", |a| a.phase.as_str())
    }
}

/// `workflows/<run>.json`, written when the run ends.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct RunEnd {
    name: String,
    /// "completed", or how else it ended.
    status: String,
    model: String,
    phases: Vec<String>,
}

/// The session's workflow runs (the `RUNS` most recent), each with an agent that stands
/// for it, child of the session. `cache`: what the last collection read; `seen` gets what
/// this one used.
fn workflows(session: &Path, parent: &Agent, now: i64, cache: &mut Runs, seen: &mut Runs) -> Vec<RunDir> {
    let Ok(entries) = std::fs::read_dir(session.join("subagents").join("workflows")) else { return Vec::new() };
    let mut runs = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        let Some(id) = dir.file_name().map(|n| n.to_string_lossy().into_owned()) else { continue };
        let Ok(meta) = std::fs::metadata(dir.join("journal.jsonl")) else { continue };
        let mtime = mtime_ms(&meta);
        if now - mtime > RECENT.as_millis() as i64 {
            continue;
        }
        let born = meta.created().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis() as i64);
        let stamp = (meta.len(), mtime);
        let mut run = cache.remove(&dir).unwrap_or_default();
        if run.stamp != stamp {
            let Ok(text) = std::fs::read_to_string(dir.join("journal.jsonl")) else { continue };
            run.journal = parse_journal(&text);
            run.stamp = stamp;
        }
        if run.end.is_none() {
            run.end = std::fs::read_to_string(session.join("workflows").join(format!("{id}.json")))
                .ok()
                .and_then(|t| parse_run_end(&t));
        }
        if run.script.is_none() {
            run.script = Some(read_script(&session.join("workflows").join("scripts"), &id).unwrap_or_default());
        }
        let end = run.end.clone().unwrap_or_default();
        let script = run.script.clone().unwrap_or_default();
        let name = [end.name.as_str(), script.name.as_str()].into_iter().find(|n| !n.is_empty()).unwrap_or(&id).to_string();
        let agent = Agent {
            tool: Tool::Claude,
            id: id.clone(),
            title: clip(&name, 60),
            cwd: parent.cwd.clone(),
            state: State::Idle,
            // until `sum_run` knows whether an agent works: start (working) or last write
            since_ms: born,
            detail: String::new(),
            action: String::new(),
            parent: Some(parent.id.clone()),
            details: Details { kind: WORKFLOW.into(), model: end.model, wrote_ms: mtime, ..Details::default() },
            log: None,
        };
        let phases = if end.phases.is_empty() { script.phases } else { end.phases };
        runs.push(RunDir { dir: dir.clone(), agent, journal: run.journal.clone(), status: end.status, phases });
        seen.insert(dir, run);
    }
    runs.sort_by_key(|r| std::cmp::Reverse(r.agent.details.wrote_ms));
    runs.truncate(RUNS);
    runs
}

/// Fills in a run's agents what the journal says (label and phase) and what their
/// transcripts say (tokens; the tool being called, while working). `logs`: what was
/// already read of each transcript; `used` gets the transcripts read now.
fn fill_run_agents(agents: &mut [Agent], journal: &Journal, logs: &mut HashMap<PathBuf, Log>, used: &mut Vec<PathBuf>) {
    for a in agents {
        if let Some(j) = a.id.strip_prefix("agent-").and_then(|id| journal.agents.iter().find(|j| j.id == id)) {
            if !j.label.is_empty() {
                a.title = clip(&j.label, 60);
            }
            a.details.phase = j.phase.clone();
        }
        let Some(path) = a.log.clone() else { continue };
        let log = logs.entry(path.clone()).or_insert_with(|| Log { sidechain: true, ..Log::default() });
        log.read(&path);
        let total = log.open.as_ref().map_or(log.tokens, |(_, t)| sum(log.tokens, *t));
        a.details.tokens = (total != Tokens::default()).then_some(total);
        if a.details.model.is_empty() {
            a.details.model = log.model.clone();
        }
        if a.state == State::Working {
            a.action = log.action.clone();
        }
        used.push(path);
    }
}

/// The run takes its state from its agents: working while one of them works; stopped,
/// with how it ended, otherwise. Its phases get the count of agents started and returned
/// (a phase the script doesn't declare goes at the end); its `action` is the progress,
/// "<phase> · <returned>/<started>" while working and "<returned>/<started>" afterwards.
fn sum_run(run: &mut RunDir, agents: &[Agent]) {
    let a = &mut run.agent;
    let j = &run.journal;
    let mut phases: Vec<Phase> = run.phases.iter().map(|t| Phase { title: t.clone(), ..Phase::default() }).collect();
    for ag in &j.agents {
        let i = match phases.iter().position(|p| p.title == ag.phase) {
            Some(i) => i,
            None => {
                phases.push(Phase { title: ag.phase.clone(), ..Phase::default() });
                phases.len() - 1
            }
        };
        phases[i].agents.push(format!("agent-{}", ag.id));
        phases[i].done += u32::from(j.ended.contains(&ag.id));
    }
    a.details.phases = phases;
    a.details.phase = j.phase().to_string();
    a.details.wrote_ms = agents.iter().map(|s| s.details.wrote_ms).fold(a.details.wrote_ms, i64::max);
    if a.details.model.is_empty() {
        a.details.model = agents.iter().map(|s| s.details.model.clone()).find(|m| !m.is_empty()).unwrap_or_default();
    }
    let progress = format!("{}/{}", j.ended.len(), j.agents.len().max(j.ended.len()));
    if agents.iter().any(|s| s.state == State::Working) {
        a.state = State::Working;
        a.action = if j.phase().is_empty() { progress } else { format!("{} · {progress}", j.phase()) };
        return;
    }
    a.since_ms = a.details.wrote_ms;
    a.action = progress;
    a.detail = match run.status.as_str() {
        "completed" => String::new(),
        // no end file and nobody working: the session that ran it stopped
        "" if agents.iter().any(|s| s.detail == tr!("interrupted")) => tr!("interrupted").into(),
        "" => tr!("inactive").into(),
        s if s.contains("fail") || s.contains("error") => tr!("failed").into(),
        _ => tr!("interrupted").into(),
    };
}

/// Reads a run's `journal.jsonl`.
fn parse_journal(text: &str) -> Journal {
    let mut j = Journal::default();
    for line in text.lines() {
        // a `result` line carries the agent's whole answer: only the head says what it is
        let head = &line[..line.floor_char_boundary(200)];
        let started = head.starts_with(r#"{"type":"started""#);
        if !started && !head.starts_with(r#"{"type":"result""#) {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let Some(id) = v["agentId"].as_str() else { continue };
        if started {
            let text_of = |key: &str| v[key].as_str().unwrap_or_default().to_string();
            j.agents.push(RunAgent { id: id.to_string(), label: text_of("label"), phase: text_of("phase") });
        } else {
            j.ended.insert(id.to_string());
        }
    }
    j
}

/// Reads a `workflows/<run>.json`.
fn parse_run_end(text: &str) -> Option<RunEnd> {
    let v: Value = serde_json::from_str(text).ok()?;
    let text_of = |key: &str| v[key].as_str().unwrap_or_default().to_string();
    let phases = v["phases"].as_array().map_or(Vec::new(), |list| {
        list.iter().filter_map(|p| p["title"].as_str()).map(String::from).collect()
    });
    Some(RunEnd { name: text_of("workflowName"), status: text_of("status"), model: text_of("defaultModel"), phases })
}

/// Script of run `id`, `<name>-<id>.js`: its name and the phases it declares.
fn read_script(scripts: &Path, id: &str) -> Option<Script> {
    let suffix = format!("-{id}.js");
    let (name, path) = std::fs::read_dir(scripts)
        .ok()?
        .flatten()
        .find_map(|e| e.file_name().to_string_lossy().strip_suffix(&suffix).map(|n| (n.to_string(), e.path())))?;
    let phases = std::fs::read_to_string(path).map(|js| script_phases(&js)).unwrap_or_default();
    Some(Script { name, phases })
}

/// Phase titles in a workflow script's `meta` (`phases: [{ title: '…', … }, …]`). The
/// script is JavaScript, not JSON: the titles are found by their key and quotes.
fn script_phases(js: &str) -> Vec<String> {
    let Some(rest) = js.find("phases").map(|i| &js[i..]) else { return Vec::new() };
    let Some(open) = rest.find('[') else { return Vec::new() };
    let mut depth = 0;
    let Some(close) = rest[open..].char_indices().find_map(|(i, c)| {
        match c {
            '[' => depth += 1,
            ']' => depth -= 1,
            _ => {}
        }
        (depth == 0).then_some(open + i)
    }) else {
        return Vec::new();
    };
    let mut list = &rest[open..close];
    let mut titles = Vec::new();
    while let Some(i) = list.find("title") {
        list = list[i + 5..].trim_start_matches(['"', '\'']).trim_start();
        let Some(after) = list.strip_prefix(':') else { continue };
        let after = after.trim_start();
        let Some(quote) = after.chars().next().filter(|c| matches!(c, '\'' | '"' | '`')) else { continue };
        let body = &after[1..];
        let Some(end) = body.find(quote) else { break };
        titles.push(body[..end].to_string());
        list = &body[end + 1..];
    }
    titles
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
    fn workflow_journal_and_end() {
        let journal = [
            r#"{"type":"launched"}"#,
            r#"{"type":"started","key":"v2:1","agentId":"a1","label":"find:core","phase":"Discover"}"#,
            r#"{"type":"started","key":"v2:2","agentId":"a2","label":"find:hub","phase":"Discover"}"#,
            r#"{"type":"result","key":"v2:1","agentId":"a1","result":{"type":"started","phase":"not a phase"}}"#,
            r#"{"type":"started","key":"v2:3","agentId":"a3","label":"fix","phase":"Fix"}"#,
            r#"{"type":"started", broken"#,
        ]
        .join("\n");
        let j = parse_journal(&journal);
        let started: Vec<(&str, &str, &str)> = j.agents.iter().map(|a| (a.id.as_str(), a.label.as_str(), a.phase.as_str())).collect();
        assert_eq!(started, [("a1", "find:core", "Discover"), ("a2", "find:hub", "Discover"), ("a3", "fix", "Fix")]);
        assert_eq!((j.phase(), &j.ended), ("Fix", &HashSet::from(["a1".to_string()])));

        let end = parse_run_end(
            r#"{"runId":"wf_1","workflowName":"sweep","status":"completed","defaultModel":"claude-x","phases":[{"title":"Find"},{"title":"Fix"}]}"#,
        );
        let phases = vec!["Find".to_string(), "Fix".to_string()];
        assert_eq!(end, Some(RunEnd { name: "sweep".into(), status: "completed".into(), model: "claude-x".into(), phases }));
        assert_eq!(parse_run_end("garbage"), None);
    }

    #[test]
    fn phases_declared_in_the_script() {
        let js = r#"export const meta = {
  name: 'k8s',
  phases: [
    { title: 'Inventário', detail: 'map [cli] to routes' },
    { "title": "Review" },
    {title:`Fix: it's done`},
  ],
}
const phases = [{ title: 'not meta' }]"#;
        assert_eq!(script_phases(js), ["Inventário", "Review", "Fix: it's done"]);
        assert!(script_phases("export const meta = { name: 'x' }").is_empty());
        assert!(script_phases("phases: [ { title: 'open").is_empty());
    }

    #[test]
    fn workflow_run_under_its_session() {
        let session = std::env::temp_dir().join(format!("miniutils-claude-wf-{}", std::process::id()));
        let run = session.join("subagents").join("workflows").join("wf_1");
        let scripts = session.join("workflows").join("scripts");
        std::fs::create_dir_all(&run).unwrap();
        std::fs::create_dir_all(&scripts).unwrap();
        std::fs::write(scripts.join("big-sweep-wf_1.js"), "export const meta = { phases: [{ title: 'Discover' }, { title: 'Fix' }, { title: 'Check' }] }").unwrap();
        let journal = [
            r#"{"type":"started","agentId":"a1","label":"look around","phase":"Discover"}"#,
            r#"{"type":"started","agentId":"a2","phase":"Fix"}"#,
            r#"{"type":"result","agentId":"a1","result":{}}"#,
        ];
        std::fs::write(run.join("journal.jsonl"), journal.join("\n")).unwrap();
        // workflow agents have no `finished`: the journal says who returned
        let meta = r#"{"agentType":"workflow-subagent","description":"find:core","workflowPhase":"Discover"}"#;
        // a workflow agent's own replies are all `isSidechain`
        let reply = json!({"type":"assistant","isSidechain":true,"message":{"id":"m1","model":"claude-y",
            "usage":{"input_tokens":10,"output_tokens":20},
            "content":[{"type":"tool_use","name":"Bash","input":{"command":"cargo test","description":"Run the tests"}}]}});
        for id in ["a1", "a2"] {
            std::fs::write(run.join(format!("agent-{id}.meta.json")), meta).unwrap();
            std::fs::write(run.join(format!("agent-{id}.jsonl")), format!("{reply}\n")).unwrap();
        }
        let parent = parse_session(r#"{"pid":1,"sessionId":"s","cwd":"/p","status":"idle"}"#).unwrap().2;
        let now = mtime_ms(&std::fs::metadata(run.join("journal.jsonl")).unwrap());
        let read = |cache: &mut Runs| {
            let mut seen = Runs::new();
            let mut runs = workflows(&session, &parent, now, cache, &mut seen);
            assert_eq!(runs.len(), 1);
            let mut r = runs.remove(0);
            let mut agents = subagents(&r.dir, &r.agent, now, RUN_AGENTS, &r.journal.ended, &Interrupts::new(), &mut Interrupts::new());
            fill_run_agents(&mut agents, &r.journal, &mut HashMap::new(), &mut Vec::new());
            sum_run(&mut r, &agents);
            *cache = seen;
            (r.agent, agents)
        };

        // running: no end file yet, the name comes from the script; a2 is still writing
        let mut cache = Runs::new();
        let (a, agents) = read(&mut cache);
        assert_eq!((a.title.as_str(), a.parent.as_deref(), a.details.kind.as_str()), ("big-sweep", Some("s"), WORKFLOW));
        assert_eq!((a.state, a.action.as_str()), (State::Working, "Fix · 1/2"));
        let state = |id: &str| agents.iter().find(|s| s.id == format!("agent-{id}")).map(|s| (s.state, s.parent.clone()));
        assert_eq!(state("a1"), Some((State::Idle, Some("wf_1".into()))));
        assert_eq!(state("a2"), Some((State::Working, Some("wf_1".into()))));
        // the journal's label and phase; the transcript's tokens and (working) tool
        let a1 = agents.iter().find(|s| s.id == "agent-a1").unwrap();
        let a2 = agents.iter().find(|s| s.id == "agent-a2").unwrap();
        assert_eq!((a1.title.as_str(), a1.details.phase.as_str(), a1.action.as_str()), ("look around", "Discover", ""));
        assert_eq!((a2.title.as_str(), a2.action.as_str()), ("find:core", "Bash: Run the tests"));
        assert_eq!(a2.details.tokens.map(|t| t.input + t.output), Some(30));
        // the script's phases, each with its agents (the third hasn't started)
        let phases: Vec<(&str, usize, u32)> = a.details.phases.iter().map(|p| (p.title.as_str(), p.agents.len(), p.done)).collect();
        assert_eq!(phases, [("Discover", 1, 1), ("Fix", 1, 0), ("Check", 0, 0)]);
        assert_eq!(a.details.phase, "Fix");

        // ended: the end file gives the name and the status
        std::fs::write(run.join("journal.jsonl"), [&journal[..], &[r#"{"type":"result","agentId":"a2"}"#]].concat().join("\n")).unwrap();
        std::fs::write(session.join("workflows").join("wf_1.json"), r#"{"workflowName":"sweep","status":"completed"}"#).unwrap();
        let (a, _) = read(&mut cache);
        assert_eq!((a.title.as_str(), a.state, a.detail.as_str(), a.action.as_str()), ("sweep", State::Idle, "", "2/2"));

        // stopped midway: no end file and nobody working
        std::fs::remove_file(session.join("workflows").join("wf_1.json")).unwrap();
        std::fs::write(run.join("journal.jsonl"), journal.join("\n") + "\n").unwrap();
        // well past stale: the agent files are written after the journal (`now`), a few ms
        // later on Windows
        let later = now + SUB_STALE.as_millis() as i64 + 60_000;
        let mut r = workflows(&session, &parent, later, &mut Runs::new(), &mut Runs::new()).remove(0);
        let agents = subagents(&r.dir, &r.agent, later, RUN_AGENTS, &r.journal.ended, &Interrupts::new(), &mut Interrupts::new());
        sum_run(&mut r, &agents);
        assert_eq!((r.agent.state, r.agent.detail.as_str()), (State::Idle, "inactive"));
        std::fs::remove_dir_all(&session).unwrap();
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
