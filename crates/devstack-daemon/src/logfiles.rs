//! The kept logs of a service: one per start (`run-<ms>-<port>[-<build>].log`)
//! and one per build (`build-<id>.log`), all in `logs/<service>/` (plan §11).
//! A process writes its output straight into its file, so lines carry no time
//! of their own: selecting "by time" picks whole logs, by when they were written.

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LogFile {
    /// File name; what `?file=` takes.
    pub file: String,
    /// `run` (a started instance) or `build`.
    pub kind: &'static str,
    /// When the instance started (from the name); for a build log the time of
    /// its last line.
    pub started_ms: u64,
    /// The last write.
    pub modified_ms: u64,
    /// The build an instance ran, or the build a build log belongs to.
    pub build: Option<String>,
    pub port: Option<u16>,
    pub bytes: u64,
    /// The log of the instance that runs now.
    pub current: bool,
}

/// The run log name of an instance started now.
pub fn run_name(started_ms: u128, port: Option<u16>, build: Option<&str>) -> String {
    match build {
        Some(b) => format!("run-{started_ms}-{}-{b}.log", port.unwrap_or(0)),
        None => format!("run-{started_ms}-{}.log", port.unwrap_or(0)),
    }
}

/// Kind, start, port and build, from a log's file name.
type Parsed = (&'static str, Option<u64>, Option<u16>, Option<String>);

fn parse(name: &str) -> Option<Parsed> {
    let stem = name.strip_suffix(".log")?;
    if let Some(id) = stem.strip_prefix("build-") {
        return Some(("build", None, None, Some(id.to_string())));
    }
    let mut parts = stem.strip_prefix("run-")?.splitn(3, '-');
    let started = parts.next()?.parse().ok()?;
    let port = parts.next()?.parse().ok().filter(|p| *p != 0);
    let build = parts.next().map(str::to_string);
    Some(("run", Some(started), port, build))
}

fn ms(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

/// Every kept log in `dir`, newest first.
pub fn list(dir: &Path, current: Option<&Path>) -> Vec<LogFile> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let current = current
        .and_then(|c| c.file_name())
        .map(|f| f.to_string_lossy().into_owned());
    let mut out: Vec<LogFile> = rd
        .flatten()
        .filter_map(|e| {
            let file = e.file_name().to_string_lossy().into_owned();
            let (kind, started, port, build) = parse(&file)?;
            let meta = e.metadata().ok()?;
            let modified_ms = meta.modified().map(ms).unwrap_or(0);
            Some(LogFile {
                current: current.as_deref() == Some(file.as_str()),
                started_ms: started.unwrap_or(modified_ms),
                file,
                kind,
                modified_ms,
                build,
                port,
                bytes: meta.len(),
            })
        })
        .collect();
    out.sort_by_key(|f| std::cmp::Reverse((f.started_ms, f.modified_ms)));
    out
}

/// `file` as a path in `dir`, if it is a kept log there (no other path).
pub fn resolve(dir: &Path, file: &str) -> Option<PathBuf> {
    if file.contains(['/', '\\']) || parse(file).is_none() {
        return None;
    }
    let p = dir.join(file);
    p.is_file().then_some(p)
}

/// What a query selects, oldest first: a build's log and the logs of the
/// instances that ran it, or the run logs written to within `since`.
pub fn select(files: &[LogFile], build: Option<&str>, since: Option<Duration>) -> Vec<LogFile> {
    let after = since.map(|d| ms(SystemTime::now()).saturating_sub(d.as_millis() as u64));
    let mut sel: Vec<LogFile> = files
        .iter()
        .filter(|f| match build {
            // A short id is enough, as `devstack status` shows it.
            Some(b) => f.build.as_deref().is_some_and(|id| id.starts_with(b)),
            None => f.kind == "run",
        })
        .filter(|f| after.is_none_or(|a| f.modified_ms >= a))
        .cloned()
        .collect();
    sel.reverse();
    sel
}

/// The line that separates one log from the next in a combined view.
pub fn header(f: &LogFile) -> String {
    let at = jiff::Timestamp::from_millisecond(f.started_ms as i64)
        .map(|t| {
            t.to_zoned(jiff::tz::TimeZone::system())
                .strftime("%Y-%m-%d %H:%M:%S")
                .to_string()
        })
        .unwrap_or_default();
    let mut s = match f.kind {
        "build" => format!("── build {}", f.build.as_deref().unwrap_or("?")),
        _ => format!("── started {at}"),
    };
    if f.kind == "run" {
        if let Some(b) = &f.build {
            s.push_str(&format!(", build {}", &b[..b.len().min(12)]));
        }
        if let Some(p) = f.port {
            s.push_str(&format!(", port {p}"));
        }
    }
    if f.current {
        s.push_str(", running now");
    }
    s.push_str(" ──");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        assert_eq!(
            parse(&run_name(17, Some(5000), Some("abc"))),
            Some(("run", Some(17), Some(5000), Some("abc".into())))
        );
        assert_eq!(
            parse(&run_name(17, None, None)),
            Some(("run", Some(17), None, None))
        );
        assert_eq!(
            parse("build-fee4.log"),
            Some(("build", None, None, Some("fee4".into())))
        );
        assert_eq!(parse("daemon.log"), None);
        assert_eq!(parse("run-x-1.log"), None);
    }

    #[test]
    fn lists_selects_and_resolves() {
        let d = tempfile::tempdir().unwrap();
        for n in [
            "run-1000-5000-aaaa.log",
            "run-2000-5001-bbbb.log",
            "run-3000-0.log",
            "build-bbbb.log",
            "notes.txt",
        ] {
            std::fs::write(d.path().join(n), "x\n").unwrap();
        }
        let cur = d.path().join("run-3000-0.log");
        let all = list(d.path(), Some(&cur));
        assert_eq!(all.len(), 4);
        assert!(
            all.iter()
                .find(|f| f.file == "run-3000-0.log")
                .unwrap()
                .current
        );

        let b: Vec<_> = select(&all, Some("bb"), None)
            .into_iter()
            .map(|f| f.file)
            .collect();
        assert!(
            b.contains(&"build-bbbb.log".to_string())
                && b.contains(&"run-2000-5001-bbbb.log".to_string())
        );
        assert_eq!(b.len(), 2);

        let runs: Vec<_> = select(&all, None, Some(Duration::from_secs(60)))
            .into_iter()
            .map(|f| f.file)
            .collect();
        assert_eq!(
            runs,
            [
                "run-1000-5000-aaaa.log",
                "run-2000-5001-bbbb.log",
                "run-3000-0.log"
            ]
        );

        assert!(resolve(d.path(), "build-bbbb.log").is_some());
        assert!(resolve(d.path(), "../build-bbbb.log").is_none());
        assert!(resolve(d.path(), "notes.txt").is_none());
    }
}
