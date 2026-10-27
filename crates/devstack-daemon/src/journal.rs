//! The journal of what changed the stack at runtime, and through which
//! client (ADR-0067): the last [`KEEP`] changes, kept across restarts in
//! `.devstack/journal.ndjson`. The daemon's `audit` layer writes every
//! request that changes something; devstack's own changes of shared state
//! (the main slot given back) are written as `auto` with the path the API
//! would have taken. Events are the live view; this is the record.

use std::collections::VecDeque;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

/// Entries kept, in memory and on disk.
pub const KEEP: usize = 500;

/// The clients that say what they are (`X-Devstack-Client`).
pub const CLIENTS: [&str; 5] = ["cli", "mcp", "dashboard", "tray", "auto"];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// Unix ms.
    pub at: u64,
    /// The client: one of [`CLIENTS`], or `unknown`.
    pub via: String,
    /// The lane it was for; absent: the main working tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane: Option<String>,
    /// `POST`, `DELETE`; `AUTO` for devstack's own.
    pub method: String,
    pub path: String,
    pub status: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Entry {
    /// Whether the stream of `tree` carries it: the main tree's (`""`)
    /// every entry, a lane's those of its lane.
    pub fn for_tree(&self, tree: &str) -> bool {
        tree.is_empty() || self.lane.as_deref() == Some(tree)
    }
}

/// Who changed something, and when: what a view shows of the journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Change {
    pub via: String,
    /// Unix ms.
    pub at: u64,
    /// The action (`stop`, `restart`, `promote`), where the view needs it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

pub struct Journal {
    path: Option<PathBuf>,
    entries: Mutex<VecDeque<Entry>>,
    /// Lines in the file: it is rewritten with the last [`KEEP`] at twice that.
    lines: Mutex<usize>,
    /// Each entry as it is recorded: the supervisors send it on as a
    /// `journal` event.
    tx: broadcast::Sender<Entry>,
}

static JOURNAL: OnceLock<Journal> = OnceLock::new();

/// The daemon's journal, read from `state_dir` the first time.
pub fn init(state_dir: &Path) -> &'static Journal {
    JOURNAL.get_or_init(|| Journal::open(Some(state_dir.join("journal.ndjson"))))
}

/// The journal; one that keeps nothing on disk before [`init`] (tests).
pub fn get() -> &'static Journal {
    JOURNAL.get_or_init(|| Journal::open(None))
}

/// The client a request names, or `unknown`.
pub fn client(header: Option<&str>) -> String {
    header
        .map(str::trim)
        .filter(|h| CLIENTS.contains(h))
        .unwrap_or("unknown")
        .to_string()
}

/// devstack's own change of what others see, as the API would have made it
/// (`/v1/lanes/main/main`: the main slot given back).
pub fn auto(path: &str, lane: Option<&str>) {
    get().record(Entry {
        at: now(),
        via: "auto".into(),
        lane: lane.map(str::to_string),
        method: "AUTO".into(),
        path: path.into(),
        status: 200,
        error: None,
    });
}

fn now() -> u64 {
    crate::state::now_ms() as u64
}

impl Journal {
    fn open(path: Option<PathBuf>) -> Self {
        let mut entries = VecDeque::new();
        let mut lines = 0;
        if let Some(text) = path.as_ref().and_then(|p| std::fs::read_to_string(p).ok()) {
            for l in text.lines() {
                lines += 1;
                if let Ok(e) = serde_json::from_str::<Entry>(l) {
                    entries.push_back(e);
                    if entries.len() > KEEP {
                        entries.pop_front();
                    }
                }
            }
        }
        Self {
            path,
            entries: Mutex::new(entries),
            lines: Mutex::new(lines),
            tx: broadcast::channel(256).0,
        }
    }

    /// The entries recorded from now on.
    pub fn subscribe(&self) -> broadcast::Receiver<Entry> {
        self.tx.subscribe()
    }

    pub fn record(&self, e: Entry) {
        let line = serde_json::to_string(&e).unwrap_or_default();
        let mut entries = self.entries.lock().unwrap();
        // Sent under the lock: the stream has the entries in the journal's order.
        let _ = self.tx.send(e.clone());
        entries.push_back(e);
        if entries.len() > KEEP {
            entries.pop_front();
        }
        let Some(path) = &self.path else {
            return;
        };
        let mut lines = self.lines.lock().unwrap();
        if *lines >= 2 * KEEP {
            // Down to what is kept: one write, then appends again.
            let text: String = entries
                .iter()
                .filter_map(|e| serde_json::to_string(e).ok())
                .map(|l| l + "\n")
                .collect();
            let tmp = path.with_extension("ndjson.tmp");
            if std::fs::write(&tmp, text).is_ok() && std::fs::rename(&tmp, path).is_ok() {
                *lines = entries.len();
            }
            return;
        }
        let appended = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut f| writeln!(f, "{line}"));
        if appended.is_ok() {
            *lines += 1;
        }
    }

    /// The entries since `since` (Unix ms), oldest first.
    pub fn since(&self, since: u64) -> Vec<Entry> {
        self.entries
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.at > since)
            .cloned()
            .collect()
    }

    /// The last change that succeeded at `path`.
    pub fn last_at(&self, path: &str) -> Option<Change> {
        self.entries
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|e| e.status < 400 && e.path == path)
            .map(|e| Change {
                via: e.via.clone(),
                at: e.at,
                action: None,
            })
    }

    /// The last action that succeeded on a service, its path's last part
    /// (`<prefix><service>/<action>`).
    pub fn last_under(&self, prefix: &str) -> Option<Change> {
        self.entries
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|e| {
                e.status < 400
                    && e.path
                        .strip_prefix(prefix)
                        .is_some_and(|a| !a.is_empty() && !a.contains('/'))
            })
            .map(|e| Change {
                via: e.via.clone(),
                at: e.at,
                action: e.path.strip_prefix(prefix).map(str::to_string),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(at: u64, via: &str, path: &str, status: u16) -> Entry {
        Entry {
            at,
            via: via.into(),
            lane: None,
            method: "POST".into(),
            path: path.into(),
            status,
            error: None,
        }
    }

    #[test]
    fn it_keeps_the_last_ones_across_a_restart() {
        let d = tempfile::tempdir().unwrap();
        let file = d.path().join("journal.ndjson");
        let j = Journal::open(Some(file.clone()));
        for i in 0..(2 * KEEP as u64 + 5) {
            j.record(entry(i, "cli", "/v1/up", 204));
        }
        let lines = std::fs::read_to_string(&file).unwrap().lines().count();
        assert!(lines <= 2 * KEEP, "{lines}");
        let again = Journal::open(Some(file));
        let all = again.since(0);
        assert_eq!(all.len(), KEEP);
        assert_eq!(all.last().unwrap().at, 2 * KEEP as u64 + 4);
    }

    #[test]
    fn a_view_finds_who_did_it_last() {
        let j = Journal::open(None);
        j.record(entry(1, "cli", "/v1/lanes/feat/main", 200));
        j.record(entry(2, "dashboard", "/v1/lanes/feat/main", 200));
        j.record(entry(3, "mcp", "/v1/lanes/feat/main", 409));
        j.record(entry(4, "mcp", "/v1/services/api/stop", 200));
        j.record(entry(5, "tray", "/v1/services/api/logs/x", 200));
        let main = j.last_at("/v1/lanes/feat/main").unwrap();
        assert_eq!((main.via.as_str(), main.at), ("dashboard", 2));
        let api = j.last_under("/v1/services/api/").unwrap();
        assert_eq!(
            (api.via.as_str(), api.action.as_deref()),
            ("mcp", Some("stop"))
        );
        assert!(j.last_under("/v1/services/web/").is_none());
        assert_eq!(client(Some("dashboard")), "dashboard");
        assert_eq!(client(Some("curl")), "unknown");
        assert_eq!(client(None), "unknown");
    }

    #[test]
    fn a_recorded_entry_is_sent_on() {
        let j = Journal::open(None);
        let mut rx = j.subscribe();
        j.record(entry(1, "dashboard", "/v1/services/api/stop", 200));
        let e = rx.try_recv().unwrap();
        assert_eq!(
            (e.via.as_str(), e.path.as_str()),
            ("dashboard", "/v1/services/api/stop")
        );
        assert!(e.for_tree(""));
        assert!(!e.for_tree("feat"));
        let lane = Entry {
            lane: Some("feat".into()),
            ..e
        };
        assert!(lane.for_tree("") && lane.for_tree("feat") && !lane.for_tree("other"));
    }
}
