import sys; sys.path.insert(0,'D:/Sources/devstack/.claude/scratch-review-daemon')
from rep import apply
W='D:/Sources/devstack/.claude/worktrees/review-daemon/crates/devstack-daemon/src/'
apply(W+'api.rs',[
("""use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;""",
"""use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;

/// The most lines one log request returns (a service's, a container's, a
/// command run's).
const MAX_TAIL: usize = 20_000;"""),
("""        .route("/v1/runs/{id}/log/stream", get(run_log_stream))""",
"""        .route("/v1/runs/{id}/log/stream", get(run_log_stream))
        .route("/v1/runs/{id}/cancel", post(run_cancel))"""),
("""        .layer(middleware::from_fn_with_state(state.clone(), guard))
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
}""",
"""        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .layer(middleware::from_fn_with_state(state.clone(), audit))
        .with_state(state)
}

/// Every request that changes something goes to the diagnostic log with its
/// answer, refused ones included (outermost layer): "who restarted that, and
/// did it work" is answerable afterwards. Once: a lane's request is logged
/// by the main API it came through (with the lane), not again by the lane's.
async fn audit(State(s): State<AppState>, req: Request, next: Next) -> Response {
    let method = req.method().clone();
    if method == axum::http::Method::GET
        || method == axum::http::Method::HEAD
        || !s.sup.lane.is_empty()
    {
        return next.run(req).await;
    }
    let path = req.uri().path().to_string();
    let res = next.run(req).await;
    let status = res.status().as_u16();
    let mut fields =
        serde_json::json!({ "method": method.as_str(), "path": path, "status": status });
    if let Some(lane) = path
        .strip_prefix("/v1/lanes/")
        .and_then(|r| r.split_once('/'))
        .map(|(l, _)| l)
    {
        fields["lane"] = lane.into();
    }
    crate::diag::log(
        if status >= 400 { "warn" } else { "info" },
        "request",
        fields,
    );
    res
}"""),
# lane_forward: keep the upgrade
("""    let (parts, body) = req.into_parts();
    let mut inner = Request::new(body);
    *inner.method_mut() = parts.method;
    *inner.uri_mut() = uri;
    *inner.version_mut() = parts.version;
    *inner.headers_mut() = parts.headers;""",
"""    let (mut parts, body) = req.into_parts();
    let mut inner = Request::new(body);
    *inner.method_mut() = parts.method;
    *inner.uri_mut() = uri;
    *inner.version_mut() = parts.version;
    *inner.headers_mut() = parts.headers;
    // Of the extensions only the connection's upgrade: a WebSocket or a
    // tunnel through a lane's API needs it.
    if let Some(up) = parts.extensions.remove::<hyper::upgrade::OnUpgrade>() {
        inner.extensions_mut().insert(up);
    }"""),
# logs: off the async workers
("""async fn logs(
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
    let tail = q.tail.unwrap_or(200);""",
"""async fn logs(
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
    // Files are read: off the async workers.
    tokio::task::spawn_blocking(move || service_logs(&s, &name, &q))
        .await
        .unwrap_or_else(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

fn service_logs(s: &AppState, name: &str, q: &LogQuery) -> Response {
    let tail = q.tail.unwrap_or(200).min(MAX_TAIL);"""),
("""    let dir = s.sup.log_dir(&name);
    let current = s.sup.service_log(&name);
    if let Some(file) = q.file.as_deref() {""",
"""    let dir = s.sup.log_dir(name);
    let current = s.sup.service_log(name);
    if let Some(file) = q.file.as_deref() {"""),
("""/// `GET /v1/services/{name}/logs/files`: the kept logs, newest first.
async fn log_files(State(s): State<AppState>, Path(name): Path<String>) -> Response {
    if !s.sup.stack.services.contains_key(&name) {
        return err(StatusCode::NOT_FOUND, format!("no service `{name}`"));
    }
    let current = s.sup.service_log(&name);
    Json(logfiles::list(&s.sup.log_dir(&name), current.as_deref())).into_response()
}""",
"""/// `GET /v1/services/{name}/logs/files`: the kept logs, newest first.
async fn log_files(State(s): State<AppState>, Path(name): Path<String>) -> Response {
    if !s.sup.stack.services.contains_key(&name) {
        return err(StatusCode::NOT_FOUND, format!("no service `{name}`"));
    }
    let current = s.sup.service_log(&name);
    let dir = s.sup.log_dir(&name);
    match tokio::task::spawn_blocking(move || logfiles::list(&dir, current.as_deref())).await {
        Ok(list) => Json(list).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}"""),
("""    let mut parts: Vec<(String, Vec<String>)> = sel
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
        }""",
"""    // No part contributes more than `tail` lines; one more says that there
    // are more than were read.
    let mut parts: Vec<(String, Vec<String>, bool)> = sel
        .iter()
        .map(|f| {
            let mut lines = matching(&dir.join(&f.file), tail.saturating_add(1), grep);
            let more = lines.len() > tail;
            if more {
                lines.remove(0);
            }
            (logfiles::header(f), lines, more)
        })
        .filter(|(_, lines, _)| grep.is_none() || !lines.is_empty())
        .collect();
    // Keep the last `tail` lines across all parts; every part keeps its
    // header, so what was cut is visible.
    let mut left = tail;
    let mut cut = vec![0usize; parts.len()];
    for (i, (_, lines, _)) in parts.iter_mut().enumerate().rev() {
        let keep = lines.len().min(left);
        cut[i] = lines.len() - keep;
        lines.drain(..cut[i]);
        left -= keep;
    }
    let pre = prefix.map(|p| format!("{p} | ")).unwrap_or_default();
    let mut out = Vec::new();
    for ((h, lines, more), cut) in parts.into_iter().zip(cut) {
        out.push(format!("{pre}{h}"));
        if cut > 0 || more {
            let plus = if more { "+" } else { "" };
            out.push(format!(
                "{pre}… {}{plus} earlier line(s) not shown (raise `tail`)",
                cut + usize::from(more)
            ));
        }"""),
("""    let tail = q.tail.unwrap_or(50);
    let mut out = Vec::new();
    for svc in s.sup.view().services {""",
"""    let tail = q.tail.unwrap_or(50).min(MAX_TAIL);
    let grep = grep.to_string();
    tokio::task::spawn_blocking(move || {
        let grep = grep.as_str();
        let mut out = Vec::new();
        for svc in s.sup.view().services {"""),
("""            let text = combined(&dir, &sel, tail, Some(grep), Some(&svc.name));
            if !text.is_empty() {
                out.push(text);
            }
        } else if let Some(p) = s.sup.service_log(&svc.name) {
            for line in matching(&p, tail, Some(grep)) {
                out.push(format!("{} | {line}", svc.name));
            }
        }
    }
    text(out.join("\\n"))
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
        // `grep` matches what a terminal shows, not the colour codes around it.
        .filter(|l| {
            needle
                .as_ref()
                .is_none_or(|g| crate::ansi::strip(l).to_lowercase().contains(g))
        })
        .take(n)
        .map(str::to_string)
        .collect();
    lines.reverse();
    lines
}""",
"""            let text = combined(&dir, &sel, tail, Some(grep), Some(&svc.name));
            if !text.is_empty() {
                out.push(text);
            }
        } else if let Some(p) = s.sup.service_log(&svc.name) {
            for line in matching(&p, tail, Some(grep)) {
                out.push(format!("{} | {line}", svc.name));
            }
        }
        }
        text(out.join("\\n"))
    })
    .await
    .unwrap_or_else(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// The last `n` lines of a file, optionally only those containing `grep`
/// (what a terminal shows, not the colour codes around it); read from the
/// end. Blocking.
fn matching(path: &std::path::Path, n: usize, grep: Option<&str>) -> Vec<String> {
    logfiles::last_lines(path, n, grep)
}"""),
# events: lagged -> snapshot
("""    let stream = tokio_stream::once(snapshot)
        .chain(BroadcastStream::new(rx).filter_map(|e| e.ok()))
        .map(|e| {""",
"""    let sup = s.sup.clone();
    let stream = tokio_stream::once(snapshot)
        .chain(BroadcastStream::new(rx).map(move |e| match e {
            Ok(e) => e,
            // The client fell behind (it stalled, or a burst came): the
            // events it missed are gone, a fresh snapshot sets it right.
            Err(BroadcastStreamRecvError::Lagged(_)) => crate::state::Event {
                seq: 0,
                at: crate::state::now_ms(),
                body: crate::state::EventBody::Snapshot {
                    stack: Box::new(sup.view()),
                },
            },
        }))
        .map(|e| {"""),
("""async fn run_log(State(s): State<AppState>, Path(id): Path<u64>) -> Response {
    match s.runs.get(id) {
        Some(r) => text_tail(&r.log, usize::MAX),
        None => err(StatusCode::NOT_FOUND, format!("no run {id}")),
    }
}""",
"""async fn run_log(State(s): State<AppState>, Path(id): Path<u64>) -> Response {
    match s.runs.get(id) {
        Some(r) => tokio::task::spawn_blocking(move || text_tail(&r.log, MAX_TAIL))
            .await
            .unwrap_or_else(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
        None => err(StatusCode::NOT_FOUND, format!("no run {id}")),
    }
}

/// `POST /v1/runs/{id}/cancel`: stop a running command run (its `exec`
/// step killed, its `run` step's job stopped, no further step); `202`, or
/// `409` when it is not running.
async fn run_cancel(State(s): State<AppState>, Path(id): Path<u64>) -> Response {
    let (sup, runs) = (s.sup.clone(), s.runs.clone());
    match tokio::task::spawn_blocking(move || runs.cancel(&sup, id)).await {
        Ok(Ok(true)) => StatusCode::ACCEPTED.into_response(),
        Ok(Ok(false)) => err(StatusCode::CONFLICT, format!("run {id} is not running")),
        Ok(Err(e)) if e.starts_with("no run") => err(StatusCode::NOT_FOUND, e),
        Ok(Err(e)) => err(StatusCode::CONFLICT, e),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}"""),
("""fn last_lines(path: &std::path::Path, n: usize) -> (Vec<String>, u64) {
    let text = std::fs::read(path).unwrap_or_default();
    let len = text.len() as u64;
    let text = String::from_utf8_lossy(&text);
    let mut lines: Vec<String> = text.lines().rev().take(n).map(str::to_string).collect();
    lines.reverse();
    (lines, len)
}""",
"""fn last_lines(path: &std::path::Path, n: usize) -> (Vec<String>, u64) {
    logfiles::last_lines_at(path, n, None)
}"""),
("""    match s.sup.latest_build_log(&name) {
        Some(text) => {""",
"""    let sup = s.sup.clone();
    let log = tokio::task::spawn_blocking(move || sup.latest_build_log(&name)).await;
    let Ok(log) = log else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "reading the build log failed");
    };
    match log {
        Some(text) => {"""),
])
print("ok")
