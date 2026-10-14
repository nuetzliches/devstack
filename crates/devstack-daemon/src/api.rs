//! The control API (ADR-0007), secured per ADR-0019: loopback only, every
//! request authenticated, `Host` checked against DNS rebinding, `Origin`
//! checked on mutating requests, no CORS.

use crate::commands::{self, RunError, Runs};
use crate::supervisor::Supervisor;
use axum::Json;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use std::sync::Arc;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;

#[derive(Clone)]
pub struct AppState {
    pub sup: Arc<Supervisor>,
    pub runs: Arc<Runs>,
    pub usage: Arc<crate::usage::Sampler>,
    pub token: Arc<String>,
    pub port: u16,
    pub shutdown: Arc<tokio::sync::Notify>,
}

pub fn router(state: AppState) -> axum::Router {
    axum::Router::new()
        .route("/v1/stack", get(stack))
        .route("/v1/up", post(up))
        .route("/v1/down", post(down))
        .route("/v1/shutdown", post(shutdown))
        .route("/v1/services/{name}/{action}", post(service_action))
        .route("/v1/services/{name}/logs", get(logs))
        .route("/v1/events", get(events))
        .route("/v1/usage", get(usage))
        .route("/v1/commands/{name}", post(run_command))
        .route("/v1/commands/{name}/runs", get(command_runs))
        .route("/v1/runs/{id}", get(run_info))
        .route("/v1/runs/{id}/log", get(run_log))
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state)
}

fn err(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(serde_json::json!({ "error": msg.into() }))).into_response()
}

/// ADR-0019, in order: Host, credential, Origin on mutating methods.
async fn guard(State(s): State<AppState>, req: Request, next: Next) -> Response {
    let h = req.headers();
    let host = h
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let allowed = [
        format!("127.0.0.1:{}", s.port),
        format!("localhost:{}", s.port),
        format!("[::1]:{}", s.port),
    ];
    if !allowed.iter().any(|a| a == host) {
        return err(StatusCode::FORBIDDEN, "unexpected Host");
    }
    let bearer = h
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    if !bearer.is_some_and(|t| constant_time_eq(t.as_bytes(), s.token.as_bytes())) {
        return err(
            StatusCode::UNAUTHORIZED,
            "missing or wrong token (see `devstack` docs, ADR-0019)",
        );
    }
    if req.method() != Method::GET
        && let Some(origin) = h.get(header::ORIGIN).and_then(|v| v.to_str().ok())
        && !allowed.iter().any(|a| origin == format!("http://{a}"))
    {
        return err(StatusCode::FORBIDDEN, "foreign Origin");
    }
    next.run(req).await
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn stack(State(s): State<AppState>) -> Response {
    Json(s.sup.view()).into_response()
}

/// Long operations run in the background; progress arrives on `/v1/events`.
async fn up(State(s): State<AppState>) -> Response {
    let sup = s.sup.clone();
    tokio::task::spawn_blocking(move || {
        if let Err(e) = sup.up() {
            sup.emit(crate::state::EventBody::Infra {
                group: "*".into(),
                message: format!("up stopped: {e}"),
            });
        }
    });
    StatusCode::ACCEPTED.into_response()
}

#[derive(Deserialize)]
struct DownQuery {
    #[serde(default)]
    infra: bool,
}

async fn down(State(s): State<AppState>, Query(q): Query<DownQuery>) -> Response {
    let sup = s.sup.clone();
    match tokio::task::spawn_blocking(move || sup.down(q.infra)).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn shutdown(State(s): State<AppState>) -> Response {
    s.shutdown.notify_one();
    StatusCode::ACCEPTED.into_response()
}

async fn service_action(
    State(s): State<AppState>,
    Path((name, action)): Path<(String, String)>,
) -> Response {
    if !s.sup.stack.services.contains_key(&name) {
        return err(StatusCode::NOT_FOUND, format!("no service `{name}`"));
    }
    let sup = s.sup.clone();
    let result = match action.as_str() {
        "start" => tokio::task::spawn_blocking(move || sup.start(&name)).await,
        "stop" => tokio::task::spawn_blocking(move || sup.stop(&name)).await,
        "restart" => tokio::task::spawn_blocking(move || sup.restart(&name)).await,
        "promote" => tokio::task::spawn_blocking(move || sup.promote(&name)).await,
        "rollback" => tokio::task::spawn_blocking(move || sup.rollback(&name)).await,
        _ => return err(StatusCode::NOT_FOUND, format!("unknown action `{action}`")),
    };
    match result {
        Ok(Ok(())) => Json(s.sup.view()).into_response(),
        Ok(Err(e)) => err(StatusCode::CONFLICT, e),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[derive(Deserialize)]
struct LogQuery {
    tail: Option<usize>,
}

async fn logs(
    State(s): State<AppState>,
    Path(name): Path<String>,
    Query(q): Query<LogQuery>,
) -> Response {
    match s.sup.service_log(&name) {
        None => err(StatusCode::NOT_FOUND, format!("`{name}` has no log yet")),
        Some(p) => text_tail(&p, q.tail.unwrap_or(200)),
    }
}

fn text_tail(path: &std::path::Path, n: usize) -> Response {
    let text = std::fs::read(path)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    let lines: Vec<&str> = text.lines().rev().take(n).collect();
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        "text/plain; charset=utf-8".parse().unwrap(),
    );
    (
        headers,
        lines.into_iter().rev().collect::<Vec<_>>().join("\n"),
    )
        .into_response()
}

async fn events(State(s): State<AppState>) -> Response {
    let stream = BroadcastStream::new(s.sup.events.subscribe())
        .filter_map(|e| e.ok())
        .map(|e| {
            Ok::<_, std::convert::Infallible>(SseEvent::default().json_data(e).unwrap_or_default())
        });
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

#[derive(Deserialize)]
struct UsageQuery {
    #[serde(default)]
    history: bool,
}

/// Plan §12: current load per service, build job, container, group and lane;
/// `?history=true` adds the last ten minutes per series.
async fn usage(State(s): State<AppState>, Query(q): Query<UsageQuery>) -> Response {
    Json(s.usage.report(&s.sup, q.history)).into_response()
}

#[derive(Deserialize, Default)]
struct RunBody {
    #[serde(default)]
    confirm: bool,
}

async fn run_command(
    State(s): State<AppState>,
    Path(name): Path<String>,
    body: Option<Json<RunBody>>,
) -> Response {
    let confirm = body.map(|b| b.0.confirm).unwrap_or(false);
    match commands::start(&s.sup, &s.runs, &name, confirm) {
        Ok(id) => (StatusCode::ACCEPTED, Json(serde_json::json!({ "run": id }))).into_response(),
        Err(e @ RunError::Unknown(_)) => err(StatusCode::NOT_FOUND, e.to_string()),
        Err(RunError::NeedsConfirmation(text)) => (
            StatusCode::PRECONDITION_REQUIRED,
            Json(serde_json::json!({ "confirm": text })),
        )
            .into_response(),
        Err(e) => err(StatusCode::CONFLICT, e.to_string()),
    }
}

async fn command_runs(State(s): State<AppState>, Path(name): Path<String>) -> Response {
    Json(s.runs.list(Some(&name))).into_response()
}

async fn run_info(State(s): State<AppState>, Path(id): Path<u64>) -> Response {
    match s.runs.get(id) {
        Some(r) => Json(r).into_response(),
        None => err(StatusCode::NOT_FOUND, format!("no run {id}")),
    }
}

async fn run_log(State(s): State<AppState>, Path(id): Path<u64>) -> Response {
    match s.runs.get(id) {
        Some(r) => text_tail(&r.log, usize::MAX),
        None => err(StatusCode::NOT_FOUND, format!("no run {id}")),
    }
}
