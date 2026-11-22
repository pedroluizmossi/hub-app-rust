//! Oh My Pi: project buckets of JSONL sessions with a title slot and an event journal.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use same_file::Handle;
use serde::Deserialize;
use serde_json::Value;

use super::{
    Agent, Artifact, Details, LIMIT, Lease, RECENT, STALE, Source, State, Step, StepKind, Tokens, Tool, base_name, clip,
    describe, iso_ms, last_steps, mtime_ms, push_step,
};
use crate::tr;

const SUBAGENT_LIMIT: usize = 24;
const MAX_DEPTH: u32 = 16;
type Stamp = (u64, SystemTime);

#[derive(Default)]
pub(super) struct Omp {
    sessions: HashMap<PathBuf, (Stamp, Option<Journal>)>,
}

impl Source for Omp {
    fn tool(&self) -> Tool {
        Tool::Omp
    }

    fn home(&self) -> Option<PathBuf> {
        let profile = std::env::var("OMP_PROFILE").or_else(|_| std::env::var("PI_PROFILE")).unwrap_or_default();
        let profile = profile.trim();
        let profile = match profile {
            "" | "default" => None,
            name if name.len() <= 64
                && !name.ends_with('.')
                && name.as_bytes().first().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
                && name.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"._-".contains(&c)) => Some(name),
            _ => return None,
        };
        let profile_root = |root: PathBuf| match profile {
            Some(name) => root.join("profiles").join(name),
            None => root,
        };
        let config = std::env::var_os("PI_CONFIG_DIR").filter(|s| !s.is_empty()).unwrap_or_else(|| ".omp".into());
        let default = profile_root(dirs::home_dir()?.join(config)).join("agent");
        if profile.is_none()
            && let Some(dir) = std::env::var_os("PI_CODING_AGENT_DIR").filter(|s| !s.is_empty())
        {
            let dir = std::path::absolute(dir).ok()?;
            if dir != default {
                return Some(dir);
            }
        }
        #[cfg(target_os = "linux")]
        if let Some(dir) = std::env::var_os("XDG_DATA_HOME").filter(|s| !s.is_empty()) {
            // OMP only switches to XDG once that profile's data root exists.
            let root = profile_root(PathBuf::from(dir).join("omp"));
            if root.is_dir() {
                return Some(root);
            }
        }
        Some(default)
    }

    fn collect(&mut self, home: &Path, now: i64) -> Result<Vec<Agent>, String> {
        let buckets = match fs::read_dir(home.join("sessions")) {
            Ok(dir) => dir,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.sessions.clear();
                return Ok(Vec::new());
            }
            Err(_) => return Err(tr!("could not read the sessions").into()),
        };
        let cutoff = now - RECENT.as_millis() as i64;
        let mut roots = Vec::new();
        for bucket in buckets.flatten().filter(|b| b.file_type().is_ok_and(|ty| ty.is_dir())) {
            roots.extend(candidates(&bucket.path(), cutoff, 0));
        }
        sort_recent(&mut roots);
        let mut agents = Vec::new();
        let mut ids = HashSet::new();
        let mut used = HashSet::new();
        let mut roots_left = LIMIT;
        let mut children_left = LIMIT;
        for root in &roots {
            let mut left = SUBAGENT_LIMIT + 1;
            let mut tree = Tree {
                now, agents: &mut agents, ids: &mut ids, used: &mut used, left: &mut left,
                children_left: &mut children_left,
            };
            if self.collect_tree(root, None, 0, &mut tree) {
                roots_left -= 1;
                if roots_left == 0 {
                    break;
                }
            }
        }
        self.sessions.retain(|path, _| used.contains(path));
        if let Some(leases) = session_leases(agents.iter().filter(|a| a.parent.is_none()).map(|a| a.id.as_str())) {
            for a in agents.iter_mut().filter(|a| a.parent.is_none()) {
                a.details.lease = leases.get(&a.id);
                if a.details.lease == Lease::Free && a.state == State::Working {
                    // A dead writer can leave a tool or response in progress in the journal.
                    a.state = State::Idle;
                    a.since_ms = a.details.wrote_ms;
                    a.detail = tr!("interrupted").into();
                    a.action.clear();
                }
            }
        }
        Ok(agents)
    }
}

#[derive(Deserialize)]
pub(crate) struct SessionLeases {
    held: Vec<String>,
    unknown: Vec<String>,
}

impl SessionLeases {
    pub(crate) fn get(&self, id: &str) -> Lease {
        if self.held.iter().any(|s| s == id) {
            Lease::Held
        } else if !valid_session_id(id) || self.unknown.iter().any(|s| s == id) {
            Lease::Unknown
        } else {
            Lease::Free
        }
    }
}

pub(crate) fn session_leases<'a>(ids: impl Iterator<Item = &'a str>) -> Option<SessionLeases> {
    let mut args = vec!["session", "lease", "--json", "--"];
    args.extend(ids.filter(|id| valid_session_id(id)));
    if args.len() == 4 {
        return None;
    }
    let output = crate::core::exec::run_timeout("omp", &args, Duration::from_millis(750)).ok()?;
    serde_json::from_str(&output).ok()
}

fn valid_session_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Only the watched subagent's sibling artifacts are read, never paths supplied by tool
/// output. Preview Markdown and diffs as text; do not render links or apply patches.
#[derive(Default)]
pub(super) struct Artifacts {
    journal: PathBuf,
    result: ArtifactCache,
    patch: ArtifactCache,
}

type ArtifactCache = Option<(Stamp, Artifact)>;
const ARTIFACT_LIMIT: usize = 64 * 1024;

impl Artifacts {
    pub(super) fn read(&mut self, journal: Option<&Path>) -> (Option<Artifact>, Option<Artifact>) {
        let Some(journal) = journal else { return (None, None) };
        if self.journal != journal {
            self.journal = journal.into();
            self.result = None;
            self.patch = None;
        }
        (
            artifact(&journal.with_extension("md"), &mut self.result),
            artifact(&journal.with_extension("patch"), &mut self.patch),
        )
    }
}

fn artifact(path: &Path, cache: &mut ArtifactCache) -> Option<Artifact> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() && meta.len() != 0 => meta,
        _ => {
            *cache = None;
            return None;
        }
    };
    let stamp = (meta.len(), meta.modified().ok()?);
    if let Some((previous, preview)) = cache
        && *previous == stamp
    {
        return Some(preview.clone());
    }
    let mut bytes = Vec::with_capacity(meta.len().min(ARTIFACT_LIMIT as u64 + 1) as usize);
    File::open(path).ok()?.take(ARTIFACT_LIMIT as u64 + 1).read_to_end(&mut bytes).ok()?;
    let truncated = bytes.len() > ARTIFACT_LIMIT;
    bytes.truncate(ARTIFACT_LIMIT);
    // Do not replace a UTF-8 character cut in half by the preview limit.
    if truncated && let Err(e) = std::str::from_utf8(&bytes) && e.error_len().is_none() {
        bytes.truncate(e.valid_up_to());
    }
    let preview = Artifact { text: String::from_utf8_lossy(&bytes).into(), truncated };
    *cache = Some((stamp, preview.clone()));
    Some(preview)
}

struct Candidate {
    path: PathBuf,
    stamp: Stamp,
    written: i64,
    recent: i64,
    children: Vec<Candidate>,
}

fn candidates(dir: &Path, cutoff: i64, depth: u32) -> Vec<Candidate> {
    let Ok(entries) = fs::read_dir(dir) else { return Vec::new() };
    entries.flatten().filter_map(|entry| {
        if !entry.file_name().as_encoded_bytes().ends_with(b".jsonl") || !entry.file_type().ok()?.is_file() {
            return None;
        }
        let meta = entry.metadata().ok()?;
        let stamp = (meta.len(), meta.modified().ok()?);
        let written = mtime_ms(&meta);
        let path = entry.path();
        let mut children = Vec::new();
        if depth < MAX_DEPTH {
            // A journal's artifact directory is its sibling without .jsonl. Each
            // child journal has its own artifact directory for the next level.
            let artifacts = path.with_extension("");
            if fs::symlink_metadata(&artifacts).is_ok_and(|meta| meta.is_dir()) {
                children = candidates(&artifacts, cutoff, depth + 1);
                sort_recent(&mut children);
            }
        }
        let recent = children.first().map_or(written, |child| written.max(child.recent));
        (recent >= cutoff).then_some(Candidate { path, stamp, written, recent, children })
    }).collect()
}

fn sort_recent(files: &mut [Candidate]) {
    files.sort_unstable_by(|a, b| b.recent.cmp(&a.recent).then_with(|| a.path.cmp(&b.path)));
}

struct Tree<'a> {
    now: i64,
    agents: &'a mut Vec<Agent>,
    ids: &'a mut HashSet<String>,
    used: &'a mut HashSet<PathBuf>,
    left: &'a mut usize,
    children_left: &'a mut usize,
}

impl Omp {
    fn collect_tree(&mut self, file: &Candidate, parent: Option<&str>, depth: u32, tree: &mut Tree<'_>) -> bool {
        if *tree.left == 0 || (depth != 0 && *tree.children_left == 0) {
            return false;
        }
        let cache = self.sessions.entry(file.path.clone()).or_insert((file.stamp, None));
        if cache.1.is_none() || cache.0 != file.stamp {
            cache.1 = Journal::read(&file.path, file.stamp.0, cache.1.take(), cache.0.0).ok();
            cache.0 = file.stamp;
        }
        tree.used.insert(file.path.clone());
        let Some(journal) = &cache.1 else { return false };
        if parent.is_some()
            && !journal.header.parent
            && journal.session.kind.is_empty()
        {
            return false;
        }
        let Some(agent) = journal.agent(file, parent, depth, tree.now) else { return false };
        if !tree.ids.insert(agent.id.clone()) {
            return false;
        }
        *tree.left -= 1;
        if depth != 0 {
            *tree.children_left -= 1;
        }
        let id = (*tree.left != 0 && !file.children.is_empty()).then(|| agent.id.clone());
        tree.agents.push(agent);
        if let Some(id) = id {
            for child in &file.children {
                if *tree.left == 0 || *tree.children_left == 0 {
                    break;
                }
                self.collect_tree(child, Some(&id), depth + 1, tree);
            }
        }
        true
    }
}

struct Journal {
    identity: Handle,
    offset: u64,
    header: Header,
    session: Session,
}

impl Journal {
    fn read(path: &Path, len: u64, previous: Option<Self>, previous_len: u64) -> io::Result<Self> {
        let identity = Handle::from_file(File::open(path)?)?;
        let mut reader = BufReader::new(identity.as_file());
        let (header, first_entry) = Header::read(&mut reader)?;
        // OMP appends to the same file but rewrites by atomic replacement.
        let (offset, mut session) = match previous {
            Some(old) if old.identity == identity && len > previous_len && old.header.id == header.id && old.offset >= first_entry => {
                (old.offset, old.session)
            }
            _ => (first_entry, Session { title: header.title.clone(), ..Session::default() }),
        };
        if offset != first_entry {
            reader.seek(SeekFrom::Start(offset))?;
        }
        let offset = session.read(reader.take(len.saturating_sub(offset)), offset)?;
        Ok(Self { identity, offset, header, session })
    }

    fn agent(&self, file: &Candidate, parent: Option<&str>, depth: u32, now: i64) -> Option<Agent> {
        let Session { title, prompt, model, effort, running, turn_at, action, detail, stop_reason, tokens, last_turn, kind, .. } =
            &self.session;
        let title = if self.header.slot || title.is_empty() { &self.header.title } else { title };
        if title.is_empty() && *turn_at == 0 && kind.is_empty() && file.children.is_empty() {
            return None;
        }
        let title = if parent.is_some() {
            // Task names are the artifact stems. Nested names are dot-qualified.
            let stem = file.path.file_stem()?.to_string_lossy();
            let parent_stem = file.path.parent()?.file_name()?.to_string_lossy();
            let name = stem.strip_prefix(parent_stem.as_ref()).and_then(|s| s.strip_prefix('.')).unwrap_or(&stem);
            clip(name, 120)
        } else if title.is_empty() {
            base_name(&self.header.cwd)
        } else {
            title.clone()
        };
        let expired = *running && now - file.written >= STALE.as_millis() as i64;
        let active = *running && !expired;
        Some(Agent {
            tool: Tool::Omp,
            id: self.header.id.clone(),
            title,
            cwd: self.header.cwd.clone(),
            state: if active { State::Working } else { State::Idle },
            since_ms: if expired || *turn_at == 0 { file.written } else { *turn_at },
            detail: if expired { tr!("interrupted") } else { detail }.into(),
            action: if active { action.clone() } else { String::new() },
            parent: parent.map(str::to_owned),
            log: Some(file.path.clone()),
            details: Details {
                last_prompt: prompt.clone(),
                model: model.clone(),
                effort: effort.clone(),
                stop_reason: (*stop_reason).into(),
                tokens: *tokens,
                last_turn: *last_turn,
                kind: kind.clone(),
                depth: parent.map(|_| depth),
                wrote_ms: file.written,
                ..Details::default()
            },
        })
    }
}

struct Header {
    id: String,
    cwd: String,
    title: String,
    slot: bool,
    parent: bool,
}

impl Header {
    fn read(reader: &mut impl BufRead) -> io::Result<(Self, u64)> {
        let mut line = Vec::new();
        let invalid = || io::Error::from(io::ErrorKind::InvalidData);
        let mut line_value = || {
            line.clear();
            let len = reader.take(64 * 1024).read_until(b'\n', &mut line)?;
            let value: Value = serde_json::from_slice(&line).map_err(|_| invalid())?;
            Ok::<_, io::Error>((value, len as u64))
        };
        let (mut header, mut offset) = line_value()?;
        let title = if header["type"] == "title" {
            let title = clip(text(&header["title"]), 120);
            let (next, len) = line_value()?;
            header = next;
            offset += len;
            title
        } else {
            String::new()
        };
        let field = |k| header[k].as_str().filter(|s| !s.is_empty()).ok_or_else(invalid);
        if header["type"] != "session" {
            return Err(invalid());
        }
        Ok((Self {
            id: field("id")?.into(),
            cwd: field("cwd")?.into(),
            slot: !title.is_empty(),
            title: if title.is_empty() { clip(text(&header["title"]), 120) } else { title },
            parent: field("parentSession").is_ok(),
        }, offset))
    }
}

fn records(text: &str) -> impl Iterator<Item = Value> + '_ {
    text.lines().filter_map(|line| serde_json::from_str(line).ok())
}

fn text(v: &Value) -> &str {
    v.as_str().unwrap_or_default()
}

fn text_parts(content: &Value) -> impl Iterator<Item = &str> {
    content.as_str().into_iter().chain(
        content.as_array().into_iter().flatten().filter(|p| p["type"] == "text").filter_map(|p| p["text"].as_str()),
    )
}

fn calls(message: &Value) -> impl DoubleEndedIterator<Item = (&str, &Value)> {
    message["content"].as_array().into_iter().flatten().filter_map(|p| {
        (p["type"] == "toolCall").then_some(())?;
        Some((p["name"].as_str()?, &p["arguments"]))
    })
}

fn prompt(message: &Value) -> String {
    text_parts(&message["content"]).find(|s| !s.trim().is_empty()).map_or_else(String::new, |s| clip(s, 400))
}

#[derive(Default)]
struct Session {
    title: String,
    prompt: String,
    model: String,
    effort: String,
    running: bool,
    turn_at: i64,
    action: String,
    detail: &'static str,
    stop_reason: &'static str,
    tokens: Option<Tokens>,
    last_turn: Option<(i64, u32)>,
    /// Start and message count are only known after a recorded user prompt.
    turn: Option<(i64, u32)>,
    kind: String,
}

impl Session {
    fn read(&mut self, mut reader: impl BufRead, mut offset: u64) -> io::Result<u64> {
        let mut line = Vec::new();
        loop {
            line.clear();
            let n = reader.read_until(b'\n', &mut line)?;
            if line.last() != Some(&b'\n') {
                return Ok(offset);
            }
            if let Ok(entry) = serde_json::from_slice(&line) {
                self.observe(&entry);
            }
            offset += n as u64;
        }
    }

    fn add_usage(&mut self, usage: &Value) {
        let fields = ["input", "output", "cacheRead", "cacheWrite"].map(|key| usage[key].as_u64());
        if fields.iter().all(Option::is_none) {
            return;
        }
        let [input, output, cache_read, cache_write] = fields.map(Option::unwrap_or_default);
        let total = self.tokens.get_or_insert_with(Tokens::default);
        total.input = total.input.saturating_add(input);
        total.output = total.output.saturating_add(output);
        total.cache_read = total.cache_read.saturating_add(cache_read);
        total.cache_write = total.cache_write.saturating_add(cache_write);
    }

    fn count_message(&mut self) {
        if let Some((_, count)) = &mut self.turn {
            *count = count.saturating_add(1);
        }
    }

    fn finish(&mut self, at: i64) {
        if let Some((start, count)) = self.turn.take().filter(|(start, _)| at >= *start) {
            self.last_turn = Some((at - start, count));
        }
        self.running = false;
        self.turn_at = at;
    }

    fn observe(&mut self, entry: &Value) {
        let at = || iso_ms(&entry["timestamp"]);
        match text(&entry["type"]) {
            "title_change" => {
                let title = clip(text(&entry["title"]), 120);
                if !title.is_empty() {
                    self.title = title;
                }
            }
            "model_change" if matches!(text(&entry["role"]), "" | "default") => text(&entry["model"]).clone_into(&mut self.model),
            "thinking_level_change" => text(&entry["thinkingLevel"]).clone_into(&mut self.effort),
            "model_usage" => self.add_usage(&entry["usage"]),
            "session_init" => {
                if let Some(agent) = entry["agent"].as_str() {
                    agent.clone_into(&mut self.kind);
                }
                if self.model.is_empty() {
                    text(&entry["resolvedModel"]).clone_into(&mut self.model);
                }
            }
            "branch_summary" => self.turn = None,
            "reset_boundary" => {
                self.running = false;
                self.turn = None;
                self.turn_at = at();
                self.action.clear();
                self.prompt.clear();
                self.detail = "";
                self.stop_reason = "";
            }
            "custom" if entry["customType"] == "session_exit" => {
                if self.running {
                    self.running = false;
                    self.turn = None;
                    self.turn_at = at();
                    self.detail = tr!("interrupted");
                }
            }
            "custom" if entry["customType"] == "tool_execution_start" => {
                self.start(at());
                if let Some(name) = entry["data"]["toolName"].as_str() {
                    self.action = describe(name, &entry["data"]["args"]);
                }
            }
            "message" => {
                let msg = &entry["message"];
                match text(&msg["role"]) {
                    "user" => {
                        self.prompt = prompt(msg);
                        if self.title.is_empty() {
                            self.title = clip(&self.prompt, 120);
                        }
                        self.running = false;
                        let at = at();
                        self.turn = (at > 0).then_some((at, 1));
                        self.start(at);
                        self.action.clear();
                    }
                    "toolResult" => {
                        self.count_message();
                        self.start(at());
                    }
                    "assistant" => {
                        self.add_usage(&msg["usage"]);
                        self.count_message();
                        if let Some(model) = msg["model"].as_str().filter(|s| !s.is_empty()) {
                            model.clone_into(&mut self.model);
                        }
                        let stop = text(&msg["stopReason"]);
                        if let Some((name, args)) = calls(msg).next_back().filter(|_| matches!(stop, "toolUse" | "")) {
                            self.start(at());
                            self.action = describe(name, args);
                        } else if stop == "toolUse" {
                            self.start(at());
                        } else {
                            self.finish(at());
                            self.detail = if matches!(stop, "aborted" | "error") { tr!("interrupted") } else { "" };
                            self.stop_reason = match stop {
                                "stop" => "end_turn",
                                "length" => "max_tokens",
                                "aborted" => "aborted",
                                "error" => "error",
                                _ => "",
                            };
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn start(&mut self, at: i64) {
        if !self.running {
            self.running = true;
            self.turn_at = at;
            self.detail = "";
            self.stop_reason = "";
        }
    }
}

pub(super) fn timeline(journal: &str) -> Vec<Step> {
    let mut steps = Vec::new();
    for entry in records(journal).filter(|e| e["type"] == "message") {
        let at = iso_ms(&entry["timestamp"]);
        let msg = &entry["message"];
        match msg["role"].as_str() {
            Some("user") => {
                for part in text_parts(&msg["content"]) {
                    push_step(&mut steps, at, StepKind::User, part);
                }
            }
            Some("assistant") => {
                if let Some(parts) = msg["content"].as_array() {
                    for part in parts {
                        match part["type"].as_str() {
                            Some("text") => push_step(&mut steps, at, StepKind::Reply, text(&part["text"])),
                            Some("toolCall") => {
                                if let Some(name) = part["name"].as_str() {
                                    push_step(&mut steps, at, StepKind::Tool, &describe(name, &part["arguments"]));
                                }
                            }
                            _ => {}
                        }
                    }
                }
                if msg["stopReason"] == "aborted" || msg["stopReason"] == "error" {
                    push_step(&mut steps, at, StepKind::Error, tr!("interrupted"));
                }
            }
            Some("toolResult") if msg["isError"] == true => push_step(&mut steps, at, StepKind::Error, tr!("the tool failed")),
            _ => {}
        }
    }
    last_steps(steps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "miniutils-omp-{}-{}", std::process::id(), SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_nanos()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn write(&self, path: &str, text: &str) -> PathBuf {
            let path = self.0.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, text.trim_start()).unwrap();
            path
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn candidate(path: &Path) -> Candidate {
        let meta = fs::metadata(path).unwrap();
        let written = mtime_ms(&meta);
        Candidate { path: path.into(), stamp: (meta.len(), meta.modified().unwrap()), written, recent: written, children: vec![] }
    }

    const JOURNAL: &str = r#"
{"type":"title","title":"Current name","pad":"     "}
{"type":"session","id":"omp-id","cwd":"/work/project","title":"Old name"}
{"type":"title_change","title":"Previous name"}
{"type":"thinking_level_change","thinkingLevel":"medium"}
{"type":"message","timestamp":"2026-11-21T12:00:00Z","message":{"role":"user","content":[{"type":"image"},{"type":"text","text":"Update the docs"}]}}
{"type":"message","timestamp":"2026-11-21T12:00:01Z","message":{"role":"assistant","model":"my-model","stopReason":"toolUse","usage":{"input":10,"output":20,"cacheRead":30,"cacheWrite":40},"content":[{"type":"toolCall","name":"bash","arguments":{"command":"cargo check --token=secret"}}]}}
{"type":"message","timestamp":"2026-11-21T12:00:02Z","message":{"role":"toolResult","isError":true,"content":[{"type":"text","text":"private output"}]}}
"#;

    #[test]
    fn reads_title_slot_and_tracks_a_turn_past_its_tool_result() {
        let fixture = Fixture::new();
        let path = fixture.write("session.jsonl", JOURNAL);
        let file = candidate(&path);
        let journal = Journal::read(&path, file.stamp.0, None, 0).unwrap();
        let running = journal.agent(&file, None, 0, file.written).unwrap();
        assert_eq!((running.id.as_str(), running.cwd.as_str(), running.title.as_str()), ("omp-id", "/work/project", "Current name"));
        assert_eq!((running.state, running.since_ms), (State::Working, 1_795_262_400_000));
        assert_eq!(running.action, "bash: cargo check");
        assert_eq!((running.details.model.as_str(), running.details.effort.as_str()), ("my-model", "medium"));

        let mut writer = fs::OpenOptions::new().append(true).open(&path).unwrap();
        let reply = r#"{"type":"message","timestamp":"2026-11-21T12:00:03Z","message":{"role":"assistant","stopReason":"stop","usage":{"input":3,"output":5,"cacheRead":7}}}"#;
        writer.write_all(&reply.as_bytes()[..40]).unwrap();
        let partial = candidate(&path);
        let journal = Journal::read(&path, partial.stamp.0, Some(journal), file.stamp.0).unwrap();
        assert!(journal.agent(&partial, None, 0, partial.written).unwrap().details.last_turn.is_none());
        writeln!(writer, "{}", &reply[40..]).unwrap();
        writeln!(writer, r#"{{"type":"model_usage","usage":{{"input":2,"output":11,"cacheWrite":17}}}}"#).unwrap();
        writeln!(writer, r#"{{"type":"custom","timestamp":"2026-11-21T12:00:04Z","customType":"session_exit","data":{{"kind":"normal"}}}}"#).unwrap();
        drop(writer);
        let complete = candidate(&path);
        let journal = Journal::read(&path, complete.stamp.0, Some(journal), partial.stamp.0).unwrap();
        let ended = journal.agent(&complete, None, 0, complete.written).unwrap();
        assert_eq!((ended.state, ended.since_ms), (State::Idle, running.since_ms + 3000));
        assert!(ended.detail.is_empty() && ended.action.is_empty());
        assert_eq!(ended.details.last_turn, Some((3000, 4)));
        assert_eq!(ended.details.tokens, Some(Tokens { input: 15, output: 36, cache_read: 37, cache_write: 57 }));

        // Replacing the journal with a longer file is not an append.
        let replacement = fixture.write("new.jsonl", &format!("{}{}\n", JOURNAL.replace(r#""output":20"#, r#""output":60"#), " ".repeat(600)));
        fs::rename(replacement, &path).unwrap();
        let rewritten = candidate(&path);
        let journal = Journal::read(&path, rewritten.stamp.0, Some(journal), complete.stamp.0).unwrap();
        let replaced = journal.agent(&rewritten, None, 0, rewritten.written).unwrap();
        assert_eq!(replaced.details.tokens.unwrap().output, 60);
        assert_eq!(replaced.details.last_turn, None);
    }

    #[test]
    fn legacy_header_and_exit_without_a_final_reply() {
        let journal = r#"
{"type":"session","id":"legacy","cwd":"/work/project"}
{"type":"message","timestamp":"2026-11-21T12:00:00Z","message":{"role":"user","content":"Find the issue"}}
{"type":"custom","timestamp":"2026-11-21T12:00:01Z","customType":"session_exit","data":{"kind":"signal"}}
{"type":"message",
"#;
        let fixture = Fixture::new();
        let path = fixture.write("session.jsonl", journal);
        let file = candidate(&path);
        let parsed = Journal::read(&path, file.stamp.0, None, 0).unwrap();
        let session = parsed.agent(&file, None, 0, file.written).unwrap();
        assert_eq!(session.title, "Find the issue");
        assert_eq!(session.state, State::Idle);
        assert_eq!(session.detail, tr!("interrupted"));
    }

    #[test]
    fn follows_nested_artifacts_from_a_quiet_root_but_ignores_sidecars_and_forks() {
        let fixture = Fixture::new();
        let root = fixture.write("sessions/project/main.jsonl", JOURNAL);
        let child = |id: &str, parent: &str| format!(
            "{{\"type\":\"session\",\"id\":\"{id}\",\"cwd\":\"/work/project\",\"parentSession\":\"{parent}\"}}\n\
             {{\"type\":\"session_init\",\"agent\":\"scout\",\"resolvedModel\":\"child-model\"}}\n"
        );
        let former = fixture.write("sessions/project/main/Review.jsonl", &child("review", "omp-id"));
        let leaf = fixture.write("sessions/project/main/Review/Review.Lint.jsonl", &child("lint", "review"));
        fixture.write("sessions/project/main/changes.jsonl", "{\"type\":\"change\"}\n");
        fixture.write("sessions/project/fork.jsonl", &format!("{}{}", child("fork", "omp-id"), "{\"type\":\"title_change\",\"title\":\"Fork\"}\n"));
        let old = fs::FileTimes::new().set_modified(SystemTime::UNIX_EPOCH);
        for path in [&root, &former] {
            File::options().write(true).open(path).unwrap().set_times(old).unwrap();
        }
        let agents = Omp::default().collect(&fixture.0, candidate(&leaf).written).unwrap();
        let find = |id: &str| agents.iter().find(|a| a.id == id).unwrap();
        assert_eq!(agents.len(), 4);
        assert_eq!(find("review").parent.as_deref(), Some("omp-id"));
        assert_eq!((find("lint").parent.as_deref(), find("lint").details.depth), (Some("review"), Some(2)));
        assert_eq!(find("lint").title, "Lint");
        assert_eq!(find("lint").details.model, "child-model");
        assert!(find("fork").parent.is_none());
    }

    #[test]
    fn reads_only_the_watched_subagent_artifacts_and_bounds_the_patch() {
        let fixture = Fixture::new();
        let log = fixture.write("Review.jsonl", JOURNAL);
        fixture.write("Review.md", "Summary from Review");
        let patch = fixture.write("Review.patch", &format!("{}é\n", "x".repeat(ARTIFACT_LIMIT - 1)));
        let mut artifacts = Artifacts::default();
        let (result, diff) = artifacts.read(Some(&log));
        assert_eq!(result.unwrap().text.as_ref(), "Summary from Review");
        let diff = diff.unwrap();
        assert!(diff.truncated);
        assert_eq!(diff.text.len(), ARTIFACT_LIMIT - 1);
        assert!(diff.text.ends_with('x'));
        fs::remove_file(patch).unwrap();
        assert!(artifacts.read(Some(&log)).1.is_none());
        assert_eq!(artifacts.read(Some(&fixture.0.join("Other.jsonl"))), (None, None));
    }

    #[test]
    fn lease_probe_distinguishes_held_unknown_and_free_ids() {
        let leases: SessionLeases = serde_json::from_str(r#"{"held":["active-id"],"unknown":["uncertain-id"]}"#).unwrap();
        assert_eq!(leases.get("active-id"), Lease::Held);
        assert_eq!(leases.get("uncertain-id"), Lease::Unknown);
        assert_eq!(leases.get("closed-id"), Lease::Free);
        assert_eq!(leases.get("cannot pass to CLI"), Lease::Unknown);
    }

    #[test]
    fn timeline_omits_raw_tool_inputs_and_results() {
        let steps = timeline(JOURNAL);
        let visible: Vec<_> = steps.iter().map(|s| (s.kind, s.text.as_str())).collect();
        assert_eq!(visible, [
            (StepKind::User, "Update the docs"),
            (StepKind::Tool, "bash: cargo check"),
            (StepKind::Error, tr!("the tool failed")),
        ]);
    }
}
