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
//!   upstream is accepted, the pattern matches the prompt, not given yet in
//!   this session) and prints their texts. It never blocks a prompt: on
//!   any error it prints nothing.

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

/// An event's name in an agent's configuration.
fn event_name(format: HookFormat, e: Event) -> &'static str {
    match (format, e) {
        (HookFormat::Claude | HookFormat::Codex, Event::SessionStart) => "SessionStart",
        (HookFormat::Claude | HookFormat::Codex, Event::PromptSubmit) => "UserPromptSubmit",
    }
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
    for e in [Event::SessionStart, Event::PromptSubmit] {
        let name = event_name(format, e);
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
        if !events.contains(&h.hook.event) {
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
}

/// Where the dispatcher looks things up: the hub's state and git.
pub struct Lookup<'a> {
    pub state: &'a Path,
    /// The `origin` of the working tree at a path.
    pub origin: &'a dyn Fn(&Path) -> Option<String>,
    /// Whether the user accepted a pool's upstream.
    pub accepted: &'a dyn Fn(&str, &str) -> bool,
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
    let cwd = input.cwd.as_deref().map(PathBuf::from);
    let mut origin: Option<Option<String>> = None;
    let prompt = input.prompt.as_deref().unwrap_or("");
    let mut out: Vec<&str> = Vec::new();
    let before = given.len();
    for h in index.hooks.iter().filter(|h| h.hook.event == event) {
        if let Some(u) = &h.hook.upstream
            && !(at.accepted)(&h.pool, u)
        {
            continue;
        }
        if !h.repos.is_empty() {
            let o = origin.get_or_insert_with(|| cwd.as_deref().and_then(|c| (at.origin)(c)));
            if !o
                .as_deref()
                .is_some_and(|o| devstack_node::skills::repo_matches(&h.repos, o))
            {
                continue;
            }
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
        out.push(&h.hook.context);
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

/// The dispatcher as `devstack hook` runs it: this machine's hub state,
/// its git, its upstreams.
pub fn run_here(event: Event, agent: &str, input: &str) -> String {
    let state = crate::state_dir();
    run(
        event,
        agent,
        input,
        &Lookup {
            state: &state,
            origin: &|p| crate::branches::origin(p),
            accepted: &|pool, name| crate::upstreams::Upstreams::pool_accepted(&state, pool, name),
            warn: &|error| {
                devstack_daemon::diag::DiagLog::open(crate::log_path()).write(
                    "warn",
                    "agent_hook",
                    serde_json::json!({ "event": event.as_str(), "agent": agent, "error": error }),
                );
            },
        },
    )
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
            accepted: &|pool, name| pool == "default" && name == "timetrack" && accepted.get(),
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
    }
}
