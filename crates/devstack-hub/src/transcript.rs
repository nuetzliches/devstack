//! A coding agent's session in time (the lane's Activity, view Timeline):
//! what devstack recorded for it, and, once switched on, what the agent's
//! own transcript tells: the model's turns with their tokens, every tool
//! call with how long it took, when a prompt came.
//!
//! - **Only times, counts and names.** A reader takes from the transcript
//!   the moments, the model's name, the token counts and the tools' names;
//!   never a prompt, an answer, a tool's arguments or its output. Nothing
//!   of it is kept: the hub reads the file when the page asks, on this
//!   machine. The path comes from the agent's hooks (`transcript_path`).
//! - **Off until switched on**, per machine (`transcripts.json` in the
//!   hub's state). Off, the timeline holds devstack's records only.
//! - **A reader per agent.** Every agent keeps its own format: Claude Code
//!   a JSONL of messages with their `usage`, Codex a rollout of events with
//!   `token_count`. An agent without a reader shows devstack's records.

use crate::activity::{Prefixes, Record};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::io::BufRead as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The setting's file in the hub's state.
const SETTING: &str = "transcripts.json";

/// Whether the hub reads the agents' transcripts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Setting {
    pub enabled: bool,
}

impl Setting {
    pub fn load(state: &Path) -> Self {
        std::fs::read_to_string(state.join(SETTING))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(self, state: &Path) -> Result<(), String> {
        std::fs::create_dir_all(state).map_err(|e| e.to_string())?;
        let path = state.join(SETTING);
        std::fs::write(&path, serde_json::to_string(&self).expect("serialises"))
            .map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// An answer of the model: when, which model, the tokens it took.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Turn {
    pub at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Input not from the cache.
    pub input: u64,
    pub cache_write: u64,
    pub cache_read: u64,
    pub output: u64,
    /// A sub-agent's.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub sidechain: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// One of devstack's own tools.
    Devstack,
    /// A pool upstream's, through `devstack mcp`.
    Upstream,
    /// Another MCP server's.
    Mcp,
    /// The agent's own tool (shell, editing, reading).
    Builtin,
    /// A sub-agent.
    Agent,
}

/// A tool call: its name and how long it took.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Call {
    pub at: u64,
    /// None: no answer (yet).
    pub ms: Option<u64>,
    pub tool: String,
    pub kind: Kind,
    pub ok: bool,
    /// `Upstream`: which; `Mcp`: the server.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MarkKind {
    Prompt,
    Interrupt,
    Compact,
    Clear,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Mark {
    pub at: u64,
    pub kind: MarkKind,
}

/// What a reader took from a transcript.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Parsed {
    pub turns: Vec<Turn>,
    pub calls: Vec<Call>,
    pub marks: Vec<Mark>,
}

/// The format of an agent's transcripts, by the agent's id; None: devstack
/// cannot read this agent's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Claude,
    Codex,
}

fn format(agent: &str) -> Option<Format> {
    match agent {
        "claude-code" | "claude" => Some(Format::Claude),
        "codex" => Some(Format::Codex),
        _ => None,
    }
}

/// A reader's state after the lines it was fed.
#[derive(Debug, Clone)]
enum State {
    Claude(Claude),
    Codex(Codex),
}

impl State {
    fn line(&mut self, l: &Value) {
        match self {
            State::Claude(c) => c.line(l),
            State::Codex(c) => c.line(l),
        }
    }

    fn parsed(&self) -> Parsed {
        match self {
            State::Claude(c) => c.out.clone(),
            State::Codex(c) => c.parsed(),
        }
    }
}

/// A file followed: how far it was read, and the reader's state there. A
/// transcript only grows while its session runs, so it is read once and
/// then only what was added.
#[derive(Debug, Clone)]
struct Followed {
    format: Format,
    sidechain: bool,
    offset: u64,
    state: State,
}

impl Followed {
    fn new(format: Format, sidechain: bool) -> Self {
        let state = match format {
            Format::Claude => State::Claude(Claude {
                sidechain,
                ..Claude::default()
            }),
            Format::Codex => State::Codex(Codex::default()),
        };
        Followed {
            format,
            sidechain,
            offset: 0,
            state,
        }
    }

    /// Read what was added since the last time: whole lines only (one being
    /// written is read the next time). A file shorter than what was read
    /// was written anew: from its start.
    fn read(&mut self, path: &Path) -> std::io::Result<()> {
        use std::io::{Seek as _, SeekFrom};
        let mut file = std::fs::File::open(path)?;
        if file.metadata()?.len() < self.offset {
            *self = Followed::new(self.format, self.sidechain);
        }
        file.seek(SeekFrom::Start(self.offset))?;
        let mut r = std::io::BufReader::new(file);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            let n = r.read_until(b'\n', &mut buf)?;
            if n == 0 || buf.last() != Some(&b'\n') {
                return Ok(());
            }
            self.offset += n as u64;
            // A line that is not JSON is skipped.
            if let Ok(v) = serde_json::from_slice::<Value>(&buf) {
                self.state.line(&v);
            }
        }
    }
}

/// A session's transcript at `path` read on from where `files` left it; a
/// Claude Code session with its sub-agents' files beside it
/// (`<session>/subagents/*.jsonl`). None: the transcript cannot be read.
fn session(format: Format, path: &Path, files: &mut HashMap<PathBuf, Followed>) -> Option<Parsed> {
    let main = files
        .entry(path.to_path_buf())
        .or_insert_with(|| Followed::new(format, false));
    main.read(path).ok()?;
    let mut out = main.state.parsed();
    if format == Format::Claude
        && let Ok(rd) = std::fs::read_dir(path.with_extension("").join("subagents"))
    {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().is_none_or(|x| x != "jsonl") {
                continue;
            }
            let f = files
                .entry(p.clone())
                .or_insert_with(|| Followed::new(format, true));
            if f.read(&p).is_ok() {
                out.turns.extend(f.state.parsed().turns);
            }
        }
        out.turns.sort_by_key(|t| t.at);
    }
    Some(out)
}

/// An ISO 8601 time as Unix ms.
fn ms(v: &Value) -> Option<u64> {
    let t: jiff::Timestamp = v.as_str()?.parse().ok()?;
    u64::try_from(t.as_millisecond()).ok()
}

fn num(v: &Value) -> u64 {
    v.as_u64().unwrap_or(0)
}

/// A tool's name as the agent calls it: an MCP server's `mcp__<server>__<tool>`.
fn split_mcp(name: &str) -> Option<(&str, &str)> {
    name.strip_prefix("mcp__")?.split_once("__")
}

/// Claude Code: a JSONL of messages. An answer of the model comes as one
/// line per part with the same `message.id` and its `usage`; a tool call is
/// a `tool_use` part, its answer a `tool_result` in a later user line; a
/// prompt is a user line with text; a compaction a `compact_boundary`. A
/// sub-agent's turns are marked `isSidechain`, or kept beside the transcript
/// (`<session>/subagents/*.jsonl`). Fed line by line.
#[derive(Debug, Clone, Default)]
struct Claude {
    out: Parsed,
    /// A message's turn, by its id: the first line's time, the last line's usage.
    turns: HashMap<String, usize>,
    /// A call waiting for its answer, by the tool_use id.
    open: HashMap<String, usize>,
    /// A sub-agent's file: every turn is its.
    sidechain: bool,
}

impl Claude {
    fn line(&mut self, l: &Value) {
        let Some(at) = ms(&l["timestamp"]) else {
            if l["type"] == "system" && l["subtype"] == "compact_boundary" {
                // Without a time of its own: right after the last thing seen.
                let at = self.out.turns.last().map_or(0, |t| t.at + 1);
                self.out.marks.push(Mark {
                    at,
                    kind: MarkKind::Compact,
                });
            }
            return;
        };
        let side = self.sidechain || l["isSidechain"] == true;
        match l["type"].as_str() {
            Some("assistant") => {
                let m = &l["message"];
                if l["isApiErrorMessage"] == true {
                    self.out.marks.push(Mark {
                        at,
                        kind: MarkKind::Error,
                    });
                    return;
                }
                let u = &m["usage"];
                let turn = Turn {
                    at,
                    model: m["model"]
                        .as_str()
                        .filter(|s| !s.starts_with('<'))
                        .map(String::from),
                    input: num(&u["input_tokens"]),
                    cache_write: num(&u["cache_creation_input_tokens"]),
                    cache_read: num(&u["cache_read_input_tokens"]),
                    output: num(&u["output_tokens"]),
                    sidechain: side,
                };
                let id = m["id"].as_str().unwrap_or_default().to_string();
                match self.turns.get(&id).filter(|_| !id.is_empty()) {
                    Some(&i) => {
                        let t = &mut self.out.turns[i];
                        t.input = t.input.max(turn.input);
                        t.cache_write = t.cache_write.max(turn.cache_write);
                        t.cache_read = t.cache_read.max(turn.cache_read);
                        t.output = t.output.max(turn.output);
                    }
                    None => {
                        self.turns.insert(id, self.out.turns.len());
                        self.out.turns.push(turn);
                    }
                }
                if side {
                    return;
                }
                for c in m["content"].as_array().into_iter().flatten() {
                    if c["type"] != "tool_use" {
                        return;
                    }
                    let name = c["name"].as_str().unwrap_or("?");
                    let (tool, kind, upstream) = match split_mcp(name) {
                        Some((server, tool)) => {
                            (tool.to_string(), Kind::Mcp, Some(server.to_string()))
                        }
                        None if name == "Task" || name == "Agent" => {
                            (name.to_string(), Kind::Agent, None)
                        }
                        None => (name.to_string(), Kind::Builtin, None),
                    };
                    if let Some(id) = c["id"].as_str() {
                        self.open.insert(id.to_string(), self.out.calls.len());
                    }
                    self.out.calls.push(Call {
                        at,
                        ms: None,
                        tool,
                        kind,
                        ok: true,
                        upstream,
                    });
                }
            }
            Some("user") if !side => {
                let content = &l["message"]["content"];
                let mut text = content.as_str().map(str::to_string);
                for c in content.as_array().into_iter().flatten() {
                    match c["type"].as_str() {
                        Some("tool_result") => {
                            if let Some(&i) =
                                c["tool_use_id"].as_str().and_then(|id| self.open.get(id))
                            {
                                let call = &mut self.out.calls[i];
                                call.ms = Some(at.saturating_sub(call.at));
                                call.ok = c["is_error"] != true;
                            }
                        }
                        Some("text") if text.is_none() => {
                            text = c["text"].as_str().map(str::to_string);
                        }
                        _ => {}
                    }
                }
                // Read only to tell what the line is, never kept.
                let Some(text) = text else { return };
                // Not the user's: text the agent added, a summary, a
                // background task's notice (`origin`).
                if l["isMeta"] == true || l["isCompactSummary"] == true || l["origin"].is_object() {
                    return;
                }
                let t = text.trim_start();
                if t.starts_with("[Request interrupted") {
                    self.out.marks.push(Mark {
                        at,
                        kind: MarkKind::Interrupt,
                    });
                } else if t.starts_with("<command-name>/clear") {
                    self.out.marks.push(Mark {
                        at,
                        kind: MarkKind::Clear,
                    });
                } else if !(t.starts_with("<local-command")
                    || t.starts_with("<command-")
                    || t.starts_with("<task-notification")
                    || t.starts_with("Caveat:"))
                {
                    self.out.marks.push(Mark {
                        at,
                        kind: MarkKind::Prompt,
                    });
                }
            }
            Some("system") if l["subtype"] == "compact_boundary" => self.out.marks.push(Mark {
                at,
                kind: MarkKind::Compact,
            }),
            _ => {}
        }
    }
}

/// Codex: a rollout of events. `token_count` carries the last turn's usage
/// (`last_token_usage`; given twice, the second time with the same total),
/// `user_message` a prompt (an older rollout has none: `task_started`
/// instead), `turn_aborted` an interruption; a call is a `function_call` (or `custom_tool_call`) with
/// its output by `call_id`, an MCP server's tool an `mcp_tool_call_end` with
/// its server and duration; `turn_context` names the model. Fed line by
/// line.
#[derive(Debug, Clone, Default)]
struct Codex {
    out: Parsed,
    model: Option<String>,
    open: HashMap<String, usize>,
    mcp_ids: Vec<String>,
    /// The total of the last `token_count`: the same again is no new turn.
    total: Option<u64>,
    /// `task_started`: the prompts of a rollout without `user_message`.
    tasks: Vec<Mark>,
}

impl Codex {
    fn line(&mut self, l: &Value) {
        let Some(at) = ms(&l["timestamp"]) else {
            return;
        };
        let p = &l["payload"];
        match (l["type"].as_str(), p["type"].as_str()) {
            (Some("turn_context"), _) => {
                self.model = p["model"].as_str().map(String::from).or(self.model.take());
            }
            (Some("event_msg"), Some("user_message")) => self.out.marks.push(Mark {
                at,
                kind: MarkKind::Prompt,
            }),
            (Some("event_msg"), Some("task_started")) => self.tasks.push(Mark {
                at,
                kind: MarkKind::Prompt,
            }),
            (Some("event_msg"), Some("turn_aborted")) => self.out.marks.push(Mark {
                at,
                kind: MarkKind::Interrupt,
            }),
            (Some("event_msg"), Some("context_compacted")) | (Some("compacted"), _) => {
                self.out.marks.push(Mark {
                    at,
                    kind: MarkKind::Compact,
                });
            }
            (Some("event_msg"), Some("stream_error" | "error")) => self.out.marks.push(Mark {
                at,
                kind: MarkKind::Error,
            }),
            (Some("event_msg"), Some("token_count")) => {
                let u = &p["info"]["last_token_usage"];
                let sum = p["info"]["total_token_usage"]["total_tokens"].as_u64();
                if u.is_null() || (sum.is_some() && sum == self.total) {
                    return;
                }
                self.total = sum;
                let cached = num(&u["cached_input_tokens"]);
                let written = num(&u["cache_write_input_tokens"]);
                self.out.turns.push(Turn {
                    at,
                    model: self.model.clone(),
                    input: num(&u["input_tokens"]).saturating_sub(cached + written),
                    cache_write: written,
                    cache_read: cached,
                    output: num(&u["output_tokens"]),
                    sidechain: false,
                });
            }
            (Some("event_msg"), Some("mcp_tool_call_end")) => {
                let inv = &p["invocation"];
                let took = codex_duration(&p["duration"]);
                if let Some(id) = p["call_id"].as_str() {
                    self.mcp_ids.push(id.to_string());
                }
                self.out.calls.push(Call {
                    at: took.map_or(at, |d| at.saturating_sub(d)),
                    ms: took,
                    tool: inv["tool"].as_str().unwrap_or("?").to_string(),
                    kind: Kind::Mcp,
                    ok: p["result"].get("Err").is_none() && p["result"]["Ok"]["isError"] != true,
                    upstream: inv["server"].as_str().map(String::from),
                });
            }
            (Some("response_item"), Some("function_call" | "custom_tool_call")) => {
                if let Some(id) = p["call_id"].as_str() {
                    self.open.insert(id.to_string(), self.out.calls.len());
                }
                self.out.calls.push(Call {
                    at,
                    ms: None,
                    tool: p["name"].as_str().unwrap_or("?").to_string(),
                    kind: Kind::Builtin,
                    ok: true,
                    upstream: None,
                });
            }
            (Some("response_item"), Some("function_call_output" | "custom_tool_call_output")) => {
                if let Some(&i) = p["call_id"].as_str().and_then(|id| self.open.get(id)) {
                    let c = &mut self.out.calls[i];
                    c.ms = Some(at.saturating_sub(c.at));
                }
            }
            _ => {}
        }
    }

    fn parsed(&self) -> Parsed {
        let mut out = self.out.clone();
        if !out.marks.iter().any(|m| m.kind == MarkKind::Prompt) {
            out.marks.extend(self.tasks.iter().copied());
            out.marks.sort_by_key(|m| m.at);
        }
        // An MCP tool's call shows once: as the MCP event, which names its server.
        let mcp: Vec<usize> = self
            .mcp_ids
            .iter()
            .filter_map(|id| self.open.get(id).copied())
            .collect();
        let mut i = 0;
        out.calls.retain(|_| {
            i += 1;
            !mcp.contains(&(i - 1))
        });
        out.calls.sort_by_key(|c| c.at);
        out
    }
}

/// A whole file, as one look reads it.
#[cfg(test)]
fn claude(path: &Path) -> Parsed {
    session(Format::Claude, path, &mut HashMap::new()).unwrap_or_default()
}

#[cfg(test)]
fn codex(path: &Path) -> Parsed {
    session(Format::Codex, path, &mut HashMap::new()).unwrap_or_default()
}

/// Codex's duration: `{secs, nanos}`, or a string such as `1.2s`.
fn codex_duration(v: &Value) -> Option<u64> {
    if let Some(s) = v["secs"].as_u64() {
        return Some(s * 1000 + v["nanos"].as_u64().unwrap_or(0) / 1_000_000);
    }
    let s = v.as_str()?;
    if let Some(ms) = s.strip_suffix("ms") {
        return ms.trim().parse::<f64>().ok().map(|x| x as u64);
    }
    s.strip_suffix('s')?
        .trim()
        .parse::<f64>()
        .ok()
        .map(|x| (x * 1000.0) as u64)
}

/// A call through `devstack mcp`: one of devstack's own tools, or an
/// upstream's by its prefix (`<prefix>__<tool>`).
fn through_devstack(tool: &str, prefixes: &Prefixes) -> (Kind, Option<String>) {
    match tool
        .split_once("__")
        .and_then(|(p, _)| prefixes.iter().find(|(_, _, x)| x == p))
    {
        Some((_, name, _)) => (Kind::Upstream, Some(name.clone())),
        None => (Kind::Devstack, None),
    }
}

/// The MCP servers' calls that went through devstack: the server's name is
/// devstack's (the agent registered it so), or the call matches one of
/// devstack's records (same tool, within two seconds).
fn classify(calls: &mut [Call], own: &[(u64, String)], prefixes: &Prefixes) {
    for c in calls.iter_mut().filter(|c| c.kind == Kind::Mcp) {
        let by_name = c.upstream.as_deref().is_some_and(|s| s == "devstack");
        let by_record = own
            .iter()
            .any(|(at, tool)| tool == &c.tool && at.abs_diff(c.at) < 2_000);
        if by_name || by_record {
            (c.kind, c.upstream) = through_devstack(&c.tool, prefixes);
        }
    }
}

/// The transcripts followed, by path.
static FILES: Mutex<Option<HashMap<PathBuf, Followed>>> = Mutex::new(None);

/// A session's transcript, read on from where the last look left it.
fn cached(format: Format, path: &Path) -> Option<Parsed> {
    let mut g = FILES.lock().unwrap_or_else(|e| e.into_inner());
    let files = g.get_or_insert_with(HashMap::new);
    // A handful of sessions (and their sub-agents) are looked at; the rest go.
    if files.len() > 64 {
        files.clear();
    }
    session(format, path, files)
}

/// `agent`'s session `session` in time: devstack's records of it and, with
/// `enabled`, its transcript's turns, calls and marks. None: no such session.
pub fn timeline(
    records: &[Record],
    session: &str,
    agent: Option<&str>,
    prefixes: &Prefixes,
    index: &crate::agenthooks::Index,
    enabled: bool,
) -> Option<Value> {
    let view = crate::activity::view(records, None, Some(session), None, prefixes, index);
    let s = view
        .sessions
        .iter()
        .find(|s| s.session == session && agent.is_none_or(|a| a == s.agent))?
        .clone();
    let hooks: Vec<&Value> = view
        .records
        .iter()
        .filter(|r| r["kind"] == "hook")
        .collect();
    let own: Vec<Call> = view
        .records
        .iter()
        .filter(|r| r["kind"] == "tool")
        .map(|r| {
            let tool = r["tool"].as_str().unwrap_or("?").to_string();
            let took = r["ms"].as_u64().unwrap_or(0);
            let (kind, upstream) = through_devstack(&tool, prefixes);
            Call {
                // Recorded when it ended.
                at: r["at"].as_u64().unwrap_or(0).saturating_sub(took),
                ms: Some(took),
                tool,
                kind,
                ok: r["ok"] == true,
                upstream,
            }
        })
        .collect();
    // The latest path the session's hooks named.
    let path = records.iter().rev().find_map(|r| match r {
        Record::Hook {
            session: Some(id),
            agent: a,
            transcript: Some(t),
            ..
        } if id == session && a == &s.agent => Some(PathBuf::from(t)),
        _ => None,
    });
    let (source, why, parsed) = match (enabled, format(&s.agent), path) {
        (false, ..) => ("activity", Some("off"), None),
        (true, None, _) => ("activity", Some("agent"), None),
        (true, Some(_), None) => ("activity", Some("missing"), None),
        (true, Some(f), Some(p)) => match cached(f, &p) {
            Some(parsed) => ("transcript", None, Some(parsed)),
            None => ("activity", Some("missing"), None),
        },
    };
    let (turns, calls, marks) = match parsed {
        Some(mut p) => {
            let mine: Vec<(u64, String)> = own
                .iter()
                .map(|c| (c.at + c.ms.unwrap_or(0), c.tool.clone()))
                .collect();
            classify(&mut p.calls, &mine, prefixes);
            (p.turns, p.calls, p.marks)
        }
        None => (Vec::new(), own, Vec::new()),
    };
    let mut v = serde_json::to_value(&s).expect("serialises");
    v["source"] = source.into();
    if let Some(w) = why {
        v["why"] = w.into();
    }
    v["turns"] = serde_json::to_value(turns).expect("serialises");
    v["calls"] = serde_json::to_value(calls).expect("serialises");
    v["marks"] = serde_json::to_value(marks).expect("serialises");
    v["hooks"] = serde_json::to_value(hooks).expect("serialises");
    Some(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn file(lines: &[Value]) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
        f
    }

    const T0: &str = "2026-11-09T13:00:00.000Z";
    fn at(s: u64) -> String {
        format!("2026-11-09T13:00:{s:02}.000Z")
    }
    fn base() -> u64 {
        ms(&Value::from(T0)).unwrap()
    }

    #[test]
    fn claude_turns_once_per_message_calls_with_their_time_prompts_without_text() {
        let f = file(&[
            serde_json::json!({"type":"user","timestamp":at(0),"message":{"role":"user","content":"secret prompt text"}}),
            serde_json::json!({"type":"assistant","timestamp":at(2),"message":{"id":"m1","model":"claude-opus","content":[{"type":"text","text":"x"}],"usage":{"input_tokens":2,"cache_creation_input_tokens":900,"cache_read_input_tokens":60000,"output_tokens":10}}}),
            serde_json::json!({"type":"assistant","timestamp":at(3),"message":{"id":"m1","model":"claude-opus","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"rm -rf secret"}}],"usage":{"input_tokens":2,"cache_creation_input_tokens":900,"cache_read_input_tokens":60000,"output_tokens":248}}}),
            serde_json::json!({"type":"assistant","timestamp":at(3),"message":{"id":"m1","model":"claude-opus","content":[{"type":"tool_use","id":"t2","name":"mcp__devstack__nuts_timetrack__start_timer","input":{}}],"usage":{"input_tokens":2,"cache_creation_input_tokens":900,"cache_read_input_tokens":60000,"output_tokens":248}}}),
            serde_json::json!({"type":"user","timestamp":at(8),"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","is_error":true,"content":"secret output"}]}}),
            serde_json::json!({"type":"user","timestamp":at(9),"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t2","content":"ok"}]}}),
            serde_json::json!({"type":"system","subtype":"compact_boundary","timestamp":at(20)}),
            serde_json::json!({"type":"user","timestamp":at(21),"isCompactSummary":true,"message":{"role":"user","content":"summary"}}),
            serde_json::json!({"type":"user","timestamp":at(30),"message":{"role":"user","content":[{"type":"text","text":"[Request interrupted by user]"}]}}),
            serde_json::json!({"type":"user","timestamp":at(31),"isMeta":true,"message":{"role":"user","content":"<local-command-caveat>"}}),
            serde_json::json!({"type":"user","timestamp":at(32),"origin":{"kind":"task-notification"},"message":{"role":"user","content":"<task-notification>done</task-notification>"}}),
            serde_json::json!({"type":"assistant","timestamp":at(40),"isSidechain":true,"message":{"id":"m2","model":"claude-haiku","content":[{"type":"tool_use","id":"t3","name":"Read"}],"usage":{"input_tokens":5,"output_tokens":7}}}),
        ]);
        let p = claude(f.path());
        let b = base();
        assert_eq!(p.turns.len(), 2, "{:?}", p.turns);
        assert_eq!(
            p.turns[0],
            Turn {
                at: b + 2000,
                model: Some("claude-opus".into()),
                input: 2,
                cache_write: 900,
                cache_read: 60000,
                output: 248,
                sidechain: false
            }
        );
        assert!(p.turns[1].sidechain);
        // A sub-agent's calls are its own: not on the main agent's rows.
        assert_eq!(p.calls.len(), 2);
        assert_eq!(
            (p.calls[0].tool.as_str(), p.calls[0].ms, p.calls[0].ok),
            ("Bash", Some(5000), false)
        );
        assert_eq!(p.calls[1].kind, Kind::Mcp);
        assert_eq!(p.calls[1].upstream.as_deref(), Some("devstack"));
        assert_eq!(p.calls[1].tool, "nuts_timetrack__start_timer");
        let kinds: Vec<MarkKind> = p.marks.iter().map(|m| m.kind).collect();
        assert_eq!(
            kinds,
            [MarkKind::Prompt, MarkKind::Compact, MarkKind::Interrupt]
        );
        // Nothing of what was said or done is in what the reader took.
        let all = format!("{p:?}");
        for word in ["secret", "summary", "rm -rf"] {
            assert!(!all.contains(word), "{word} in {all}");
        }
    }

    #[test]
    fn claude_reads_the_sub_agents_kept_beside_the_transcript() {
        let d = tempfile::tempdir().unwrap();
        let main = d.path().join("s1.jsonl");
        std::fs::write(&main, format!("{}\n", serde_json::json!({"type":"assistant","timestamp":at(1),"message":{"id":"a","usage":{"output_tokens":1}}}))).unwrap();
        std::fs::create_dir_all(d.path().join("s1").join("subagents")).unwrap();
        std::fs::write(
            d.path().join("s1").join("subagents").join("agent-x.jsonl"),
            format!("{}\n", serde_json::json!({"type":"assistant","timestamp":at(5),"message":{"id":"b","usage":{"output_tokens":9}}})),
        )
        .unwrap();
        let p = claude(&main);
        assert_eq!(
            p.turns
                .iter()
                .map(|t| (t.output, t.sidechain))
                .collect::<Vec<_>>(),
            [(1, false), (9, true)]
        );
    }

    #[test]
    fn codex_turns_from_token_counts_and_mcp_calls_once() {
        let f = file(&[
            serde_json::json!({"timestamp":at(0),"type":"turn_context","payload":{"model":"gpt-5-codex"}}),
            serde_json::json!({"timestamp":at(1),"type":"event_msg","payload":{"type":"user_message","message":"secret"}}),
            serde_json::json!({"timestamp":at(2),"type":"response_item","payload":{"type":"function_call","name":"shell","arguments":"{\"cmd\":\"secret\"}","call_id":"c1"}}),
            serde_json::json!({"timestamp":at(6),"type":"response_item","payload":{"type":"function_call_output","call_id":"c1","output":"secret"}}),
            serde_json::json!({"timestamp":at(7),"type":"response_item","payload":{"type":"function_call","name":"mcp__devstack__up","call_id":"c2"}}),
            serde_json::json!({"timestamp":at(9),"type":"event_msg","payload":{"type":"mcp_tool_call_end","call_id":"c2","invocation":{"server":"devstack","tool":"up","arguments":{}},"duration":{"secs":2,"nanos":0},"result":{"Ok":{"content":[]}}}}),
            serde_json::json!({"timestamp":at(10),"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"total_tokens":1050},"last_token_usage":{"input_tokens":1000,"cached_input_tokens":800,"output_tokens":50}}}}),
            serde_json::json!({"timestamp":at(11),"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"total_tokens":1050},"last_token_usage":{"input_tokens":1000,"cached_input_tokens":800,"output_tokens":50}}}}),
            serde_json::json!({"timestamp":at(11),"type":"event_msg","payload":{"type":"token_count","info":null}}),
            serde_json::json!({"timestamp":at(12),"type":"event_msg","payload":{"type":"turn_aborted"}}),
        ]);
        let p = codex(f.path());
        let b = base();
        assert_eq!(
            p.turns,
            [Turn {
                at: b + 10_000,
                model: Some("gpt-5-codex".into()),
                input: 200,
                cache_write: 0,
                cache_read: 800,
                output: 50,
                sidechain: false
            }]
        );
        assert_eq!(p.calls.len(), 2, "{:?}", p.calls);
        assert_eq!(
            (p.calls[0].tool.as_str(), p.calls[0].ms),
            ("shell", Some(4000))
        );
        assert_eq!(
            (
                p.calls[1].tool.as_str(),
                p.calls[1].kind,
                p.calls[1].ms,
                p.calls[1].at
            ),
            ("up", Kind::Mcp, Some(2000), b + 7000)
        );
        assert_eq!(
            p.marks.iter().map(|m| m.kind).collect::<Vec<_>>(),
            [MarkKind::Prompt, MarkKind::Interrupt]
        );
        assert!(!format!("{p:?}").contains("secret"));
    }

    fn hook(at: u64, session: &str, transcript: Option<&Path>) -> Record {
        Record::Hook {
            at,
            event: "session-start".into(),
            agent: "claude-code".into(),
            session: Some(session.into()),
            tree: "/w".into(),
            hooks: Vec::new(),
            recorded: false,
            transcript: transcript.map(|p| p.to_string_lossy().into_owned()),
        }
    }
    fn tool(at: u64, name: &str, took: u64) -> Record {
        Record::Tool {
            at,
            tool: name.into(),
            client: None,
            tree: "/w".into(),
            pid: 1,
            ms: took,
            ok: true,
        }
    }

    #[test]
    fn the_timeline_holds_devstacks_records_until_transcripts_are_switched_on() {
        let b = base();
        let f = file(&[
            serde_json::json!({"type":"user","timestamp":at(0),"message":{"content":"go"}}),
            serde_json::json!({"type":"assistant","timestamp":at(2),"message":{"id":"m","content":[{"type":"tool_use","id":"t","name":"mcp__ds__tt__start_timer"}],"usage":{"output_tokens":5}}}),
            serde_json::json!({"type":"user","timestamp":at(3),"message":{"content":[{"type":"tool_result","tool_use_id":"t"}]}}),
        ]);
        let records = [
            hook(b, "s", Some(f.path())),
            tool(b + 3000, "tt__start_timer", 900),
        ];
        let prefixes: Prefixes = vec![("default".into(), "timetrack".into(), "tt".into())];
        let index = crate::agenthooks::Index::default();

        let off = timeline(&records, "s", None, &prefixes, &index, false).unwrap();
        assert_eq!(off["source"], "activity");
        assert_eq!(off["why"], "off");
        assert_eq!(off["calls"][0]["kind"], "upstream");
        assert_eq!(off["calls"][0]["upstream"], "timetrack");
        assert_eq!(off["calls"][0]["at"], b + 2100);
        assert!(off["turns"].as_array().unwrap().is_empty());
        // The path stays with the hub.
        assert!(off["hooks"][0].get("transcript").is_none());

        let on = timeline(&records, "s", Some("claude-code"), &prefixes, &index, true).unwrap();
        assert_eq!(on["source"], "transcript");
        assert_eq!(on["turns"][0]["output"], 5);
        // The server is named `ds` here: the call still is devstack's, by its record.
        assert_eq!(on["calls"][0]["kind"], "upstream");
        assert_eq!(on["calls"][0]["tool"], "tt__start_timer");
        assert_eq!(on["marks"][0]["kind"], "prompt");

        assert!(timeline(&records, "other", None, &prefixes, &index, true).is_none());
    }

    #[test]
    fn the_timeline_says_why_it_has_no_transcript() {
        let index = crate::agenthooks::Index::default();
        let gone = timeline(
            &[hook(1, "s", Some(Path::new("/nowhere/x.jsonl")))],
            "s",
            None,
            &Vec::new(),
            &index,
            true,
        )
        .unwrap();
        assert_eq!(gone["why"], "missing");
        let mut r = hook(1, "s", None);
        if let Record::Hook { agent, .. } = &mut r {
            *agent = "aider".into();
        }
        assert_eq!(
            timeline(&[r], "s", None, &Vec::new(), &index, true).unwrap()["why"],
            "agent"
        );
    }

    #[test]
    fn a_growing_transcript_is_read_on_from_where_it_was_left() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("s.jsonl");
        let turn = |id: &str, s: u64, out: u64| {
            serde_json::json!({"type":"assistant","timestamp":at(s),"message":{"id":id,"content":[],"usage":{"output_tokens":out}}}).to_string()
        };
        let mut files = HashMap::new();
        // The second line is being written: not read yet.
        let half = turn("b", 2, 7);
        std::fs::write(&path, format!("{}\n{}", turn("a", 1, 5), &half[..10])).unwrap();
        let p = session(Format::Claude, &path, &mut files).unwrap();
        assert_eq!(p.turns.iter().map(|t| t.output).collect::<Vec<_>>(), [5]);
        let first = files[&path].offset;
        // Written on: only what was added is read, the turn of `a` grows.
        std::fs::write(
            &path,
            format!("{}\n{half}\n{}\n", turn("a", 1, 5), turn("a", 1, 9)),
        )
        .unwrap();
        let p = session(Format::Claude, &path, &mut files).unwrap();
        assert!(files[&path].offset > first);
        assert_eq!(p.turns.iter().map(|t| t.output).collect::<Vec<_>>(), [9, 7]);
        assert_eq!(p, claude(&path));
        // Written anew (shorter): from its start.
        std::fs::write(&path, format!("{}\n", turn("c", 3, 1))).unwrap();
        let p = session(Format::Claude, &path, &mut files).unwrap();
        assert_eq!(p.turns.iter().map(|t| t.output).collect::<Vec<_>>(), [1]);
        // Gone: not read.
        std::fs::remove_file(&path).unwrap();
        assert!(session(Format::Claude, &path, &mut files).is_none());
    }

    #[test]
    fn the_setting_is_off_until_saved() {
        let d = tempfile::tempdir().unwrap();
        assert!(!Setting::load(d.path()).enabled);
        Setting { enabled: true }.save(d.path()).unwrap();
        assert!(Setting::load(d.path()).enabled);
    }
}
