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

/// The process's log (the daemon's or the hub's); set once at start.
pub fn init(path: impl Into<PathBuf>) -> &'static DiagLog {
    GLOBAL.get_or_init(|| DiagLog::open(path))
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
