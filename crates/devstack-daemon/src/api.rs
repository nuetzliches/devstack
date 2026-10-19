//! The control API (ADR-0007), secured per ADR-0019: loopback only, every
//! request authenticated, `Host` checked against DNS rebinding, `Origin`
//! checked on mutating requests, no CORS.

use crate::commands::{self, RunError, Runs};
use crate::logfiles;
use crate::supervisor::{ContainerTarget, Supervisor};
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
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;

/// The most lines one log request returns (a service's, a container's, a
/// command run's).
const MAX_TAIL: usize = 20_000;

#[derive(Clone)]
pub struct AppState {
    pub sup: Arc<Supervisor>,
    pub runs: Arc<Runs>,
    pub usage: Arc<crate::usage::Sampler>,
    pub token: Arc<String>,
    pub port: u16,
    pub shutdown: Arc<tokio::sync::Notify>,
    /// The other working trees (plan §3.5); `None` inside a lane's own API.
    pub lanes: Option<Arc<crate::lanes::Lanes>>,
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
        .route("/v1/services/{name}/env", get(service_env))
        .route("/v1/services/{name}/logs/stream", get(log_stream))
        .route("/v1/services/{name}/logs/files", get(log_files))
        .route("/v1/services/{name}/builds", get(builds))
        .route("/v1/services/{name}/build-log", get(build_log))
        .route(
            "/v1/infra/{group}/{service}/{action}",
            post(container_action),
        )
        .route("/v1/infra/{group}/{service}/logs", get(infra_logs))
        .route(
            "/v1/infra/{group}/{service}/logs/stream",
            get(infra_log_stream),
        )
        .route("/v1/infra/check", post(infra_check))
        .route("/v1/events", get(events))
        .route("/v1/usage", get(usage))
        .route("/v1/commands/{name}", post(run_command))
        .route("/v1/commands/{name}/runs", get(command_runs))
        .route("/v1/runs/{id}", get(run_info))
        .route("/v1/runs/{id}/log", get(run_log))
        .route("/v1/runs/{id}/log/stream", get(run_log_stream))
        .route("/v1/runs/{id}/cancel", post(run_cancel))
        .route("/v1/tunnel/{port}", get(tunnel))
        .route("/v1/lanes", get(lanes_list).post(lanes_start))
        .route("/v1/lanes/{lane}/stop", post(lanes_stop))
        .route("/v1/lanes/{lane}/move", post(lanes_move))
        .route("/v1/lanes/{lane}/share", post(lanes_share))
        .route("/v1/main-slot", post(main_slot))
        .route("/v1/lanes/{lane}/main", post(lanes_take_main))
        .route(
            "/v1/lanes/{lane}/v1/{*rest}",
            axum::routing::any(lane_forward),
        )
        .layer(middleware::from_fn_with_state(state.clone(), guard))
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

/// `204` once every service (and with `infra` every Compose group) stopped;
/// `409` naming what did not. In the main tree, lanes that still run keep the
/// infrastructure their `shared` services use (ADR-0030), and the daemon they
/// live in (ADR-0024): then `200` with `{"lanes": [...]}`, and `infra` is not
/// done.
async fn down(State(s): State<AppState>, Query(q): Query<DownQuery>) -> Response {
    let lanes = s.lanes.as_ref().map(|l| l.active()).unwrap_or_default();
    let infra = q.infra && lanes.is_empty();
    let sup = s.sup.clone();
    match tokio::task::spawn_blocking(move || sup.down(infra)).await {
        Ok(failed) if !failed.is_empty() => err(
            StatusCode::CONFLICT,
            format!(
                "did not stop:
{}",
                failed.join(
                    "
"
                )
            ),
        ),
        Ok(_) if !lanes.is_empty() => Json(serde_json::json!({ "lanes": lanes })).into_response(),
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[derive(Deserialize)]
struct ShutdownQuery {
    /// Not while a lane runs: `409` with `{"lanes": [...]}` instead.
    #[serde(default)]
    unless_lanes: bool,
}

async fn shutdown(State(s): State<AppState>, Query(q): Query<ShutdownQuery>) -> Response {
    if q.unless_lanes {
        let lanes = s.lanes.as_ref().map(|l| l.active()).unwrap_or_default();
        if !lanes.is_empty() {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": format!("lanes run in this daemon: {}", lanes.join(", ")),
                    "lanes": lanes,
                })),
            )
                .into_response();
        }
    }
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
        "rebuild" => tokio::task::spawn_blocking(move || sup.rebuild(&name)).await,
        "promote" => tokio::task::spawn_blocking(move || sup.promote(&name)).await,
        "rollback" => tokio::task::spawn_blocking(move || sup.rollback(&name)).await,
        "hot-reload-on" => tokio::task::spawn_blocking(move || sup.hot_reload(&name, true)).await,
        "hot-reload-off" => tokio::task::spawn_blocking(move || sup.hot_reload(&name, false)).await,
        _ => return err(StatusCode::NOT_FOUND, format!("unknown action `{action}`")),
    };
    match result {
        Ok(Ok(())) => Json(s.sup.view()).into_response(),
        Ok(Err(e)) => err(StatusCode::CONFLICT, e),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[allow(clippy::result_large_err)] // the error is the answer itself
fn lanes_of(s: &AppState) -> Result<&Arc<crate::lanes::Lanes>, Response> {
    s.lanes.as_ref().ok_or_else(|| {
        err(
            StatusCode::NOT_FOUND,
            "lanes are managed by the main working tree's daemon",
        )
    })
}

/// `GET /v1/lanes`: the other working trees this daemon runs (plan §3.5).
async fn lanes_list(State(s): State<AppState>) -> Response {
    let lanes = match lanes_of(&s) {
        Ok(l) => l.clone(),
        Err(r) => return r,
    };
    // `git worktree list` runs: off the async workers.
    match tokio::task::spawn_blocking(move || lanes.list()).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[derive(Deserialize)]
struct LaneStart {
    /// A worktree of the repository (git makes them; devstack never does).
    path: std::path::PathBuf,
    /// Make sure what the lane uses from the main tree runs (`shared`
    /// Compose services, ADR-0030); its own infrastructure starts with its
    /// `up`.
    #[serde(default)]
    infra: bool,
    /// Its main instances run their background workers (ADR-0030).
    #[serde(default)]
    workers: bool,
    /// Start it although the memory check says it does not fit.
    #[serde(default)]
    force: bool,
    /// Run it on this node of the pool instead of here (ADR-0025): its name
    /// or member id.
    #[serde(default)]
    on: Option<String>,
}

/// `POST /v1/lanes` with `{"path", "infra"?}`: start the lane of that
/// worktree (`devstack up` in it does this; with `infra` the shared
/// infrastructure is started if it is not up); `/v1/lanes/<name>/v1/up`
/// then starts its services. `201` with its entry when it is started; with
/// `on`, `202` with its entry while its start on the node goes on (its
/// `moving`; gone once it runs there, or carrying the error), `201` if it
/// runs there already.
async fn lanes_start(State(s): State<AppState>, Json(body): Json<LaneStart>) -> Response {
    let lanes = match lanes_of(&s) {
        Ok(l) => l.clone(),
        Err(r) => return r,
    };
    let main = s.sup.clone();
    let usage = s.usage.clone();
    let res = tokio::task::spawn_blocking(move || {
        let view = |name: &str| {
            lanes
                .list()
                .into_iter()
                .find(|v| v.name == name)
                .ok_or_else(|| format!("lane `{name}` is gone"))
        };
        if let Some(on) = body.on.as_deref().filter(|o| !o.trim().is_empty()) {
            // What a local lane would need here is what it needs there.
            let need = devstack_node::share::Amount {
                cpus: 0.5,
                memory: lane_need(&main, &usage).max(1 << 30),
            };
            return match lanes.activate_remote(&body.path, on.trim(), need)? {
                crate::lanes::RemoteStart::Running(name) => {
                    view(&name).map(|v| (StatusCode::CREATED, v))
                }
                crate::lanes::RemoteStart::Starting(name) => {
                    view(&name).map(|v| (StatusCode::ACCEPTED, v))
                }
            };
        }
        if !body.force {
            memory_fits(&main, &usage)?;
        }
        let lane = lanes.activate(&body.path)?;
        lane.sup.set_lane_workers(body.workers);
        if body.infra && main.stack.compose.values().any(|g| !g.shared.is_empty()) {
            main.infra_up()?;
        }
        view(&lane.entry.name).map(|v| (StatusCode::CREATED, v))
    })
    .await;
    match res {
        Ok(Ok((status, view))) => (status, Json(view)).into_response(),
        Ok(Err(e)) => err(StatusCode::CONFLICT, e),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// What a lane takes: about what the main tree takes now, containers
/// included (ADR-0030).
fn lane_need(main: &crate::supervisor::Supervisor, usage: &crate::usage::Sampler) -> u64 {
    let r = usage.report(main, false);
    r.services.iter().map(|e| e.load.memory).sum::<u64>()
        + r.containers.iter().map(|e| e.load.memory).sum::<u64>()
}

/// A lane is a second copy of the stack, containers included (ADR-0030): it
/// needs about what the main tree takes now. Refuse when the machine does
/// not have that, with a margin, instead of pushing it into swap.
fn memory_fits(
    main: &crate::supervisor::Supervisor,
    usage: &crate::usage::Sampler,
) -> Result<(), String> {
    const MARGIN: u64 = 1 << 30;
    let need = lane_need(main, usage);
    let Some(free) = devstack_proc::memory_available() else {
        return Ok(()); // not measurable here: no reason to refuse
    };
    if need == 0 || free >= need + MARGIN {
        return Ok(());
    }
    let gb = |b: u64| b as f64 / (1u64 << 30) as f64;
    Err(format!(
        "a lane needs about {:.1} GB (what the main tree takes now, containers included), {:.1} GB are free: \
         stop another lane or close something, start it on another machine of the pool \
         (`devstack up --on <node>`, see `devstack pool`), or start it anyway with `devstack up --force`",
        gb(need),
        gb(free)
    ))
}

/// `GET /v1/tunnel/{port}` with `Upgrade: devstack-tcp`: the connection
/// becomes a TCP connection to a service's port on this machine's loopback
/// (ADR-0029 point 4). A lane on a node is reached this way: services there
/// listen on its container's loopback, and the requester forwards its
/// local ports through the node agent to here. Only the ports of the stack's
/// services, behind the API's token like every route.
async fn tunnel(State(s): State<AppState>, Path(port): Path<u16>, mut req: Request) -> Response {
    // A service's port on this machine's loopback, or (a lane on a node) a
    // container's, at its address on the lane's network.
    let container = s.sup.infra_tunnel(port);
    if container.is_none()
        && !s
            .sup
            .stack
            .services
            .values()
            .any(|svc| svc.port == Some(port))
    {
        return err(
            StatusCode::FORBIDDEN,
            // The release names what the ports mean (a lane on a node
            // serves its stack's ports since 0.10.0): a requester or a member
            // of another release sees the difference.
            format!(
                "port {port} is no service's or container's port in this stack (devstack {} here)",
                env!("CARGO_PKG_VERSION")
            ),
        );
    }
    let wants = req
        .headers()
        .get(header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case(TUNNEL_PROTOCOL));
    if !wants {
        return err(
            StatusCode::BAD_REQUEST,
            format!("a tunnel needs `Upgrade: {TUNNEL_PROTOCOL}`"),
        );
    }
    let connected = match &container {
        Some(addr) => tokio::net::TcpStream::connect(addr.as_str()).await,
        // Dev servers bound to `localhost` may hold only `::1`.
        None => match tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
            Ok(t) => Ok(t),
            Err(_) => tokio::net::TcpStream::connect(("::1", port)).await,
        },
    };
    let tcp = match connected {
        Ok(t) => t,
        Err(e) => {
            let at = container.unwrap_or_else(|| format!("port {port}"));
            return err(
                StatusCode::BAD_GATEWAY,
                format!("nothing answers at {at}: {e}"),
            );
        }
    };
    let _ = tcp.set_nodelay(true);
    let upgrade = hyper::upgrade::on(&mut req);
    tokio::spawn(async move {
        let Ok(up) = upgrade.await else { return };
        let mut io = hyper_util::rt::TokioIo::new(up);
        let mut tcp = tcp;
        let _ = tokio::io::copy_bidirectional(&mut io, &mut tcp).await;
    });
    Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header(header::CONNECTION, "upgrade")
        .header(header::UPGRADE, TUNNEL_PROTOCOL)
        .body(axum::body::Body::empty())
        .expect("a valid response")
}

/// The `Upgrade` protocol of a TCP tunnel.
pub const TUNNEL_PROTOCOL: &str = "devstack-tcp";

/// `POST /v1/lanes/{lane}/stop`: its services stop and it is no longer
/// started (`devstack lane stop`); the worktree stays as it is.
async fn lanes_stop(State(s): State<AppState>, Path(lane): Path<String>) -> Response {
    let lanes = match lanes_of(&s) {
        Ok(l) => l.clone(),
        Err(r) => return r,
    };
    match tokio::task::spawn_blocking(move || lanes.stop(&lane).map(|()| lanes.list())).await {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => err(StatusCode::CONFLICT, e),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[derive(Deserialize)]
struct LaneMove {
    /// A node of the pool (its name or member id), or `local`.
    to: String,
}

/// `POST /v1/lanes/{lane}/move` with `{"to"}`: move a started lane to
/// another node of the pool or to this machine (ADR-0025 point 10), a cold
/// start at the target that takes over once it is ready. Answers when it is
/// done: the lanes after it.
#[derive(Deserialize)]
struct MainSlot {
    owns: bool,
}

/// `POST /v1/main-slot {"owns"}`: a lane on a node holds its requester's
/// main slot, or no longer (ADR-0025 point 4). Its worker services restart
/// with the switch. Only a daemon in a lane container takes it.
async fn main_slot(State(s): State<AppState>, Json(b): Json<MainSlot>) -> Response {
    if !crate::remote_lane() {
        return err(
            StatusCode::CONFLICT,
            "only a lane on a node takes the main slot this way",
        );
    }
    let was = crate::REMOTE_MAIN.swap(b.owns, std::sync::atomic::Ordering::SeqCst);
    if was != b.owns {
        let sup = s.sup.clone();
        let _ = tokio::task::spawn_blocking(move || {
            for svc in sup.running_worker_services() {
                let _ = sup.relaunch(&svc);
            }
        })
        .await;
        crate::diag::info("main_slot", serde_json::json!({ "owns": b.owns }));
    }
    Json(serde_json::json!({ "owns": b.owns })).into_response()
}

#[derive(Deserialize)]
struct LaneShare {
    /// Member ids or tags of the pool, `*` for every member; empty: nobody.
    #[serde(default)]
    with: Vec<String>,
}

/// `POST /v1/lanes/{lane}/share {"with"}`: share a lane on a node with
/// others in its pool, or stop (ADR-0035). Answers with the lanes.
async fn lanes_share(
    State(s): State<AppState>,
    Path(lane): Path<String>,
    Json(body): Json<LaneShare>,
) -> Response {
    let lanes = match lanes_of(&s) {
        Ok(l) => l.clone(),
        Err(r) => return r,
    };
    let res = tokio::task::spawn_blocking(move || {
        lanes.share_lane(&lane, body.with).map(|()| lanes.list())
    })
    .await;
    match res {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => err(StatusCode::CONFLICT, e),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn lanes_move(
    State(s): State<AppState>,
    Path(lane): Path<String>,
    Json(body): Json<LaneMove>,
) -> Response {
    let lanes = match lanes_of(&s) {
        Ok(l) => l.clone(),
        Err(r) => return r,
    };
    let main = s.sup.clone();
    let usage = s.usage.clone();
    let res = tokio::task::spawn_blocking(move || {
        let need = devstack_node::share::Amount {
            cpus: 0.5,
            memory: lane_need(&main, &usage).max(1 << 30),
        };
        lanes
            .move_lane(&lane, body.to.trim(), need)
            .map(|()| lanes.list())
    })
    .await;
    match res {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => err(StatusCode::CONFLICT, e),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// `POST /v1/lanes/{lane}/main`: that lane's instances serve the main slot
/// (`devstack promote --lane`); `main` gives it back to the main working tree.
async fn lanes_take_main(State(s): State<AppState>, Path(lane): Path<String>) -> Response {
    let lanes = match lanes_of(&s) {
        Ok(l) => l.clone(),
        Err(r) => return r,
    };
    match tokio::task::spawn_blocking(move || lanes.take_main(&lane).map(|()| lanes.list())).await {
        Ok(Ok(list)) => Json(list).into_response(),
        Ok(Err(e)) => err(StatusCode::CONFLICT, e),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// `/v1/lanes/{lane}/v1/...`: the lane's own API, the same as the main one.
async fn lane_forward(
    State(s): State<AppState>,
    Path((lane, rest)): Path<(String, String)>,
    req: Request,
) -> Response {
    use tower::ServiceExt;
    let lanes = match lanes_of(&s) {
        Ok(l) => l,
        Err(r) => return r,
    };
    if let Some(r) = lanes.remote(&lane) {
        // On a node: its daemon's API, passed through as it is.
        let query = req
            .uri()
            .query()
            .map(|q| format!("?{q}"))
            .unwrap_or_default();
        let (parts, body) = req.into_parts();
        let mut out = axum::http::Request::new(body);
        *out.method_mut() = parts.method;
        let Ok(uri) = format!("/v1/{rest}{query}").parse::<axum::http::Uri>() else {
            return err(StatusCode::BAD_REQUEST, "bad path");
        };
        *out.uri_mut() = uri;
        for (k, v) in &parts.headers {
            if k != header::HOST && k != header::AUTHORIZATION {
                out.headers_mut().insert(k, v.clone());
            }
        }
        return r.forward(out).await;
    }
    let l = {
        let lanes = lanes.clone();
        match tokio::task::spawn_blocking(move || lanes.get(&lane)).await {
            Ok(Ok(l)) => l,
            Ok(Err(e)) => return err(StatusCode::NOT_FOUND, e),
            Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        }
    };
    let query = req
        .uri()
        .query()
        .map(|q| format!("?{q}"))
        .unwrap_or_default();
    let Ok(uri) = format!("/v1/{rest}{query}").parse::<axum::http::Uri>() else {
        return err(StatusCode::BAD_REQUEST, "bad path");
    };
    // A new request, not this one with another URI: its extensions hold this
    // route's path parameters (`lane`, `rest`), and the lane's handlers would
    // read them as theirs ("Wrong number of path arguments").
    let (mut parts, body) = req.into_parts();
    let mut inner = Request::new(body);
    *inner.method_mut() = parts.method;
    *inner.uri_mut() = uri;
    *inner.version_mut() = parts.version;
    *inner.headers_mut() = parts.headers;
    // Of the extensions only the connection's upgrade: a WebSocket or a
    // tunnel through a lane's API needs it.
    if let Some(up) = parts.extensions.remove::<hyper::upgrade::OnUpgrade>() {
        inner.extensions_mut().insert(up);
    }
    match l.router.clone().oneshot(inner).await {
        Ok(r) => r,
        Err(e) => match e {},
    }
}

#[derive(Deserialize)]
struct EnvQuery {
    /// Only what Compose and the stack file give (`devstack env --export`).
    #[serde(default)]
    export: bool,
}

/// `GET /v1/services/{name}/env[?export=true]`: the environment the service
/// gets from this daemon (in a lane: the lane's), `[{name, value, origin}]`.
async fn service_env(
    State(s): State<AppState>,
    Path(name): Path<String>,
    Query(q): Query<EnvQuery>,
) -> Response {
    let sup = s.sup.clone();
    match tokio::task::spawn_blocking(move || sup.env_of(&name, !q.export)).await {
        Ok(Ok(env)) => Json(
            env.into_iter()
                .map(|(k, v)| serde_json::json!({ "name": k, "value": v.value, "origin": v.origin.to_string() }))
                .collect::<Vec<_>>(),
        )
        .into_response(),
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
    // Files are read: off the async workers.
    tokio::task::spawn_blocking(move || service_logs(&s, &name, &q))
        .await
        .unwrap_or_else(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

fn service_logs(s: &AppState, name: &str, q: &LogQuery) -> Response {
    let tail = q.tail.unwrap_or(200).min(MAX_TAIL);
    let grep = q.grep.as_deref().filter(|g| !g.is_empty());
    let dir = s.sup.log_dir(name);
    let current = s.sup.service_log(name);
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

#[derive(Deserialize)]
struct LimitQuery {
    limit: Option<usize>,
}

/// `GET /v1/services/{name}/builds?limit=50`: the builds of a service, newest
/// first, from the diagnostic log (plan §6).
async fn builds(
    State(s): State<AppState>,
    Path(name): Path<String>,
    Query(q): Query<LimitQuery>,
) -> Response {
    if !s.sup.stack.services.contains_key(&name) {
        return err(StatusCode::NOT_FOUND, format!("no service `{name}`"));
    }
    let current = s
        .sup
        .view()
        .services
        .into_iter()
        .find(|v| v.name == name)
        .and_then(|v| v.build_id);
    // One diagnostic log per daemon, the main tree's; a lane's lines carry its name.
    let path = if s.sup.lane.is_empty() {
        s.sup.state_dir.join("daemon.log")
    } else {
        s.sup
            .state_dir
            .parent()
            .unwrap_or(&s.sup.state_dir)
            .join("daemon.log")
    };
    let lane = s.sup.lane.clone();
    let limit = q.limit.unwrap_or(50).min(500);
    let list = tokio::task::spawn_blocking(move || {
        crate::timeline::from_log(&path, &name, &lane, current.as_deref(), limit)
    })
    .await
    .unwrap_or_default();
    Json(list).into_response()
}

/// `GET /v1/services/{name}/logs/files`: the kept logs, newest first.
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
    // No part contributes more than `tail` lines; one more says that there
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
    let tail = q.tail.unwrap_or(50).min(MAX_TAIL);
    let grep = grep.to_string();
    tokio::task::spawn_blocking(move || {
        let grep = grep.as_str();
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
    })
    .await
    .unwrap_or_else(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// The last `n` lines of a file, optionally only those containing `grep`
/// (what a terminal shows, not the colour codes around it); read from the
/// end. Blocking.
fn matching(path: &std::path::Path, n: usize, grep: Option<&str>) -> Vec<String> {
    logfiles::last_lines(path, n, grep)
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
    let sup = s.sup.clone();
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
    logfiles::last_lines_at(path, n, None)
}

#[derive(Deserialize)]
struct StreamQuery {
    tail: Option<usize>,
    /// Follow the newest instance of this build (a candidate) instead of the
    /// current one; a prefix of the id is enough.
    build: Option<String>,
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
    let build = q.build.filter(|b| !b.is_empty());
    std::thread::spawn(move || {
        let mut current: Option<std::path::PathBuf> = None;
        let (mut pos, mut partial) = (0u64, String::new());
        let mut first = true;
        loop {
            let path = match &build {
                None => sup.service_log(&name),
                Some(b) => {
                    let dir = sup.log_dir(&name);
                    logfiles::list(&dir, None)
                        .into_iter()
                        .find(|f| {
                            f.kind == "run"
                                && f.build
                                    .as_deref()
                                    .is_some_and(|id| id.starts_with(b.as_str()))
                        })
                        .map(|f| dir.join(f.file))
                }
            };
            if path != current {
                // A build's newest instance starting is no "another instance took over".
                if !first && build.is_none() {
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
    let (sup, n) = (s.sup.clone(), name.clone());
    let log = tokio::task::spawn_blocking(move || sup.latest_build_log(&n)).await;
    let Ok(log) = log else {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "reading the build log failed",
        );
    };
    match log {
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

/// `POST /v1/infra/check`: probe every running container's published ports
/// from this machine; `[{group, service, ports}]` whose forward is lost
/// (accepted and closed at once). Recorded in the view as `unreachable`.
async fn infra_check(State(s): State<AppState>) -> Response {
    let sup = s.sup.clone();
    match tokio::task::spawn_blocking(move || sup.check_forwards()).await {
        Ok(lost) => Json(
            lost.into_iter()
                .map(|(group, service, ports)| serde_json::json!({ "group": group, "service": service, "ports": ports }))
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// `GET /v1/infra/{group}/{service}/logs?tail=&grep=&since=`: a container's
/// output, timestamped (`docker compose logs`).
async fn infra_logs(
    State(s): State<AppState>,
    Path((group, service)): Path<(String, String)>,
    Query(q): Query<LogQuery>,
) -> Response {
    if let Err(e) = infra_target(&s, &group, &service) {
        return err(StatusCode::NOT_FOUND, e);
    }
    container_logs(&s, group, service, &q).await
}

/// `GET /v1/infra/{group}/{service}/logs/stream?tail=`: a container's output
/// as a stream, the same events as a service's (`{"type":"line","text"}`),
/// so the dashboard shows both with one log view. `docker compose logs
/// --follow` runs while someone reads.
async fn infra_log_stream(
    State(s): State<AppState>,
    Path((group, service)): Path<(String, String)>,
    Query(q): Query<StreamQuery>,
) -> Response {
    if let Err(e) = infra_target(&s, &group, &service) {
        return err(StatusCode::NOT_FOUND, e);
    }
    let tail = q.tail.unwrap_or(500).min(5000);
    let mut child = match s.sup.follow_container_logs(&group, &service, tail) {
        Ok(c) => c,
        Err(e) => return err(StatusCode::BAD_GATEWAY, e),
    };
    let (tx, rx) = tokio::sync::mpsc::channel(512);
    for pipe in [
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
    ]
    .into_iter()
    .flatten()
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            use std::io::BufRead;
            for l in std::io::BufReader::new(pipe).lines() {
                let Ok(l) = l else { return };
                if tx.blocking_send(line(l)).is_err() {
                    return;
                }
            }
        });
    }
    // The reader is gone, or the container's log ended: stop following.
    std::thread::spawn(move || {
        loop {
            if tx.is_closed() || child.try_wait().ok().flatten().is_some() {
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    });
    sse_json(rx)
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
                .filter(|l| {
                    needle
                        .as_ref()
                        .is_none_or(|g| crate::ansi::strip(l).to_lowercase().contains(g))
                })
                .take(tail)
                .collect();
            lines.reverse();
            text(lines.join("\n"))
        }
        Ok(Err(e)) => err(StatusCode::BAD_GATEWAY, e),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// What a `/v1/infra/{group}/{service}/…` route names: a container of the
/// view, keyed by its Compose service name. `404` with the reason otherwise.
fn infra_target(s: &AppState, group: &str, service: &str) -> Result<ContainerTarget, String> {
    match s.sup.container_target(group, service) {
        None => Err(format!("no compose group `{group}`")),
        Some(ContainerTarget::Unknown) => Err(format!(
            "no compose service `{group}.{service}`: compose group `{group}` does not start it and its project has no container of that service (a container is named by its Compose service, not its container name)"
        )),
        Some(t) => Ok(t),
    }
}

/// `POST /v1/infra/{group}/{service}/{start|stop|restart}`: one container.
/// One the group does not start (`managed: false` in the view) can be
/// stopped only: `409` for `start` and `restart`.
async fn container_action(
    State(s): State<AppState>,
    Path((group, service, action)): Path<(String, String, String)>,
) -> Response {
    if !matches!(action.as_str(), "start" | "stop" | "restart") {
        return err(
            StatusCode::BAD_REQUEST,
            format!("unknown action `{action}`: start, stop or restart"),
        );
    }
    let target = match infra_target(&s, &group, &service) {
        Ok(t) => t,
        Err(e) => return err(StatusCode::NOT_FOUND, e),
    };
    let sup = s.sup.clone();
    let res = match target {
        ContainerTarget::Unmanaged { name } if action == "stop" => {
            tokio::task::spawn_blocking(move || sup.stop_unmanaged(&group, &service, &name)).await
        }
        ContainerTarget::Unmanaged { name } => {
            return err(
                StatusCode::CONFLICT,
                format!(
                    "`{service}` (container `{name}`) belongs to the Compose project, but compose group `{group}` does not start it (it is not in its `services`): devstack can stop it, not {action} it. Add it to `services` to manage it"
                ),
            );
        }
        _ => {
            tokio::task::spawn_blocking(move || sup.container_action(&group, &service, &action))
                .await
        }
    };
    match res {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(e)) => err(StatusCode::CONFLICT, e),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}
