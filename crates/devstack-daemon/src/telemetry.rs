//! A daemon's or the hub's telemetry to its pools' collectors (ADR-0037):
//! only to pools this machine agreed to send to (`telemetry.json` beside the
//! membership) and that collect (the coordinator says so). Every minute: the
//! diagnostic lines since (as OTLP logs), the durations among them (as
//! histograms, by event, repository, lane, node, service and result), the
//! tunnels closed since, and gauges the process registers (lanes and
//! services running). Lossy: a bounded buffer, dropped when nobody takes it.
//!
//! From the process's start until the first round has asked the pools, the
//! lines are kept (bounded): a daemon's start, its restores and early errors
//! come before any coordinator could answer. Nothing is sent before that
//! round finds a pool that collects and this machine's consent to it; when
//! it finds none, what was kept is dropped and nothing more is kept.

use devstack_node::member::Local;
use devstack_node::telemetry::{Consent, Durations, Gauge, metrics_body};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const EVERY: Duration = Duration::from_secs(60);
/// How long a coordinator's "we collect" holds before it is asked again.
const ASK_EVERY: Duration = Duration::from_secs(600);
const MAX_LINES: usize = 5000;
/// Tunnel connections kept per round (each is one duration and its bytes).
const MAX_TUNNELS: usize = 5000;
/// Rounds the lines of the start are kept while a coordinator with this
/// machine's consent has not answered yet (it may start with this process).
const UNDECIDED_ROUNDS: u32 = 5;

/// Not started (the CLI, tests): nothing is kept.
const IDLE: u8 = 0;
/// Started, no pool asked yet: lines are kept, nothing is sent.
const UNDECIDED: u8 = 1;
/// A pool takes telemetry: lines are kept and sent.
const ON: u8 = 2;
/// No pool takes it: nothing is kept.
const OFF: u8 = 3;

static STATE: AtomicU8 = AtomicU8::new(IDLE);
static BUFFER: Mutex<Vec<Value>> = Mutex::new(Vec::new());
static TUNNELS: Mutex<Vec<TunnelClosed>> = Mutex::new(Vec::new());
static TUNNELS_OPEN: AtomicU64 = AtomicU64::new(0);
static REPOSITORY: OnceLock<String> = OnceLock::new();
type GaugeFn = Box<dyn Fn() -> Vec<Gauge> + Send + Sync>;
static GAUGES: OnceLock<GaugeFn> = OnceLock::new();

/// Whether diagnostic lines are kept now: a pool takes them, or none was
/// asked yet.
pub fn active() -> bool {
    matches!(STATE.load(Ordering::Relaxed), UNDECIDED | ON)
}

/// Keep a diagnostic line for the next send (when a pool takes them).
pub fn record(line: &serde_json::Map<String, Value>) {
    if !active() {
        return;
    }
    let mut b = BUFFER.lock().unwrap();
    if b.len() < MAX_LINES {
        b.push(Value::Object(line.clone()));
    }
}

/// The repository (the stack's name) this process runs: a label of its
/// log records and its duration histograms. The hub has none.
pub fn set_repository(name: &str) {
    let _ = REPOSITORY.set(name.to_string());
}

pub fn repository() -> Option<&'static str> {
    REPOSITORY.get().map(String::as_str)
}

/// What the process measures now, read at each send.
pub fn set_gauges(f: impl Fn() -> Vec<Gauge> + Send + Sync + 'static) {
    let _ = GAUGES.set(Box::new(f));
}

/// One connection through a remote lane's tunnel, open while this lives;
/// [`Tunnel::closed`] records its duration and bytes.
pub struct Tunnel {
    lane: String,
    node: String,
    t0: Instant,
}

struct TunnelClosed {
    lane: String,
    node: String,
    ms: f64,
    bytes: u64,
}

impl Tunnel {
    pub fn open(lane: &str, node: &str) -> Self {
        TUNNELS_OPEN.fetch_add(1, Ordering::Relaxed);
        Self {
            lane: lane.to_string(),
            node: node.to_string(),
            t0: Instant::now(),
        }
    }

    /// The connection ended after `bytes` both ways.
    pub fn closed(mut self, bytes: u64) {
        if !active() {
            return;
        }
        let mut t = TUNNELS.lock().unwrap();
        if t.len() < MAX_TUNNELS {
            t.push(TunnelClosed {
                lane: std::mem::take(&mut self.lane),
                node: std::mem::take(&mut self.node),
                ms: self.t0.elapsed().as_secs_f64() * 1000.0,
                bytes,
            });
        }
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        TUNNELS_OPEN.fetch_sub(1, Ordering::Relaxed);
    }
}

fn state_dir() -> PathBuf {
    crate::token::token_path()
        .parent()
        .expect("token path has a parent")
        .to_path_buf()
}

/// Start sending, once per process, as `service`. Lines are kept from now
/// on, until the first round knows whether a pool takes them.
pub fn start(service: &'static str) {
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    STATE.store(UNDECIDED, Ordering::Relaxed);
    let spawned = std::thread::Builder::new()
        .name("telemetry".into())
        .spawn(move || run(service));
    if spawned.is_err() {
        STATE.store(OFF, Ordering::Relaxed);
    }
}

/// What a round does with what was kept.
#[derive(Debug, PartialEq, Eq)]
enum Round {
    /// Send it: a pool takes it.
    Send,
    /// Keep it: a coordinator with this machine's consent did not answer
    /// yet, and the process is young.
    Keep,
    /// Drop it, and keep nothing more: no pool takes it.
    Drop,
}

/// The state after a round and what it does: `targets` pools take
/// telemetry; `pending` one with consent did not answer; `round` counts
/// from 1.
fn decide(state: u8, targets: bool, pending: bool, round: u32) -> (u8, Round) {
    if targets {
        (ON, Round::Send)
    } else if state == UNDECIDED && pending && round < UNDECIDED_ROUNDS {
        (UNDECIDED, Round::Keep)
    } else {
        (OFF, Round::Drop)
    }
}

fn run(service: &str) {
    // Per pool directory: whether its coordinator collects, as of when.
    let mut collects: HashMap<PathBuf, (Instant, bool)> = HashMap::new();
    let mut wait = Duration::from_secs(10);
    let mut round = 0u32;
    loop {
        std::thread::sleep(wait);
        wait = EVERY;
        round += 1;
        let mut pending = false;
        let targets: Vec<Local> = devstack_node::member::pools(&state_dir())
            .into_iter()
            .filter(|(_, dir)| Consent::load(dir).enabled)
            .filter_map(|(_, dir)| {
                let local = Local::new(&dir);
                let approved = local
                    .membership()
                    .ok()
                    .flatten()
                    .is_some_and(|m| m.state == devstack_node::store::State::Approved);
                if !approved {
                    return None;
                }
                let known = collects
                    .get(&dir)
                    .filter(|(at, _)| at.elapsed() < ASK_EVERY)
                    .map(|(_, on)| *on);
                // Only an answer is kept; a coordinator that does not
                // answer is asked again at the next round.
                let on = known.unwrap_or_else(|| match local.telemetry_info() {
                    Ok(i) => {
                        collects.insert(dir.clone(), (Instant::now(), i.enabled));
                        i.enabled
                    }
                    Err(_) => {
                        pending = true;
                        false
                    }
                });
                on.then_some(local)
            })
            .collect();
        let (state, what) = decide(
            STATE.load(Ordering::Relaxed),
            !targets.is_empty(),
            pending,
            round,
        );
        STATE.store(state, Ordering::Relaxed);
        match what {
            Round::Keep => continue,
            Round::Drop => {
                BUFFER.lock().unwrap().clear();
                TUNNELS.lock().unwrap().clear();
                continue;
            }
            Round::Send => {}
        }
        let lines = std::mem::take(&mut *BUFFER.lock().unwrap());
        let tunnels = std::mem::take(&mut *TUNNELS.lock().unwrap());
        let repo = repository();
        let logs = (!lines.is_empty()).then(|| crate::diag::otlp_body(service, repo, &lines));
        let mut gauges = GAUGES.get().map(|f| f()).unwrap_or_default();
        gauges.extend(tunnel_gauges(repo, &tunnels));
        let mut durations = durations(repo, &lines);
        durations.extend(tunnel_durations(repo, &tunnels));
        let metrics = metrics_body(service, &gauges, &durations);
        for t in &targets {
            if let Some(l) = &logs {
                let _ = t.telemetry_send("logs", l);
            }
            let _ = t.telemetry_send("metrics", &metrics);
        }
    }
}

/// The labels a duration is counted by: its event, the process's
/// repository, and the line's lane, node, place, service, result and phase
/// (`remote_lane_phase`) where it has them (each of few values).
fn labels(repo: Option<&str>, event: &str, line: &Value) -> Vec<(String, String)> {
    let mut l = vec![("devstack.event".to_string(), event.to_string())];
    if let Some(r) = repo {
        l.push(("devstack.repository".into(), r.into()));
    }
    for k in ["lane", "node", "place", "service", "result", "phase"] {
        if let Some(v) = line[k].as_str().filter(|v| !v.is_empty()) {
            l.push((format!("devstack.{k}"), v.to_string()));
        }
    }
    l
}

/// The durations among `lines` (`ms`): `devstack.duration`, one histogram
/// point per set of [`labels`].
fn durations(repo: Option<&str>, lines: &[Value]) -> Vec<Durations> {
    let mut by: BTreeMap<Vec<(String, String)>, Vec<f64>> = BTreeMap::new();
    for l in lines {
        let (Some(event), Some(ms)) = (l["event"].as_str(), l["ms"].as_f64()) else {
            continue;
        };
        by.entry(labels(repo, event, l)).or_default().push(ms);
    }
    by.into_iter()
        .map(|(labels, values)| Durations {
            name: "devstack.duration".into(),
            values,
            labels,
        })
        .collect()
}

fn tunnel_labels(repo: Option<&str>, t: &TunnelClosed) -> Vec<(String, String)> {
    let line = serde_json::json!({ "lane": t.lane, "node": t.node });
    labels(repo, "tunnel", &line)
}

/// How long the tunnel connections closed since the last round were open:
/// `devstack.duration` with the event `tunnel`.
fn tunnel_durations(repo: Option<&str>, tunnels: &[TunnelClosed]) -> Vec<Durations> {
    let mut by: BTreeMap<Vec<(String, String)>, Vec<f64>> = BTreeMap::new();
    for t in tunnels {
        by.entry(tunnel_labels(repo, t)).or_default().push(t.ms);
    }
    by.into_iter()
        .map(|(labels, values)| Durations {
            name: "devstack.duration".into(),
            values,
            labels,
        })
        .collect()
}

/// Tunnels open now, and the bytes through those closed since the last
/// round, by lane and node.
fn tunnel_gauges(repo: Option<&str>, tunnels: &[TunnelClosed]) -> Vec<Gauge> {
    let mut by: BTreeMap<Vec<(String, String)>, u64> = BTreeMap::new();
    for t in tunnels {
        let mut l = tunnel_labels(repo, t);
        l.remove(0); // the event is for durations
        *by.entry(l).or_default() += t.bytes;
    }
    // The hub has no tunnels (and no repository).
    let mut out: Vec<Gauge> = repo
        .map(|r| Gauge {
            name: "devstack.tunnels".into(),
            unit: "1".into(),
            value: TUNNELS_OPEN.load(Ordering::Relaxed) as f64,
            labels: vec![("devstack.repository".to_string(), r.to_string())],
        })
        .into_iter()
        .collect();
    out.extend(by.into_iter().map(|(labels, bytes)| Gauge {
        name: "devstack.tunnel.bytes".into(),
        unit: "By".into(),
        value: bytes as f64,
        labels,
    }));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_are_grouped_by_event_and_labels() {
        let lines = vec![
            serde_json::json!({ "event": "remote_lane_started", "lane": "fix", "node": "n1", "ms": 1200 }),
            serde_json::json!({ "event": "remote_lane_started", "lane": "fix", "node": "n1", "ms": 800 }),
            serde_json::json!({ "event": "remote_lane_started", "lane": "x", "node": "n1", "ms": 900 }),
            serde_json::json!({ "event": "lane_moved", "lane": "fix", "ms": 5000 }),
            serde_json::json!({ "event": "service", "service": "api", "result": "ok", "lane": "main", "ms": 300 }),
            serde_json::json!({ "event": "service", "status": { "state": "running" } }),
        ];
        let d = durations(Some("shop"), &lines);
        assert_eq!(d.len(), 4);
        let started: Vec<&Durations> = d
            .iter()
            .filter(|d| d.labels[0].1 == "remote_lane_started")
            .collect();
        assert_eq!(started.len(), 2);
        let fix = started
            .iter()
            .find(|d| d.labels.contains(&("devstack.lane".into(), "fix".into())))
            .unwrap();
        assert_eq!(fix.values, [1200.0, 800.0]);
        assert!(
            fix.labels
                .contains(&("devstack.repository".into(), "shop".into()))
        );
        assert!(fix.labels.contains(&("devstack.node".into(), "n1".into())));
        let svc = d.iter().find(|d| d.labels[0].1 == "service").unwrap();
        for l in [
            ("devstack.service", "api"),
            ("devstack.result", "ok"),
            ("devstack.lane", "main"),
        ] {
            assert!(
                svc.labels.contains(&(l.0.into(), l.1.into())),
                "{:?}",
                svc.labels
            );
        }
        // The hub has no repository: no such label.
        let d = durations(None, &lines[3..4]);
        assert!(!d[0].labels.iter().any(|l| l.0 == "devstack.repository"));
    }

    #[test]
    fn tunnels_become_durations_and_bytes() {
        let t = vec![
            TunnelClosed {
                lane: "fix".into(),
                node: "n1".into(),
                ms: 100.0,
                bytes: 10,
            },
            TunnelClosed {
                lane: "fix".into(),
                node: "n1".into(),
                ms: 300.0,
                bytes: 32,
            },
        ];
        let d = tunnel_durations(Some("shop"), &t);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].values, [100.0, 300.0]);
        assert_eq!(d[0].labels[0], ("devstack.event".into(), "tunnel".into()));
        let g = tunnel_gauges(Some("shop"), &t);
        let bytes = g
            .iter()
            .find(|g| g.name == "devstack.tunnel.bytes")
            .unwrap();
        assert_eq!(bytes.value, 42.0);
        assert!(
            bytes
                .labels
                .contains(&("devstack.lane".into(), "fix".into()))
        );
    }

    #[test]
    fn the_start_is_kept_until_a_round_decides_and_sent_only_with_consent() {
        // The first round finds a pool that collects, with consent: sent.
        assert_eq!(decide(UNDECIDED, true, false, 1), (ON, Round::Send));
        // No pool with consent collects: dropped at the first round.
        assert_eq!(decide(UNDECIDED, false, false, 1), (OFF, Round::Drop));
        // A consenting pool's coordinator did not answer yet: kept a while.
        assert_eq!(decide(UNDECIDED, false, true, 1), (UNDECIDED, Round::Keep));
        assert_eq!(
            decide(UNDECIDED, false, true, UNDECIDED_ROUNDS),
            (OFF, Round::Drop)
        );
        // Once decided, an unanswered coordinator keeps nothing.
        assert_eq!(decide(OFF, false, true, 2), (OFF, Round::Drop));
        assert_eq!(decide(ON, false, true, 9), (OFF, Round::Drop));
        // Consent given (or a coordinator that collects) later: on again.
        assert_eq!(decide(OFF, true, false, 9), (ON, Round::Send));
    }

    #[test]
    fn lines_are_buffered_before_the_first_round_and_not_when_off() {
        let line = |n: &str| {
            let mut m = serde_json::Map::new();
            m.insert("event".into(), Value::String(n.into()));
            m
        };
        let has = |n: &str| BUFFER.lock().unwrap().iter().any(|l| l["event"] == n);
        STATE.store(UNDECIDED, Ordering::Relaxed);
        record(&line("telemetry_test_early"));
        assert!(has("telemetry_test_early"));
        STATE.store(OFF, Ordering::Relaxed);
        record(&line("telemetry_test_off"));
        assert!(!has("telemetry_test_off"));
        STATE.store(IDLE, Ordering::Relaxed);
        record(&line("telemetry_test_idle"));
        assert!(!has("telemetry_test_idle"));
        BUFFER
            .lock()
            .unwrap()
            .retain(|l| l["event"] != "telemetry_test_early");
    }

    #[test]
    fn each_phase_of_a_lane_start_is_a_histogram_of_its_own() {
        let lines = vec![
            serde_json::json!({ "event": "remote_lane_phase", "phase": "sync", "ms": 600 }),
            serde_json::json!({ "event": "remote_lane_phase", "phase": "infrastructure: app: waiting until healthy", "ms": 221_000 }),
            serde_json::json!({ "event": "remote_lane_phase", "phase": "sync", "ms": 700 }),
        ];
        let d = super::durations(None, &lines);
        assert_eq!(d.len(), 2);
        let sync = d
            .iter()
            .find(|d| d.labels.contains(&("devstack.phase".into(), "sync".into())))
            .unwrap();
        assert_eq!(sync.values, [600.0, 700.0]);
    }
}
