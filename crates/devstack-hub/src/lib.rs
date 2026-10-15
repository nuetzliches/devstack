//! The hub (ADR-0021): one long-lived process on a fixed loopback port that
//! serves the dashboard as a progressive web app, knows every stack (the
//! registry the daemons write), forwards the dashboard's requests and streams
//! to the stacks' daemons, and starts and stops stacks.
//!
//! Security per ADR-0019: loopback only, `Host` checked, the browser pairs
//! with a one-time code (`POST /hub/pair` with the user's token, then
//! `/auth?code=`) and holds an `HttpOnly; SameSite=Strict` cookie; mutating
//! requests need the hub's own `Origin`; no CORS. The daemons keep their
//! token-only API; the hub adds the token when it forwards. The dashboard's
//! live streams come over WebSocket (`ws`), which needs the own `Origin` too.

use axum::Json;
use axum::body::Body;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, delete, get, post};
use devstack_compose::engine::{self, Engine};
use devstack_daemon::{diag, registry};
use hyper_util::rt::TokioIo;
use include_dir::{Dir, include_dir};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::watch;
use tokio_stream::StreamExt;

static UI: Dir<'_> = include_dir!("$OUT_DIR/ui");

mod dialog;
pub mod release;
mod ws;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const DEFAULT_PORT: u16 = 4250;
const COOKIE: &str = "devstack_hub";

/// The hub's port: `DEVSTACK_HUB_PORT`, else 4250.
pub fn port() -> u16 {
    std::env::var("DEVSTACK_HUB_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(DEFAULT_PORT)
}

/// Written next to the token while the hub runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HubInfo {
    pub pid: u32,
    pub port: u16,
    pub version: String,
    pub started: u128,
}

fn state_dir() -> PathBuf {
    devstack_daemon::token::token_path()
        .parent()
        .expect("token path has a parent")
        .join("hub")
}

/// The hub's diagnostic log (`devstack logs --hub`).
pub fn log_path() -> PathBuf {
    state_dir().join("hub.log")
}

pub fn read_info() -> Option<HubInfo> {
    serde_json::from_slice(&std::fs::read(state_dir().join("hub.json")).ok()?).ok()
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DaemonEntry {
    pub pid: u32,
    pub port: u16,
    pub started: u128,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct StackEntry {
    pub id: String,
    pub name: String,
    pub file: PathBuf,
    pub repo: PathBuf,
    pub running: bool,
    pub daemon: Option<DaemonEntry>,
    pub last_seen: Option<u128>,
}

#[derive(Clone)]
struct AppState {
    port: u16,
    token: Arc<String>,
    /// blake3 of each session cookie -> created (ms); persisted, see `Sessions`.
    sessions: Arc<Mutex<Sessions>>,
    codes: Arc<Mutex<HashMap<String, Instant>>>,
    stacks: watch::Receiver<Vec<StackEntry>>,
    /// The Docker engine as last seen, see `watch_engine`.
    engine: watch::Receiver<Engine>,
    /// A newer release than this hub, if the check found one (`release`).
    newer: watch::Receiver<Option<String>>,
    /// Until when (ms) to look at the engine every 2 s: after a start request.
    engine_fast_until: Arc<std::sync::atomic::AtomicU64>,
    shutdown: Arc<tokio::sync::Notify>,
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis())
}

fn random_hex() -> String {
    let bytes: [u8; 24] = rand::random();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Which registered stacks run: their daemon's info file exists and its port
/// answers. Checked by the hub every second; the dashboard only hears changes.
fn scan(last_seen: &mut HashMap<String, u128>) -> Vec<StackEntry> {
    registry::list()
        .into_iter()
        .map(|e| {
            let state = e.repo.join(".devstack");
            let daemon = devstack_daemon::read_info(&state).filter(|i| {
                std::net::TcpStream::connect_timeout(
                    &std::net::SocketAddr::from(([127, 0, 0, 1], i.port)),
                    Duration::from_millis(200),
                )
                .is_ok()
            });
            if daemon.is_some() {
                last_seen.insert(e.id.clone(), now_ms());
            }
            StackEntry {
                running: daemon.is_some(),
                daemon: daemon.map(|i| DaemonEntry {
                    pid: i.pid,
                    port: i.port,
                    started: i.started,
                    version: i.version,
                }),
                last_seen: last_seen.get(&e.id).copied(),
                id: e.id,
                name: e.name,
                file: e.file,
                repo: e.repo,
            }
        })
        .collect()
}

/// Run the hub in the foreground until `POST /hub/shutdown` or Ctrl+C. A second
/// hub on the same port exits at once (the first one serves).
/// Paired browsers survive a hub restart (an update, a new login): the hub
/// keeps a hash of each session cookie here, never the cookie itself. Deleting
/// the file unpairs every browser.
const SESSIONS_FILE: &str = "hub-sessions.json";
/// A session lives as long as its cookie (`Max-Age`).
const SESSION_LIFETIME_MS: u128 = 365 * 24 * 60 * 60 * 1000;

struct Sessions {
    path: PathBuf,
    by_hash: HashMap<String, u128>,
}

impl Sessions {
    fn load(path: PathBuf) -> Self {
        let now = now_ms();
        let by_hash: HashMap<String, u128> = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let by_hash = by_hash
            .into_iter()
            .filter(|(_, created)| now.saturating_sub(*created) < SESSION_LIFETIME_MS)
            .collect();
        Self { path, by_hash }
    }

    fn hash(cookie: &str) -> String {
        blake3::hash(cookie.as_bytes()).to_hex().to_string()
    }

    fn contains(&self, cookie: &str) -> bool {
        self.by_hash
            .get(&Self::hash(cookie))
            .is_some_and(|created| now_ms().saturating_sub(*created) < SESSION_LIFETIME_MS)
    }

    fn add(&mut self, cookie: &str) {
        self.by_hash.insert(Self::hash(cookie), now_ms());
        // Best effort: without the file, the session still works until the hub stops.
        if let Ok(json) = serde_json::to_vec_pretty(&self.by_hash) {
            let tmp = self.path.with_extension("json.tmp");
            if std::fs::write(&tmp, json).is_ok() {
                let _ = std::fs::rename(&tmp, &self.path);
            }
        }
    }
}

/// How long a stopping hub waits for open requests before it exits anyway.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

pub fn serve() -> Result<(), String> {
    let port = port();
    let dir = state_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    // A hub that was just told to stop (`devstack update`, a restart) can hold
    // the lock a few seconds longer while it lets go of open streams; wait for
    // it instead of exiting at once (the daemon does the same).
    let t0 = std::time::Instant::now();
    let _lock = loop {
        match devstack_proc::StateLock::acquire(&dir) {
            Ok(l) => break l,
            Err(devstack_proc::LockError::Held(_)) if t0.elapsed() < Duration::from_secs(15) => {
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => return Err(e.to_string()),
        }
    };
    diag::init(log_path());
    let token = devstack_daemon::token::load_or_create().map_err(|e| format!("token: {e}"))?;
    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    rt.block_on(async move {
        let v4 = tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .map_err(|e| format!("port {port}: {e}"))?;
        let v6 = tokio::net::TcpListener::bind(("::1", port)).await.ok();
        let info = HubInfo {
            pid: std::process::id(),
            port,
            version: VERSION.into(),
            started: now_ms(),
        };
        std::fs::write(
            dir.join("hub.json"),
            serde_json::to_vec_pretty(&info).unwrap(),
        )
        .map_err(|e| e.to_string())?;

        let (tx, rx) = watch::channel(Vec::new());
        tokio::task::spawn_blocking(move || {
            let mut seen = HashMap::new();
            loop {
                let next = scan(&mut seen);
                tx.send_if_modified(|cur| {
                    if *cur == next {
                        false
                    } else {
                        *cur = next;
                        true
                    }
                });
                if tx.is_closed() {
                    return;
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        });

        let engine_fast_until = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let engine_rx = watch_engine(engine_fast_until.clone());
        let newer_rx = watch_release();

        let shutdown = Arc::new(tokio::sync::Notify::new());
        let state = AppState {
            port,
            token: Arc::new(token),
            sessions: Arc::new(Mutex::new(Sessions::load(dir.join(SESSIONS_FILE)))),
            codes: Arc::new(Mutex::new(HashMap::new())),
            stacks: rx,
            engine: engine_rx,
            newer: newer_rx,
            engine_fast_until,
            shutdown: shutdown.clone(),
        };
        let app = router(state);
        if let Some(v6) = v6 {
            let app6 = app.clone();
            let s6 = shutdown.clone();
            tokio::spawn(async move {
                let _ = axum::serve(v6, app6)
                    .with_graceful_shutdown(async move { s6.notified().await })
                    .await;
            });
        }
        let hub_json = dir.join("hub.json");
        diag::info(
            "hub_start",
            serde_json::json!({ "version": VERSION, "pid": std::process::id(), "port": port }),
        );
        let stop = async move {
            let why = tokio::select! {
                _ = shutdown.notified() => "shutdown requested",
                _ = tokio::signal::ctrl_c() => "ctrl-c",
            };
            diag::info("hub_stop", serde_json::json!({ "reason": why }));
            // Open streams (every dashboard tab holds one) never end on their own,
            // and a graceful shutdown waits for them. Give requests a moment, then go.
            std::thread::spawn(move || {
                std::thread::sleep(SHUTDOWN_GRACE);
                let _ = std::fs::remove_file(hub_json);
                std::process::exit(0);
            });
        };
        let served = axum::serve(v4, app)
            .with_graceful_shutdown(stop)
            .await
            .map_err(|e| e.to_string());
        let _ = std::fs::remove_file(dir.join("hub.json"));
        served
    })
}

fn router(state: AppState) -> axum::Router {
    axum::Router::new()
        .route("/auth", get(auth))
        .route("/hub/pair", post(pair))
        .route("/hub/shutdown", post(shutdown))
        .route("/hub/session", get(session))
        .route("/hub/ping", get(ping).options(ping_preflight))
        .route("/hub/stacks", get(stacks))
        .route("/hub/docker", get(docker))
        .route("/hub/docker/start", post(docker_start))
        .route("/hub/events", get(events))
        .route("/hub/ws", get(ws::ws))
        .route("/hub/inspect", get(inspect))
        .route("/hub/pick-folder", post(pick_folder))
        .route("/hub/stacks/add", post(stack_add))
        .route("/hub/stacks/init", post(stack_init))
        .route("/hub/stacks/scan", post(stack_scan))
        .route("/hub/stacks/{id}/up", post(stack_up))
        .route("/hub/stacks/{id}/down", post(stack_down))
        .route("/hub/stacks/{id}", delete(forget))
        .route("/hub/stacks/{id}/api/{*rest}", any(forward))
        .fallback(get(asset))
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .layer(middleware::from_fn(audit))
        .with_state(state)
}

/// Every request that changes something, with its answer (refused ones too,
/// this is the outer layer). Paths only: no query (a pairing code), no body.
async fn audit(req: Request, next: Next) -> Response {
    let method = req.method().clone();
    if method == Method::GET || method == Method::HEAD || method == Method::OPTIONS {
        return next.run(req).await;
    }
    let path = req.uri().path().to_string();
    let res = next.run(req).await;
    let status = res.status().as_u16();
    diag::log(
        if status >= 400 { "warn" } else { "info" },
        "request",
        serde_json::json!({ "method": method.as_str(), "path": path, "status": status }),
    );
    res
}

fn err(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(serde_json::json!({ "error": msg.into() }))).into_response()
}

fn cookie_session(h: &HeaderMap) -> Option<String> {
    h.get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .find_map(|c| {
            c.trim()
                .strip_prefix(&format!("{COOKIE}="))
                .map(str::to_string)
        })
}

fn bearer_ok(h: &HeaderMap, token: &str) -> bool {
    h.get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|t| {
            t.len() == token.len()
                && t.bytes()
                    .zip(token.bytes())
                    .fold(0u8, |a, (x, y)| a | (x ^ y))
                    == 0
        })
}

/// ADR-0019 on the hub: Host, then the credential for `/hub/*`, then Origin on
/// mutating requests. The app shell and `/auth` need no session.
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
    let path = req.uri().path();
    if path == "/hub/ping" {
        return next.run(req).await; // public on purpose, see `ping`
    }
    if path == "/hub/pair" || path == "/hub/shutdown" {
        if !bearer_ok(h, &s.token) {
            return err(StatusCode::UNAUTHORIZED, "missing or wrong token");
        }
        return next.run(req).await;
    }
    if path.starts_with("/hub/") {
        let ok = cookie_session(h).is_some_and(|c| s.sessions.lock().unwrap().contains(&c))
            || bearer_ok(h, &s.token);
        if !ok {
            return err(StatusCode::UNAUTHORIZED, "not paired: run `devstack ui`");
        }
        // A WebSocket handshake is a GET, but a page of any origin may open one.
        if (req.method() != Method::GET && req.method() != Method::HEAD) || path == "/hub/ws" {
            let origin = h.get(header::ORIGIN).and_then(|v| v.to_str().ok());
            let cookie_auth = !bearer_ok(h, &s.token);
            let own = origin.is_some_and(|o| allowed.iter().any(|a| o == format!("http://{a}")));
            if cookie_auth && !own {
                return err(StatusCode::FORBIDDEN, "foreign or missing Origin");
            }
        }
    }
    next.run(req).await
}

#[derive(Deserialize)]
struct CodeQuery {
    code: String,
}

/// Exchange a one-time code (60 s, single use) for the session cookie.
async fn auth(State(s): State<AppState>, Query(q): Query<CodeQuery>) -> Response {
    let valid = s
        .codes
        .lock()
        .unwrap()
        .remove(&q.code)
        .is_some_and(|t| t.elapsed() < Duration::from_secs(60));
    diag::log(
        if valid { "info" } else { "warn" },
        if valid { "paired" } else { "pairing_refused" },
        serde_json::json!({}),
    );
    if !valid {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            "This pairing link is used up or expired. Run `devstack ui` again.",
        )
            .into_response();
    }
    let id = random_hex();
    s.sessions.lock().unwrap().add(&id);
    let cookie = format!("{COOKIE}={id}; HttpOnly; SameSite=Strict; Path=/; Max-Age=31536000");
    (
        StatusCode::SEE_OTHER,
        [
            (header::SET_COOKIE, cookie),
            (header::LOCATION, "/".to_string()),
        ],
    )
        .into_response()
}

async fn pair(State(s): State<AppState>) -> Response {
    let code = random_hex();
    s.codes.lock().unwrap().insert(code.clone(), Instant::now());
    let url = format!("http://127.0.0.1:{}/auth?code={code}", s.port);
    Json(serde_json::json!({ "code": code, "url": url })).into_response()
}

async fn shutdown(State(s): State<AppState>) -> Response {
    s.shutdown.notify_waiters();
    StatusCode::ACCEPTED.into_response()
}

/// The landing page (and only it) may ask whether a hub runs on this machine.
const PING_ORIGINS: &[&str] = &["https://nuetzliches.github.io"];

/// `PING_ORIGINS` plus `DEVSTACK_HUB_PING_ORIGINS` (comma-separated), for a
/// team that hosts its own copy of the page.
fn ping_origin_allowed(origin: &str) -> bool {
    PING_ORIGINS.contains(&origin)
        || std::env::var("DEVSTACK_HUB_PING_ORIGINS")
            .is_ok_and(|v| v.split(',').any(|o| o.trim() == origin))
}

fn ping_cors(h: &HeaderMap) -> Vec<(header::HeaderName, HeaderValue)> {
    let Some(origin) = h
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .filter(|o| ping_origin_allowed(o))
    else {
        return Vec::new();
    };
    vec![
        (
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
            HeaderValue::from_str(origin).expect("checked origin"),
        ),
        (header::VARY, HeaderValue::from_static("Origin")),
        // Chrome asks a public page's request to the local machine first
        // (Private Network Access preflight).
        (
            header::HeaderName::from_static("access-control-allow-private-network"),
            HeaderValue::from_static("true"),
        ),
    ]
}

/// `GET /hub/ping`: is a hub running, which version. No session, no stack
/// names, nothing that changes state: the one thing a web page may learn about
/// this machine, and only the landing page (CORS). The landing page uses it to
/// show "open the dashboard", "update available" or the install command.
async fn ping(h: HeaderMap) -> Response {
    let mut res = Json(serde_json::json!({ "devstack": true, "version": VERSION })).into_response();
    for (k, v) in ping_cors(&h) {
        res.headers_mut().insert(k, v);
    }
    res
}

async fn ping_preflight(h: HeaderMap) -> Response {
    let mut res = StatusCode::NO_CONTENT.into_response();
    let cors = ping_cors(&h);
    if !cors.is_empty() {
        for (k, v) in cors {
            res.headers_mut().insert(k, v);
        }
        res.headers_mut().insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static("GET"),
        );
    }
    res
}

async fn session() -> Response {
    Json(serde_json::json!({ "version": VERSION })).into_response()
}

/// Look at the Docker engine every 10 s (every 2 s after a start request) and
/// publish changes. One `docker version` per look; nothing when it did not change.
fn watch_engine(fast_until: Arc<std::sync::atomic::AtomicU64>) -> watch::Receiver<Engine> {
    let (tx, rx) = watch::channel(engine::status("docker"));
    std::thread::spawn(move || {
        loop {
            let fast = now_ms() < u128::from(fast_until.load(std::sync::atomic::Ordering::Relaxed));
            let step = if fast { 2 } else { 10 };
            std::thread::sleep(Duration::from_secs(step));
            if tx.is_closed() {
                return;
            }
            let next = engine::status("docker");
            tx.send_if_modified(|cur| {
                if *cur == next {
                    false
                } else {
                    diag::log(
                        if next.is_running() { "info" } else { "warn" },
                        "docker",
                        serde_json::to_value(&next).unwrap_or_default(),
                    );
                    *cur = next;
                    true
                }
            });
        }
    });
    rx
}

/// Look for a newer release at start and every 12 hours (`release::latest`),
/// unless `DEVSTACK_NO_UPDATE_CHECK` is set. Publishes the newer version, or
/// `None`; a failed check changes nothing.
fn watch_release() -> watch::Receiver<Option<String>> {
    let (tx, rx) = watch::channel(None);
    if release::check_disabled() {
        return rx;
    }
    std::thread::spawn(move || {
        loop {
            match release::latest() {
                Ok(latest) => {
                    let next = release::newer(&latest, VERSION).then_some(latest);
                    tx.send_if_modified(|cur| {
                        if *cur == next {
                            false
                        } else {
                            if let Some(v) = &next {
                                diag::info("release", serde_json::json!({ "newer": v }));
                            }
                            *cur = next;
                            true
                        }
                    });
                }
                Err(e) => diag::warn("release_check", serde_json::json!({ "error": e })),
            }
            if tx.is_closed() {
                return;
            }
            std::thread::sleep(Duration::from_secs(12 * 3600));
        }
    });
    rx
}

async fn docker(State(s): State<AppState>) -> Response {
    Json(s.engine.borrow().clone()).into_response()
}

/// Start Docker Desktop. `202` once started; the engine stream says when it answers.
async fn docker_start(State(s): State<AppState>) -> Response {
    if s.engine.borrow().is_running() {
        return (StatusCode::OK, Json(serde_json::json!({ "started": null }))).into_response();
    }
    match tokio::task::spawn_blocking(engine::start).await {
        Ok(Ok(name)) => {
            let until = now_ms() + 180_000;
            s.engine_fast_until.store(
                u64::try_from(until).unwrap_or(u64::MAX),
                std::sync::atomic::Ordering::Relaxed,
            );
            (
                StatusCode::ACCEPTED,
                Json(serde_json::json!({ "started": name })),
            )
                .into_response()
        }
        Ok(Err(e)) => err(StatusCode::CONFLICT, e),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn stacks(State(s): State<AppState>) -> Response {
    Json(s.stacks.borrow().clone()).into_response()
}

/// The stack list and the Docker engine first, then each on every change.
async fn events(State(s): State<AppState>) -> Response {
    let stream = hub_events(&s).map(|v| {
        Ok::<_, std::convert::Infallible>(SseEvent::default().json_data(v).unwrap_or_default())
    });
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// What `/hub/events` sends, as values (also over WebSocket, `ws`).
fn hub_events(s: &AppState) -> impl tokio_stream::Stream<Item = serde_json::Value> + Send + use<> {
    let stacks = tokio_stream::wrappers::WatchStream::new(s.stacks.clone())
        .map(|stacks| serde_json::json!({ "type": "stacks", "stacks": stacks }));
    let docker = tokio_stream::wrappers::WatchStream::new(s.engine.clone())
        .map(|engine| serde_json::json!({ "type": "docker", "docker": engine }));
    let release = tokio_stream::wrappers::WatchStream::new(s.newer.clone())
        .map(|newer| serde_json::json!({ "type": "release", "current": VERSION, "newer": newer }));
    stacks.merge(docker).merge(release)
}

fn entry(s: &AppState, id: &str) -> Option<StackEntry> {
    s.stacks
        .borrow()
        .iter()
        .find(|e| e.id == id)
        .cloned()
        .or_else(|| {
            registry::list()
                .into_iter()
                .find(|e| e.id == id)
                .map(|e| StackEntry {
                    id: e.id,
                    name: e.name,
                    file: e.file,
                    repo: e.repo,
                    running: false,
                    daemon: None,
                    last_seen: None,
                })
        })
}

/// Start the stack the way `devstack up` does, in the background.
// ---------------------------------------------------------- adding a stack
//
// The dashboard cannot pick a folder by path (a browser hands out handles,
// never absolute paths), so the user pastes one. Inspect it, then register a
// folder that has a stack file, or write a first draft (`devstack init`) and
// register that. Both are mutations: cookie and own Origin (guard).

#[derive(Deserialize)]
struct PathQuery {
    path: String,
}

/// The repository root and its stack file, in the form the daemon registers
/// (same id, so the daemon's own registration later is the same entry).
fn stack_paths(dir: &str) -> Result<(PathBuf, PathBuf), String> {
    let raw = PathBuf::from(dir.trim().trim_matches('"'));
    if !raw.is_absolute() {
        return Err("an absolute path, e.g. C:\\src\\shop or /home/me/src/shop".into());
    }
    let abs = std::path::absolute(&raw).map_err(|e| e.to_string())?;
    // A pasted path to the stack file itself names its folder.
    let repo = if abs.file_name().is_some_and(|n| n == "devstack.toml") {
        abs.parent().map(PathBuf::from).unwrap_or(abs)
    } else {
        abs
    };
    let file = repo.join("devstack.toml");
    Ok((
        devstack_proc::long_path(&repo),
        devstack_proc::long_path(&file),
    ))
}

/// One folder dialog at a time.
static PICKING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// `POST /hub/pick-folder`: the operating system's own folder dialog, shown by
/// the hub (a local program may, a web page may not learn a folder's path).
/// `{"path": "..."}`, `{"path": null}` when cancelled, `501` where there is no
/// dialog (a server without a desktop); the page then keeps its text field.
async fn pick_folder() -> Response {
    use std::sync::atomic::Ordering;
    if PICKING.swap(true, Ordering::SeqCst) {
        return err(StatusCode::CONFLICT, "a folder dialog is open already");
    }
    let picked = tokio::task::spawn_blocking(dialog::pick_folder).await;
    PICKING.store(false, Ordering::SeqCst);
    match picked {
        Ok(Ok(path)) => Json(serde_json::json!({ "path": path })).into_response(),
        Ok(Err(e)) => err(StatusCode::NOT_IMPLEMENTED, e),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// `GET /hub/inspect?path=`: is it a folder, does it have a stack file, is
/// it registered already.
async fn inspect(Query(q): Query<PathQuery>) -> Response {
    let (repo, file) = match stack_paths(&q.path) {
        Ok(p) => p,
        Err(e) => return err(StatusCode::BAD_REQUEST, e),
    };
    let id = registry::id_for(&file);
    let has_file = file.is_file();
    Json(serde_json::json!({
        "repo": repo,
        "is_dir": repo.is_dir(),
        "has_stack_file": has_file,
        "valid": has_file.then(|| devstack_config::load(&file).map(|_| ()).map_err(|e| e.to_string())),
        "registered": registry::list().iter().any(|e| e.id == id).then_some(&id),
        "id": id,
    }))
    .into_response()
}

fn register_folder(
    repo: &std::path::Path,
    file: &std::path::Path,
) -> Result<registry::Entry, String> {
    let stack = devstack_config::load(file).map_err(|e| e.to_string())?;
    let entry = registry::Entry {
        id: registry::id_for(file),
        name: stack.stack.name.clone(),
        file: file.to_path_buf(),
        repo: repo.to_path_buf(),
    };
    registry::register(&entry).map_err(|e| e.to_string())?;
    diag::info(
        "stack_added",
        serde_json::json!({ "id": entry.id, "name": entry.name }),
    );
    Ok(entry)
}

/// `POST /hub/stacks/add {"path"}`: register a folder that has a stack file.
async fn stack_add(Json(q): Json<PathQuery>) -> Response {
    let (repo, file) = match stack_paths(&q.path) {
        Ok(p) => p,
        Err(e) => return err(StatusCode::BAD_REQUEST, e),
    };
    if !file.is_file() {
        return err(
            StatusCode::CONFLICT,
            format!("no devstack.toml in {}", repo.display()),
        );
    }
    match tokio::task::spawn_blocking(move || register_folder(&repo, &file)).await {
        Ok(Ok(e)) => Json(serde_json::json!({ "id": e.id, "name": e.name })).into_response(),
        Ok(Err(e)) => err(StatusCode::CONFLICT, e),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// Directories a scan never enters.
const SCAN_SKIP: &[&str] = &[
    "node_modules",
    "target",
    "bin",
    "obj",
    "dist",
    "build",
    "vendor",
    "packages",
    "AppData",
    "Program Files",
    "Program Files (x86)",
    "Windows",
    "$RECYCLE.BIN",
];

/// Repositories with a stack file below `root`, at most `depth` levels down.
/// A repository (a `.git` directory) is looked at only at its root and not
/// entered: its sub-folders (examples, fixtures) are no stacks of their own.
/// A linked worktree (a `.git` file) is skipped: it runs as a lane of its
/// main working tree (plan §3.5).
pub fn scan_for_stacks(root: &std::path::Path, depth: usize) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, d)) = stack.pop() {
        let git = dir.join(".git");
        if git.is_file() {
            continue;
        }
        if git.is_dir() || dir.join("devstack.toml").is_file() {
            if dir.join("devstack.toml").is_file() {
                out.push(dir.clone());
            }
            if git.is_dir() {
                continue;
            }
        }
        if d >= depth {
            continue;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || SCAN_SKIP.contains(&name.as_str()) {
                continue;
            }
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                stack.push((e.path(), d + 1));
            }
        }
    }
    out.sort();
    out
}

#[derive(serde::Deserialize)]
struct ScanQuery {
    path: String,
    /// Register every valid stack found that is not registered yet.
    #[serde(default)]
    add: bool,
    #[serde(default = "scan_depth")]
    depth: usize,
}

fn scan_depth() -> usize {
    3
}

/// `POST /hub/stacks/scan {"path", "add"?, "depth"?}`: every repository with
/// a devstack.toml below a folder (e.g. the one all projects live in), with
/// whether it loads and is registered; with `add` the new ones are registered.
async fn stack_scan(Json(q): Json<ScanQuery>) -> Response {
    let root = match stack_paths(&q.path) {
        Ok((r, _)) => r,
        Err(e) => return err(StatusCode::BAD_REQUEST, e),
    };
    if !root.is_dir() {
        return err(
            StatusCode::BAD_REQUEST,
            format!("{} is not a folder", root.display()),
        );
    }
    let depth = q.depth.clamp(1, 6);
    let found = tokio::task::spawn_blocking(move || {
        let known: Vec<String> = registry::list().into_iter().map(|e| e.id).collect();
        scan_for_stacks(&root, depth)
            .into_iter()
            .map(|repo| {
                let (repo, file) = stack_paths(&repo.to_string_lossy())
                    .unwrap_or((repo.clone(), repo.join("devstack.toml")));
                let id = registry::id_for(&file);
                let loaded = devstack_config::load(&file);
                let name = loaded.as_ref().map(|s| s.stack.name.clone()).ok();
                let error = loaded.as_ref().err().map(|e| e.to_string());
                let registered = known.contains(&id);
                let added = q.add
                    && !registered
                    && error.is_none()
                    && register_folder(&repo, &file).is_ok();
                serde_json::json!({
                    "repo": repo, "id": id, "name": name, "error": error,
                    "registered": registered || added, "added": added,
                })
            })
            .collect::<Vec<_>>()
    })
    .await;
    match found {
        Ok(list) => Json(list).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// `POST /hub/stacks/init {"path"}`: write a first draft (`devstack init`,
/// never over an existing file) and register it, so it shows in the list.
async fn stack_init(Json(q): Json<PathQuery>) -> Response {
    let (repo, file) = match stack_paths(&q.path) {
        Ok(p) => p,
        Err(e) => return err(StatusCode::BAD_REQUEST, e),
    };
    if !repo.is_dir() {
        return err(
            StatusCode::BAD_REQUEST,
            format!("{} is not a folder", repo.display()),
        );
    }
    if file.exists() {
        return err(
            StatusCode::CONFLICT,
            "it has a devstack.toml already; add it instead",
        );
    }
    let Ok(exe) = std::env::current_exe() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "cannot find devstack");
    };
    let run = {
        let repo = repo.clone();
        tokio::task::spawn_blocking(move || {
            let mut cmd = std::process::Command::new(exe);
            cmd.args(["init", "--json", "--dir"])
                .arg(&repo)
                .stdin(std::process::Stdio::null());
            no_window(&mut cmd);
            cmd.output()
        })
        .await
    };
    let out = match run {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    if !out.status.success() {
        return err(
            StatusCode::CONFLICT,
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        );
    }
    let draft: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
    diag::info(
        "stack_init",
        serde_json::json!({ "repo": repo, "todos": draft["todos"] }),
    );
    // A draft may not load yet (TODOs); then it is written but not listed.
    let registered = tokio::task::spawn_blocking({
        let (repo, file) = (repo.clone(), file.clone());
        move || register_folder(&repo, &file)
    })
    .await
    .ok()
    .and_then(Result::ok);
    Json(serde_json::json!({
        "file": file,
        "services": draft["services"],
        "compose_files": draft["compose_files"],
        "todos": draft["todos"],
        "id": registered.as_ref().map(|e| e.id.clone()),
        "name": registered.as_ref().map(|e| e.name.clone()),
    }))
    .into_response()
}

async fn stack_up(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    let Some(e) = entry(&s, &id) else {
        return err(StatusCode::NOT_FOUND, format!("no stack {id}"));
    };
    let Ok(exe) = std::env::current_exe() else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "cannot find devstack");
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("up")
        .arg("-f")
        .arg(&e.file)
        .current_dir(&e.repo)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    no_window(&mut cmd);
    match cmd.spawn() {
        Ok(_) => StatusCode::ACCEPTED.into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[derive(Deserialize)]
struct DownQuery {
    #[serde(default)]
    infra: bool,
}

async fn stack_down(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<DownQuery>,
) -> Response {
    let Some(port) = entry(&s, &id).and_then(|e| e.daemon).map(|d| d.port) else {
        return err(StatusCode::SERVICE_UNAVAILABLE, "stack not running");
    };
    let token = s.token.clone();
    tokio::spawn(async move {
        let _ = daemon_request(
            port,
            &token,
            Method::POST,
            &format!("/v1/down?infra={}", q.infra),
        )
        .await;
        let _ = daemon_request(port, &token, Method::POST, "/v1/shutdown").await;
    });
    StatusCode::ACCEPTED.into_response()
}

async fn forget(Path(id): Path<String>) -> Response {
    match registry::forget(&id) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn daemon_request(port: u16, token: &str, method: Method, path: &str) -> Result<u16, String> {
    let stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .map_err(|e| e.to_string())?;
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .map_err(|e| e.to_string())?;
    tokio::spawn(conn);
    let req = hyper::Request::builder()
        .method(method)
        .uri(path)
        .header(header::HOST, format!("127.0.0.1:{port}"))
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(http_body_util::Empty::<bytes::Bytes>::new())
        .map_err(|e| e.to_string())?;
    let resp = sender.send_request(req).await.map_err(|e| e.to_string())?;
    Ok(resp.status().as_u16())
}

/// `/hub/stacks/{id}/api/<rest>` -> the daemon's `/<rest>`, with the token, the
/// daemon's own Host, without the browser's cookie and Origin; bodies and
/// streams pass through.
async fn forward(
    State(s): State<AppState>,
    Path((id, rest)): Path<(String, String)>,
    req: Request,
) -> Response {
    let Some(port) = entry(&s, &id).and_then(|e| e.daemon).map(|d| d.port) else {
        return err(StatusCode::SERVICE_UNAVAILABLE, "stack not running");
    };
    let query = req
        .uri()
        .query()
        .map(|q| format!("?{q}"))
        .unwrap_or_default();
    let (mut parts, body) = req.into_parts();
    parts.uri = match format!("/{rest}{query}").parse() {
        Ok(u) => u,
        Err(_) => return err(StatusCode::BAD_REQUEST, "bad path"),
    };
    parts.headers.remove(header::COOKIE);
    parts.headers.remove(header::ORIGIN);
    parts.headers.remove(header::REFERER);
    parts.headers.insert(
        header::HOST,
        HeaderValue::from_str(&format!("127.0.0.1:{port}")).unwrap(),
    );
    parts.headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", s.token)).unwrap(),
    );
    match daemon_send(port, hyper::Request::from_parts(parts, body)).await {
        Ok(resp) => {
            let (parts, body) = resp.into_parts();
            Response::from_parts(parts, Body::new(body))
        }
        Err((status, msg)) => err(status, msg),
    }
}

/// Why the hub could not reach a daemon: the status and message to answer with.
type DaemonError = (StatusCode, String);

/// `GET <path>` on the daemon of stack `id`, with the token (the `ws` bridge).
async fn daemon_get(
    s: &AppState,
    id: &str,
    path: &str,
) -> Result<hyper::Response<hyper::body::Incoming>, DaemonError> {
    let Some(port) = entry(s, id).and_then(|e| e.daemon).map(|d| d.port) else {
        return Err((StatusCode::SERVICE_UNAVAILABLE, "stack not running".into()));
    };
    let req = hyper::Request::get(path)
        .header(header::HOST, format!("127.0.0.1:{port}"))
        .header(header::AUTHORIZATION, format!("Bearer {}", s.token))
        .header(header::ACCEPT, "text/event-stream")
        .body(Body::empty())
        .map_err(|_| (StatusCode::BAD_REQUEST, "bad path".to_string()))?;
    daemon_send(port, req).await
}

async fn daemon_send(
    port: u16,
    req: hyper::Request<Body>,
) -> Result<hyper::Response<hyper::body::Incoming>, DaemonError> {
    let stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .map_err(|e| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                format!("stack not running: {e}"),
            )
        })?;
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
    tokio::spawn(conn);
    sender
        .send_request(req)
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))
}

/// The embedded dashboard; unknown paths get `index.html` (client-side routes).
async fn asset(req: Request) -> Response {
    let path = req.uri().path().trim_start_matches('/');
    let (file, path) = match UI.get_file(path) {
        Some(f) if !path.is_empty() => (f, path),
        _ => match UI.get_file("index.html") {
            Some(f) => (f, "index.html"),
            None => return err(StatusCode::NOT_FOUND, "no dashboard in this build"),
        },
    };
    let mime = match path.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("webmanifest") => "application/manifest+json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    };
    // The shell and the service worker must be revalidated, hashed assets not.
    let cache = if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    (
        [(header::CONTENT_TYPE, mime), (header::CACHE_CONTROL, cache)],
        file.contents(),
    )
        .into_response()
}

#[cfg(windows)]
fn no_window(cmd: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn no_window(_cmd: &mut std::process::Command) {}

/// Start a hub in the background unless one answers; returns its port.
pub fn ensure_running() -> Result<u16, String> {
    let port = port();
    if answers(port) {
        return Ok(port);
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    // What the hub prints before its own log exists (a failed start) goes to
    // hub.out, like the daemon's daemon.out.
    let _ = std::fs::create_dir_all(state_dir());
    let out = state_dir().join("hub.out");
    let log = std::fs::File::create(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("hub")
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone().map_err(|e| e.to_string())?)
        .stderr(log);
    devstack_proc::spawn_detached(&mut cmd).map_err(|e| format!("start the hub: {e}"))?;
    // A hub that was just stopped can hold the state lock a few seconds more
    // (the new one waits up to 15 s for it), and a fresh binary may be read
    // by a virus scanner first.
    const WAIT: Duration = Duration::from_secs(30);
    let t0 = Instant::now();
    while t0.elapsed() < WAIT {
        if answers(port) {
            return Ok(port);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let said = std::fs::read_to_string(&out).unwrap_or_default();
    Err(format!(
        "the hub did not answer on port {port} within {} s{}",
        WAIT.as_secs(),
        if said.trim().is_empty() {
            String::new()
        } else {
            format!(":\n{}", said.trim())
        }
    ))
}

/// Is a hub (or anything) answering on `port`?
pub fn answers(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_millis(300),
    )
    .is_ok()
}

#[cfg(test)]
mod scan_tests {
    use super::scan_for_stacks;

    #[test]
    fn finds_repositories_with_a_stack_file_not_their_subfolders_or_worktrees() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        let mk = |p: &str| std::fs::create_dir_all(r.join(p)).unwrap();
        let touch = |p: &str, t: &str| std::fs::write(r.join(p), t).unwrap();
        mk("shop/.git");
        touch("shop/devstack.toml", "");
        mk("shop/examples");
        touch("shop/examples/devstack.toml", ""); // inside a repo: not a stack
        mk("group/crm/.git");
        touch("group/crm/devstack.toml", "");
        mk("plain/.git"); // a repo without a stack file
        mk("shop-wt");
        touch("shop-wt/.git", "gitdir: x/.git/worktrees/shop-wt");
        touch("shop-wt/devstack.toml", ""); // a worktree: a lane
        mk("node_modules/pkg/.git");
        touch("node_modules/pkg/devstack.toml", "");
        let found: Vec<String> = scan_for_stacks(r, 3)
            .into_iter()
            .map(|p| {
                p.strip_prefix(r)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        assert_eq!(found, ["group/crm", "shop"]);
    }
}
