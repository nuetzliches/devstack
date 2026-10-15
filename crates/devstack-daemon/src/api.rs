//! The control API (ADR-0007), secured per ADR-0019: loopback only, every
//! request authenticated, `Host` checked against DNS rebinding, `Origin`
//! checked on mutating requests, no CORS.

use crate::commands::{self, RunError, Runs};
use crate::logfiles;
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
        .route("/v1/groups/{name}/{action}", post(group_action))
        .route("/v1/logs", get(logs_all))
        .route("/v1/services/{name}/logs", get(logs))
        .route("/v1/services/{name}/logs/stream", get(log_stream))
        .route("/v1/services/{name}/logs/files", get(log_files))
        .route("/v1/services/{name}/build-log", get(build_log))
        .route(
            "/v1/infra/{group}/{service}/restart",
            post(restart_container),
        )
        .route("/v1/infra/{group}/{service}/logs", get(infra_logs))
        .route("/v1/events", get(events))
        .route("/v1/usage", get(usage))
        .route("/v1/commands/{name}", post(run_command))
        .route("/v1/commands/{name}/runs", get(command_runs))
        .route("/v1/runs/{id}", get(run_info))
        .route("/v1/runs/{id}/log", get(run_log))
        .route("/v1/runs/{id}/log/stream", get(run_log_stream))
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .layer(middleware::from_fn(audit))
        .with_state(state)
}

/// Every request that changes something goes to the diagnostic log with its
/// answer, refused ones included (outermost layer): "who restarted that, and
/// did it work" is answerable afterwards.
async fn audit(req: Request, next: Next) -> Response {
    let method = req.method().clone();
    if method == axum::http::Method::GET || method == axum::http::Method::HEAD {
        return next.run(req).await;
    }
    let path = req.uri().path().to_string();
    let res = next.run(req).await;
    let status = res.status().as_u16();
    crate::diag::log(
        if status >= 400 { "warn" } else { "info" },
        "request",
        serde_json::json!({ "method": method.as_str(), "path": path, "status": status }),
    );
    res
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
        // A group by its name, so `devstack restart backend` works.
        if s.sup.stack.groups.contains_key(&name) {
            return group_action(State(s), Path((name, action))).await;
        }
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

/// `POST /v1/groups/{name}/{start|stop|restart}`: every member; `409` names
/// the members that failed (the others ran).
async fn group_action(
    State(s): State<AppState>,
    Path((name, action)): Path<(String, String)>,
) -> Response {
    if !s.sup.stack.groups.contains_key(&name) {
        return err(StatusCode::NOT_FOUND, format!("no group `{name}`"));
    }
    if !matches!(action.as_str(), "start" | "stop" | "restart") {
        return err(
            StatusCode::NOT_FOUND,
            format!("unknown group action `{action}`"),
        );
    }
    let sup = s.sup.clone();
    match tokio::task::spawn_blocking(move || sup.group_action(&name, &action)).await {
        Ok(Ok(())) => Json(s.sup.view()).into_response(),
        Ok(Err(e)) => err(StatusCode::CONFLICT, e),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[derive(Deserialize)]
struct LogQuery {
    tail: Option<usize>,
    /// Only lines containing this, case-insensitive.
    grep: Option<String>,
    /// The build log of this build and the logs of the instances that ran it
    /// (a prefix of the id is enough).
    build: Option<String>,
    /// The logs of the instances written to within this duration (`30m`, `2h`).
    since: Option<String>,
    /// One kept log by its name (see `logs/files`).
    file: Option<String>,
}

impl LogQuery {
    fn since(&self) -> Result<Option<std::time::Duration>, String> {
        self.since
            .as_deref()
            .filter(|v| !v.is_empty())
            .map(devstack_config::model::parse_duration)
            .transpose()
            .map_err(|e| format!("since: {e}"))
    }
}

async fn logs(
    State(s): State<AppState>,
    Path(name): Path<String>,
    Query(q): Query<LogQuery>,
) -> Response {
    if !s.sup.stack.services.contains_key(&name) {
        // A container by its Compose service name, so `devstack logs postgres` works.
        return match s.sup.container_group(&name) {
            Some(group) => container_logs(&s, group, name, &q).await,
            None => err(StatusCode::NOT_FOUND, format!("no service `{name}`")),
        };
    }
    let tail = q.tail.unwrap_or(200);
    let grep = q.grep.as_deref().filter(|g| !g.is_empty());
    let dir = s.sup.log_dir(&name);
    let current = s.sup.service_log(&name);
    if let Some(file) = q.file.as_deref() {
        let Some(path) = logfiles::resolve(&dir, file) else {
            return err(
                StatusCode::NOT_FOUND,
                format!("`{name}` has no log `{file}`"),
            );
        };
        let lines = matching(&path, tail, grep).join("\n");
        return text(if file.starts_with("build-") {
            s.sup.repo_paths(&lines)
        } else {
            lines
        });
    }
    let since = match q.since() {
        Ok(v) => v,
        Err(e) => return err(StatusCode::BAD_REQUEST, e),
    };
    let build = q.build.as_deref().filter(|b| !b.is_empty());
    if build.is_none() && since.is_none() {
        return match current {
            None => err(StatusCode::NOT_FOUND, format!("`{name}` has no log yet")),
            Some(p) => text(matching(&p, tail, grep).join("\n")),
        };
    }
    let sel = logfiles::select(&logfiles::list(&dir, current.as_deref()), build, since);
    if sel.is_empty() {
        return match build {
            Some(b) => err(
                StatusCode::NOT_FOUND,
                format!("`{name}` has no kept log of build `{b}`"),
            ),
            None => text(String::new()),
        };
    }
    text(s.sup.repo_paths(&combined(&dir, &sel, tail, grep, None)))
}

/// `GET /v1/services/{name}/logs/files`: the kept logs, newest first.
async fn log_files(State(s): State<AppState>, Path(name): Path<String>) -> Response {
    if !s.sup.stack.services.contains_key(&name) {
        return err(StatusCode::NOT_FOUND, format!("no service `{name}`"));
    }
    let current = s.sup.service_log(&name);
    Json(logfiles::list(&s.sup.log_dir(&name), current.as_deref())).into_response()
}

/// Several logs as one text, oldest first, each after a header line; `tail`
/// counts the lines of the logs, not the headers. With `prefix`, every line
/// starts with it (the service, when searching all).
fn combined(
    dir: &std::path::Path,
    sel: &[logfiles::LogFile],
    tail: usize,
    grep: Option<&str>,
    prefix: Option<&str>,
) -> String {
    let mut parts: Vec<(String, Vec<String>)> = sel
        .iter()
        .map(|f| {
            (
                logfiles::header(f),
                matching(&dir.join(&f.file), usize::MAX, grep),
            )
        })
        .filter(|(_, lines)| grep.is_none() || !lines.is_empty())
        .collect();
    // Keep the last `tail` lines across all parts; every part keeps its
    // header, so what was cut is visible.
    let mut left = tail;
    let mut cut = vec![0usize; parts.len()];
    for (i, (_, lines)) in parts.iter_mut().enumerate().rev() {
        let keep = lines.len().min(left);
        cut[i] = lines.len() - keep;
        lines.drain(..cut[i]);
        left -= keep;
    }
    let pre = prefix.map(|p| format!("{p} | ")).unwrap_or_default();
    let mut out = Vec::new();
    for ((h, lines), cut) in parts.into_iter().zip(cut) {
        out.push(format!("{pre}{h}"));
        if cut > 0 {
            out.push(format!(
                "{pre}… {cut} earlier line(s) not shown (raise `tail`)"
            ));
        }
        out.extend(lines.into_iter().map(|l| format!("{pre}{l}")));
    }
    out.join("\n")
}

/// `GET /v1/logs?grep=…`: "where did this come from" across every service's
/// current log, each line prefixed with its service; `tail` lines per service.
async fn logs_all(State(s): State<AppState>, Query(q): Query<LogQuery>) -> Response {
    let Some(grep) = q.grep.as_deref().filter(|g| !g.is_empty()) else {
        return err(StatusCode::BAD_REQUEST, "`grep` is required");
    };
    let since = match q.since() {
        Ok(v) => v,
        Err(e) => return err(StatusCode::BAD_REQUEST, e),
    };
    let tail = q.tail.unwrap_or(50);
    let mut out = Vec::new();
    for svc in s.sup.view().services {
        if let Some(since) = since {
            // Every instance written to within `since`, not only the current one.
            let dir = s.sup.log_dir(&svc.name);
            let current = s.sup.service_log(&svc.name);
            let sel =
                logfiles::select(&logfiles::list(&dir, current.as_deref()), None, Some(since));
            let text = combined(&dir, &sel, tail, Some(grep), Some(&svc.name));
            if !text.is_empty() {
                out.push(text);
            }
        } else if let Some(p) = s.sup.service_log(&svc.name) {
            for line in matching(&p, tail, Some(grep)) {
                out.push(format!("{} | {line}", svc.name));
            }
        }
    }
    text(out.join("\n"))
}

/// The last `n` lines of a file, optionally only those containing `grep`.
fn matching(path: &std::path::Path, n: usize, grep: Option<&str>) -> Vec<String> {
    let text = std::fs::read(path)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    let needle = grep.map(str::to_lowercase);
    let mut lines: Vec<String> = text
        .lines()
        .rev()
        .filter(|l| needle.as_ref().is_none_or(|g| l.to_lowercase().contains(g)))
        .take(n)
        .map(str::to_string)
        .collect();
    lines.reverse();
    lines
}

fn text(body: String) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        "text/plain; charset=utf-8".parse().unwrap(),
    );
    (headers, body).into_response()
}

fn text_tail(path: &std::path::Path, n: usize) -> Response {
    text(matching(path, n, None).join("\n"))
}

/// The event stream starts with a snapshot of the whole state, so a client
/// never needs to ask for it on a timer (ADR-0021). Subscribed before the
/// snapshot is taken: nothing that happens in between is lost.
async fn events(State(s): State<AppState>) -> Response {
    let rx = s.sup.events.subscribe();
    let snapshot = crate::state::Event {
        seq: 0,
        at: crate::state::now_ms(),
        body: crate::state::EventBody::Snapshot {
            stack: Box::new(s.sup.view()),
        },
    };
    let stream = tokio_stream::once(snapshot)
        .chain(BroadcastStream::new(rx).filter_map(|e| e.ok()))
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

// ------------------------------------------------------------ log streams

fn sse_json(rx: tokio::sync::mpsc::Receiver<serde_json::Value>) -> Response {
    let stream = tokio_stream::wrappers::ReceiverStream::new(rx)
        .map(|v| Ok::<_, std::convert::Infallible>(SseEvent::default().data(v.to_string())));
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// Lines appended to `path` since `pos`; a trailing partial line waits.
fn read_new(path: &std::path::Path, pos: &mut u64, partial: &mut String) -> Vec<String> {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if len < *pos {
        *pos = 0; // truncated or replaced
        partial.clear();
    }
    if len == *pos || f.seek(SeekFrom::Start(*pos)).is_err() {
        return Vec::new();
    }
    let mut buf = Vec::new();
    let _ = f.read_to_end(&mut buf);
    *pos += buf.len() as u64;
    partial.push_str(&String::from_utf8_lossy(&buf));
    let mut lines: Vec<String> = partial
        .split('\n')
        .map(|l| l.trim_end_matches('\r').to_string())
        .collect();
    *partial = lines.pop().unwrap_or_default();
    lines
}

fn last_lines(path: &std::path::Path, n: usize) -> (Vec<String>, u64) {
    let text = std::fs::read(path).unwrap_or_default();
    let len = text.len() as u64;
    let text = String::from_utf8_lossy(&text);
    let mut lines: Vec<String> = text.lines().rev().take(n).map(str::to_string).collect();
    lines.reverse();
    (lines, len)
}

#[derive(Deserialize)]
struct StreamQuery {
    tail: Option<usize>,
}

fn line(text: String) -> serde_json::Value {
    serde_json::json!({"type": "line", "text": text})
}

/// The current instance's console output, then every new line; follows the
/// service across a replacement (an `instance` event marks the switch).
async fn log_stream(
    State(s): State<AppState>,
    Path(name): Path<String>,
    Query(q): Query<StreamQuery>,
) -> Response {
    if !s.sup.stack.services.contains_key(&name) {
        return err(StatusCode::NOT_FOUND, format!("no service `{name}`"));
    }
    let (tx, rx) = tokio::sync::mpsc::channel(512);
    let sup = s.sup.clone();
    let tail = q.tail.unwrap_or(200).min(5000);
    std::thread::spawn(move || {
        let mut current: Option<std::path::PathBuf> = None;
        let (mut pos, mut partial) = (0u64, String::new());
        let mut first = true;
        loop {
            let path = sup.service_log(&name);
            if path != current {
                if !first {
                    let build = sup
                        .view()
                        .services
                        .into_iter()
                        .find(|v| v.name == name)
                        .and_then(|v| v.build_id);
                    let ev = serde_json::json!({"type": "instance", "build_id": build});
                    if tx.blocking_send(ev).is_err() {
                        return;
                    }
                }
                current = path;
                partial.clear();
                pos = 0;
                if first && let Some(p) = &current {
                    let (lines, len) = last_lines(p, tail);
                    pos = len;
                    for l in lines {
                        if tx.blocking_send(line(l)).is_err() {
                            return;
                        }
                    }
                }
                first = false;
            }
            if let Some(p) = &current {
                for l in read_new(p, &mut pos, &mut partial) {
                    if tx.blocking_send(line(l)).is_err() {
                        return;
                    }
                }
            }
            if tx.is_closed() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
    });
    sse_json(rx)
}

/// A command run's output as it is written, then its end.
async fn run_log_stream(State(s): State<AppState>, Path(id): Path<u64>) -> Response {
    if s.runs.get(id).is_none() {
        return err(StatusCode::NOT_FOUND, format!("no run {id}"));
    }
    let (tx, rx) = tokio::sync::mpsc::channel(512);
    let runs = s.runs.clone();
    std::thread::spawn(move || {
        let (mut pos, mut partial) = (0u64, String::new());
        loop {
            let Some(run) = runs.get(id) else { return };
            let done = run.status != "running";
            let mut lines = read_new(&run.log, &mut pos, &mut partial);
            if done && !partial.is_empty() {
                lines.push(std::mem::take(&mut partial));
            }
            for l in lines {
                if tx.blocking_send(line(l)).is_err() {
                    return;
                }
            }
            if done {
                let end =
                    serde_json::json!({"type": "end", "status": run.status, "error": run.error});
                let _ = tx.blocking_send(end);
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    });
    sse_json(rx)
}

/// The latest build log of a service, compiler paths pointing at the repository.
async fn build_log(State(s): State<AppState>, Path(name): Path<String>) -> Response {
    if !s.sup.stack.services.contains_key(&name) {
        return err(StatusCode::NOT_FOUND, format!("no service `{name}`"));
    }
    match s.sup.latest_build_log(&name) {
        Some(text) => {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                "text/plain; charset=utf-8".parse().unwrap(),
            );
            (headers, text).into_response()
        }
        None => err(
            StatusCode::NOT_FOUND,
            format!("`{name}` has no build log yet"),
        ),
    }
}

/// `GET /v1/infra/{group}/{service}/logs?tail=&grep=&since=`: a container's
/// output, timestamped (`docker compose logs`).
async fn infra_logs(
    State(s): State<AppState>,
    Path((group, service)): Path<(String, String)>,
    Query(q): Query<LogQuery>,
) -> Response {
    let known = s
        .sup
        .stack
        .compose
        .get(&group)
        .is_some_and(|g| g.services.contains(&service));
    if !known {
        return err(
            StatusCode::NOT_FOUND,
            format!("no compose service `{group}.{service}`"),
        );
    }
    container_logs(&s, group, service, &q).await
}

async fn container_logs(s: &AppState, group: String, service: String, q: &LogQuery) -> Response {
    let tail = q.tail.unwrap_or(200).min(20_000);
    let grep = q.grep.clone().filter(|g| !g.is_empty());
    let since = q.since.clone().filter(|v| !v.is_empty());
    // With a filter, Docker hands over more and the filter keeps the last `tail`.
    let fetch = if grep.is_some() { 20_000 } else { tail };
    let sup = s.sup.clone();
    let res = tokio::task::spawn_blocking(move || {
        sup.container_logs(&group, &service, fetch, since.as_deref())
    })
    .await;
    match res {
        Ok(Ok(out)) => {
            let needle = grep.map(|g| g.to_lowercase());
            let mut lines: Vec<&str> = out
                .lines()
                .rev()
                .filter(|l| needle.as_ref().is_none_or(|g| l.to_lowercase().contains(g)))
                .take(tail)
                .collect();
            lines.reverse();
            text(lines.join("\n"))
        }
        Ok(Err(e)) => err(StatusCode::BAD_GATEWAY, e),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn restart_container(
    State(s): State<AppState>,
    Path((group, service)): Path<(String, String)>,
) -> Response {
    let known = s
        .sup
        .stack
        .compose
        .get(&group)
        .is_some_and(|g| g.services.contains(&service));
    if !known {
        return err(
            StatusCode::NOT_FOUND,
            format!("no compose service `{group}.{service}`"),
        );
    }
    let sup = s.sup.clone();
    match tokio::task::spawn_blocking(move || sup.restart_container(&group, &service)).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}
