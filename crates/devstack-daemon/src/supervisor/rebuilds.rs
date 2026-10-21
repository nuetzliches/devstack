//! A dev server's rebuilds on its view (ADR-0042), and a service's in
//! hot-reload mode: its log is read as it
//! grows, each run with a [`Tracker`] of its own; what the tracker says goes
//! on the service's view, to the dashboard as an event and, once a rebuild
//! is over, to the diagnostic log with its time (ADR-0037 point 4).

use super::Supervisor;
use crate::rebuild::{RebuildView, Tracker};
use crate::state::{EventBody, Status, now_ms};
use devstack_config::model::Kind;
use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::{Arc, Weak};
use std::time::Duration;

const EVERY: Duration = Duration::from_millis(400);
/// Read at most this much of a log per round: a dev server that prints a
/// flood is followed, not read whole at once.
const CHUNK: u64 = 256 * 1024;

/// One run of one dev server, as followed.
struct Run {
    log: PathBuf,
    pos: u64,
    partial: String,
    tracker: Option<Tracker>,
    /// It is up: its first build is over, what follows is a rebuild.
    live: bool,
    /// The finished rebuild last logged (its start): error lines that follow
    /// it update the view, not the log.
    logged: Option<u128>,
    /// A failed rebuild whose error lines have not come yet (Angular prints
    /// them after its marker on Windows): its start, when it last changed and
    /// its diagnostic fields, logged [`HOLD`] after its last error line came,
    /// or when the next rebuild starts.
    held: Option<Held>,
}

type Held = (u128, u128, serde_json::Map<String, serde_json::Value>);

/// How long a failed rebuild's diagnostic line waits for more error lines.
const HOLD: u128 = 2_000;

impl Supervisor {
    /// Follow every dev server's log for its rebuilds, as long as this
    /// supervisor lives (the main tree's, a lane's).
    pub fn follow_rebuilds(self: &Arc<Self>) {
        if !self
            .stack
            .services
            .values()
            .any(|s| s.kind == Kind::Devserver || s.hot_reload.is_some())
        {
            return;
        }
        let me: Weak<Self> = Arc::downgrade(self);
        let _ = std::thread::Builder::new()
            .name("rebuilds".into())
            .spawn(move || {
                let mut runs: BTreeMap<String, Run> = BTreeMap::new();
                loop {
                    std::thread::sleep(EVERY);
                    let Some(sup) = me.upgrade() else {
                        return;
                    };
                    sup.rebuild_round(&mut runs);
                }
            });
    }

    fn rebuild_round(&self, runs: &mut BTreeMap<String, Run>) {
        for (name, svc) in &self.stack.services {
            if svc.kind != Kind::Devserver && svc.hot_reload.is_none() {
                continue;
            }
            // A service rebuilds by itself in hot-reload mode only (its
            // toolchain's watcher, plan §3.4a); built, devstack builds it.
            // Read under the lock, acted on after it: `set_rebuild` takes it
            // again.
            let seen = self
                .slots
                .lock()
                .unwrap()
                .get(name)
                .filter(|s| svc.kind == Kind::Devserver || s.view.hot_reload)
                .map(|s| (s.view.log.clone(), s.view.status.clone()));
            let Some((log, status)) = seen else {
                if runs.remove(name).is_some() {
                    self.set_rebuild(name, None, None);
                }
                continue;
            };
            let up = matches!(status, Status::Running | Status::Healthy);
            let running = up || matches!(status, Status::Starting | Status::Unhealthy { .. });
            let Some(log) = log.filter(|_| running) else {
                if runs.remove(name).is_some() {
                    self.set_rebuild(name, None, None);
                }
                continue;
            };
            let run = match runs.get_mut(name) {
                Some(r) if r.log == log => r,
                _ => {
                    self.set_rebuild(name, None, None);
                    runs.insert(
                        name.clone(),
                        Run {
                            log: log.clone(),
                            pos: 0,
                            partial: String::new(),
                            tracker: Tracker::new(svc.rebuild.as_ref()),
                            live: false,
                            logged: None,
                            held: None,
                        },
                    );
                    runs.get_mut(name).expect("inserted")
                }
            };
            let Some(tracker) = run.tracker.as_mut() else {
                continue;
            };
            // Lines of its first build (before it was up) fix the tool but
            // are no rebuild.
            let was_live = run.live;
            run.live |= up;
            for line in new_lines(&run.log, &mut run.pos, &mut run.partial) {
                let now = now_ms();
                if let Some(v) = tracker.feed(&line, now)
                    && was_live
                {
                    if run
                        .held
                        .as_ref()
                        .is_some_and(|(since, _, _)| *since != v.since)
                    {
                        self.log_held(name, &mut run.held);
                    }
                    let finished = v.state != "rebuilding" && run.logged != Some(v.since);
                    if finished {
                        run.logged = Some(v.since);
                    }
                    let mut done = finished.then(|| {
                        let mut extra = serde_json::Map::new();
                        if let Some(ms) = v.ms {
                            extra.insert("ms".into(), ms.into());
                        }
                        extra.insert(
                            "result".into(),
                            if v.state == "ok" { "ok" } else { "failed" }.into(),
                        );
                        extra
                    });
                    if tracker.waits_for_errors() {
                        // Its error lines follow: the log waits for them.
                        if let Some(extra) = done.take() {
                            run.held = Some((v.since, now, extra));
                        }
                    } else if let Some((since, at, _)) = run.held.as_mut()
                        && *since == v.since
                    {
                        // One more of its error lines: the next may follow.
                        *at = now;
                    }
                    self.set_rebuild(name, Some(v), done);
                }
            }
            if run
                .held
                .as_ref()
                .is_some_and(|(_, at, _)| now_ms().saturating_sub(*at) >= HOLD)
            {
                self.log_held(name, &mut run.held);
            }
            if !was_live {
                continue;
            }
            // A finished one goes after a while.
            let shown = tracker.view(now_ms());
            let has = self
                .slots
                .lock()
                .unwrap()
                .get(name)
                .is_some_and(|s| s.view.rebuild.is_some());
            if shown.is_none() && has {
                self.set_rebuild(name, None, None);
            }
        }
    }

    /// A held failed rebuild to the diagnostic log, as the view has it now
    /// (with what error lines came).
    fn log_held(&self, name: &str, held: &mut Option<Held>) {
        let Some((_, _, extra)) = held.take() else {
            return;
        };
        let rebuild = self
            .slots
            .lock()
            .unwrap()
            .get(name)
            .and_then(|s| s.view.rebuild.clone());
        self.emit_with(
            EventBody::DevserverRebuild {
                service: name.into(),
                rebuild,
            },
            Some(extra),
        );
    }

    /// The service's rebuild on its view and to the dashboard; `extra` (the
    /// time, the result) goes to the diagnostic line of a finished one.
    fn set_rebuild(
        &self,
        name: &str,
        rebuild: Option<RebuildView>,
        extra: Option<serde_json::Map<String, serde_json::Value>>,
    ) {
        if let Some(slot) = self.slots.lock().unwrap().get_mut(name) {
            if slot.view.rebuild == rebuild {
                return;
            }
            slot.view.rebuild = rebuild.clone();
        }
        let body = EventBody::DevserverRebuild {
            service: name.into(),
            rebuild,
        };
        match extra {
            Some(extra) => self.emit_with(body, Some(extra)),
            // A rebuild under way, or one gone from the view: the dashboard
            // hears it, the log has the finished ones.
            None => self.emit_quiet(body),
        }
    }
}

/// The complete lines `log` got since `pos`, at most [`CHUNK`] of it; a
/// line not yet ended waits in `partial`.
fn new_lines(log: &PathBuf, pos: &mut u64, partial: &mut String) -> Vec<String> {
    let Ok(mut f) = std::fs::File::open(log) else {
        return Vec::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if len < *pos {
        // Truncated: from its start again.
        *pos = 0;
        partial.clear();
    }
    if len == *pos || f.seek(SeekFrom::Start(*pos)).is_err() {
        return Vec::new();
    }
    let mut buf = Vec::new();
    let _ = f.take(CHUNK).read_to_end(&mut buf);
    *pos += buf.len() as u64;
    partial.push_str(&String::from_utf8_lossy(&buf));
    let mut lines: Vec<String> = Vec::new();
    while let Some(i) = partial.find('\n') {
        lines.push(partial[..i].to_string());
        partial.drain(..=i);
    }
    lines
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_log_is_read_as_it_grows() {
        use std::io::Write;
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("run.log");
        let mut f = std::fs::File::create(&p).unwrap();
        let (mut pos, mut partial) = (0, String::new());
        write!(f, "one\ntw").unwrap();
        assert_eq!(super::new_lines(&p, &mut pos, &mut partial), ["one"]);
        write!(f, "o\nthree\n").unwrap();
        assert_eq!(
            super::new_lines(&p, &mut pos, &mut partial),
            ["two", "three"]
        );
        assert!(super::new_lines(&p, &mut pos, &mut partial).is_empty());
        // Truncated (a new run in the same file): from the start.
        std::fs::write(&p, "again\n").unwrap();
        assert_eq!(super::new_lines(&p, &mut pos, &mut partial), ["again"]);
    }
}
