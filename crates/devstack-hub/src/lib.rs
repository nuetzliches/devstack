//! The hub (ADR-0021): one long-lived process on a fixed loopback port that
//! serves the dashboard as a progressive web app, knows every stack (the
//! registry the daemons write), forwards the dashboard's requests and streams
//! to the stacks' daemons, and starts and stops stacks.
//!
//! Security per ADR-0019: loopback only, `Host` checked, the browser pairs
//! with a one-time code (`POST /hub/pair` with the user's token, then
//! `/auth?code=`) and holds an `HttpOnly; SameSite=Strict` cookie; every
//! request with it must come from the hub's own page (`guard`), mutations and
//! the WebSocket (`ws`) with the hub's own `Origin`; no CORS; no framing, no
//! inline script (`page_headers`). The daemons keep their token-only API; the
//! hub adds the token when it forwards.

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
mod pool;
pub mod release;
mod share;
mod shared;
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
    /// Why the last start from the dashboard failed (the end of `devstack
    /// up`'s output), while the stack is not running (#11).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_error: Option<String>,
}

/// Start failures by stack id, from `stack_up` to `scan`.
type StartErrors = Arc<Mutex<HashMap<String, String>>>;

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
    /// Asks the release check to look again if its last answer is older than
    /// an hour (a dashboard connected, see `watch_release`).
    release_poke: std::sync::mpsc::Sender<()>,
    /// `POST /hub/update`: running, or how the last one failed.
    update: watch::Sender<UpdateState>,
    /// Why a start from the dashboard failed, by stack id (#11).
    start_errors: StartErrors,
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
fn scan(last_seen: &mut HashMap<String, u128>, errors: &StartErrors) -> Vec<StackEntry> {
    let mut errors = errors.lock().unwrap();
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
                errors.remove(&e.id); // it runs: whatever failed before is over
            }
            StackEntry {
                start_error: errors.get(&e.id).cloned(),
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
    /// The file as last read or written (modified time, length); `None`: no
    /// file. A different one on disk is read again, so deleting or editing
    /// the file takes effect with the next request, not the next start.
    stamp: Option<(std::time::SystemTime, u64)>,
}

fn file_stamp(path: &std::path::Path) -> Option<(std::time::SystemTime, u64)> {
    let m = std::fs::metadata(path).ok()?;
    Some((m.modified().ok()?, m.len()))
}

impl Sessions {
    fn load(path: PathBuf) -> Self {
        let mut s = Self {
            path,
            by_hash: HashMap::new(),
            stamp: None,
        };
        s.reload();
        s
    }

    fn reload(&mut self) {
        let now = now_ms();
        self.stamp = file_stamp(&self.path);
        let by_hash: HashMap<String, u128> = std::fs::read(&self.path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        self.by_hash = by_hash
            .into_iter()
            .filter(|(_, created)| now.saturating_sub(*created) < SESSION_LIFETIME_MS)
            .collect();
    }

    fn hash(cookie: &str) -> String {
        blake3::hash(cookie.as_bytes()).to_hex().to_string()
    }

    fn contains(&mut self, cookie: &str) -> bool {
        if file_stamp(&self.path) != self.stamp {
            self.reload();
        }
        self.by_hash
            .get(&Self::hash(cookie))
            .is_some_and(|created| now_ms().saturating_sub(*created) < SESSION_LIFETIME_MS)
    }

    fn add(&mut self, cookie: &str) {
        // Sessions someone removed from the file stay removed.
        if file_stamp(&self.path) != self.stamp {
            self.reload();
        }
        self.by_hash.insert(Self::hash(cookie), now_ms());
        // Best effort: without the file, the session still works until the
        // hub stops (or until a file appears that does not list it).
        if let Ok(json) = serde_json::to_vec_pretty(&self.by_hash) {
            let tmp = self.path.with_extension("json.tmp");
            if write_private(&tmp, &json).is_ok() && std::fs::rename(&tmp, &self.path).is_ok() {
                self.stamp = file_stamp(&self.path);
            }
        }
    }
}

/// A file only this user may read (`0600` on Unix; on Windows the profile
/// directory it lives in is the user's alone).
fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        // `mode` applies to a new file only.
        f.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
        f.write_all(bytes)
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, bytes)
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
        let start_errors: StartErrors = Arc::default();
        let errors = start_errors.clone();
        tokio::task::spawn_blocking(move || {
            let mut seen = HashMap::new();
            loop {
                let next = scan(&mut seen, &errors);
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

        share::start();
        let engine_fast_until = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let engine_rx = watch_engine(engine_fast_until.clone());
        let (newer_rx, release_poke) = watch_release();

        let shutdown = Arc::new(tokio::sync::Notify::new());
        let state = AppState {
            port,
            token: Arc::new(token),
            sessions: Arc::new(Mutex::new(Sessions::load(dir.join(SESSIONS_FILE)))),
            codes: Arc::new(Mutex::new(HashMap::new())),
            stacks: rx,
            engine: engine_rx,
            newer: newer_rx,
            release_poke,
            update: watch::channel(UpdateState::default()).0,
            start_errors,
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
        .route("/hub/update", post(update))
        .route("/hub/inspect", get(inspect))
        .route("/hub/pick-folder", post(pick_folder))
        .route("/hub/stacks/add", post(stack_add))
        .route("/hub/stacks/init", post(stack_init))
        .route("/hub/stacks/scan", post(stack_scan))
        .route("/hub/stacks/{id}/up", post(stack_up))
        .route("/hub/stacks/{id}/down", post(stack_down))
        .route("/hub/stacks/{id}", delete(forget))
        .route("/hub/stacks/{id}/api/{*rest}", any(forward))
        .route("/hub/pool", get(pool::overview))
        .route("/hub/pool/share", get(share::get).put(share::put))
        .route("/hub/pool/shared", get(shared::list))
        .route("/hub/pool/shared/{lease}/open", post(shared::open))
        .route("/hub/pool/members/{id}/approve", post(pool::approve))
        .route("/hub/pool/members/{id}/remove", post(pool::remove))
        .route("/hub/pool/members/{id}/role", post(pool::role))
        .route("/hub/pool/nodes/{id}/pause", post(pool::pause))
        .route("/hub/pool/nodes/{id}/resume", post(pool::resume))
        .route("/hub/pool/leases/{id}/end", post(pool::end))
        .route("/hub/pool/members/{id}/tags", post(pool::tags))
        .route("/hub/pools", get(pool::pools))
        .route("/hub/pool/history", get(pool::history))
        .route(
            "/hub/pool/telemetry",
            get(pool::telemetry).put(pool::set_telemetry),
        )
        .route("/hub/pool/nodes/{id}/policy", post(pool::policy))
        .route("/hub/pool/join", post(pool::join))
        .fallback(get(asset))
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .layer(middleware::from_fn(audit))
        .layer(middleware::from_fn_with_state(state.clone(), page_headers))
        .with_state(state)
}

/// What the browser may do with the hub's pages (H-2), on every answer,
/// refused ones included: no framing (another loopback page could otherwise
/// lay the paired dashboard under its own clicks), scripts and connections
/// from the hub alone, no sniffing, no referrer. Inline styles stay allowed
/// (`:style` bindings and the log view's colours); no inline script is.
async fn page_headers(State(s): State<AppState>, req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    // `'self'` covers the page's own `ws://` in current browsers; named
    // anyway for older ones. Not `[::1]`: a CSP source cannot be an IPv6
    // literal, `'self'` covers it.
    let ws: Vec<String> = own_hosts(s.port)
        .iter()
        .filter(|a| !a.starts_with('['))
        .map(|a| format!("ws://{a}"))
        .collect();
    let csp = format!(
        "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
         img-src 'self' data:; font-src 'self'; connect-src 'self' {}; \
         manifest-src 'self'; worker-src 'self'; frame-ancestors 'none'; \
         base-uri 'none'; form-action 'self'; object-src 'none'",
        ws.join(" ")
    );
    if let Ok(v) = HeaderValue::from_str(&csp) {
        h.insert(header::CONTENT_SECURITY_POLICY, v);
    }
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    res
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

/// Every `devstack_hub` cookie the request carries. Cookies are not scoped by
/// port: another server on a loopback port may set one of the same name, and
/// the browser then sends both. The guard takes whichever is a session.
fn cookie_sessions(h: &HeaderMap) -> impl Iterator<Item = &str> {
    h.get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|c| c.trim().strip_prefix(COOKIE)?.strip_prefix('='))
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

/// ADR-0019 on the hub: Host, then the credential for `/hub/*`, then where
/// the request comes from. The app shell and `/auth` need no session.
///
/// `SameSite=Strict` does not separate loopback ports: a page on
/// `http://127.0.0.1:3000` is same-site with the hub, and its requests carry
/// the cookie. So a request with the cookie must come from the hub's own
/// page, whatever its method: a mutation and the WebSocket handshake name the
/// hub's own `Origin`; a `GET` names it, or says with `Sec-Fetch-Site` that
/// it is same-origin or the user's own navigation (`none`). Every current
/// browser sends `Sec-Fetch-Site`. A request with the token (the CLI) is no
/// browser's and passes.
async fn guard(State(s): State<AppState>, req: Request, next: Next) -> Response {
    let h = req.headers();
    let host = h
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let allowed = own_hosts(s.port);
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
        if bearer_ok(h, &s.token) {
            return next.run(req).await;
        }
        let paired = {
            let mut sessions = s.sessions.lock().unwrap();
            cookie_sessions(h).any(|c| sessions.contains(c))
        };
        if !paired {
            return err(StatusCode::UNAUTHORIZED, "not paired: run `devstack ui`");
        }
        if !from_own_page(h, req.method(), path, &allowed) {
            return err(StatusCode::FORBIDDEN, "foreign or missing Origin");
        }
    }
    next.run(req).await
}

/// The `Host` values of this hub (with `http://`, its origins).
fn own_hosts(port: u16) -> [String; 3] {
    [
        format!("127.0.0.1:{port}"),
        format!("localhost:{port}"),
        format!("[::1]:{port}"),
    ]
}

/// Whether a request with the session cookie comes from the hub's own page
/// (see `guard`).
fn from_own_page(h: &HeaderMap, method: &Method, path: &str, own: &[String]) -> bool {
    let own_origin = h
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|o| own.iter().any(|a| o == format!("http://{a}")));
    let fetch_site = h
        .get("sec-fetch-site")
        .and_then(|v| v.to_str().ok())
        .map(str::to_ascii_lowercase);
    // A browser that says the request is not same-origin is believed,
    // whatever else it sends.
    if fetch_site
        .as_deref()
        .is_some_and(|f| f != "same-origin" && f != "none")
    {
        return false;
    }
    // A WebSocket handshake is a GET, but a page of any origin may open one.
    let reads = (method == Method::GET || method == Method::HEAD) && path != "/hub/ws";
    if reads {
        own_origin || fetch_site.is_some()
    } else {
        own_origin
    }
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
        .is_some_and(|t| t.elapsed() < CODE_LIFETIME);
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

/// How long a pairing code works.
const CODE_LIFETIME: Duration = Duration::from_secs(60);

async fn pair(State(s): State<AppState>) -> Response {
    let code = random_hex();
    {
        let mut codes = s.codes.lock().unwrap();
        // Codes nobody used are dropped once expired, not kept for the hub's life.
        codes.retain(|_, t| t.elapsed() < CODE_LIFETIME);
        codes.insert(code.clone(), Instant::now());
    }
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
///
/// With `?nonce=<16 to 128 characters>` the answer also carries `proof`, a
/// keyed hash of the nonce under a key derived from the user's token (see
/// `ping_proof`): the CLI asks before it sends the token, so a program of
/// another user that took the hub's port while the hub was down does not get
/// it. The proof reveals nothing of the token.
async fn ping(State(s): State<AppState>, Query(q): Query<PingQuery>, h: HeaderMap) -> Response {
    let mut body = serde_json::json!({ "devstack": true, "version": VERSION });
    if let Some(nonce) = q.nonce.filter(|n| (16..=128).contains(&n.len())) {
        body["proof"] = ping_proof(&s.token, &nonce).into();
    }
    let mut res = Json(body).into_response();
    for (k, v) in ping_cors(&h) {
        res.headers_mut().insert(k, v);
    }
    res
}

#[derive(Deserialize)]
struct PingQuery {
    #[serde(default)]
    nonce: Option<String>,
}

/// What a hub that knows `token` answers to `nonce` (hex).
fn ping_proof(token: &str, nonce: &str) -> String {
    let key = blake3::derive_key("devstack hub ping proof v1", token.as_bytes());
    blake3::keyed_hash(&key, nonce.as_bytes())
        .to_hex()
        .to_string()
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

/// Look for a newer release at start, then every 12 hours by the wall clock,
/// and when a dashboard connects and the last answer is older than an hour
/// (the `Sender` pokes). Not with a 12-hour sleep: on Windows a sleeping
/// thread's time stands still while the machine is in standby, so a hub from
/// last night would not look again until well into the day. A failed check
/// is tried again after an hour. `DEVSTACK_NO_UPDATE_CHECK` turns it off.
fn watch_release() -> (watch::Receiver<Option<String>>, std::sync::mpsc::Sender<()>) {
    use std::sync::mpsc::RecvTimeoutError;
    use std::time::SystemTime;
    const EVERY: Duration = Duration::from_secs(12 * 3600);
    const RETRY: Duration = Duration::from_secs(3600);
    const FRESH: Duration = Duration::from_secs(3600);
    let (tx, rx) = watch::channel(None);
    let (poke, pokes) = std::sync::mpsc::channel::<()>();
    if release::check_disabled() {
        return (rx, poke);
    }
    std::thread::spawn(move || {
        let mut last: Option<SystemTime> = None;
        let mut next = SystemTime::now();
        loop {
            // The first look at once, then once a minute or when poked.
            let wait = if last.is_none() {
                Duration::ZERO
            } else {
                Duration::from_secs(60)
            };
            let poked = match pokes.recv_timeout(wait) {
                Ok(()) => true,
                Err(RecvTimeoutError::Timeout) => false,
                Err(RecvTimeoutError::Disconnected) => return,
            };
            let now = SystemTime::now();
            let stale = |t: Option<SystemTime>| {
                t.is_none_or(|t| now.duration_since(t).unwrap_or(FRESH) >= FRESH)
            };
            if now < next && !(poked && stale(last)) {
                continue;
            }
            last = Some(now);
            match release::latest() {
                Ok(latest) => {
                    next = now + EVERY;
                    let newer = release::newer(&latest, VERSION).then_some(latest);
                    tx.send_if_modified(|cur| {
                        if *cur == newer {
                            false
                        } else {
                            if let Some(v) = &newer {
                                diag::info("release", serde_json::json!({ "newer": v }));
                            }
                            *cur = newer;
                            true
                        }
                    });
                }
                Err(e) => {
                    next = now + RETRY;
                    diag::warn("release_check", serde_json::json!({ "error": e }));
                }
            }
            if tx.is_closed() {
                return;
            }
        }
    });
    (rx, poke)
}

/// The dashboard's update (`POST /hub/update`), as the hub stream tells it.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
struct UpdateState {
    running: bool,
    /// Why the last one failed: the end of its output.
    error: Option<String>,
}

/// `POST /hub/update`: `devstack update` in the background, as if typed in a
/// terminal. On success it stops this hub and starts the new one, so the
/// dashboard reconnects to the new version; running stacks keep theirs until
/// they restart. On failure the hub stream says why. Output goes to
/// `hub/update.log`.
async fn update(State(s): State<AppState>) -> Response {
    // Check and set in one step: a double click starts one updater.
    let claimed = s.update.send_if_modified(|u| {
        if u.running {
            false
        } else {
            *u = UpdateState {
                running: true,
                error: None,
            };
            true
        }
    });
    if !claimed {
        return err(StatusCode::CONFLICT, "an update is running");
    }
    let log = state_dir().join("update.log");
    let spawned = tokio::task::spawn_blocking({
        let log = log.clone();
        move || {
            let exe = std::env::current_exe().map_err(|_| "cannot find devstack".to_string())?;
            let out = std::fs::File::create(&log).map_err(|e| e.to_string())?;
            let out2 = out
                .try_clone()
                .map_err(|_| "cannot open the update log".to_string())?;
            let mut cmd = std::process::Command::new(exe);
            cmd.arg("update")
                .stdin(std::process::Stdio::null())
                .stdout(out)
                .stderr(out2);
            no_window(&mut cmd);
            cmd.spawn().map_err(|e| e.to_string())
        }
    })
    .await
    .unwrap_or_else(|e| Err(e.to_string()));
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => {
            s.update.send_replace(UpdateState {
                running: false,
                error: Some(e.clone()),
            });
            return err(StatusCode::INTERNAL_SERVER_ERROR, e);
        }
    };
    diag::info("update", serde_json::json!({ "started": true }));
    let tx = s.update.clone();
    std::thread::spawn(move || {
        let ok = child.wait().is_ok_and(|st| st.success());
        let error = (!ok).then(|| {
            let text = std::fs::read_to_string(&log).unwrap_or_default();
            let lines: Vec<&str> = text.lines().collect();
            lines[lines.len().saturating_sub(10)..].join("\n")
        });
        if let Some(e) = &error {
            diag::warn("update_failed", serde_json::json!({ "output": e }));
        }
        tx.send_replace(UpdateState {
            running: false,
            error,
        });
    });
    StatusCode::ACCEPTED.into_response()
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
    // A dashboard just connected, often after the machine woke up: a release
    // check from yesterday evening is not what it should show.
    let _ = s.release_poke.send(());
    let update = tokio_stream::wrappers::WatchStream::new(s.update.subscribe())
        .map(|u| serde_json::json!({ "type": "update", "update": u }));
    let stacks = tokio_stream::wrappers::WatchStream::new(s.stacks.clone())
        .map(|stacks| serde_json::json!({ "type": "stacks", "stacks": stacks }));
    let docker = tokio_stream::wrappers::WatchStream::new(s.engine.clone())
        .map(|engine| serde_json::json!({ "type": "docker", "docker": engine }));
    let release = tokio_stream::wrappers::WatchStream::new(s.newer.clone())
        .map(|newer| serde_json::json!({ "type": "release", "current": VERSION, "newer": newer }));
    stacks.merge(docker).merge(release).merge(update)
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
                    start_error: None,
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
///
/// Only local paths: a network path (`\\host\share`, `//host/share`, a
/// device path) would have the hub, as the user, connect to that host just to
/// look at it (on Windows over SMB or WebDAV, with the user's credentials).
fn stack_paths(dir: &str) -> Result<(PathBuf, PathBuf), String> {
    let text = dir.trim().trim_matches('"');
    let raw = PathBuf::from(text);
    if !raw.is_absolute() {
        return Err("an absolute path, e.g. C:\\src\\shop or /home/me/src/shop".into());
    }
    if !local_path(text, &raw) {
        return Err("a folder on this machine, not a network path".into());
    }
    let abs = std::path::absolute(&raw).map_err(|e| e.to_string())?;
    if !local_path(&abs.to_string_lossy(), &abs) {
        return Err("a folder on this machine, not a network path".into());
    }
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

/// A path on a local drive: on Windows one that starts with a drive letter
/// (`C:\`, or `\\?\C:\`), anywhere one that does not start with two slashes.
fn local_path(text: &str, path: &std::path::Path) -> bool {
    use std::path::{Component, Prefix};
    if cfg!(windows) {
        return matches!(
            path.components().next(),
            Some(Component::Prefix(p)) if matches!(p.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_))
        );
    }
    !(text.starts_with("//") || text.starts_with("\\\\"))
}

/// One folder dialog at a time.
static PICKING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Lets the next folder dialog open when dropped: when the dialog closes,
/// also if the page that asked went away in the meantime.
struct Picking;

impl Drop for Picking {
    fn drop(&mut self) {
        PICKING.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

/// `POST /hub/pick-folder`: the operating system's own folder dialog, shown by
/// the hub (a local program may, a web page may not learn a folder's path).
/// `{"path": "..."}`, `{"path": null}` when cancelled, `501` where there is no
/// dialog (a server without a desktop); the page then keeps its text field.
async fn pick_folder() -> Response {
    use std::sync::atomic::Ordering;
    if PICKING.swap(true, Ordering::SeqCst) {
        return err(StatusCode::CONFLICT, "a folder dialog is open already");
    }
    let picking = Picking;
    // The guard goes with the dialog: a request the browser gave up on drops
    // this future, but not the dialog's thread.
    let picked = tokio::task::spawn_blocking(move || {
        let _picking = picking;
        dialog::pick_folder()
    })
    .await;
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
    // What `up` says goes to a file: when it fails before the daemon runs
    // (a stack file that needs a newer devstack, a port taken), its last
    // lines are what the dashboard shows instead of "Starting…" (#11).
    s.start_errors.lock().unwrap().remove(&id);
    let log = state_dir().join(format!("up-{id}.log"));
    let (out, out2) = match std::fs::File::create(&log).and_then(|f| Ok((f.try_clone()?, f))) {
        Ok(f) => f,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("up")
        .arg("-f")
        .arg(&e.file)
        .current_dir(&e.repo)
        .env("NO_COLOR", "1")
        .stdin(std::process::Stdio::null())
        .stdout(out)
        .stderr(out2);
    no_window(&mut cmd);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let errors = s.start_errors.clone();
    let repo = e.repo.clone();
    std::thread::spawn(move || {
        if child.wait().is_ok_and(|st| st.success()) {
            return;
        }
        // `up` also fails when a service does; only a stack whose daemon
        // does not run is a failed start.
        let running = devstack_daemon::read_info(&repo.join(".devstack")).is_some_and(|i| {
            std::net::TcpStream::connect_timeout(
                &std::net::SocketAddr::from(([127, 0, 0, 1], i.port)),
                Duration::from_millis(300),
            )
            .is_ok()
        });
        if running {
            return;
        }
        let text = std::fs::read_to_string(&log).unwrap_or_default();
        let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
        let tail = lines[lines.len().saturating_sub(12)..].join("\n");
        let tail = if tail.is_empty() {
            "devstack up ended without a word".to_string()
        } else {
            tail
        };
        diag::warn(
            "stack_start_failed",
            serde_json::json!({ "id": id, "output": tail }),
        );
        errors.lock().unwrap().insert(id, tail);
    });
    StatusCode::ACCEPTED.into_response()
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

/// How long the hub waits for a daemon's port to take a connection.
const DAEMON_CONNECT: Duration = Duration::from_secs(5);
/// How long a call the hub makes on its own (`stack_down`) may take in all.
/// A forwarded request has no such limit: the browser decides, and the
/// forward ends when it goes away.
const DAEMON_CALL: Duration = Duration::from_secs(300);

async fn connect_daemon(port: u16) -> Result<tokio::net::TcpStream, String> {
    match tokio::time::timeout(
        DAEMON_CONNECT,
        tokio::net::TcpStream::connect(("127.0.0.1", port)),
    )
    .await
    {
        Ok(r) => r.map_err(|e| e.to_string()),
        Err(_) => Err(format!(
            "no connection within {} s",
            DAEMON_CONNECT.as_secs()
        )),
    }
}

async fn daemon_request(port: u16, token: &str, method: Method, path: &str) -> Result<u16, String> {
    tokio::time::timeout(DAEMON_CALL, daemon_request_once(port, token, method, path))
        .await
        .unwrap_or_else(|_| Err(format!("no answer within {} s", DAEMON_CALL.as_secs())))
}

async fn daemon_request_once(
    port: u16,
    token: &str,
    method: Method,
    path: &str,
) -> Result<u16, String> {
    let stream = connect_daemon(port).await?;
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
    strip_hop_by_hop(&mut parts.headers);
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
            let (mut parts, body) = resp.into_parts();
            // The hub's origin is the browser's; a daemon sets no cookie on it.
            parts.headers.remove(header::SET_COOKIE);
            strip_hop_by_hop(&mut parts.headers);
            Response::from_parts(parts, Body::new(body))
        }
        Err((status, msg)) => err(status, msg),
    }
}

/// Headers that belong to one connection, not to the message a proxy passes
/// on (RFC 9110 §7.6.1), the ones `Connection` names included.
fn strip_hop_by_hop(h: &mut HeaderMap) {
    let named: Vec<String> = h
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(|n| n.trim().to_ascii_lowercase())
        .filter(|n| !n.is_empty())
        .collect();
    for n in named {
        h.remove(n.as_str());
    }
    for n in [
        header::CONNECTION,
        header::PROXY_AUTHENTICATE,
        header::PROXY_AUTHORIZATION,
        header::TE,
        header::TRAILER,
        header::TRANSFER_ENCODING,
        header::UPGRADE,
    ] {
        h.remove(n);
    }
    h.remove("keep-alive");
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
    let stream = connect_daemon(port).await.map_err(|e| {
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
///
/// Only this user's hub counts (`who_answers`): the callers send it the
/// token next. Anything else on the port is an error, never a hub.
pub fn ensure_running() -> Result<u16, String> {
    let port = port();
    match who_answers(port) {
        Listener::Ours => return Ok(port),
        Listener::Foreign => return Err(foreign(port)),
        Listener::Nothing => {}
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    // What the hub prints before its own log exists (a failed start) goes to
    // hub.out, like the daemon's daemon.out.
    let _ = std::fs::create_dir_all(state_dir());
    let out = state_dir().join("hub.out");
    let log = std::fs::File::create(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("hub")
        // Not the caller's directory (a worktree, say): Windows refuses to
        // remove a directory a running process has as its own.
        .current_dir(state_dir())
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone().map_err(|e| e.to_string())?)
        .stderr(log);
    devstack_proc::spawn_detached(&mut cmd).map_err(|e| format!("start the hub: {e}"))?;
    // A hub that was just stopped can hold the state lock a few seconds more
    // (the new one waits up to 15 s for it), and a fresh binary may be read
    // by a virus scanner first.
    const WAIT: Duration = Duration::from_secs(30);
    let t0 = Instant::now();
    let mut seen = Listener::Nothing;
    while t0.elapsed() < WAIT {
        if answers(port) {
            seen = who_answers(port);
            if seen == Listener::Ours {
                return Ok(port);
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if seen == Listener::Foreign {
        return Err(foreign(port));
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

/// What listens on the hub's port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Listener {
    Nothing,
    /// This user's hub: it proved that it knows the token.
    Ours,
    /// Something else: another user's program, or anything that is no hub.
    /// It must not be sent the token.
    Foreign,
}

fn foreign(port: u16) -> String {
    format!(
        "port {port} answers, but not as your devstack hub; not sending it your token. \
         Stop what listens there, or give the hub another port with DEVSTACK_HUB_PORT"
    )
}

/// Who answers on `port`. On a machine several people use, another user may
/// take the port while this user's hub is down; a TCP answer alone proves
/// nothing. The hub proves it knows the token by hashing a fresh nonce with
/// it (`GET /hub/ping?nonce=`); the token never goes over the wire for that.
pub fn who_answers(port: u16) -> Listener {
    if !answers(port) {
        return Listener::Nothing;
    }
    let Some(token) = devstack_daemon::token::load().filter(|t| !t.is_empty()) else {
        return Listener::Foreign;
    };
    let nonce = random_hex();
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(3)))
        .http_status_as_error(false)
        .build()
        .new_agent();
    let answer: Option<serde_json::Value> = agent
        .get(&format!("http://127.0.0.1:{port}/hub/ping?nonce={nonce}"))
        .call()
        .ok()
        .filter(|r| r.status() == 200)
        .and_then(|mut r| r.body_mut().read_to_string().ok())
        .and_then(|t| serde_json::from_str(&t).ok());
    let Some(v) = answer else {
        return Listener::Foreign;
    };
    if let Some(proof) = v["proof"].as_str() {
        let want = ping_proof(&token, &nonce);
        let same = proof.len() == want.len()
            && proof
                .bytes()
                .zip(want.bytes())
                .fold(0u8, |a, (x, y)| a | (x ^ y))
                == 0;
        return if same {
            Listener::Ours
        } else {
            Listener::Foreign
        };
    }
    // A hub from before the proof (devstack 0.9.8 and older) answers without
    // one. It is this user's if this user's hub holds its state lock and
    // wrote this port into hub.json: both live in the user's own profile,
    // and a hub takes the lock before it binds the port.
    let legacy =
        v["devstack"] == true && read_info().is_some_and(|i| i.port == port) && hub_lock_held();
    if legacy {
        Listener::Ours
    } else {
        Listener::Foreign
    }
}

/// Whether a hub of this user holds the state lock now.
fn hub_lock_held() -> bool {
    let Ok(file) = std::fs::OpenOptions::new()
        .write(true)
        .open(state_dir().join("lock"))
    else {
        return false;
    };
    matches!(file.try_lock(), Err(std::fs::TryLockError::WouldBlock))
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

#[cfg(test)]
mod guard_tests {
    use super::*;

    #[test]
    fn network_paths_are_refused() {
        for p in [
            r"\\attacker.example\x",
            r"\\attacker.example@80\dav",
            "//attacker.example/x",
            r"\\?\UNC\attacker.example\x",
            r"\\.\pipe\x",
        ] {
            assert!(stack_paths(p).is_err(), "{p}");
        }
        let local = if cfg!(windows) {
            vec![r"C:\src\shop", r"\\?\C:\src\shop", "C:/src/shop"]
        } else {
            vec!["/home/me/src/shop"]
        };
        for p in local {
            assert!(stack_paths(p).is_ok(), "{p}");
        }
    }

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, HeaderValue::from_str(v).unwrap());
        }
        h
    }

    #[test]
    fn a_cookie_request_must_come_from_the_hubs_own_page() {
        let own = own_hosts(4250);
        let ok = |m: Method, path: &str, h: &[(&'static str, &str)]| {
            from_own_page(&headers(h), &m, path, &own)
        };
        let same = ("sec-fetch-site", "same-origin");
        let origin = ("origin", "http://127.0.0.1:4250");
        // The dashboard's own reads, and the user typing the address.
        assert!(ok(Method::GET, "/hub/stacks", &[same]));
        assert!(ok(
            Method::GET,
            "/hub/stacks",
            &[("sec-fetch-site", "none")]
        ));
        assert!(ok(Method::GET, "/hub/stacks", &[origin]));
        // Another loopback port is same-site, not same-origin (H-1).
        assert!(!ok(
            Method::GET,
            "/hub/inspect",
            &[("sec-fetch-site", "same-site")]
        ));
        assert!(!ok(
            Method::GET,
            "/hub/inspect",
            &[("sec-fetch-site", "cross-site"), origin]
        ));
        assert!(!ok(Method::GET, "/hub/inspect", &[]));
        // Mutations and the WebSocket need the own Origin.
        assert!(ok(Method::POST, "/hub/update", &[origin, same]));
        assert!(!ok(Method::POST, "/hub/update", &[same]));
        assert!(!ok(Method::GET, "/hub/ws", &[same]));
        assert!(ok(Method::GET, "/hub/ws", &[origin, same]));
        assert!(!ok(
            Method::POST,
            "/hub/update",
            &[("origin", "http://127.0.0.1:3000")]
        ));
    }

    #[test]
    fn every_session_cookie_is_tried() {
        let h = headers(&[("cookie", "devstack_hub=junk; other=1; devstack_hub=good")]);
        let all: Vec<&str> = cookie_sessions(&h).collect();
        assert_eq!(all, ["junk", "good"]);
        let h = headers(&[("cookie", "devstack_hubx=1")]);
        assert_eq!(cookie_sessions(&h).count(), 0);
    }

    #[test]
    fn the_ping_proof_needs_the_token() {
        let a = ping_proof("token-a", "0123456789abcdef");
        assert_eq!(a, ping_proof("token-a", "0123456789abcdef"));
        assert_ne!(a, ping_proof("token-b", "0123456789abcdef"));
        assert_ne!(a, ping_proof("token-a", "0123456789abcdee"));
    }

    #[test]
    fn a_deleted_sessions_file_unpairs_at_once() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join(SESSIONS_FILE);
        let mut s = Sessions::load(path.clone());
        s.add("cookie");
        assert!(s.contains("cookie"));
        std::fs::remove_file(&path).unwrap();
        assert!(!s.contains("cookie"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            s.add("again");
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn hop_by_hop_headers_are_dropped() {
        let mut h = headers(&[
            ("connection", "keep-alive, x-private"),
            ("x-private", "1"),
            ("keep-alive", "timeout=5"),
            ("transfer-encoding", "chunked"),
            ("content-type", "application/json"),
        ]);
        strip_hop_by_hop(&mut h);
        assert_eq!(h.len(), 1, "{h:?}");
        assert!(h.contains_key("content-type"));
    }
}
