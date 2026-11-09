//! The hooks of the pools' skills (ADR-0094), given to the coding agents.
//!
//! - **The index.** Each sync of the pools' skills writes `agent-hooks.json`
//!   in the hub's state: every hook of a skill the user accepted and has
//!   switched on, as the version they agreed to has it, with the
//!   repositories the skill is bound to, and the agents that get them.
//! - **One entry per agent and event.** Into the hook configuration of each
//!   agent switched on that has `[agent.hooks]` in the agent table, the hub
//!   writes one entry per event that has hooks: `devstack hook <event>
//!   --agent <id>`. The entry stays the same whatever the hooks say (Codex
//!   asks the user to trust a hook again when it changes); the user's own
//!   entries and the file's other keys stay as they are. An agent switched
//!   off, or an event without hooks, loses the entry.
//! - **The dispatcher** ([`run`]) is what that entry starts. It reads the
//!   index and the hub's state from disk (no hub needs to run), picks the
//!   hooks that apply (the working tree is of a bound repository, the bound
//!   upstream is accepted or the agent has it as a server of its own, the
//!   pattern matches the prompt, not given yet in
//!   this session) and prints their texts. It never blocks a prompt: on
//!   any error it prints nothing.

use crate::activity::{HookRef, Record, tree_of};
use devstack_node::hooks::{Event, Hook, Once};
use devstack_skills::agents::{Agent, HookFormat};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// The index in the hub's state.
pub const INDEX: &str = "agent-hooks.json";
/// What the hub's entries start with: how it knows its own.
const COMMAND: &str = "devstack hook ";
/// The seconds an agent gives the dispatcher.
const TIMEOUT: u64 = 5;
/// The folder of what each session was given.
const SESSIONS: &str = "hook-sessions";
/// The folder of the sessions that ended, kept for the next start.
const ENDED: &str = "hook-ended";
/// What a session was given is forgotten after this long.
const KEEP: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 3600);

/// A hook of an installed skill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub pool: String,
    pub set: String,
    pub skill: String,
    /// Its place in the skill's `hooks.toml`.
    pub index: usize,
    /// The repositories its skill is bound to; none: every one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repos: Vec<String>,
    #[serde(flatten)]
    pub hook: Hook,
}

impl Entry {
    fn key(&self) -> String {
        format!("{}/{}/{}#{}", self.pool, self.set, self.skill, self.index)
    }

    fn reference(&self) -> HookRef {
        HookRef {
            pool: self.pool.clone(),
            set: self.set.clone(),
            skill: self.skill.clone(),
            index: self.index,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Index {
    #[serde(default)]
    pub hooks: Vec<Entry>,
    /// The agents (ids) whose configuration has the hub's entries.
    #[serde(default)]
    pub agents: Vec<String>,
    /// The agents switched on that get no hooks (no `[agent.hooks]`), by
    /// name: what `devstack doctor` and the Skills page tell.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub without: Vec<String>,
}

pub fn read_index(state: &Path) -> Index {
    std::fs::read(state.join(INDEX))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// The events an agent's configuration can carry an entry for.
const AGENT_EVENTS: [Event; 3] = [Event::SessionStart, Event::PromptSubmit, Event::SessionEnd];

/// An event's name in an agent's configuration; `None`: the agent has no
/// such event. `lane-stop` is no agent's: its texts follow `devstack lane
/// stop`'s output. Codex documents no end of a session (#462).
fn event_name(format: HookFormat, e: Event) -> Option<&'static str> {
    match (format, e) {
        (HookFormat::Claude | HookFormat::Codex, Event::SessionStart) => Some("SessionStart"),
        (HookFormat::Claude | HookFormat::Codex, Event::PromptSubmit) => Some("UserPromptSubmit"),
        (HookFormat::Claude, Event::SessionEnd) => Some("SessionEnd"),
        (HookFormat::Codex, Event::SessionEnd) | (_, Event::LaneStop) => None,
    }
}

/// Whether the agent `id` reports the end of a session (`session-end`).
pub fn has_session_end(id: &str) -> bool {
    devstack_skills::agents::get(id)
        .and_then(|a| a.hooks.as_ref())
        .is_some_and(|h| event_name(h.format, Event::SessionEnd).is_some())
}

/// The hub's entry for `agent` on `event`.
fn command(event: Event, agent: &str) -> String {
    format!("{COMMAND}{} --agent {agent}", event.as_str())
}

fn is_ours(handler: &Value) -> bool {
    handler
        .get("command")
        .and_then(Value::as_str)
        .is_some_and(|c| c.starts_with(COMMAND) && c.contains(" --agent "))
}

/// Bring the hook configuration `file` of `agent` to the hub's entries for
/// `events` (none: take them out). Written only when that changes it;
/// a file that is not a JSON object is left alone, with why.
pub fn configure(
    file: &Path,
    format: HookFormat,
    agent: &str,
    events: &[Event],
) -> Result<(), String> {
    let text = match std::fs::read_to_string(file) {
        Ok(t) => Some(t),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("{}: {e}", file.display())),
    };
    if text.is_none() && events.is_empty() {
        return Ok(());
    }
    let src = text.as_deref().map(|t| t.trim_start_matches('\u{feff}'));
    let mut doc: IndexMap<String, Box<RawValue>> = match src {
        Some(t) if !t.trim().is_empty() => serde_json::from_str(t).map_err(|e| {
            format!(
                "{}: not a JSON object ({e}): hooks not written",
                file.display()
            )
        })?,
        _ => IndexMap::new(),
    };
    let mut hooks: IndexMap<String, Box<RawValue>> = match doc.get("hooks") {
        Some(h) => serde_json::from_str(h.get()).map_err(|e| {
            format!(
                "{}: `hooks` is not an object ({e}): hooks not written",
                file.display()
            )
        })?,
        None => IndexMap::new(),
    };
    let mut changed = false;
    for e in AGENT_EVENTS {
        let Some(name) = event_name(format, e) else {
            continue;
        };
        let before: Value = match hooks.get(name) {
            Some(r) => serde_json::from_str(r.get()).map_err(|e| e.to_string())?,
            None => Value::Null,
        };
        let mut groups: Vec<Value> = match &before {
            Value::Array(a) => a.clone(),
            Value::Null => Vec::new(),
            _ => {
                return Err(format!(
                    "{}: `hooks.{name}` is not a list: hooks not written",
                    file.display()
                ));
            }
        };
        // The hub's entries out, and the groups that held only them.
        groups.retain_mut(|g| {
            let Some(list) = g.get_mut("hooks").and_then(Value::as_array_mut) else {
                return true;
            };
            let had = list.len();
            list.retain(|h| !is_ours(h));
            !(list.is_empty() && had > 0)
        });
        if events.contains(&e) {
            groups.push(serde_json::json!({
                "hooks": [{ "type": "command", "command": command(e, agent), "timeout": TIMEOUT }]
            }));
        }
        let after = if groups.is_empty() {
            Value::Null
        } else {
            Value::Array(groups)
        };
        if after == before {
            continue;
        }
        changed = true;
        if after.is_null() {
            hooks.shift_remove(name);
        } else {
            let raw = serde_json::to_string_pretty(&after).expect("serialises");
            hooks.insert(
                name.to_string(),
                RawValue::from_string(raw).expect("valid JSON"),
            );
        }
    }
    if !changed {
        return Ok(());
    }
    if hooks.is_empty() {
        doc.shift_remove("hooks");
    } else {
        let raw = serde_json::to_string_pretty(&hooks).expect("serialises");
        doc.insert(
            "hooks".into(),
            RawValue::from_string(raw).expect("valid JSON"),
        );
    }
    let mut out = serde_json::to_string_pretty(&doc).expect("serialises");
    out.push('\n');
    write(file, out.as_bytes())
}

/// Write `bytes` to `path` through a file beside it.
fn write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension(format!("devstack-{:08x}.tmp", rand::random::<u32>()));
    std::fs::write(&tmp, bytes)
        .and_then(|()| std::fs::rename(&tmp, path))
        .map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("{}: {e}", path.display())
        })
}

/// Write the index, and bring each agent's configuration to it: the
/// agents switched on (`on`) with `[agent.hooks]` get an entry per event
/// that has hooks, every other one of the table loses the hub's. `file`
/// says where an agent's configuration is (the table's; tests: another).
/// Why a file could not be written.
pub fn apply(
    state: &Path,
    hooks: Vec<Entry>,
    on: &dyn Fn(&Agent) -> bool,
    file: &dyn Fn(&Agent) -> Option<PathBuf>,
) -> Vec<String> {
    let mut errors = Vec::new();
    let mut events: Vec<Event> = Vec::new();
    for h in &hooks {
        if AGENT_EVENTS.contains(&h.hook.event) && !events.contains(&h.hook.event) {
            events.push(h.hook.event);
        }
    }
    let mut index = Index {
        hooks,
        ..Default::default()
    };
    for a in devstack_skills::agents::all() {
        let enabled = on(a);
        let Some(adapter) = &a.hooks else {
            if enabled && !index.hooks.is_empty() {
                index.without.push(a.name.clone());
            }
            continue;
        };
        let Some(path) = file(a) else { continue };
        let want: &[Event] = if enabled { &events } else { &[] };
        // An agent's folder that is not there: the agent is not either.
        if !want.is_empty() && !path.parent().is_some_and(Path::is_dir) {
            continue;
        }
        match configure(&path, adapter.format, &a.id, want) {
            Ok(()) if !want.is_empty() => index.agents.push(a.id.clone()),
            Ok(()) => {}
            Err(e) if !want.is_empty() => errors.push(e),
            // Nothing to write there: a file the hub cannot read is not
            // the user's concern on the Skills page.
            Err(_) => {}
        }
    }
    if read_index(state) != index
        && let Err(e) = write(
            &state.join(INDEX),
            &serde_json::to_vec_pretty(&index).expect("serialises"),
        )
    {
        errors.push(e);
    }
    errors
}

// -------------------------------------------------------------- dispatcher

/// What an agent hands a hook on stdin, as far as the dispatcher reads it.
#[derive(Debug, Default, Deserialize)]
struct Input {
    #[serde(default, alias = "sessionId")]
    session_id: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    prompt: Option<String>,
    /// Why a session starts: `startup`, `resume`, `clear`, `compact`.
    #[serde(default)]
    source: Option<String>,
    /// The session's transcript: named at the next start (`session-end`),
    /// and recorded for the timeline (its path only).
    #[serde(default, alias = "transcriptPath")]
    transcript_path: Option<String>,
}

/// The end of a session, kept for the next start in its working tree.
#[derive(Debug, Serialize, Deserialize)]
struct Ended {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    transcript_path: Option<String>,
    at: String,
}

/// Where the dispatcher looks things up: the hub's state and git.
pub struct Lookup<'a> {
    pub state: &'a Path,
    /// The `origin` of the working tree at a path.
    pub origin: &'a dyn Fn(&Path) -> Option<String>,
    /// Whether a pool's upstream (pool, name) is there for the agent in
    /// the working tree (`cwd`): accepted in the hub, or configured in the
    /// agent itself at the same URL (#473).
    pub accepted: &'a dyn Fn(&str, &str, Option<&Path>) -> bool,
    /// Where an error goes (the hub's diagnostic log).
    pub warn: &'a dyn Fn(String),
}

/// The texts of the hooks of `event` that apply to the agent `agent` in
/// the session `input` describes, joined; empty when none (or on any
/// error: a hook never stands in a prompt's way).
pub fn run(event: Event, agent: &str, input: &str, at: &Lookup<'_>) -> String {
    let input: Input = serde_json::from_str(input).unwrap_or_default();
    let index = match std::fs::read(at.state.join(INDEX)) {
        Ok(b) => match serde_json::from_slice::<Index>(&b) {
            Ok(i) => i,
            Err(e) => {
                (at.warn)(format!("{INDEX}: {e}"));
                return String::new();
            }
        },
        Err(_) => return String::new(),
    };
    if !index.agents.iter().any(|a| a == agent) {
        return String::new();
    }
    let cwd = input.cwd.as_deref().map(PathBuf::from);
    let mut origin: Option<Option<String>> = None;
    if event == Event::SessionEnd {
        if let Some(refs) = end(agent, &input, &index, cwd.as_deref(), at) {
            record(at, event, agent, &input, cwd.as_deref(), refs, true);
        }
        return String::new();
    }
    let session = input
        .session_id
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(|s| at.state.join(SESSIONS).join(session_file(agent, s)));
    let mut given: Vec<String> = session
        .as_ref()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    if event == Event::SessionStart {
        prune(&at.state.join(SESSIONS));
        // The context was emptied: what it was given is gone with it.
        if matches!(input.source.as_deref(), Some("clear" | "compact")) {
            given.clear();
        }
    }
    let prompt = input.prompt.as_deref().unwrap_or("");
    let mut out: Vec<String> = Vec::new();
    let mut refs: Vec<HookRef> = Vec::new();
    let before = given.len();
    for h in index.hooks.iter().filter(|h| h.hook.event == event) {
        if !applies(h, cwd.as_deref(), &mut origin, at) {
            continue;
        }
        let matched = match &h.hook.pattern {
            Some(p) => match regex::Regex::new(p).ok().and_then(|r| r.find(prompt)) {
                Some(m) => Some(m.as_str().trim().to_string()),
                None => continue,
            },
            None => None,
        };
        let key = match (h.hook.once, matched) {
            (Once::Match, Some(m)) => format!("{}\0{m}", h.key()),
            _ => h.key(),
        };
        if given.contains(&key) {
            continue;
        }
        given.push(key);
        out.push(h.hook.context.clone());
        refs.push(h.reference());
    }
    if event == Event::SessionStart
        && let Some(cwd) = &cwd
        && let Some(texts) = ended(agent, &index, cwd, &mut origin, at, &mut refs)
    {
        out.push(texts);
    }
    // A session's start always, to know the session; a prompt only when it
    // was given something.
    if event == Event::SessionStart || !refs.is_empty() {
        record(at, event, agent, &input, cwd.as_deref(), refs, false);
    }
    if let Some(p) = &session
        && (given.len() != before || event == Event::SessionStart)
    {
        let _ = std::fs::create_dir_all(p.parent().expect("in the sessions folder"));
        if let Err(e) = write(p, &serde_json::to_vec(&given).expect("serialises")) {
            (at.warn)(e);
        }
    }
    out.join("\n\n")
}

/// Whether a hook applies in the working tree at `cwd`: its upstream
/// accepted, its skill's repositories matching the tree's `origin` (asked
/// once, kept in `origin`).
fn applies(
    h: &Entry,
    cwd: Option<&Path>,
    origin: &mut Option<Option<String>>,
    at: &Lookup<'_>,
) -> bool {
    if let Some(u) = &h.hook.upstream
        && !(at.accepted)(&h.pool, u, cwd)
    {
        return false;
    }
    if !h.repos.is_empty() {
        let o = origin.get_or_insert_with(|| cwd.and_then(|c| (at.origin)(c)));
        if !o
            .as_deref()
            .is_some_and(|o| devstack_node::skills::repo_matches(&h.repos, o))
        {
            return false;
        }
    }
    true
}

/// The file that keeps the end of `agent`'s last session in `tree`.
fn ended_file(state: &Path, agent: &str, tree: &str) -> PathBuf {
    let h = Sha256::digest(format!("{agent}\0{tree}").as_bytes());
    state
        .join(ENDED)
        .join(format!("{}.json", &devstack_node::skills::hex(&h)[..32]))
}

/// What happened, into the activity (#482): never the prompt or a text.
fn record(
    at: &Lookup<'_>,
    event: Event,
    agent: &str,
    input: &Input,
    cwd: Option<&Path>,
    hooks: Vec<HookRef>,
    recorded: bool,
) {
    let r = Record::Hook {
        at: crate::activity::now_ms(),
        event: event.as_str().into(),
        agent: agent.into(),
        session: input.session_id.clone().filter(|s| !s.is_empty()),
        tree: cwd.map(tree_of).unwrap_or_default(),
        hooks,
        recorded,
        transcript: input.transcript_path.clone().filter(|s| !s.is_empty()),
    };
    if let Err(e) = crate::activity::write(at.state, &r) {
        (at.warn)(e);
    }
}

/// `session-end`: the session can be told nothing any more. Where a
/// `session-end` hook applies in its working tree, keep that it ended, for
/// the next start of the same agent there ([`ended`]). The hooks it was
/// kept for; `None` when none applies.
fn end(
    agent: &str,
    input: &Input,
    index: &Index,
    cwd: Option<&Path>,
    at: &Lookup<'_>,
) -> Option<Vec<HookRef>> {
    let cwd = cwd?;
    let mut origin = None;
    let refs: Vec<HookRef> = index
        .hooks
        .iter()
        .filter(|h| h.hook.event == Event::SessionEnd)
        .filter(|h| applies(h, Some(cwd), &mut origin, at))
        .map(Entry::reference)
        .collect();
    if refs.is_empty() {
        return None;
    }
    let file = ended_file(at.state, agent, &tree_of(cwd));
    let record = Ended {
        session_id: input.session_id.clone().filter(|s| !s.is_empty()),
        transcript_path: input.transcript_path.clone().filter(|s| !s.is_empty()),
        at: jiff::Zoned::now()
            .strftime("%Y-%m-%dT%H:%M:%S%:z")
            .to_string(),
    };
    let _ = std::fs::create_dir_all(file.parent().expect("in the folder of ends"));
    if let Err(e) = write(&file, &serde_json::to_vec(&record).expect("serialises")) {
        (at.warn)(e);
    }
    Some(refs)
}

/// At a session's start: the texts of the `session-end` hooks for the
/// previous session of `agent` in this working tree, if one ended there
/// since, with a line that names it. Given once: the record goes.
fn ended(
    agent: &str,
    index: &Index,
    cwd: &Path,
    origin: &mut Option<Option<String>>,
    at: &Lookup<'_>,
    refs: &mut Vec<HookRef>,
) -> Option<String> {
    prune(&at.state.join(ENDED));
    let file = ended_file(at.state, agent, &tree_of(cwd));
    let record: Ended = serde_json::from_slice(&std::fs::read(&file).ok()?).ok()?;
    let _ = std::fs::remove_file(&file);
    let given: Vec<&Entry> = index
        .hooks
        .iter()
        .filter(|h| h.hook.event == Event::SessionEnd)
        .filter(|h| applies(h, Some(cwd), origin, at))
        .collect();
    if given.is_empty() {
        return None;
    }
    refs.extend(given.iter().map(|h| h.reference()));
    let texts: Vec<&str> = given.iter().map(|h| h.hook.context.as_str()).collect();
    let mut which = Vec::new();
    if let Some(id) = &record.session_id {
        which.push(format!("session `{id}`"));
    }
    if let Some(t) = &record.transcript_path {
        which.push(format!("transcript `{t}`"));
    }
    let which = if which.is_empty() {
        String::new()
    } else {
        format!(" ({})", which.join(", "))
    };
    Some(format!(
        "The previous session in this working tree{which} ended at {}. What follows was meant for its end:\n\n{}",
        record.at,
        texts.join("\n\n")
    ))
}

/// The texts of the hooks of `event` that apply in the working tree at
/// `cwd`, for every agent: `lane-stop`, whose texts follow `devstack lane
/// stop`'s output. Empty when none, or on any error.
pub fn texts_for(event: Event, cwd: &Path, at: &Lookup<'_>) -> String {
    let index = read_index(at.state);
    let mut origin = None;
    let given: Vec<&Entry> = index
        .hooks
        .iter()
        .filter(|h| h.hook.event == event)
        .filter(|h| applies(h, Some(cwd), &mut origin, at))
        .collect();
    if !given.is_empty() {
        let refs = given.iter().map(|h| h.reference()).collect();
        record(at, event, "-", &Input::default(), Some(cwd), refs, false);
    }
    given
        .iter()
        .map(|h| h.hook.context.as_str())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// [`texts_for`] with this machine's hub state, its git, its upstreams.
pub fn texts_here(event: Event, cwd: &Path) -> String {
    here(event, "-", |at| texts_for(event, cwd, at))
}

/// The file of what `agent`'s session `id` was given.
fn session_file(agent: &str, id: &str) -> String {
    let h = Sha256::digest(format!("{agent}\0{id}").as_bytes());
    format!("{}.json", &devstack_node::skills::hex(&h)[..32])
}

/// Forget the sessions not seen for a week.
fn prune(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > KEEP);
        if old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// A pool's upstream is there for `agent` in `cwd`: accepted in the hub, or,
/// read only then, configured in the agent itself at the pool's URL (#473:
/// a member who added the server by its own setup hint got no hook).
/// `own` reads the agent's servers.
pub(crate) fn upstream_there(
    state: &Path,
    pool: &str,
    name: &str,
    own: &dyn Fn() -> Vec<crate::agentmcp::Direct>,
) -> bool {
    use crate::upstreams::Upstreams;
    Upstreams::pool_accepted(state, pool, name)
        || Upstreams::pool_url(state, pool, name).is_some_and(|url| {
            own()
                .iter()
                .any(|d| crate::agentmcp::same_url(&d.url, &url))
        })
}

/// The dispatcher as `devstack hook` runs it: this machine's hub state,
/// its git, its upstreams.
pub fn run_here(event: Event, agent: &str, input: &str) -> String {
    here(event, agent, |at| run(event, agent, input, at))
}

/// This machine's hub state, its git and its upstreams, for `f`.
fn here<R>(event: Event, agent: &str, f: impl FnOnce(&Lookup<'_>) -> R) -> R {
    let state = crate::state_dir();
    f(&Lookup {
        state: &state,
        origin: &|p| crate::branches::origin(p),
        accepted: &|pool, name, cwd| {
            upstream_there(&state, pool, name, &|| {
                crate::agentmcp::of_agent(agent, cwd)
            })
        },
        warn: &|error| {
            devstack_daemon::diag::DiagLog::open(crate::log_path()).write(
                "warn",
                "agent_hook",
                serde_json::json!({ "event": event.as_str(), "agent": agent, "error": error }),
            );
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(skill: &str, hook: Hook) -> Entry {
        Entry {
            pool: "default".into(),
            set: "core".into(),
            skill: skill.into(),
            index: 0,
            repos: Vec::new(),
            hook,
        }
    }

    fn hook(event: Event, pattern: Option<&str>, context: &str) -> Hook {
        Hook {
            event,
            pattern: pattern.map(str::to_string),
            once: if pattern.is_some() {
                Once::Match
            } else {
                Once::Session
            },
            upstream: None,
            context: context.into(),
        }
    }

    #[test]
    fn the_entries_go_in_beside_the_users_own_and_out_again() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("settings.json");
        let user = r#"{
  "model": "opus",
  "permissions": { "allow": ["Bash(ls)"] },
  "hooks": {
    "UserPromptSubmit": [
      { "hooks": [ { "type": "command", "command": "~/mine.sh" } ] }
    ]
  },
  "alwaysThinkingEnabled": true
}"#;
        std::fs::write(&f, user).unwrap();
        let both = [Event::SessionStart, Event::PromptSubmit];
        configure(&f, HookFormat::Claude, "claude-code", &both).unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&f).unwrap()).unwrap();
        assert_eq!(
            v["hooks"]["SessionStart"][0]["hooks"][0]["command"],
            "devstack hook session-start --agent claude-code"
        );
        let prompt = v["hooks"]["UserPromptSubmit"].as_array().unwrap();
        assert_eq!(prompt.len(), 2);
        assert_eq!(prompt[0]["hooks"][0]["command"], "~/mine.sh");
        assert_eq!(v["permissions"]["allow"][0], "Bash(ls)");
        // The file's keys keep their order.
        let text = std::fs::read_to_string(&f).unwrap();
        let (m, a) = (
            text.find("\"model\"").unwrap(),
            text.find("alwaysThinking").unwrap(),
        );
        assert!(m < text.find("\"hooks\"").unwrap() && text.find("\"hooks\"").unwrap() < a);

        // Again: nothing changes, nothing is written.
        let mtime = std::fs::metadata(&f).unwrap().modified().unwrap();
        configure(&f, HookFormat::Claude, "claude-code", &both).unwrap();
        assert_eq!(std::fs::metadata(&f).unwrap().modified().unwrap(), mtime);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), text);

        // Out again: the user's file as it was, but for its layout.
        configure(&f, HookFormat::Claude, "claude-code", &[]).unwrap();
        let back: Value = serde_json::from_str(&std::fs::read_to_string(&f).unwrap()).unwrap();
        assert_eq!(back, serde_json::from_str::<Value>(user).unwrap());

        // A file that is not JSON is left alone.
        std::fs::write(&f, "{ // a comment\n}").unwrap();
        assert!(configure(&f, HookFormat::Claude, "claude-code", &both).is_err());
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "{ // a comment\n}");
        // No file and nothing to write: none made.
        let none = d.path().join("hooks.json");
        configure(&none, HookFormat::Codex, "codex", &[]).unwrap();
        assert!(!none.exists());
    }

    /// `session-end` is Claude Code's `SessionEnd`; Codex documents no end
    /// of a session, and `lane-stop` is no agent's (#462).
    #[test]
    fn the_end_of_a_session_has_an_entry_where_the_agent_has_the_event() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("settings.json");
        let all = [Event::SessionStart, Event::SessionEnd, Event::LaneStop];
        configure(&f, HookFormat::Claude, "claude-code", &all).unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&f).unwrap()).unwrap();
        assert_eq!(
            v["hooks"]["SessionEnd"][0]["hooks"][0]["command"],
            "devstack hook session-end --agent claude-code"
        );
        let names: Vec<&String> = v["hooks"].as_object().unwrap().keys().collect();
        assert_eq!(names, ["SessionEnd", "SessionStart"]);
        let c = d.path().join("hooks.json");
        configure(&c, HookFormat::Codex, "codex", &all).unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&c).unwrap()).unwrap();
        let names: Vec<&String> = v["hooks"].as_object().unwrap().keys().collect();
        assert_eq!(names, ["SessionStart"]);
        // Only `lane-stop` hooks: no entry at all.
        let st = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".claude")).unwrap();
        let file =
            |a: &Agent| (a.id == "claude-code").then(|| home.path().join(".claude/settings.json"));
        let lane = vec![entry(
            "timer",
            hook(Event::LaneStop, None, "Stop the timer."),
        )];
        assert!(apply(st.path(), lane, &|_| true, &file).is_empty());
        assert!(!home.path().join(".claude/settings.json").exists());
        assert_eq!(read_index(st.path()).hooks.len(), 1);
    }

    /// The end of a session is kept, and its texts are given once at the
    /// next start of the same agent in the same working tree (#462).
    #[test]
    fn the_end_of_a_session_reaches_the_next_start_in_its_tree() {
        let st = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        let shop = src.path().join("shop");
        let blog = src.path().join("blog");
        for d in [&shop, &blog] {
            std::fs::create_dir_all(d.join(".git")).unwrap();
        }
        std::fs::create_dir_all(shop.join("apps/web")).unwrap();
        let mut review = entry("work", hook(Event::SessionEnd, None, "Review the session."));
        review.repos = vec!["acme/shop".into()];
        let start = entry("work", hook(Event::SessionStart, None, "Start the timer."));
        let index = Index {
            hooks: vec![start, review],
            agents: vec!["claude-code".into(), "codex".into()],
            without: Vec::new(),
        };
        std::fs::write(st.path().join(INDEX), serde_json::to_vec(&index).unwrap()).unwrap();
        let at = Lookup {
            state: st.path(),
            origin: &|p| {
                p.ancestors()
                    .any(|a| a.ends_with("shop"))
                    .then(|| "https://github.com/acme/shop".into())
            },
            accepted: &|_, _, _| true,
            warn: &|e| panic!("{e}"),
        };
        let input = |sid: &str, cwd: &Path| {
            serde_json::json!({
                "session_id": sid,
                "cwd": cwd,
                "transcript_path": format!("/t/{sid}.jsonl"),
                "source": "startup",
            })
            .to_string()
        };
        // Ending prints nothing; in a tree where no hook applies, nothing is kept.
        assert_eq!(
            run(Event::SessionEnd, "claude-code", &input("a", &blog), &at),
            ""
        );
        assert!(!st.path().join(ENDED).exists());
        assert_eq!(
            run(
                Event::SessionEnd,
                "claude-code",
                &input("a", &shop.join("apps/web")),
                &at
            ),
            ""
        );
        // Another agent, another tree: not theirs.
        assert_eq!(
            run(Event::SessionStart, "codex", &input("b", &shop), &at),
            "Start the timer."
        );
        assert_eq!(
            run(Event::SessionStart, "claude-code", &input("b", &blog), &at),
            "Start the timer."
        );
        // The same agent in the same tree, from another folder of it.
        let next = run(Event::SessionStart, "claude-code", &input("c", &shop), &at);
        assert!(next.starts_with("Start the timer.\n\nThe previous session in this working tree (session `a`, transcript `/t/a.jsonl`) ended at "), "{next}");
        assert!(
            next.ends_with("was meant for its end:\n\nReview the session."),
            "{next}"
        );
        // Once.
        assert_eq!(
            run(Event::SessionStart, "claude-code", &input("d", &shop), &at),
            "Start the timer."
        );
    }

    /// `lane-stop`: the texts that apply in the lane's worktree, every time.
    #[test]
    fn a_lane_stop_gives_its_texts_every_time() {
        let st = tempfile::tempdir().unwrap();
        let mut timer = entry("timer", hook(Event::LaneStop, None, "Stop the timer."));
        timer.hook.upstream = Some("timetrack".into());
        let mut shop = entry("shop", hook(Event::LaneStop, None, "Shop teardown."));
        shop.repos = vec!["acme/shop".into()];
        let index = Index {
            hooks: vec![
                timer,
                shop,
                entry("x", hook(Event::SessionStart, None, "Start.")),
            ],
            agents: Vec::new(),
            without: Vec::new(),
        };
        std::fs::write(st.path().join(INDEX), serde_json::to_vec(&index).unwrap()).unwrap();
        let accepted = std::cell::Cell::new(true);
        let at = Lookup {
            state: st.path(),
            origin: &|p| {
                p.ends_with("shop")
                    .then(|| "git@github.com:acme/shop.git".into())
            },
            accepted: &|_, name, _| name == "timetrack" && accepted.get(),
            warn: &|e| panic!("{e}"),
        };
        let shop_tree = Path::new("/src/shop");
        for _ in 0..2 {
            assert_eq!(
                texts_for(Event::LaneStop, shop_tree, &at),
                "Stop the timer.\n\nShop teardown."
            );
        }
        accepted.set(false);
        assert_eq!(texts_for(Event::LaneStop, Path::new("/src/blog"), &at), "");
    }

    #[test]
    fn the_index_reaches_the_agents_switched_on_that_have_hooks() {
        let (st, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        for d in [".claude", ".codex"] {
            std::fs::create_dir_all(home.path().join(d)).unwrap();
        }
        let file = |a: &Agent| {
            let rel = match a.id.as_str() {
                "claude-code" => ".claude/settings.json",
                "codex" => ".codex/hooks.json",
                _ => return None,
            };
            Some(home.path().join(rel))
        };
        let hooks = vec![entry("work", hook(Event::SessionStart, None, "Load work."))];
        let on = |a: &Agent| ["claude-code", "cursor"].contains(&a.id.as_str());
        let errors = apply(st.path(), hooks.clone(), &on, &file);
        assert!(errors.is_empty(), "{errors:?}");
        let i = read_index(st.path());
        assert_eq!(i.agents, ["claude-code"]);
        assert_eq!(i.without, ["Cursor"]);
        let claude = std::fs::read_to_string(home.path().join(".claude/settings.json")).unwrap();
        assert!(claude.contains("SessionStart") && !claude.contains("UserPromptSubmit"));
        assert!(!home.path().join(".codex/hooks.json").exists());

        // Switched off: the entry goes, and with it the file's `hooks`.
        apply(st.path(), hooks, &|_| false, &file);
        let claude = std::fs::read_to_string(home.path().join(".claude/settings.json")).unwrap();
        assert_eq!(claude.trim(), "{}");
        assert!(read_index(st.path()).agents.is_empty());
    }

    /// A pool's upstream the agent has as a server of its own, at the
    /// pool's URL, is there for its hooks though never accepted (#473); the
    /// agent's configuration is read only when it is not accepted.
    #[test]
    fn an_upstream_the_agent_configured_itself_counts_as_there() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("pool-upstreams.json"),
            r#"{"default":[{"name":"timetrack","url":"https://timetrack.example/mcp"}]}"#,
        )
        .unwrap();
        let own = |url: &'static str| {
            move || {
                let cfg = tempfile::NamedTempFile::new().unwrap();
                std::fs::write(
                    cfg.path(),
                    serde_json::json!({ "mcpServers": { "tt": { "type": "http", "url": url } } })
                        .to_string(),
                )
                .unwrap();
                crate::agentmcp::claude(cfg.path(), None)
            }
        };
        let s = dir.path();
        assert!(upstream_there(
            s,
            "default",
            "timetrack",
            &own("https://TimeTrack.example/mcp/")
        ));
        assert!(!upstream_there(
            s,
            "default",
            "timetrack",
            &own("https://other.example/mcp")
        ));
        assert!(!upstream_there(
            s,
            "default",
            "other",
            &own("https://timetrack.example/mcp")
        ));
        assert!(!upstream_there(
            s,
            "elsewhere",
            "timetrack",
            &own("https://timetrack.example/mcp")
        ));
        // Accepted: the agent's configuration is not read at all.
        std::fs::write(
            s.join("upstreams.toml"),
            "[[pool]]\npool = \"default\"\nname = \"timetrack\"\naccepted = true\nurl = \"https://timetrack.example/mcp\"\n",
        )
        .unwrap();
        assert!(upstream_there(s, "default", "timetrack", &|| panic!(
            "read"
        )));
    }

    #[test]
    fn the_dispatcher_gives_each_text_once_where_it_applies() {
        let st = tempfile::tempdir().unwrap();
        let mut bound = entry("shop", hook(Event::SessionStart, None, "Shop rules."));
        bound.repos = vec!["acme/shop".into()];
        let mut timer = entry("timer", hook(Event::SessionStart, None, "Start the timer."));
        timer.hook.upstream = Some("timetrack".into());
        let ticket = entry(
            "ticket",
            hook(
                Event::PromptSubmit,
                Some(r"ABC-[0-9]+"),
                "Ticket checklist.",
            ),
        );
        let index = Index {
            hooks: vec![bound, timer, ticket],
            agents: vec!["claude-code".into()],
            without: Vec::new(),
        };
        std::fs::write(st.path().join(INDEX), serde_json::to_vec(&index).unwrap()).unwrap();
        let accepted = std::cell::Cell::new(false);
        let at = Lookup {
            state: st.path(),
            origin: &|p| {
                p.ends_with("shop")
                    .then(|| "git@github.com:acme/shop.git".into())
            },
            accepted: &|pool, name, _| pool == "default" && name == "timetrack" && accepted.get(),
            warn: &|e| panic!("{e}"),
        };
        let start = |sid: &str, cwd: &str, source: &str| {
            let input = serde_json::json!({ "session_id": sid, "cwd": cwd, "source": source });
            run(Event::SessionStart, "claude-code", &input.to_string(), &at)
        };
        // Another repository, the upstream not accepted: nothing.
        assert_eq!(start("s1", "/src/blog", "startup"), "");
        accepted.set(true);
        assert_eq!(
            start("s2", "/src/shop", "startup"),
            "Shop rules.\n\nStart the timer."
        );
        // Resumed: given already. Compacted: given again.
        assert_eq!(start("s2", "/src/shop", "resume"), "");
        assert_eq!(
            start("s2", "/src/shop", "compact"),
            "Shop rules.\n\nStart the timer."
        );
        // An agent without the hub's entry gets nothing.
        let input = serde_json::json!({ "session_id": "s3", "cwd": "/src/shop" }).to_string();
        assert_eq!(run(Event::SessionStart, "codex", &input, &at), "");

        let prompt = |text: &str| {
            let input =
                serde_json::json!({ "session_id": "s2", "cwd": "/src/blog", "prompt": text });
            run(Event::PromptSubmit, "claude-code", &input.to_string(), &at)
        };
        assert_eq!(prompt("fix the login"), "");
        assert_eq!(prompt("look at ABC-12 please"), "Ticket checklist.");
        assert_eq!(prompt("ABC-12 again"), "");
        assert_eq!(prompt("now ABC-13"), "Ticket checklist.");
        // Input it cannot read: nothing, no error.
        assert_eq!(run(Event::PromptSubmit, "claude-code", "not json", &at), "");

        // The activity (#482): each start, each prompt that was given
        // something, with the hooks by where they are, never a text.
        let records = crate::activity::read(st.path());
        let hooks: Vec<(String, Vec<String>)> = records
            .iter()
            .map(|r| match r {
                crate::activity::Record::Hook { event, hooks, .. } => (
                    event.clone(),
                    hooks.iter().map(|h| h.skill.clone()).collect(),
                ),
                crate::activity::Record::Tool { .. } => unreachable!(),
            })
            .collect();
        assert_eq!(hooks.len(), 6, "{hooks:?}");
        assert_eq!(hooks[1].1, ["shop", "timer"]);
        assert_eq!(
            hooks[4],
            ("prompt-submit".to_string(), vec!["ticket".to_string()])
        );
        let text = std::fs::read_dir(st.path().join("activity"))
            .unwrap()
            .flatten()
            .map(|e| std::fs::read_to_string(e.path()).unwrap())
            .collect::<String>();
        assert!(
            !text.contains("Ticket checklist") && !text.contains("ABC-12"),
            "{text}"
        );
    }
}
