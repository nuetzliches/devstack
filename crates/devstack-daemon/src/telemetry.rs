//! A daemon's or the hub's telemetry to its pools' collectors (ADR-0037):
//! only to pools this machine agreed to send to (`telemetry.json` beside the
//! membership) and that collect (the coordinator says so). Every minute: the
//! diagnostic lines since (as OTLP logs), the durations among them (as
//! histograms, by event), and gauges the process registers (lanes and
//! services running). Lossy: a bounded buffer, dropped when nobody takes it.

use devstack_node::member::Local;
use devstack_node::telemetry::{Consent, Durations, Gauge, metrics_body};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const EVERY: Duration = Duration::from_secs(60);
/// How long a coordinator's "we collect" holds before it is asked again.
const ASK_EVERY: Duration = Duration::from_secs(600);
const MAX_LINES: usize = 5000;

/// Whether any pool takes telemetry now: only then are lines kept.
static ACTIVE: AtomicBool = AtomicBool::new(false);
static BUFFER: Mutex<Vec<Value>> = Mutex::new(Vec::new());
type GaugeFn = Box<dyn Fn() -> Vec<Gauge> + Send + Sync>;
static GAUGES: OnceLock<GaugeFn> = OnceLock::new();

/// Whether a pool takes the diagnostic lines now.
pub fn active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

/// Keep a diagnostic line for the next send (when a pool takes them).
pub fn record(line: &serde_json::Map<String, Value>) {
    if !ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    let mut b = BUFFER.lock().unwrap();
    if b.len() < MAX_LINES {
        b.push(Value::Object(line.clone()));
    }
}

/// What the process measures now, read at each send.
pub fn set_gauges(f: impl Fn() -> Vec<Gauge> + Send + Sync + 'static) {
    let _ = GAUGES.set(Box::new(f));
}

fn state_dir() -> PathBuf {
    crate::token::token_path()
        .parent()
        .expect("token path has a parent")
        .to_path_buf()
}

/// Start sending, once per process, as `service`.
pub fn start(service: &'static str) {
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("telemetry".into())
        .spawn(move || run(service));
}

fn run(service: &str) {
    // Per pool directory: whether its coordinator collects, as of when.
    let mut collects: HashMap<PathBuf, (Instant, bool)> = HashMap::new();
    let mut wait = Duration::from_secs(10);
    loop {
        std::thread::sleep(wait);
        wait = EVERY;
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
                    Err(_) => false,
                });
                on.then_some(local)
            })
            .collect();
        ACTIVE.store(!targets.is_empty(), Ordering::Relaxed);
        let lines = std::mem::take(&mut *BUFFER.lock().unwrap());
        if targets.is_empty() {
            continue;
        }
        let logs = (!lines.is_empty()).then(|| crate::diag::otlp_body(service, &lines));
        let gauges = GAUGES.get().map(|f| f()).unwrap_or_default();
        let metrics = metrics_body(service, &gauges, &durations(&lines));
        for t in &targets {
            if let Some(l) = &logs {
                let _ = t.telemetry_send("logs", l);
            }
            let _ = t.telemetry_send("metrics", &metrics);
        }
    }
}

/// The durations among `lines` (`ms`), by event: `devstack.duration` with
/// the event as its label.
fn durations(lines: &[Value]) -> Vec<Durations> {
    let mut by: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for l in lines {
        let (Some(event), Some(ms)) = (l["event"].as_str(), l["ms"].as_f64()) else {
            continue;
        };
        by.entry(event.to_string()).or_default().push(ms);
    }
    by.into_iter()
        .map(|(event, values)| Durations {
            name: "devstack.duration".into(),
            values,
            labels: vec![("devstack.event".into(), event)],
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn durations_are_grouped_by_event() {
        let lines = vec![
            serde_json::json!({ "event": "remote_lane_started", "ms": 1200 }),
            serde_json::json!({ "event": "remote_lane_started", "ms": 800 }),
            serde_json::json!({ "event": "lane_moved", "ms": 5000 }),
            serde_json::json!({ "event": "service", "state": "running" }),
        ];
        let d = super::durations(&lines);
        assert_eq!(d.len(), 2);
        assert_eq!(d[1].values, [1200.0, 800.0]);
        assert_eq!(d[1].labels[0].1, "remote_lane_started");
    }
}
