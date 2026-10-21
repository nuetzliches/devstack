//! This machine in the pool (ADR-0031): the hub measures what the machine has
//! and what is free and reports it to the coordinator every 15 s, which also
//! tells the pool the machine is online and which devstack it runs. A machine
//! in no pool, or not approved yet, reports nothing.
//!
//! The hub offers nothing to the pool. A machine runs other members' lanes
//! with a node agent (`devstack node agent`, granted by an admin), and the
//! dashboard's share switch never started one: it only reported capacity
//! nobody could use, and is gone until it does (#171; on Windows, which
//! cannot run the Linux lanes, #168).
//!
//! - `GET /hub/pool/share?pool=`: what is measured, the last report, the
//!   coordinator's devstack, and why nothing is offered (`unavailable`)

use axum::Json;
use axum::extract::Query;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use devstack_node::coordinator::Report;
use devstack_node::share::{Amount, idle_cpus};
use devstack_node::store::State;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

const EVERY: Duration = Duration::from_secs(15);

/// Why the hub offers nothing to the pool.
const NOT_SHARED: &str = "A machine runs other members' lanes with a node agent \
     (`devstack node agent`, granted by an admin); the dashboard does not start one yet \
     (#171, on Windows #168).";

#[derive(Debug, Clone, Default, Serialize)]
pub struct Status {
    /// What the machine has in all.
    pub total: Amount,
    /// What is free now.
    pub free: Amount,
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
    /// Why this machine offers the pool nothing.
    pub unavailable: &'static str,
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

/// Per pool.
static STATUS: OnceLock<Mutex<BTreeMap<String, Status>>> = OnceLock::new();

fn state() -> std::path::PathBuf {
    devstack_daemon::token::token_path()
        .parent()
        .expect("token path has a parent")
        .to_path_buf()
}

fn dir(pool: &str) -> std::path::PathBuf {
    devstack_node::member::pool_dir(&state(), pool)
}

fn entry<'a>(map: &'a mut BTreeMap<String, Status>, pool: &str) -> &'a mut Status {
    map.entry(pool.to_string()).or_insert_with(|| Status {
        unavailable: NOT_SHARED,
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
    if STATUS.set(Mutex::new(BTreeMap::new())).is_err() {
        return;
    }
    std::thread::spawn(run);
}

fn run() {
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    let mut before = devstack_proc::cpu_times();
    // The first CPU reading needs a second one to say anything.
    let mut wait = Duration::from_secs(2);
    loop {
        std::thread::sleep(wait);
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
        let Some(status) = STATUS.get() else { return };
        let gpus = devstack_node::share::gpus();
        for (pool, _) in devstack_node::member::pools(&state()) {
            let result = report(&pool, total, &gpus);
            let mut map = status.lock().unwrap();
            let st = entry(&mut map, &pool);
            st.total = total;
            st.free = free;
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
            drop(map);
            crate::guests::seen(&pool, guests(&pool));
        }
    }
}

/// Other members' lanes on this machine in `pool`; `None` when the
/// coordinator could not be asked. The member list is read only when a
/// lease names this machine.
fn guests(pool: &str) -> Option<Vec<crate::guests::Guest>> {
    let local = devstack_node::member::Local::new(&dir(pool));
    let m = match local.membership() {
        Ok(Some(m)) if m.state == State::Approved => m,
        Ok(_) => return Some(Vec::new()),
        Err(_) => return None,
    };
    let leases = local.leases().ok()?;
    if !leases.iter().any(|l| l.node == m.id && l.member != m.id) {
        return Some(Vec::new());
    }
    let nodes = local.nodes().ok()?;
    Some(crate::guests::of(pool, &m.id, &leases, &nodes))
}

/// Report to the coordinator, offering nothing: what it says it runs;
/// `Ok(None)` when this machine is no approved member (nothing to report to).
fn report(
    pool: &str,
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
            sharing: false,
            cpus: 0.0,
            memory: 0,
            api: None,
            dedicated: false,
            devstack: Some(devstack_node::version::Release::this()),
            kept: None,
            update: None,
            no_update: None,
            slots: Vec::new(),
            disk_free: None,
            leases: Vec::new(),
            total: Some(total),
            used: None,
            gpus: gpus
                .iter()
                .map(|g| devstack_node::share::Gpu {
                    offered: false,
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
    match STATUS.get() {
        Some(status) => {
            let mut map = status.lock().unwrap();
            // A pool not reported yet still shows what is measured.
            let any = map.values().next().cloned();
            let st = entry(&mut map, &pool);
            if let Some(a) = any.filter(|_| st.reported_at.is_none() && st.total.cpus == 0.0) {
                st.total = a.total;
                st.free = a.free;
            }
            Json(st.clone()).into_response()
        }
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": "reporting has not started" })),
        )
            .into_response(),
    }
}
