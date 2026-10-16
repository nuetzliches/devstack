//! This machine's share of the pool (ADR-0031 point 3): the hub measures what
//! is free, offers half of it (capped, only with the switch on) and reports it
//! to the coordinator every 15 s, which also tells the pool the machine is
//! online. A machine in no pool, or not approved yet, reports nothing.
//!
//! - `GET /hub/pool/share`: the switch, the caps, what is free and offered now,
//!   the last report
//! - `PUT /hub/pool/share` `{enabled, max_cpus?, max_memory?}`: change them;
//!   reports at once

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use devstack_node::coordinator::Report;
use devstack_node::share::{Amount, Settings, idle_cpus};
use devstack_node::store::State;
use serde::Serialize;
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
}

struct Sharing {
    status: Mutex<Status>,
    poke: Mutex<mpsc::Sender<()>>,
}

static SHARING: OnceLock<Sharing> = OnceLock::new();

fn dir() -> std::path::PathBuf {
    let state = devstack_daemon::token::token_path()
        .parent()
        .expect("token path has a parent")
        .to_path_buf();
    devstack_node::member::default_dir(&state)
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
            status: Mutex::new(Status {
                settings: Settings::load(&dir()),
                ..Status::default()
            }),
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
        let settings = s.status.lock().unwrap().settings.clone();
        let offer = settings.offer(free);
        let result = report(&settings, offer);
        let mut st = s.status.lock().unwrap();
        st.total = total;
        st.free = free;
        st.offer = offer;
        match result {
            Ok(member) => {
                st.member = member;
                st.error = None;
                if member {
                    st.reported_at = Some(now_ms());
                }
            }
            Err(e) => st.error = Some(e),
        }
    }
}

/// Report to the coordinator; `Ok(false)` when this machine is no approved
/// member (nothing to report to).
fn report(settings: &Settings, offer: Amount) -> Result<bool, String> {
    let local = devstack_node::member::Local::new(&dir());
    match local.membership() {
        Ok(Some(m)) if m.state == State::Approved => {}
        Ok(_) => return Ok(false),
        Err(e) => return Err(e.to_string()),
    }
    local
        .report(&Report {
            sharing: settings.enabled,
            cpus: offer.cpus,
            memory: offer.memory,
            api: None,
        })
        .map(|()| true)
        .map_err(|e| e.to_string())
}

pub async fn get() -> Response {
    match SHARING.get() {
        Some(s) => Json(s.status.lock().unwrap().clone()).into_response(),
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": "sharing has not started" })),
        )
            .into_response(),
    }
}

pub async fn put(Json(new): Json<Settings>) -> Response {
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
    let Some(s) = SHARING.get() else {
        return bad("sharing has not started");
    };
    if let Err(e) = new.save(&dir()) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response();
    }
    let status = {
        let mut st = s.status.lock().unwrap();
        st.settings = new;
        st.offer = st.settings.offer(st.free);
        st.clone()
    };
    let _ = s.poke.lock().unwrap().send(());
    Json(status).into_response()
}
