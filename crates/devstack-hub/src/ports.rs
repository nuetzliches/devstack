//! `GET /hub/ports` (ADR-0082 point 1): the ports of every registered stack
//! on this machine, and where two stacks meet on one. Computed on request:
//! each running stack's daemon is asked (`GET /v1/ports`), a stopped
//! stack's stack file is read, and what its containers publish that run
//! all the same (ADR-0096) is asked of Docker.

use axum::Json;
use axum::body::Body;
use axum::extract::State;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use devstack_daemon::ports::{PortEntry, StackPorts, declared, overlaps, shifted, with_held};
use http_body_util::BodyExt;
use std::collections::BTreeMap;
use std::time::Duration;

use crate::{AppState, StackEntry};

/// How long one daemon may take to say its ports.
const ASK_FOR: Duration = Duration::from_secs(5);

pub(crate) async fn get(State(s): State<AppState>) -> Response {
    let entries = s.stacks.borrow().clone();
    let held = std::sync::Arc::new(held(&entries).await);
    let mut asks = tokio::task::JoinSet::new();
    for (i, e) in entries.into_iter().enumerate() {
        let s = s.clone();
        let held = held.clone();
        asks.spawn(async move { (i, of(&s, e, &held).await) });
    }
    let mut stacks: Vec<(usize, StackPorts)> = Vec::new();
    while let Some(r) = asks.join_next().await {
        if let Ok((i, Some(p))) = r {
            stacks.push((i, p));
        }
    }
    stacks.sort_by_key(|(i, _)| *i);
    let stacks: Vec<StackPorts> = stacks.into_iter().map(|(_, p)| p).collect();
    let overlaps = overlaps(&stacks);
    Json(serde_json::json!({ "stacks": stacks, "overlaps": overlaps })).into_response()
}

/// What the running containers of the stopped stacks among `entries`
/// publish, by stack id; nothing asked of Docker when every stack runs.
async fn held(entries: &[StackEntry]) -> BTreeMap<String, Vec<PortEntry>> {
    if entries.iter().all(|e| e.running) {
        return BTreeMap::new();
    }
    let files: Vec<(String, std::path::PathBuf, bool)> = entries
        .iter()
        .map(|e| (e.id.clone(), e.file.clone(), e.running))
        .collect();
    tokio::task::spawn_blocking(move || {
        use devstack_daemon::leftover;
        // Every stack's projects, so a running one's containers are not
        // taken for a stopped one's of a shorter name.
        let known: Vec<leftover::Project> = files
            .iter()
            .filter_map(|(id, file, _)| {
                Some(leftover::projects(id, &devstack_config::load(file).ok()?))
            })
            .flatten()
            .collect();
        let mut by = leftover::entries(&known, &leftover::running());
        by.retain(|id, _| files.iter().any(|(i, _, running)| i == id && !running));
        by
    })
    .await
    .unwrap_or_default()
}

/// One stack's ports: its daemon's, or (stopped, or a daemon that does
/// not say) what its stack file declares, with what its containers that
/// run all the same publish (`held`). `None`: a stopped stack whose stack
/// file is gone.
async fn of(
    s: &AppState,
    e: StackEntry,
    held: &BTreeMap<String, Vec<PortEntry>>,
) -> Option<StackPorts> {
    if e.running
        && let Some(ports) = ask(s, &e.id).await
    {
        return Some(StackPorts {
            id: e.id,
            name: e.name,
            running: true,
            containers: false,
            ports,
        });
    }
    let file = e.file.clone();
    if !e.running && !file.is_file() {
        return None;
    }
    // On the ports its shift on this machine moves them to (ADR-0082).
    let shift = e.shift;
    let ports = tokio::task::spawn_blocking(move || {
        devstack_config::load(&file)
            .map(|st| shifted(declared(&st), shift))
            .unwrap_or_default()
    })
    .await
    .unwrap_or_default();
    let mine = held.get(&e.id).cloned().unwrap_or_default();
    Some(StackPorts {
        containers: !mine.is_empty(),
        ports: with_held(ports, mine),
        id: e.id,
        name: e.name,
        running: e.running,
    })
}

/// The daemon of stack `id`'s `GET /v1/ports`; `None` when it does not
/// answer it (an older daemon, one going away).
async fn ask(s: &AppState, id: &str) -> Option<Vec<PortEntry>> {
    #[derive(serde::Deserialize)]
    struct Answer {
        ports: Vec<PortEntry>,
    }
    let req = hyper::Request::get("/v1/ports")
        .header(header::AUTHORIZATION, format!("Bearer {}", s.token))
        .body(Body::empty())
        .ok()?;
    let go = async {
        let resp = crate::daemon_send(|| crate::daemon_now(s, id), req)
            .await
            .ok()?;
        if resp.status() != 200 {
            return None;
        }
        let body = resp.into_body().collect().await.ok()?.to_bytes();
        serde_json::from_slice::<Answer>(&body)
            .ok()
            .map(|a| a.ports)
    };
    tokio::time::timeout(ASK_FOR, go).await.ok().flatten()
}
