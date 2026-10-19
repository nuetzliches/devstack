//! This machine's share of the pool (ADR-0031 point 3): the hub measures what
//! is free, offers half of it (capped, only with the switch on) and reports it
//! to the coordinator every 15 s, which also tells the pool the machine is
//! online. A machine in no pool, or not approved yet, reports nothing.
//!
//! - `GET /hub/pool/share`: the switch, the caps, what is free and offered now,
//!   the last report
//! - `PUT /hub/pool/share` `{enabled, max_cpus?, max_memory?}`: change them;
//!   reports at once
//!
//! Both per pool (`?pool=<name>`, ADR-0034): each pool has its own switch
//! and caps, and gets its own report.

use axum::Json;
use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use devstack_node::coordinator::Report;
use devstack_node::share::{Amount, Settings, idle_cpus};
use devstack_node::store::State;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::mpsc;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

const EVERY: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Default, Serialize)]
pub struct Status {
    pub settings: Settings,
    /// What the machine has in all.
    pub total: Amount,
    /// What is free now.
    pub free: Amount,
    /// What it offers now.
    pub offer: Amount,
    /// Whether it belongs to a pool as an approved member (else nothing is
    /// reported).
    pub member: bool,
    /// When the last report reached the coordinator (Unix ms).
    pub reported_at: Option<u128>,
    /// Why the last report failed.
    pub error: Option<String>,
    /// The devstack the coordinator runs, from its answer to the last
    /// report; `None` before one, or from an older coordinator.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coordinator: Option<devstack_node::version::Release>,
}

struct Sharing {
    /// Per pool.
    status: Mutex<BTreeMap<String, Status>>,
    poke: Mutex<mpsc::Sender<()>>,
}

#[derive(Deserialize, Default)]
pub struct PoolQuery {
    #[serde(default)]
    pool: Option<String>,
}

impl PoolQuery {
    fn name(&self) -> Option<String> {
        let p = self
            .pool
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .unwrap_or(devstack_node::member::DEFAULT_POOL);
        devstack_node::member::valid_pool_name(p).then(|| p.to_string())
    }
}

static SHARING: OnceLock<Sharing> = OnceLock::new();

fn state() -> std::path::PathBuf {
    devstack_daemon::token::token_path()
        .parent()
        .expect("token path has a parent")
        .to_path_buf()
}

fn dir(pool: &str) -> std::path::PathBuf {
    devstack_node::member::pool_dir(&state(), pool)
}

/// A pool's status, its settings read from its directory the first time.
fn entry<'a>(map: &'a mut BTreeMap<String, Status>, pool: &str) -> &'a mut Status {
    map.entry(pool.to_string()).or_insert_with(|| Status {
        settings: Settings::load(&dir(pool)),
        ..Status::default()
    })
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis())
}

/// Start measuring and reporting, once per hub.
pub fn start() {
    let (tx, rx) = mpsc::channel();
    if SHARING
        .set(Sharing {
            status: Mutex::new(BTreeMap::new()),
            poke: Mutex::new(tx),
        })
        .is_err()
    {
        return;
    }
    std::thread::spawn(move || run(&rx));
}

fn run(poked: &mpsc::Receiver<()>) {
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    let mut before = devstack_proc::cpu_times();
    // The first CPU reading needs a second one to say anything.
    let mut wait = Duration::from_secs(2);
    loop {
        if let Err(mpsc::RecvTimeoutError::Disconnected) = poked.recv_timeout(wait) {
            return;
        }
        wait = EVERY;
        let after = devstack_proc::cpu_times();
        let free = Amount {
            cpus: match (before, after) {
                (Some(b), Some(a)) => idle_cpus(b, a, cores),
                _ => 0.0,
            },
            memory: devstack_proc::memory_available().unwrap_or(0),
        };
        if after.is_some() {
            before = after;
        }
        let total = Amount {
            cpus: cores as f64,
            memory: devstack_proc::memory_total().unwrap_or(0),
        };
        let Some(s) = SHARING.get() else { return };
        let gpus = devstack_node::share::gpus();
        for (pool, _) in devstack_node::member::pools(&state()) {
            let settings = entry(&mut s.status.lock().unwrap(), &pool).settings.clone();
            let offer = settings.offer(free);
            let result = report(&pool, &settings, offer, total, &gpus);
            let mut map = s.status.lock().unwrap();
            let st = entry(&mut map, &pool);
            st.total = total;
            st.free = free;
            st.offer = offer;
            match result {
                Ok(answer) => {
                    st.member = answer.is_some();
                    st.error = None;
                    if let Some(coordinator) = answer {
                        st.reported_at = Some(now_ms());
                        st.coordinator = coordinator;
                    }
                }
                Err(e) => st.error = Some(e),
            }
        }
    }
}

/// Report to the coordinator: what it says it runs; `Ok(None)` when this
/// machine is no approved member (nothing to report to).
fn report(
    pool: &str,
    settings: &Settings,
    offer: Amount,
    total: Amount,
    gpus: &[devstack_node::share::Gpu],
) -> Result<Option<Option<devstack_node::version::Release>>, String> {
    let local = devstack_node::member::Local::new(&dir(pool));
    match local.membership() {
        Ok(Some(m)) if m.state == State::Approved => {}
        Ok(_) => return Ok(None),
        Err(e) => return Err(e.to_string()),
    }
    local
        .report(&Report {
            sharing: settings.enabled,
            cpus: offer.cpus,
            memory: offer.memory,
            api: None,
            dedicated: false,
            devstack: Some(devstack_node::version::Release::this()),
            kept: None,
            leases: Vec::new(),
            total: Some(total),
            used: None,
            gpus: gpus
                .iter()
                .map(|g| devstack_node::share::Gpu {
                    offered: settings.enabled,
                    ..g.clone()
                })
                .collect(),
        })
        .map(|a| Some(a.coordinator))
        .map_err(|e| e.to_string())
}

pub async fn get(Query(q): Query<PoolQuery>) -> Response {
    let Some(pool) = q.name() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "pool: a-z, 0-9, `-` and `_`, at most 32" })),
        )
            .into_response();
    };
    match SHARING.get() {
        Some(s) => {
            let mut map = s.status.lock().unwrap();
            // A pool not reported yet still shows what is measured.
            let any = map.values().next().cloned();
            let st = entry(&mut map, &pool);
            if let Some(a) = any.filter(|_| st.reported_at.is_none() && st.total.cpus == 0.0) {
                st.total = a.total;
                st.free = a.free;
                st.offer = st.settings.offer(a.free);
            }
            Json(st.clone()).into_response()
        }
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": "sharing has not started" })),
        )
            .into_response(),
    }
}

pub async fn put(Query(q): Query<PoolQuery>, Json(new): Json<Settings>) -> Response {
    let bad = |m: &str| {
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": m })),
        )
            .into_response()
    };
    if new.max_cpus.is_some_and(|c| !c.is_finite() || c < 0.0) {
        return bad("max_cpus: a number of cores, 0 or more");
    }
    let Some(pool) = q.name() else {
        return bad("pool: a-z, 0-9, `-` and `_`, at most 32");
    };
    let Some(s) = SHARING.get() else {
        return bad("sharing has not started");
    };
    if let Err(e) = new.save(&dir(&pool)) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response();
    }
    let status = {
        let mut map = s.status.lock().unwrap();
        let st = entry(&mut map, &pool);
        st.settings = new;
        st.offer = st.settings.offer(st.free);
        st.clone()
    };
    let _ = s.poke.lock().unwrap().send(());
    Json(status).into_response()
}
