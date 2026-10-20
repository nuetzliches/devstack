//! The diagnostic log of a daemon or the hub (plan §11): one JSON object per
//! line, every decision with its reason, so that "why did that happen" can be
//! answered afterwards, from the file, by a person or an agent.
//!
//! ```json
//! {"at":"2026-10-15T12:03:01.234+02:00","level":"info","event":"candidate","service":"api","state":"failed","reason":"..."}
//! ```
//!
//! - `at`: local time with its offset (readable, and unambiguous).
//! - `level`: `info`, `warn` or `error`.
//! - `event`: what happened; the other fields depend on it.
//!
//! The file rotates at [`MAX_BYTES`] to `<name>.1` (one generation), so it
//! stays small enough to read whole. Writing never fails the caller: a log
//! that cannot be written is not worth stopping a service for.
//!
//! What a process writes to stdout/stderr (a panic, say) goes to `<name>.out`
//! next to it, not here: this file stays parseable.
//!
//! With `DEVSTACK_OTLP_ENDPOINT` (e.g. `http://127.0.0.1:4318`) every line is
//! also sent as an OTLP log record (HTTP/JSON, `/v1/logs`), batched, as
//! `service.name` `devstack-daemon` or `devstack-hub`. Optional and lossy by
//! design: a collector that is down costs nothing but the records.
//!
//! What leaves the machine (that collector, a pool's, ADR-0037) carries no
//! service output: a reason or error of several lines is devstack's own first
//! line, then the output of a build, a job or a service (which may hold a
//! connection string); only the first line goes out. Command lines,
//! environments, process output and paths on this machine stay in the file
//! ([`LOCAL_ONLY`]); a path on this machine in a text (an error, a reason)
//! goes out with its root replaced (the repository as `<repo>`, a lane's
//! worktree as `<worktree>`, devstack's state as `<devstack>`, the home
//! directory as `~`, see [`add_root`]); user names in other home paths and
//! URL credentials are masked.
//! The file keeps all. A nested value goes out as dotted attributes (`status` `{"state":…}` as
//! `devstack.status.state`), each record with the repository it concerns.

use serde_json::{Map, Value};
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Rotate beyond this size.
pub const MAX_BYTES: u64 = 5 * 1024 * 1024;

pub struct DiagLog {
    path: PathBuf,
    file: Mutex<Option<File>>,
}

impl DiagLog {
    pub fn open(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let file = open_append(&path);
        Self {
            path,
            file: Mutex::new(file),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// One line: `fields` must be an object (anything else is ignored).
    pub fn write(&self, level: &str, event: &str, fields: Value) {
        let mut line = Map::new();
        line.insert("at".into(), Value::String(now()));
        line.insert("level".into(), Value::String(level.into()));
        line.insert("event".into(), Value::String(event.into()));
        if let Value::Object(f) = fields {
            for (k, v) in f {
                line.entry(k).or_insert(v);
            }
        }
        // The main working tree is the lane `main` (as `DEVSTACK_LANE` and
        // `main_slot` say), not an empty name.
        if line.get("lane").and_then(Value::as_str) == Some("") {
            line.insert("lane".into(), Value::String("main".into()));
        }
        let otlp = OTLP.get().and_then(Option::as_ref);
        if otlp.is_some() || crate::telemetry::active() {
            let out = outbound(&line);
            crate::telemetry::record(&out);
            if let Some(tx) = otlp {
                let _ = tx.try_send(Value::Object(out));
            }
        }
        let mut text = Value::Object(line).to_string();
        text.push('\n');
        let mut guard = self.file.lock().unwrap();
        if guard
            .as_ref()
            .and_then(|f| f.metadata().ok())
            .is_some_and(|m| m.len() > MAX_BYTES)
        {
            *guard = None; // close before renaming (Windows)
            let _ = std::fs::rename(&self.path, rotated(&self.path));
        }
        if guard.is_none() {
            *guard = open_append(&self.path);
        }
        if let Some(f) = guard.as_mut() {
            let _ = f.write_all(text.as_bytes());
        }
    }
}

/// Fields that stay in the file, at any depth (ADR-0037 point 4: never
/// command lines, environment values, service output, paths outside the
/// repository): a command line (`run`, `cmd`, `command_line`, `args`), an
/// environment (`env`), a process's output (`output`), a path on this
/// machine (`repo`, `worktree`, `file`, `cwd`, `dir`). A diagnostic line
/// that carries such a value uses one of these names.
pub const LOCAL_ONLY: &[&str] = &[
    "run",
    "cmd",
    "command_line",
    "args",
    "env",
    "output",
    "repo",
    "worktree",
    "file",
    "cwd",
    "dir",
];

/// A line as it may leave the machine: the [`LOCAL_ONLY`] fields left out,
/// every text cut to its first line (ADR-0037: never service output), the
/// roots [`add_root`] knows replaced by their names, a user's name in a home
/// directory path (`C:\Users\<name>`, `/home/<name>`) replaced by `~`, and
/// the credentials of a URL (`scheme://user:pw@host`) by `***`.
pub fn outbound(line: &Map<String, Value>) -> Map<String, Value> {
    fn clean(v: &Value) -> Value {
        match v {
            Value::String(s) => {
                let s = match s.split_once('\n') {
                    Some((first, _)) => format!("{} …", first.trim_end()),
                    None => s.clone(),
                };
                Value::String(scrub(&s))
            }
            Value::Array(a) => Value::Array(a.iter().map(clean).collect()),
            Value::Object(o) => Value::Object(fields(o)),
            other => other.clone(),
        }
    }
    fn fields(o: &Map<String, Value>) -> Map<String, Value> {
        o.iter()
            .filter(|(k, _)| !LOCAL_ONLY.contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), clean(v)))
            .collect()
    }
    fields(line)
}

/// Paths on this machine whose text never leaves it: each spelling of a
/// root and the name it goes out as, longest first.
static ROOTS: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

/// Know `path` as a root on this machine: in a text that leaves it (see
/// [`outbound`]), the path goes out as `name` (`<repo>`, `<worktree>`), with
/// what follows it inside the root (`<repo>\src\a.json`). A root too short
/// to be one (`C:\`, `/`) is ignored.
pub fn add_root(path: &Path, name: &str) {
    let text = path.to_string_lossy();
    let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
    let text = text.trim_end_matches(['\\', '/']);
    if text.len() < 4 || !text.contains(['\\', '/']) {
        return;
    }
    // As written, with either separator, and as `{:?}` writes it (an
    // error's `about ["D:\\src"]`).
    let slashes = text.replace('\\', "/");
    let spellings = [
        text.to_string(),
        slashes.clone(),
        slashes.replace('/', "\\"),
        slashes.replace('/', "\\\\"),
    ];
    let mut roots = ROOTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for sp in spellings {
        if !roots.iter().any(|(r, _)| r.eq_ignore_ascii_case(&sp)) {
            roots.push((sp, name.to_string()));
        }
    }
    roots.sort_by_key(|(r, _)| std::cmp::Reverse(r.len()));
}

/// `s` with each known root (see [`add_root`]) replaced by its name, where
/// it ends a path component (`D:\src\shop` is no root of `D:\src\shop-fix`).
/// ASCII case is ignored, as Windows paths do.
fn replace_roots(s: &str) -> String {
    let roots = ROOTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut s = s.to_string();
    for (root, name) in roots.iter() {
        let needle = root.to_ascii_lowercase();
        let mut from = 0;
        // ASCII lowercase keeps the byte offsets.
        while let Some(i) = s[from..].to_ascii_lowercase().find(&needle) {
            let start = from + i;
            let end = start + needle.len();
            let whole = s[end..]
                .chars()
                .next()
                .is_none_or(|c| !(c.is_alphanumeric() || matches!(c, '-' | '_' | '.')));
            if whole {
                s.replace_range(start..end, name);
                from = start + name.len();
            } else {
                from = end;
            }
            if from >= s.len() {
                break;
            }
        }
    }
    s
}

/// A text without the roots on this machine, user names in home
/// directories and credentials in URLs (see [`outbound`]).
fn scrub(s: &str) -> String {
    let mut s = replace_roots(s);
    // ASCII lowercase keeps the byte offsets.
    for marker in ["\\users\\", "/users/", "/home/"] {
        let mut from = 0;
        while let Some(i) = s[from..].to_ascii_lowercase().find(marker) {
            let at = from + i;
            // A path's start (`C:\Users`, ` /home`, `"/Users`, `file:///Users`),
            // not a segment of a URL or a longer path (`https://x.example/users/42`).
            let starts = s[..at]
                .chars()
                .next_back()
                .is_none_or(|c| !(c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | '%')));
            if !starts {
                from = at + 1;
                continue;
            }
            let start = at + marker.len();
            let end = s[start..]
                .find(|c: char| matches!(c, '\\' | '/' | '"' | '\'') || c.is_whitespace())
                .map_or(s.len(), |e| start + e);
            if end > start && &s[start..end] != "~" {
                s.replace_range(start..end, "~");
            }
            from = start + 1;
            if from >= s.len() {
                break;
            }
        }
    }
    let mut from = 0;
    while let Some(i) = s[from..].find("://") {
        let start = from + i + 3;
        let end = s[start..]
            .find(|c: char| matches!(c, '/' | '"' | '\'' | '?' | '#') || c.is_whitespace())
            .map_or(s.len(), |e| start + e);
        if let Some(at) = s[start..end].rfind('@') {
            s.replace_range(start..start + at, "***");
            from = start + 4;
        } else {
            from = end;
        }
        if from >= s.len() {
            break;
        }
    }
    s
}

fn open_append(path: &Path) -> Option<File> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()
}

/// `daemon.log` -> `daemon.log.1`.
pub fn rotated(path: &Path) -> PathBuf {
    let mut p = path.as_os_str().to_owned();
    p.push(".1");
    PathBuf::from(p)
}

fn now() -> String {
    jiff::Zoned::now()
        .strftime("%Y-%m-%dT%H:%M:%S%.3f%:z")
        .to_string()
}

static GLOBAL: OnceLock<DiagLog> = OnceLock::new();

static OTLP: OnceLock<Option<std::sync::mpsc::SyncSender<Value>>> = OnceLock::new();

/// The process's log (the daemon's or the hub's); set once at start.
pub fn init(path: impl Into<PathBuf>) -> &'static DiagLog {
    let path = path.into();
    let service = match path.file_stem().and_then(|s| s.to_str()) {
        Some("hub") => "devstack-hub",
        _ => "devstack-daemon",
    };
    OTLP.get_or_init(|| {
        let endpoint = std::env::var("DEVSTACK_OTLP_ENDPOINT")
            .ok()
            .filter(|e| !e.trim().is_empty())?;
        let (tx, rx) = std::sync::mpsc::sync_channel(4096);
        let url = format!("{}/v1/logs", endpoint.trim_end_matches('/'));
        std::thread::Builder::new()
            .name("otlp-export".into())
            .spawn(move || export_loop(&url, service, &rx))
            .ok()?;
        Some(tx)
    });
    // devstack's state goes as itself, though it is below the home directory.
    if let Some(state) = crate::token::token_path().parent() {
        add_root(state, "<devstack>");
    }
    for var in ["USERPROFILE", "HOME"] {
        if let Some(home) = std::env::var_os(var).filter(|h| !h.is_empty()) {
            add_root(Path::new(&home), "~");
        }
    }
    // The stacks this machine knows (the hub's lines name them too).
    for stack in crate::registry::list() {
        add_root(&stack.repo, "<repo>");
    }
    crate::telemetry::start(service);
    GLOBAL.get_or_init(|| DiagLog::open(path))
}

/// Sends what arrives, in batches of up to 200 or every 2 s.
fn export_loop(url: &str, service: &str, rx: &std::sync::mpsc::Receiver<Value>) {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(3)))
        .http_status_as_error(false)
        .build()
        .new_agent();
    loop {
        let Ok(first) = rx.recv() else { return };
        let mut batch = vec![first];
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while batch.len() < 200 {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            match rx.recv_timeout(left) {
                Ok(v) => batch.push(v),
                Err(_) => break,
            }
        }
        let _ = agent
            .post(url)
            .header("Content-Type", "application/json")
            .send(otlp_body(service, crate::telemetry::repository(), &batch).to_string());
    }
}

/// An OTLP attribute value of a scalar (anything else as its JSON text).
fn otlp_value(v: &Value) -> Value {
    match v {
        Value::String(s) => serde_json::json!({ "stringValue": s }),
        Value::Bool(b) => serde_json::json!({ "boolValue": b }),
        Value::Number(n) if n.is_i64() || n.is_u64() => {
            serde_json::json!({ "intValue": n.to_string() })
        }
        Value::Number(n) => serde_json::json!({ "doubleValue": n.as_f64() }),
        other => serde_json::json!({ "stringValue": other.to_string() }),
    }
}

/// `v` as attributes under `key`: an object flattened to dotted keys
/// (`devstack.status.state`), so a collector's store can filter on them; a
/// list of scalars as an OTLP array; nulls left out.
fn flatten(key: &str, v: &Value, out: &mut Vec<Value>) {
    match v {
        Value::Null => {}
        Value::Object(o) => {
            for (k, x) in o {
                flatten(&format!("{key}.{k}"), x, out);
            }
        }
        Value::Array(a) if a.iter().all(|x| !x.is_object() && !x.is_array()) => {
            let values: Vec<Value> = a.iter().filter(|x| !x.is_null()).map(otlp_value).collect();
            out.push(
                serde_json::json!({ "key": key, "value": { "arrayValue": { "values": values } } }),
            );
        }
        other => out.push(serde_json::json!({ "key": key, "value": otlp_value(other) })),
    }
}

/// OTLP/JSON `ExportLogsServiceRequest` for diagnostic lines; `repository`
/// (the stack this process runs) labels every record that has none.
pub fn otlp_body(service: &str, repository: Option<&str>, lines: &[Value]) -> Value {
    let observed = jiff::Timestamp::now().as_nanosecond().to_string();
    let records: Vec<Value> = lines
        .iter()
        .filter_map(Value::as_object)
        .map(|m| {
            let level = m.get("level").and_then(Value::as_str).unwrap_or("info");
            let (num, text) = match level {
                "error" => (17, "ERROR"),
                "warn" => (13, "WARN"),
                _ => (9, "INFO"),
            };
            let nanos = m
                .get("at")
                .and_then(Value::as_str)
                .and_then(|a| a.parse::<jiff::Timestamp>().ok())
                .map(|t| t.as_nanosecond().to_string())
                .unwrap_or_else(|| observed.clone());
            let mut attributes = Vec::new();
            for (k, v) in m {
                if !matches!(k.as_str(), "at" | "level" | "event") {
                    flatten(&format!("devstack.{k}"), v, &mut attributes);
                }
            }
            if let Some(r) = repository
                && !m.contains_key("repository")
            {
                flatten(
                    "devstack.repository",
                    &Value::String(r.into()),
                    &mut attributes,
                );
            }
            serde_json::json!({
                "timeUnixNano": nanos,
                "observedTimeUnixNano": observed,
                "severityNumber": num,
                "severityText": text,
                "body": { "stringValue": m.get("event").and_then(Value::as_str).unwrap_or("") },
                "attributes": attributes,
            })
        })
        .collect();
    serde_json::json!({ "resourceLogs": [{
        "resource": devstack_node::telemetry::resource(service),
        "scopeLogs": [{ "scope": { "name": "devstack" }, "logRecords": records }],
    }]})
}

/// Write to the process's log, if [`init`] ran (tests and the CLI skip it).
pub fn log(level: &str, event: &str, fields: Value) {
    if let Some(d) = GLOBAL.get() {
        d.write(level, event, fields);
    }
}

pub fn info(event: &str, fields: Value) {
    log("info", event, fields);
}

pub fn warn(event: &str, fields: Value) {
    log("warn", event, fields);
}

pub fn error(event: &str, fields: Value) {
    log("error", event, fields);
}

/// A warning that may repeat many times a second (an accept loop that
/// fails, a tunnel per connection): at most one line per `key` every ten
/// seconds, with how many were left out since (`suppressed`).
pub fn warn_limited(key: &str, event: &str, fields: Value) {
    static LAST: Mutex<Option<std::collections::HashMap<String, (std::time::Instant, u64)>>> =
        Mutex::new(None);
    let suppressed = {
        let mut last = LAST.lock().unwrap();
        let map = last.get_or_insert_with(Default::default);
        match map.get_mut(key) {
            Some((at, n)) if at.elapsed() < std::time::Duration::from_secs(10) => {
                *n += 1;
                return;
            }
            Some((at, n)) => {
                *at = std::time::Instant::now();
                std::mem::take(n)
            }
            None => {
                map.insert(key.to_string(), (std::time::Instant::now(), 0));
                0
            }
        }
    };
    let mut fields = fields;
    if suppressed > 0
        && let Value::Object(f) = &mut fields
    {
        f.insert("suppressed".into(), suppressed.into());
    }
    warn(event, fields);
}

/// The last `n` lines of a log and its rotated predecessor, oldest first,
/// optionally only those containing `grep` (case-insensitive).
pub fn tail(path: &Path, n: usize, grep: Option<&str>) -> Vec<String> {
    let needle = grep.map(str::to_lowercase);
    let mut lines: Vec<String> = Vec::new();
    for p in [rotated(path), path.to_path_buf()] {
        if let Ok(text) = std::fs::read_to_string(&p) {
            lines.extend(
                text.lines()
                    .filter(|l| needle.as_ref().is_none_or(|g| l.to_lowercase().contains(g)))
                    .map(str::to_string),
            );
        }
    }
    let skip = lines.len().saturating_sub(n);
    lines.split_off(skip)
}

/// A JSON line as one readable line: `12:03:01 warn  candidate service=api state=failed`.
pub fn pretty(line: &str) -> String {
    let Ok(Value::Object(m)) = serde_json::from_str::<Value>(line) else {
        return line.to_string();
    };
    let time = m
        .get("at")
        .and_then(Value::as_str)
        .and_then(|s| s.get(11..19))
        .unwrap_or("");
    let level = m.get("level").and_then(Value::as_str).unwrap_or("");
    let event = m.get("event").and_then(Value::as_str).unwrap_or("");
    let mut out = format!("{time} {level:<5} {event}");
    for (k, v) in &m {
        if matches!(k.as_str(), "at" | "level" | "event") {
            continue;
        }
        let v = match v {
            Value::String(s) if s.contains(char::is_whitespace) => format!("{s:?}"),
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        out.push_str(&format!(" {k}={v}"));
    }
    out
}

/// Delete the oldest files in `dir` whose name starts with `prefix`, keeping
/// the newest `keep`. A file still open elsewhere (Windows) stays; the next
/// round gets it.
pub fn prune(dir: &Path, prefix: &str, keep: usize) -> usize {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = rd
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(prefix))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    files.sort_by_key(|f| std::cmp::Reverse(f.0));
    files
        .into_iter()
        .skip(keep)
        .filter(|(_, p)| std::fs::remove_file(p).is_ok())
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn service_output_does_not_leave_the_machine() {
        let line = json!({
            "event": "service",
            "status": { "state": "failed", "reason": "job exited with code 1
        Server=db;Password=secret" },
            "paths": ["a
        b"],
            "n": 3,
        });
        let out = Value::Object(outbound(line.as_object().unwrap()));
        let text = out.to_string();
        assert!(!text.contains("secret") && !text.contains("\nb"), "{text}");
        assert_eq!(out["status"]["reason"], "job exited with code 1 …");
        assert_eq!(out["n"], 3);
    }

    #[test]
    fn command_lines_paths_and_credentials_do_not_leave_the_machine() {
        let line = json!({
            "event": "prepare",
            "lane": "main",
            "service": "api",
            "run": "npm ci --token=abc",
            "worktree": "C:\\Users\\jane\\src\\shop-fix",
            "nested": { "env": { "DB": "secret" }, "args": ["--password", "pw"], "state": "ok" },
            "error": "open C:\\Users\\Jane.Doe\\src\\x\\a.json: denied; /home/jdoe/x and /Users/j d",
            "reason": "cannot reach https://bob:hunter2@registry.example/pkg (http://plain.example/x)",
            "path": "/v1/lanes/fix/stop",
        });
        let out = Value::Object(outbound(line.as_object().unwrap()));
        let text = out.to_string();
        for leak in [
            "npm ci", "abc", "jane", "Jane", "jdoe", "secret", "hunter2", "bob",
        ] {
            assert!(!text.contains(leak), "{leak} in {text}");
        }
        assert!(out.get("run").is_none() && out.get("worktree").is_none());
        assert_eq!(out["nested"], json!({ "state": "ok" }));
        assert_eq!(
            out["error"],
            "open C:\\Users\\~\\src\\x\\a.json: denied; /home/~/x and /Users/~ d"
        );
        assert_eq!(
            out["reason"],
            "cannot reach https://***@registry.example/pkg (http://plain.example/x)"
        );
        // What is not a leak goes as it was.
        assert_eq!(out["path"], "/v1/lanes/fix/stop");
        assert_eq!(out["service"], "api");
        // The file keeps all of it.
        let dir = tempfile::tempdir().unwrap();
        let d = DiagLog::open(dir.path().join("daemon.log"));
        d.write("info", "prepare", json!({ "run": "npm ci" }));
        assert!(tail(d.path(), 1, None)[0].contains("npm ci"));
    }

    #[test]
    fn roots_on_this_machine_do_not_leave_it() {
        // Roots no other test has: the list is the process's.
        add_root(Path::new(r"Q:\Work\diagtest"), "<repo>");
        add_root(Path::new(r"Q:\Work\diagtest-fix"), "<worktree>");
        add_root(Path::new("/srv/diagtest"), "<repo>");
        add_root(Path::new(r"C:\"), "<none>");
        let line = json!({
            "event": "remote_lane_unwatched",
            "error": r#"Io error: denied about ["Q:\\Work\\diagtest-fix\\src"]"#,
            "reason": r"q:\work\diagtest\a.json is gone; Q:/Work/diagtest/b; Q:\Work\diagtestx\c",
            "other": "/srv/diagtest/x and /srv/diagtest",
            "drive": r"C:\x",
        });
        let out = Value::Object(outbound(line.as_object().unwrap()));
        assert_eq!(
            out["error"],
            r#"Io error: denied about ["<worktree>\\src"]"#
        );
        assert_eq!(
            out["reason"],
            r"<repo>\a.json is gone; <repo>/b; Q:\Work\diagtestx\c"
        );
        assert_eq!(out["other"], "<repo>/x and <repo>");
        assert_eq!(out["drive"], r"C:\x");
    }

    #[test]
    fn a_url_with_a_users_segment_stays_as_it_was() {
        let line = json!({
            "reason": "GET https://api.example/users/42/orders failed; file:///Users/jane/x",
        });
        let out = Value::Object(outbound(line.as_object().unwrap()));
        assert_eq!(
            out["reason"],
            "GET https://api.example/users/42/orders failed; file:///Users/~/x"
        );
    }

    #[test]
    fn lines_are_json_with_time_level_event() {
        let dir = tempfile::tempdir().unwrap();
        let d = DiagLog::open(dir.path().join("daemon.log"));
        d.write(
            "warn",
            "candidate",
            json!({ "service": "api", "reason": "build failed" }),
        );
        let lines = tail(d.path(), 10, None);
        assert_eq!(lines.len(), 1);
        let v: Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(v["event"], "candidate");
        assert_eq!(v["service"], "api");
        assert!(v["at"].as_str().unwrap().len() >= 29, "{}", v["at"]);
        let p = pretty(&lines[0]);
        assert!(p.contains("warn  candidate "), "{p}");
        assert!(
            p.contains(" service=api") && p.contains(" reason=\"build failed\""),
            "{p}"
        );
    }

    #[test]
    fn diagnostic_lines_become_otlp_log_records() {
        let line = json!({"at":"2026-10-15T12:00:00.000+02:00","level":"warn","event":"candidate","service":"api","code":3,"candidate":{"state":"failed"},"gone":null});
        let body = otlp_body("devstack-daemon", Some("shop"), &[line]);
        let rec = &body["resourceLogs"][0]["scopeLogs"][0]["logRecords"][0];
        assert_eq!(rec["body"]["stringValue"], "candidate");
        assert_eq!(rec["severityText"], "WARN");
        assert_eq!(rec["timeUnixNano"], "1792058400000000000");
        let observed: i128 = rec["observedTimeUnixNano"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        assert!(observed > 1_792_058_400_000_000_000, "{observed}");
        let attrs = rec["attributes"].as_array().unwrap();
        assert!(attrs.contains(&json!({"key":"devstack.service","value":{"stringValue":"api"}})));
        assert!(attrs.contains(&json!({"key":"devstack.code","value":{"intValue":"3"}})));
        assert!(
            attrs.contains(&json!({"key":"devstack.repository","value":{"stringValue":"shop"}}))
        );
        assert!(attrs.iter().any(|a| a["key"] == "devstack.candidate.state"));
        assert!(!attrs.iter().any(|a| a["key"] == "devstack.gone"));
        let resource = body["resourceLogs"][0]["resource"]["attributes"]
            .as_array()
            .unwrap();
        assert_eq!(resource[0]["value"]["stringValue"], "devstack-daemon");
        assert!(resource.iter().any(|a| a["key"] == "os.type"));
    }

    #[test]
    fn nested_values_become_dotted_attributes() {
        let line = json!({"event":"service","service":"api","status":{"state":"failed","reason":"x"},
            "candidate":{"state":"built","build":{"id":"b1"}},"paths":["a","b"],"mixed":[{"a":1}]});
        let body = otlp_body("devstack-daemon", None, &[line]);
        let rec = &body["resourceLogs"][0]["scopeLogs"][0]["logRecords"][0];
        let attrs = rec["attributes"].as_array().unwrap();
        let get = |k: &str| {
            attrs
                .iter()
                .find(|a| a["key"] == k)
                .map(|a| a["value"].clone())
        };
        assert_eq!(
            get("devstack.status.state").unwrap()["stringValue"],
            "failed"
        );
        assert_eq!(get("devstack.status.reason").unwrap()["stringValue"], "x");
        assert_eq!(
            get("devstack.candidate.build.id").unwrap()["stringValue"],
            "b1"
        );
        assert!(get("devstack.status").is_none() && get("devstack.candidate").is_none());
        assert_eq!(
            get("devstack.paths").unwrap(),
            json!({"arrayValue":{"values":[{"stringValue":"a"},{"stringValue":"b"}]}})
        );
        // A list of objects stays one text.
        assert_eq!(
            get("devstack.mixed").unwrap()["stringValue"],
            r#"[{"a":1}]"#
        );
        // No repository (the hub): no such attribute. No `at`: the time it went.
        assert!(get("devstack.repository").is_none());
        assert_eq!(rec["timeUnixNano"], rec["observedTimeUnixNano"]);
    }

    #[test]
    fn the_main_lane_is_called_main() {
        let dir = tempfile::tempdir().unwrap();
        let d = DiagLog::open(dir.path().join("daemon.log"));
        d.write("info", "restored", json!({ "lane": "", "service": "api" }));
        d.write(
            "info",
            "restored",
            json!({ "lane": "fix", "service": "api" }),
        );
        let lines = tail(d.path(), 10, None);
        let v: Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(v["lane"], "main");
        let v: Value = serde_json::from_str(&lines[1]).unwrap();
        assert_eq!(v["lane"], "fix");
    }

    #[test]
    fn tail_reads_the_rotated_file_too_and_greps() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.log");
        std::fs::write(rotated(&path), "{\"event\":\"old\"}\n").unwrap();
        std::fs::write(&path, "{\"event\":\"new\"}\n{\"event\":\"Other\"}\n").unwrap();
        assert_eq!(tail(&path, 10, None).len(), 3);
        assert_eq!(tail(&path, 1, None), vec!["{\"event\":\"Other\"}"]);
        assert_eq!(tail(&path, 10, Some("OTHER")).len(), 1);
    }

    #[test]
    fn prune_keeps_the_newest() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..5 {
            std::fs::write(dir.path().join(format!("run-{i}.log")), "x").unwrap();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        std::fs::write(dir.path().join("build-1.log"), "x").unwrap();
        assert_eq!(prune(dir.path(), "run-", 2), 3);
        let mut left: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, vec!["build-1.log", "run-3.log", "run-4.log"]);
    }
}
