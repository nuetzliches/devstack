//! The hub (ADR-0021): one long-lived process on a fixed loopback port that
//! serves the dashboard as a progressive web app, knows every stack (the
//! registry the daemons write), forwards the dashboard's requests and streams
//! to the stacks' daemons, and starts and stops stacks.
//!
//! Security per ADR-0019: loopback only, `Host` checked, the browser pairs
//! with a one-time code (`POST /hub/pair` with the user's token, then
//! `/auth?code=`) and holds an `HttpOnly; SameSite=Strict` cookie; mutating
//! requests need the hub's own `Origin`; no CORS. The daemons keep their
//! token-only API; the hub adds the token when it forwards.

use axum::Json;
use axum::body::Body;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, delete, get, post};
use devstack_compose::engine::{self, Engine};
use devstack_daemon::registry;
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
    let _lock = devstack_proc::StateLock::acquire(&dir).map_err(|e| e.to_string())?;
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

        let shutdown = Arc::new(tokio::sync::Notify::new());
        let state = AppState {
            port,
            token: Arc::new(token),
            sessions: Arc::new(Mutex::new(Sessions::load(dir.join(SESSIONS_FILE)))),
            codes: Arc::new(Mutex::new(HashMap::new())),
            stacks: rx,
            engine: engine_rx,
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
        let stop = async move {
            tokio::select! {
                _ = shutdown.notified() => {}
                _ = tokio::signal::ctrl_c() => {}
            }
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
        .route("/hub/stacks", get(stacks))
        .route("/hub/docker", get(docker))
        .route("/hub/docker/start", post(docker_start))
        .route("/hub/events", get(events))
        .route("/hub/stacks/{id}/up", post(stack_up))
        .route("/hub/stacks/{id}/down", post(stack_down))
        .route("/hub/stacks/{id}", delete(forget))
        .route("/hub/stacks/{id}/api/{*rest}", any(forward))
        .fallback(get(asset))
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state)
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
        if req.method() != Method::GET && req.method() != Method::HEAD {
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
                    *cur = next;
                    true
                }
            });
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
    let stacks = tokio_stream::wrappers::WatchStream::new(s.stacks.clone())
        .map(|stacks| serde_json::json!({ "type": "stacks", "stacks": stacks }));
    let docker = tokio_stream::wrappers::WatchStream::new(s.engine.clone())
        .map(|engine| serde_json::json!({ "type": "docker", "docker": engine }));
    let stream = stacks.merge(docker).map(|v| {
        Ok::<_, std::convert::Infallible>(SseEvent::default().json_data(v).unwrap_or_default())
    });
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
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
    let stream = match tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
        Ok(st) => st,
        Err(e) => {
            return err(
                StatusCode::SERVICE_UNAVAILABLE,
                format!("stack not running: {e}"),
            );
        }
    };
    let (mut sender, conn) = match hyper::client::conn::http1::handshake(TokioIo::new(stream)).await
    {
        Ok(x) => x,
        Err(e) => return err(StatusCode::BAD_GATEWAY, e.to_string()),
    };
    tokio::spawn(conn);
    match sender
        .send_request(hyper::Request::from_parts(parts, body))
        .await
    {
        Ok(resp) => {
            let (parts, body) = resp.into_parts();
            Response::from_parts(parts, Body::new(body))
        }
        Err(e) => err(StatusCode::BAD_GATEWAY, e.to_string()),
    }
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
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("hub")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    devstack_proc::spawn_detached(&mut cmd).map_err(|e| format!("start the hub: {e}"))?;
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(10) {
        if answers(port) {
            return Ok(port);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(format!("the hub did not answer on port {port} within 10 s"))
}

/// Is a hub (or anything) answering on `port`?
pub fn answers(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        Duration::from_millis(300),
    )
    .is_ok()
}
