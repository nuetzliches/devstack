//! What devstack did for the coding agents (#482): each hook it gave text
//! for, and each tool call that passed `devstack mcp`. The Skills page shows
//! it beside the flow of the hooks.
//!
//! - **Only what happened.** A record holds the event, the agent, its
//!   session, the working tree, the hooks given (pool, set, skill, place) or
//!   the tool's name, how long it took and whether it failed. Never the
//!   prompt, a hook's text, a tool's arguments or its answer. It stays on
//!   this machine (not in the pool's telemetry) and goes after a week.
//! - **One file per process and day.** The writers are short-lived
//!   processes (`devstack hook`) and long-lived ones (`devstack mcp`), many
//!   at once: each appends to its own `activity/<day>-<pid>.jsonl`, so no
//!   two write into one file.
//! - **A session** is the agent's `session_id` from a hook's input. A tool
//!   call carries none: it belongs to the latest session that started in its
//!   working tree before it.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tokio::sync::watch;

/// The folder in the hub's state.
const DIR: &str = "activity";
/// How long a record is kept.
const KEEP: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 3600);
/// At most this many records an answer carries (the latest).
const MAX: usize = 500;

/// A hook given, by where it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookRef {
    pub pool: String,
    pub set: String,
    pub skill: String,
    pub index: usize,
}

/// One line of an activity file. Times are Unix ms as `u64`: an
/// internally tagged enum cannot read a `u128` back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Record {
    /// The dispatcher ran for an event: the hooks it gave text for.
    Hook {
        at: u64,
        event: String,
        agent: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        tree: String,
        #[serde(default)]
        hooks: Vec<HookRef>,
        /// `session-end`: the end was kept for the next start.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        recorded: bool,
    },
    /// A tool call through `devstack mcp`.
    Tool {
        at: u64,
        tool: String,
        /// The MCP client's name (`clientInfo.name`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client: Option<String>,
        tree: String,
        pid: u32,
        ms: u64,
        ok: bool,
    },
}

impl Record {
    pub fn at(&self) -> u64 {
        match self {
            Record::Hook { at, .. } | Record::Tool { at, .. } => *at,
        }
    }

    fn tree(&self) -> &str {
        match self {
            Record::Hook { tree, .. } | Record::Tool { tree, .. } => tree,
        }
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// The working tree a path is in: the nearest folder above it (or it)
/// holding `.git`; the path itself outside a repository. Written the same
/// way whatever case or separators the caller used.
pub fn tree_of(cwd: &Path) -> String {
    let tree = cwd
        .ancestors()
        .find(|d| d.join(".git").exists())
        .unwrap_or(cwd);
    let s = tree.to_string_lossy().replace('\\', "/");
    let s = s.trim_end_matches('/');
    if cfg!(windows) {
        s.to_lowercase()
    } else {
        s.to_string()
    }
}

/// Append `r` to this process's file of today under `state`.
pub fn write(state: &Path, r: &Record) -> Result<(), String> {
    let dir = state.join(DIR);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let day = jiff::Zoned::now().strftime("%Y-%m-%d").to_string();
    let path = dir.join(format!("{day}-{}.jsonl", std::process::id()));
    let mut line = serde_json::to_string(r).map_err(|e| e.to_string())?;
    line.push('\n');
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| f.write_all(line.as_bytes()))
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// [`write`] into this machine's hub state; an error goes to the hub's
/// diagnostic log, never to the caller (a hook or a tool call goes on).
pub fn write_here(r: &Record) {
    if let Err(e) = write(&crate::state_dir(), r) {
        devstack_daemon::diag::DiagLog::open(crate::log_path()).write(
            "warn",
            "activity",
            serde_json::json!({ "error": e }),
        );
    }
}

/// Every record kept under `state`, oldest first; files older than a week
/// go.
pub fn read(state: &Path) -> Vec<Record> {
    let Ok(rd) = std::fs::read_dir(state.join(DIR)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let path = e.path();
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > KEEP);
        if old {
            let _ = std::fs::remove_file(&path);
            continue;
        }
        if path.extension().is_none_or(|x| x != "jsonl") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        out.extend(
            text.lines()
                .filter_map(|l| serde_json::from_str::<Record>(l).ok()),
        );
    }
    out.sort_by_key(Record::at);
    out
}

/// A print of the activity folder: changes when a record is added.
fn print(state: &Path) -> (usize, u64) {
    let Ok(rd) = std::fs::read_dir(state.join(DIR)) else {
        return (0, 0);
    };
    rd.flatten()
        .filter_map(|e| e.metadata().ok())
        .fold((0, 0), |(n, len), m| (n + 1, len + m.len()))
}

/// An agent's session, from its hooks' records.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Session {
    pub agent: String,
    pub session: String,
    pub tree: String,
    pub started: u64,
    pub last: u64,
    /// It reported its end (`session-end`).
    pub ended: bool,
}

/// What `GET /hub/activity` answers.
#[derive(Debug, Clone, Serialize)]
pub struct View {
    pub sessions: Vec<Session>,
    /// The records asked for, newest last; a tool call with the session it
    /// is put to and, for an upstream's tool, the skills whose hooks name
    /// that upstream.
    pub records: Vec<Value>,
}

/// Which upstream a tool belongs to: its prefix's (pool, name), for the
/// pools' upstreams.
pub type Prefixes = Vec<(String, String, String)>;

/// Sessions and records, `tree` and `session` narrowing them, `since` (Unix
/// ms) leaving out older ones. `prefixes` gives an upstream tool's (pool,
/// name), `index` the skills whose hooks name an upstream.
pub fn view(
    records: &[Record],
    tree: Option<&str>,
    session: Option<&str>,
    since: Option<u64>,
    prefixes: &Prefixes,
    index: &crate::agenthooks::Index,
) -> View {
    let mut sessions: Vec<Session> = Vec::new();
    for r in records {
        let Record::Hook {
            at,
            agent,
            session: Some(id),
            tree,
            event,
            ..
        } = r
        else {
            continue;
        };
        match sessions
            .iter_mut()
            .find(|s| &s.agent == agent && &s.session == id)
        {
            Some(s) => {
                s.last = s.last.max(*at);
                s.ended |= event == "session-end";
            }
            None => sessions.push(Session {
                agent: agent.clone(),
                session: id.clone(),
                tree: tree.clone(),
                started: *at,
                last: *at,
                ended: event == "session-end",
            }),
        }
    }
    let mut out = Vec::new();
    for r in records {
        let mut v = serde_json::to_value(r).expect("serialises");
        // A record without a session of its own (a tool call, a lane's
        // stop): the latest session that started in its tree before it.
        let own = matches!(
            r,
            Record::Hook {
                session: Some(_),
                ..
            }
        );
        if !own
            && let Some(s) = sessions
                .iter_mut()
                .filter(|s| s.tree == r.tree() && s.started <= r.at())
                .max_by_key(|s| s.started)
        {
            s.last = s.last.max(r.at());
            v["session"] = Value::String(s.session.clone());
            if matches!(r, Record::Tool { .. }) {
                v["agent"] = Value::String(s.agent.clone());
            }
        }
        // A pool's upstream's tool: the upstream, and the skills whose hooks
        // name it.
        if let Record::Tool { tool, .. } = r
            && let Some((prefix, _)) = tool.split_once("__")
            && let Some((pool, name, _)) = prefixes.iter().find(|(_, _, p)| p == prefix)
        {
            v["upstream"] = Value::String(name.clone());
            let mut skills: Vec<String> = index
                .hooks
                .iter()
                .filter(|h| &h.pool == pool && h.hook.upstream.as_deref() == Some(name))
                .map(|h| h.skill.clone())
                .collect();
            skills.dedup();
            v["skills"] = serde_json::json!(skills);
        }
        if tree.is_some_and(|t| t != r.tree())
            || session.is_some_and(|s| v["session"].as_str() != Some(s))
            || since.is_some_and(|t| r.at() < t)
        {
            continue;
        }
        out.push(v);
    }
    if out.len() > MAX {
        out.drain(..out.len() - MAX);
    }
    sessions.sort_by_key(|s| std::cmp::Reverse(s.last));
    View {
        sessions,
        records: out,
    }
}

static FEED: OnceLock<watch::Sender<u64>> = OnceLock::new();

/// Look at the activity folder every two seconds while a dashboard
/// listens, and tell it when a record was added.
pub fn follow() {
    if FEED.set(watch::channel(0).0).is_err() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("activity".into())
        .spawn(|| {
            let state = crate::state_dir();
            let mut last = print(&state);
            loop {
                std::thread::sleep(std::time::Duration::from_secs(2));
                let tx = FEED.get().expect("set above");
                if tx.receiver_count() == 0 {
                    continue;
                }
                let now = print(&state);
                if now != last {
                    last = now;
                    tx.send_replace(now_ms());
                }
            }
        });
}

/// When a record was last added (Unix ms; 0: not since the hub started).
pub fn subscribe() -> watch::Receiver<u64> {
    match FEED.get() {
        Some(f) => f.subscribe(),
        None => watch::channel(0).1,
    }
}

/// The hub's state folder, for the handler.
pub fn state() -> PathBuf {
    crate::state_dir()
}

#[cfg(test)]
mod tests {
    use super::*;
    use devstack_node::hooks::{Event, Hook, Once};

    fn hook(at: u64, event: &str, session: &str, tree: &str) -> Record {
        Record::Hook {
            at,
            event: event.into(),
            agent: "claude".into(),
            session: Some(session.into()),
            tree: tree.into(),
            hooks: Vec::new(),
            recorded: event == "session-end",
        }
    }

    fn tool(at: u64, name: &str, tree: &str) -> Record {
        Record::Tool {
            at,
            tool: name.into(),
            client: Some("claude-code".into()),
            tree: tree.into(),
            pid: 7,
            ms: 12,
            ok: true,
        }
    }

    #[test]
    fn records_go_one_file_per_process_and_come_back_in_order() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), &tool(20, "status", "/w")).unwrap();
        write(d.path(), &hook(10, "session-start", "s1", "/w")).unwrap();
        let files: Vec<_> = std::fs::read_dir(d.path().join(DIR)).unwrap().collect();
        assert_eq!(files.len(), 1, "one file for this process");
        let back = read(d.path());
        assert_eq!(back.iter().map(Record::at).collect::<Vec<_>>(), [10, 20]);
    }

    /// No prompt, text or argument can be written: the record has no field
    /// for them.
    #[test]
    fn a_record_holds_what_happened_only() {
        let v = serde_json::to_value(tool(1, "status", "/w")).unwrap();
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        for k in ["arguments", "prompt", "text", "result", "context"] {
            assert!(!keys.contains(&k), "{keys:?}");
        }
    }

    #[test]
    fn a_tool_call_goes_to_the_latest_session_started_in_its_tree() {
        let entry = |skill: &str, upstream: &str| crate::agenthooks::Entry {
            pool: "default".into(),
            set: "core".into(),
            skill: skill.into(),
            index: 0,
            repos: Vec::new(),
            hook: Hook {
                event: Event::SessionStart,
                pattern: None,
                once: Once::Session,
                upstream: Some(upstream.into()),
                context: "x".into(),
            },
        };
        let index = crate::agenthooks::Index {
            hooks: vec![entry("work-session", "timetrack")],
            agents: vec!["claude".into()],
            without: Vec::new(),
        };
        let prefixes = vec![(
            "default".to_string(),
            "timetrack".to_string(),
            "nuts_timetrack".to_string(),
        )];
        let records = vec![
            hook(10, "session-start", "s1", "/a"),
            tool(15, "nuts_timetrack__start_timer", "/a"),
            hook(20, "session-start", "s2", "/a"),
            hook(21, "session-start", "s3", "/b"),
            tool(25, "status", "/a"),
            tool(26, "status", "/c"),
            hook(30, "session-end", "s2", "/a"),
            // A lane's stop has no session of its own.
            Record::Hook {
                at: 31,
                event: "lane-stop".into(),
                agent: "-".into(),
                session: None,
                tree: "/a".into(),
                hooks: Vec::new(),
                recorded: false,
            },
        ];
        let v = view(&records, None, None, None, &prefixes, &index);
        assert_eq!(v.records[1]["session"], "s1");
        assert_eq!(v.records[1]["upstream"], "timetrack");
        assert_eq!(v.records[1]["skills"], serde_json::json!(["work-session"]));
        assert_eq!(v.records[4]["session"], "s2", "the later start in /a");
        assert!(v.records[5].get("session").is_none(), "no session in /c");
        let s2 = v.sessions.iter().find(|s| s.session == "s2").unwrap();
        assert!(s2.ended);
        assert_eq!(s2.last, 31, "the lane's stop counts to it");
        assert_eq!(v.records[7]["session"], "s2", "the lane's stop");
        assert!(v.records[7].get("agent").unwrap() == "-");
        let only = view(&records, Some("/a"), Some("s2"), None, &prefixes, &index);
        assert_eq!(only.records.len(), 4, "{:?}", only.records);
        let since = view(&records, None, None, Some(25), &prefixes, &index);
        assert_eq!(since.records.len(), 4);
    }
}
