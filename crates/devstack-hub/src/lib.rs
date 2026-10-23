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

mod branches;
mod dialog;
#[cfg(windows)]
mod foreground;
mod guests;
mod notice;
mod notify;
mod open;
mod pool;
mod prompts;
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
    /// A restart carries its lanes over (ADR-0052); `false` for an older
    /// daemon, which stops them.
    pub carries_lanes: bool,
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

/// A start or a stop the hub was asked for and that is not over yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// `devstack up` runs (`stack_up`).
    Starting,
    /// `stack_down`: the daemon stops its services, then exits.
    Stopping,
}

/// Starts and stops under way, by stack id: a start waits for a stop, and
/// a request for a stack in between is told it is starting or stopping.
type Phases = Arc<Mutex<HashMap<String, Phase>>>;

#[derive(Clone)]
struct AppState {
    port: u16,
    token: Arc<String>,
    /// blake3 of each session cookie -> created (ms); persisted, see `Sessions`.
    sessions: Arc<Mutex<Sessions>>,
    codes: Codes,
    stacks: watch::Receiver<Vec<StackEntry>>,
    /// The Docker engine as last seen, see `watch_engine`.
    engine: watch::Receiver<Engine>,
    /// What the release check last found (`release`).
    release: watch::Receiver<Release>,
    /// Asks the release check to look again if its last answer is older than
    /// an hour (a dashboard connected, see `watch_release`).
    release_poke: std::sync::mpsc::Sender<Poke>,
    /// `POST /hub/update`: running, or how the last one failed.
    update: watch::Sender<UpdateState>,
    /// Why a start from the dashboard failed, by stack id (#11).
    start_errors: StartErrors,
    phases: Phases,
    /// Until when (ms) to look at the engine every 2 s: after a start request.
    engine_fast_until: Arc<std::sync::atomic::AtomicU64>,
    shutdown: Arc<tokio::sync::Notify>,
    /// Desktop notifications (ADR-0045).
    notify: Arc<notify::Center>,
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
            // A `daemon.json` left by a killed daemon names a port another
            // process may have now; one that is stopping takes no requests.
            let daemon = devstack_daemon::read_info(&state).filter(|i| {
                !i.stopping
                    && devstack_proc::pid_alive(i.pid)
                    && std::net::TcpStream::connect_timeout(
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
                    carries_lanes: i.carries_lanes,
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

/// A stack whose daemon runs another devstack than the hub, once per stack
/// and release of its daemon (ADR-0052 point 5): how long stacks stay on an
/// old release shows in the pool's telemetry.
fn note_outdated(stacks: &[StackEntry], noted: &mut HashMap<String, String>) {
    for s in stacks {
        let Some(d) = &s.daemon else {
            continue;
        };
        if d.version == VERSION {
            noted.remove(&s.id);
            continue;
        }
        if noted.get(&s.id) != Some(&d.version) {
            noted.insert(s.id.clone(), d.version.clone());
            diag::info(
                "daemon_outdated",
                serde_json::json!({ "stack": s.name, "daemon": d.version, "hub": VERSION,
                    "daemon_age_s": (now_ms().saturating_sub(d.started) / 1000) as u64 }),
            );
        }
    }
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
        let (tx, rx) = watch::channel(Vec::new());
        let start_errors: StartErrors = Arc::default();
        let errors = start_errors.clone();
        tokio::task::spawn_blocking(move || {
            let (mut seen, mut outdated) = (HashMap::new(), HashMap::new());
            loop {
                let next = scan(&mut seen, &errors);
                note_outdated(&next, &mut outdated);
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
        let (release_rx, release_poke) = watch_release();

        let shutdown = Arc::new(tokio::sync::Notify::new());
        let codes: Codes = Arc::new(Mutex::new(HashMap::new()));
        let open: notify::Open = {
            let codes = codes.clone();
            Arc::new(move |t: &notify::Target| open_dashboard(port, &codes, &t.path()))
        };
        let notify = Arc::new(notify::Center::new(&dir, notify::platform_sink(port, open)));
        {
            let notify = notify.clone();
            guests::on_started(move |g| {
                notify.guest_started(g);
            });
        }
        let state = AppState {
            port,
            token: Arc::new(token),
            sessions: Arc::new(Mutex::new(Sessions::load(dir.join(SESSIONS_FILE)))),
            codes,
            stacks: rx,
            engine: engine_rx,
            release: release_rx,
            release_poke,
            update: initial_update_state(&dir),
            start_errors,
            phases: Phases::default(),
            engine_fast_until,
            shutdown: shutdown.clone(),
            notify,
        };
        // The port last: what accepts a connection answers it. Bound before
        // the setup above (a first `docker version` alone may take 5 s), the
        // port took connections it did not answer, and the CLI, which asks
        // `/hub/ping` for 3 s, took this hub for another user's program.
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
        notify::start(state.clone());
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
        .route("/hub/update/check", post(update_check))
        .route("/hub/notify", get(notify::get).put(notify::put))
        .route("/hub/notify/test", post(notify::test))
        .route("/hub/inspect", get(inspect))
        .route("/hub/pick-folder", post(pick_folder))
        .route("/hub/openers", get(openers))
        .route("/hub/open", post(open_tree))
        .route("/hub/prompt", get(prompt))
        .route("/hub/agent", post(agent))
        .route("/hub/stacks/add", post(stack_add))
        .route("/hub/stacks/init", post(stack_init))
        .route("/hub/stacks/scan", post(stack_scan))
        .route("/hub/stacks/{id}/up", post(stack_up))
        .route("/hub/stacks/{id}/down", post(stack_down))
        .route("/hub/stacks/{id}", delete(forget))
        .route("/hub/stacks/{id}/branches", get(stack_branches))
        .route("/hub/stacks/{id}/worktrees", post(worktree_add))
        .route("/hub/stacks/{id}/worktrees/remove", post(worktree_remove))
        .route("/hub/stacks/{id}/fetch", post(stack_fetch))
        .route(
            "/hub/stacks/{id}/skills",
            get(stack_skills).post(stack_skills_install),
        )
        .route("/hub/stacks/{id}/api/{*rest}", any(forward))
        .route("/hub/pool", get(pool::overview))
        .route("/hub/pool/share", get(share::get))
        .route("/hub/pool/shared", get(shared::list))
        .route("/hub/pool/shared/{lease}/open", post(shared::open))
        .route("/hub/pool/shared/{lease}/connect", post(shared::connect))
        .route(
            "/hub/pool/shared/{lease}/disconnect",
            post(shared::disconnect),
        )
        .route("/hub/pool/shared/{lease}/api/{*rest}", get(shared::view))
        .route("/hub/pool/members/{id}/approve", post(pool::approve))
        .route("/hub/pool/members/{id}/remove", post(pool::remove))
        .route("/hub/pool/members/{id}/role", post(pool::role))
        .route("/hub/pool/nodes/{id}/pause", post(pool::pause))
        .route("/hub/pool/nodes/{id}/resume", post(pool::resume))
        .route("/hub/pool/leases/{id}/end", post(pool::end))
        .route(
            "/hub/pool/nodes/{id}/update",
            post(pool::update).delete(pool::update_cancel),
        )
        .route("/hub/pool/nodes/{id}/prune", post(pool::prune))
        .route("/hub/pool/members/{id}/tags", post(pool::tags))
        .route("/hub/pool/members/{id}/node", post(pool::node))
        .route("/hub/pools", get(pool::pools))
        .route("/hub/pool/history", get(pool::history))
        .route("/hub/pool/placement", get(pool::placement))
        .route(
            "/hub/pool/default-on",
            get(pool::default_on).put(pool::set_default_on),
        )
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
    let mut fields =
        serde_json::json!({ "method": method.as_str(), "path": path, "status": status });
    // Why it was refused: a 409 alone explains nothing afterwards.
    let res = if status >= 400 {
        let (res, error) = devstack_daemon::api::error_of(res).await;
        if let Some(e) = error {
            fields["error"] = e.into();
        }
        res
    } else {
        res
    };
    diag::log(
        if status >= 400 { "warn" } else { "info" },
        "request",
        fields,
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
    /// Where to go once paired: a dashboard route (`/#/s/<id>?lane=`), the
    /// stack a notification is about (ADR-0045). Anything else goes home.
    next: Option<String>,
}

/// A dashboard route `/auth` may send the browser on to: the hub's own
/// page with a hash route, in characters of a path and a query only.
fn dashboard_route(next: Option<&str>) -> &str {
    match next {
        Some(n)
            if n.starts_with("/#/")
                && n.len() <= 512
                && n.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"/#?=&%-._~".contains(&b)) =>
        {
            n
        }
        _ => "/",
    }
}

/// Exchange a one-time code (60 s, single use) for the session cookie. A
/// browser that is paired already keeps its session: a link from a
/// notification pairs again each time, and need not add one each time.
async fn auth(
    State(s): State<AppState>,
    Query(q): Query<CodeQuery>,
    headers: HeaderMap,
) -> Response {
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
    let next = dashboard_route(q.next.as_deref()).to_string();
    let paired = {
        let mut sessions = s.sessions.lock().unwrap();
        cookie_sessions(&headers).any(|c| sessions.contains(c))
    };
    if paired {
        return (StatusCode::SEE_OTHER, [(header::LOCATION, next)]).into_response();
    }
    let id = random_hex();
    s.sessions.lock().unwrap().add(&id);
    let cookie = format!("{COOKIE}={id}; HttpOnly; SameSite=Strict; Path=/; Max-Age=31536000");
    (
        StatusCode::SEE_OTHER,
        [(header::SET_COOKIE, cookie), (header::LOCATION, next)],
    )
        .into_response()
}

/// How long a pairing code works.
const CODE_LIFETIME: Duration = Duration::from_secs(60);

/// One-time pairing codes -> when they were made.
type Codes = Arc<Mutex<HashMap<String, Instant>>>;

fn new_code(codes: &Codes) -> String {
    let code = random_hex();
    let mut codes = codes.lock().unwrap();
    // Codes nobody used are dropped once expired, not kept for the hub's life.
    codes.retain(|_, t| t.elapsed() < CODE_LIFETIME);
    codes.insert(code.clone(), Instant::now());
    code
}

async fn pair(State(s): State<AppState>) -> Response {
    let code = new_code(&s.codes);
    let url = format!("http://127.0.0.1:{}/auth?code={code}", s.port);
    Json(serde_json::json!({ "code": code, "url": url })).into_response()
}

/// Open the dashboard at `route` in the default browser, paired: what a
/// click on a notification the hub hears itself does (Linux, ADR-0045).
/// `Err` with why it could not, for the notifications' `trouble`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn open_dashboard(port: u16, codes: &Codes, route: &str) -> Result<(), String> {
    let code = new_code(codes);
    let url = format!(
        "http://127.0.0.1:{port}/auth?code={code}&next={}",
        next_param(route)
    );
    #[cfg(target_os = "linux")]
    if let Err(e) = std::process::Command::new("xdg-open")
        .arg(&url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        diag::warn(
            "open_dashboard_failed",
            serde_json::json!({ "error": e.to_string() }),
        );
        return Err(format!(
            "A click on a notification could not open the dashboard: xdg-open did not start ({e})."
        ));
    }
    #[cfg(not(target_os = "linux"))]
    let _ = url;
    Ok(())
}

/// `route` as the value of `/auth`'s `next`.
pub fn next_param(route: &str) -> String {
    route
        .bytes()
        .map(|b| match b {
            b'#' | b'?' | b'&' | b'=' | b'%' | b'+' => format!("%{b:02X}"),
            _ => (b as char).to_string(),
        })
        .collect()
}

/// Remove what the hub registered with the system for its notifications
/// (`devstack self uninstall`).
pub fn unregister_notifications() {
    notify::unregister();
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
/// publish changes. One `docker version` per look; nothing when it did not
/// change. A running engine counts as gone only when two looks in a row say
/// so: one `docker version` that fails on a busy machine is no stopped
/// engine (the dashboard would say Docker is not running, or not installed).
fn watch_engine(fast_until: Arc<std::sync::atomic::AtomicU64>) -> watch::Receiver<Engine> {
    let (tx, rx) = watch::channel(engine::status("docker"));
    std::thread::spawn(move || {
        let mut doubt: Option<Engine> = None;
        loop {
            let fast = now_ms() < u128::from(fast_until.load(std::sync::atomic::Ordering::Relaxed));
            let step = if fast { 2 } else { 10 };
            std::thread::sleep(Duration::from_secs(step));
            if tx.is_closed() {
                return;
            }
            let t0 = Instant::now();
            let (next, why) = engine::look("docker");
            let ms = t0.elapsed().as_millis() as u64;
            let fields = |e: &Engine| {
                let mut v = serde_json::to_value(e).unwrap_or_default();
                v["ms"] = ms.into();
                if let Some(w) = &why {
                    v["error"] = w.as_str().into();
                }
                v
            };
            if tx.borrow().is_running() && !next.is_running() && doubt.as_ref() != Some(&next) {
                diag::info("docker_look_failed", fields(&next));
                doubt = Some(next);
                continue;
            }
            doubt = None;
            tx.send_if_modified(|cur| {
                if *cur == next {
                    false
                } else {
                    diag::log(
                        if next.is_running() { "info" } else { "warn" },
                        "docker",
                        fields(&next),
                    );
                    *cur = next;
                    true
                }
            });
        }
    });
    rx
}

/// What the release check last found (`watch_release`), as the `release`
/// event tells it.
#[derive(Debug, Clone, Default, PartialEq)]
struct Release {
    /// A newer release than this hub, if a check found one.
    newer: Option<String>,
    /// Why the last check failed (no network, GitHub's rate limit); gone
    /// with the next one that does not. A newer release found before stays.
    error: Option<String>,
}

impl Release {
    /// Take a check's answer: the newer release, if any, or why there is no
    /// answer. Whether anything changed.
    fn answer(&mut self, got: Result<Option<String>, String>) -> bool {
        let next = match got {
            Ok(newer) => Release { newer, error: None },
            Err(e) => Release {
                newer: self.newer.clone(),
                error: Some(e),
            },
        };
        let changed = next != *self;
        *self = next;
        changed
    }
}

/// What the release watcher is asked (`watch_release`).
enum Poke {
    /// A dashboard connected: look again if the last answer is over an hour old.
    Stale,
    /// "Check for updates": look now and answer with the newer release, if any.
    Now(std::sync::mpsc::Sender<Result<Option<String>, String>>),
}

/// Look for a newer release at start, then every 12 hours by the wall clock,
/// and when a dashboard connects and the last answer is older than an hour
/// (the `Sender` pokes), or at once when asked (`POST /hub/update/check`).
/// Not with a 12-hour sleep: on Windows a sleeping thread's time stands still
/// while the machine is in standby, so a hub from last night would not look
/// again until well into the day. A failed check is tried again after an
/// hour, and its error is told until one is not. `DEVSTACK_NO_UPDATE_CHECK`
/// turns it off.
fn watch_release() -> (watch::Receiver<Release>, std::sync::mpsc::Sender<Poke>) {
    use std::sync::mpsc::RecvTimeoutError;
    use std::time::SystemTime;
    const EVERY: Duration = Duration::from_secs(12 * 3600);
    const RETRY: Duration = Duration::from_secs(3600);
    const FRESH: Duration = Duration::from_secs(3600);
    // GitHub answers 60 unauthenticated requests an hour: a button clicked
    // again and again gets the last answer.
    const AGAIN: Duration = Duration::from_secs(30);
    let (tx, rx) = watch::channel(Release::default());
    let (poke, pokes) = std::sync::mpsc::channel::<Poke>();
    if release::check_disabled() {
        return (rx, poke);
    }
    std::thread::spawn(move || {
        let mut last: Option<SystemTime> = None;
        let mut answer: Option<Result<Option<String>, String>> = None;
        let mut next = SystemTime::now();
        loop {
            // The first look at once, then once a minute or when poked.
            let wait = if last.is_none() {
                Duration::ZERO
            } else {
                Duration::from_secs(60)
            };
            let (poked, reply) = match pokes.recv_timeout(wait) {
                Ok(Poke::Stale) => (true, None),
                Ok(Poke::Now(reply)) => (true, Some(reply)),
                Err(RecvTimeoutError::Timeout) => (false, None),
                Err(RecvTimeoutError::Disconnected) => return,
            };
            let now = SystemTime::now();
            let older = |t: Option<SystemTime>, than: Duration| {
                t.is_none_or(|t| now.duration_since(t).unwrap_or(than) >= than)
            };
            let due = now >= next
                || (poked && older(last, FRESH))
                || (reply.is_some() && older(last, AGAIN));
            if !due {
                if let (Some(reply), Some(a)) = (reply, &answer) {
                    let _ = reply.send(a.clone());
                }
                continue;
            }
            last = Some(now);
            let got = match release::latest() {
                Ok(latest) => {
                    next = now + EVERY;
                    Ok(release::newer(&latest, VERSION).then_some(latest))
                }
                Err(e) => {
                    next = now + RETRY;
                    diag::warn("release_check", serde_json::json!({ "error": e }));
                    Err(e)
                }
            };
            tx.send_if_modified(|cur| {
                let was = cur.newer.clone();
                let changed = cur.answer(got.clone());
                if cur.newer != was
                    && let Some(v) = &cur.newer
                {
                    diag::info("release", serde_json::json!({ "newer": v }));
                }
                changed
            });
            if let Some(reply) = reply {
                let _ = reply.send(got.clone());
            }
            answer = Some(got);
            if tx.is_closed() {
                return;
            }
        }
    });
    (rx, poke)
}

/// `POST /hub/update/check`: look for a newer release now. `{"current",
/// "newer"}` (`newer` is `null` when this is the latest); the hub stream's
/// `release` event says the same. Asked again within 30 s, the last answer.
async fn update_check(State(s): State<AppState>) -> Response {
    if release::check_disabled() {
        return err(
            StatusCode::CONFLICT,
            "update checks are off on this machine (DEVSTACK_NO_UPDATE_CHECK)",
        );
    }
    let (reply, answer) = std::sync::mpsc::channel();
    if s.release_poke.send(Poke::Now(reply)).is_err() {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "the release check is not running",
        );
    }
    let got = tokio::task::spawn_blocking(move || answer.recv_timeout(Duration::from_secs(30)))
        .await
        .ok()
        .and_then(Result::ok);
    match got {
        Some(Ok(newer)) => {
            Json(serde_json::json!({ "current": VERSION, "newer": newer })).into_response()
        }
        Some(Err(e)) => err(
            StatusCode::BAD_GATEWAY,
            format!("could not ask GitHub: {e}"),
        ),
        None => err(
            StatusCode::GATEWAY_TIMEOUT,
            "GitHub did not answer within 30 s",
        ),
    }
}

/// The dashboard's update (`POST /hub/update`), as the hub stream tells it.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
struct UpdateState {
    running: bool,
    /// Why the last one failed: the end of its output.
    error: Option<String>,
    /// The version the last update installed. A successful update stops the
    /// hub that started it; the new hub says so (see `PendingUpdate`).
    installed: Option<String>,
    /// When the last update ended (ms since the epoch), so a dashboard that
    /// connects later can tell a fresh outcome from an old one.
    finished: Option<u64>,
}

impl UpdateState {
    fn ended(error: Option<String>, installed: Option<String>) -> Self {
        Self {
            running: false,
            error,
            installed,
            finished: Some(u64::try_from(now_ms()).unwrap_or(u64::MAX)),
        }
    }
}

/// `hub/update.json`: an update the dashboard started. A successful update
/// stops the hub that started it, so that hub never learns the outcome; the
/// hub that starts next reads this file and reports it.
#[derive(Debug, Serialize, Deserialize)]
struct PendingUpdate {
    /// The version of the hub that started it.
    from: String,
    /// The updater (`devstack update`).
    pid: u32,
    /// When it started (ms since the epoch).
    started: u64,
}

const PENDING_UPDATE: &str = "update.json";

/// An update file older than this is a leftover, not an update.
const PENDING_UPDATE_MAX_AGE_MS: u64 = 3600 * 1000;

/// The last lines of the update log: why an update failed.
fn update_log_tail(log: &std::path::Path) -> String {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(10)..].join("\n")
}

/// The outcome of an update a previous hub started, if there is one: read
/// (and removed) at start. `alive` tells whether the updater still runs; if
/// it does, its pid comes back with a `running` state and the caller watches
/// it (and removes the file once it ends).
fn pending_update(
    dir: &std::path::Path,
    version: &str,
    now: u64,
    alive: impl Fn(u32) -> bool,
) -> Option<(UpdateState, Option<u32>)> {
    let path = dir.join(PENDING_UPDATE);
    let text = std::fs::read_to_string(&path).ok()?;
    let p = serde_json::from_str::<PendingUpdate>(&text)
        .ok()
        .filter(|p| now.saturating_sub(p.started) < PENDING_UPDATE_MAX_AGE_MS);
    let Some(p) = p else {
        let _ = std::fs::remove_file(&path);
        return None;
    };
    if p.from != version {
        let _ = std::fs::remove_file(&path);
        return Some((UpdateState::ended(None, Some(version.into())), None));
    }
    if alive(p.pid) {
        // Still at work: this hub was started meanwhile, by the old binary.
        let state = UpdateState {
            running: true,
            ..UpdateState::default()
        };
        return Some((state, Some(p.pid)));
    }
    let _ = std::fs::remove_file(&path);
    Some((update_failed(&dir.join("update.log")), None))
}

/// An update ended and this hub still runs the version it started from.
fn update_failed(log: &std::path::Path) -> UpdateState {
    let tail = update_log_tail(log);
    let error = if tail.trim().is_empty() {
        "the update ended without installing a new version".to_string()
    } else {
        tail
    };
    UpdateState::ended(Some(error), None)
}

/// The update state a starting hub reports: idle, or the outcome of the
/// update the hub before it started (see `PendingUpdate`).
fn initial_update_state(dir: &std::path::Path) -> watch::Sender<UpdateState> {
    let now = u64::try_from(now_ms()).unwrap_or(u64::MAX);
    let Some((state, running)) = pending_update(dir, VERSION, now, devstack_proc::pid_alive) else {
        return watch::channel(UpdateState::default()).0;
    };
    if state.installed.is_some() {
        diag::info("update_done", serde_json::json!({ "installed": VERSION }));
    }
    let tx = watch::channel(state).0;
    if let Some(pid) = running {
        let tx = tx.clone();
        let dir = dir.to_path_buf();
        std::thread::spawn(move || {
            while devstack_proc::pid_alive(pid) {
                std::thread::sleep(Duration::from_millis(500));
            }
            // A successful update would have stopped this hub.
            let _ = std::fs::remove_file(dir.join(PENDING_UPDATE));
            tx.send_replace(update_failed(&dir.join("update.log")));
        });
    }
    tx
}

/// `POST /hub/update`: `devstack update` in the background, as if typed in a
/// terminal. On success it stops this hub and starts the new one, so the
/// dashboard reconnects to the new version, and the new hub reports what was
/// installed (`hub/update.json`); running stacks keep theirs until they
/// restart. On failure the hub stream says why. Output goes to
/// `hub/update.log`.
async fn update(State(s): State<AppState>) -> Response {
    // Check and set in one step: a double click starts one updater.
    let claimed = s.update.send_if_modified(|u| {
        if u.running {
            false
        } else {
            *u = UpdateState {
                running: true,
                ..UpdateState::default()
            };
            true
        }
    });
    if !claimed {
        return err(StatusCode::CONFLICT, "an update is running");
    }
    let dir = state_dir();
    let log = dir.join("update.log");
    let spawned = tokio::task::spawn_blocking({
        let log = log.clone();
        let dir = dir.clone();
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
            let child = cmd.spawn().map_err(|e| e.to_string())?;
            let pending = PendingUpdate {
                from: VERSION.into(),
                pid: child.id(),
                started: u64::try_from(now_ms()).unwrap_or(u64::MAX),
            };
            if let Err(e) = std::fs::write(
                dir.join(PENDING_UPDATE),
                serde_json::to_vec(&pending).unwrap_or_default(),
            ) {
                // The update still runs; only its success goes unreported.
                diag::warn("update_file", serde_json::json!({ "error": e.to_string() }));
            }
            Ok::<_, String>(child)
        }
    })
    .await
    .unwrap_or_else(|e| Err(e.to_string()));
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => {
            s.update
                .send_replace(UpdateState::ended(Some(e.clone()), None));
            return err(StatusCode::INTERNAL_SERVER_ERROR, e);
        }
    };
    diag::info("update", serde_json::json!({ "started": true }));
    let tx = s.update.clone();
    std::thread::spawn(move || {
        let ok = child.wait().is_ok_and(|st| st.success());
        // Still here: the update did not stop this hub (it failed, or there
        // was nothing to install), so this hub reports it, not the next one.
        let _ = std::fs::remove_file(dir.join(PENDING_UPDATE));
        let error = (!ok).then(|| update_log_tail(&log));
        if let Some(e) = &error {
            diag::warn("update_failed", serde_json::json!({ "output": e }));
        }
        tx.send_replace(UpdateState::ended(error, None));
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
    let _ = s.release_poke.send(Poke::Stale);
    let update = tokio_stream::wrappers::WatchStream::new(s.update.subscribe())
        .map(|u| serde_json::json!({ "type": "update", "update": u }));
    let stacks = tokio_stream::wrappers::WatchStream::new(s.stacks.clone())
        .map(|stacks| serde_json::json!({ "type": "stacks", "stacks": stacks }));
    let docker = tokio_stream::wrappers::WatchStream::new(s.engine.clone())
        .map(|engine| serde_json::json!({ "type": "docker", "docker": engine }));
    let release = tokio_stream::wrappers::WatchStream::new(s.release.clone()).map(|r| {
        serde_json::json!({ "type": "release", "current": VERSION, "newer": r.newer, "error": r.error })
    });
    let guests = tokio_stream::wrappers::WatchStream::new(guests::subscribe())
        .map(|guests| serde_json::json!({ "type": "guests", "guests": guests }));
    // A dashboard that fell behind misses a toast, nothing it shows.
    let notices =
        tokio_stream::wrappers::BroadcastStream::new(notice::subscribe()).filter_map(|n| n.ok());
    stacks
        .merge(docker)
        .merge(release)
        .merge(update)
        .merge(guests)
        .merge(notices)
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

/// `GET /hub/openers`: what this machine can open a stack's tree with
/// (ADR-0046), `[{id, label, kind}]`; empty without a desktop.
async fn openers() -> Response {
    match tokio::task::spawn_blocking(open::found).await {
        Ok(found) => Json(found.into_iter().map(|f| f.opener).collect::<Vec<_>>()).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// Run `f` on the repository of stack `id`, git being slow: its answer as
/// JSON, `400` for what the page got wrong, `409` with what git said.
async fn with_repo<T: serde::Serialize + Send + 'static>(
    s: &AppState,
    id: &str,
    f: impl FnOnce(&std::path::Path) -> Result<T, branches::Error> + Send + 'static,
) -> Response {
    let Some(e) = entry(s, id) else {
        return err(StatusCode::NOT_FOUND, "no such stack");
    };
    match tokio::task::spawn_blocking(move || f(&e.repo)).await {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(branches::Error::Bad(m))) => err(StatusCode::BAD_REQUEST, m),
        Ok(Err(branches::Error::Git(m))) => {
            diag::warn("git_failed", serde_json::json!({ "error": m }));
            err(StatusCode::CONFLICT, m)
        }
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// `GET /hub/stacks/{id}/branches` (ADR-0047).
async fn stack_branches(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    with_repo(&s, &id, branches::list).await
}

#[derive(Deserialize)]
struct WorktreeAdd {
    branch: String,
    #[serde(default)]
    new: Option<String>,
}

/// `POST /hub/stacks/{id}/worktrees {branch, new?}`: a lane on a branch the
/// hub lists, or on a new one from it (ADR-0047): `{path}`.
async fn worktree_add(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(b): Json<WorktreeAdd>,
) -> Response {
    with_repo(&s, &id, move |repo| {
        branches::add(repo, &b.branch, b.new.as_deref()).map(|p| serde_json::json!({ "path": p }))
    })
    .await
}

#[derive(Deserialize)]
struct WorktreeRemove {
    path: String,
}

/// `POST /hub/stacks/{id}/worktrees/remove {path}`: `git worktree remove`
/// of one of its linked worktrees, which git refuses with changes in it.
async fn worktree_remove(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(b): Json<WorktreeRemove>,
) -> Response {
    let raw = PathBuf::from(&b.path);
    if !raw.is_absolute() || !local_path(&b.path, &raw) {
        return err(StatusCode::BAD_REQUEST, "a folder on this machine");
    }
    with_repo(&s, &id, move |repo| {
        branches::remove(repo, &raw).map(|()| serde_json::json!({}))
    })
    .await
}

/// `POST /hub/stacks/{id}/fetch`: `git fetch --all --prune`, no prompt.
async fn stack_fetch(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    with_repo(&s, &id, |repo| {
        branches::fetch(repo).map(|()| serde_json::json!({}))
    })
    .await
}

/// The agent skills `devstack skills install` wrote into the stack's
/// repository, held against what this devstack would write: the version they
/// name is the one the repository was last set up with.
fn skills_view(e: &StackEntry) -> serde_json::Value {
    let i = devstack_skills::installed(&e.file);
    let current = devstack_config::VERSION;
    // Written by a newer devstack (a teammate's): installing would go back.
    let ahead = i.from.as_deref().is_some_and(|f| {
        devstack_config::version_order(f) > devstack_config::version_order(current)
    });
    serde_json::json!({
        "found": i.found,
        "outdated": i.outdated,
        "from": i.from,
        "current": current,
        "ahead": ahead,
    })
}

/// `GET /hub/stacks/{id}/skills`.
async fn stack_skills(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    let Some(e) = entry(&s, &id) else {
        return err(StatusCode::NOT_FOUND, "no such stack");
    };
    match tokio::task::spawn_blocking(move || skills_view(&e)).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// `POST /hub/stacks/{id}/skills`: `devstack skills install` for the stack's
/// repository (a skill the project wrote itself is kept), then the new view.
async fn stack_skills_install(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    let Some(e) = entry(&s, &id) else {
        return err(StatusCode::NOT_FOUND, "no such stack");
    };
    let done = tokio::task::spawn_blocking(move || {
        let stack = devstack_config::load(&e.file).map_err(|err| err.to_string())?;
        let out = devstack_skills::install(&stack, &e.file, None, false)
            .map_err(|err| format!("{err:#}"))?;
        let kept: Vec<_> = out
            .iter()
            .filter_map(|o| match o {
                devstack_skills::Outcome::Kept(p) => Some(p.clone()),
                _ => None,
            })
            .collect();
        diag::info(
            "skills_install",
            serde_json::json!({ "repo": e.repo, "kept": kept }),
        );
        let mut view = skills_view(&e);
        view["kept"] = serde_json::json!(kept);
        Ok::<_, String>(view)
    })
    .await;
    match done {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(m)) => err(StatusCode::CONFLICT, m),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[derive(Deserialize)]
struct OpenQuery {
    stack: String,
    path: String,
    with: String,
}

/// `POST /hub/open {stack, path, with}`: open one of the stack's trees (its
/// repository or a worktree of it) with a program `GET /hub/openers` lists.
/// Anything else is refused: the page names a tree and a program, never a
/// command (ADR-0046).
async fn open_tree(State(s): State<AppState>, Json(q): Json<OpenQuery>) -> Response {
    let Some(e) = entry(&s, &q.stack) else {
        return err(StatusCode::NOT_FOUND, "no such stack");
    };
    let opened = tokio::task::spawn_blocking(move || {
        let raw = PathBuf::from(&q.path);
        if !raw.is_absolute() || !local_path(&q.path, &raw) {
            return Err((
                StatusCode::BAD_REQUEST,
                "a folder on this machine".to_string(),
            ));
        }
        let Some(tree) = devstack_daemon::lanes::worktree_of(&e.repo, &raw) else {
            return Err((
                StatusCode::BAD_REQUEST,
                "not a worktree of this stack".to_string(),
            ));
        };
        let file = e
            .file
            .strip_prefix(&e.repo)
            .map(|rel| tree.join(rel))
            .unwrap_or_else(|_| e.file.clone());
        open::open(&q.with, &tree, &e.repo, &file).map_err(|e| match e {
            open::OpenError::Unknown | open::OpenError::NoPrompt => (
                StatusCode::BAD_REQUEST,
                format!(
                    "{} is not a program this machine opens folders with",
                    q.with
                ),
            ),
            open::OpenError::Failed(why) => (StatusCode::BAD_GATEWAY, why),
        })
    })
    .await;
    match opened {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err((status, msg))) => {
            diag::warn("open_failed", serde_json::json!({ "error": msg }));
            err(status, msg)
        }
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

// ------------------------------------------------- a prompt for an agent
//
// The page names a prompt and its target; the hub writes it (ADR-0046,
// amendment 2026-10-22). Copied by the user, or given to an agent the hub
// starts in the target's folder.

#[derive(Deserialize)]
struct PromptQuery {
    kind: String,
    /// The stack a prompt is about (`upgrade`, `start-failed`).
    #[serde(default)]
    stack: Option<String>,
    /// The folder a prompt is about (`setup`, `fix-file`), before it is a stack.
    #[serde(default)]
    path: Option<String>,
}

/// A prompt written, and where its agent works: the repository and its stack
/// file.
struct Written {
    text: String,
    repo: PathBuf,
    file: PathBuf,
}

type Refusal = (StatusCode, String);

fn write_prompt(s: &AppState, q: &PromptQuery) -> Result<Written, Refusal> {
    use prompts::Prompt;
    let bad = |m: String| (StatusCode::BAD_REQUEST, m);
    let folder = || -> Result<(PathBuf, PathBuf), Refusal> {
        let p = q
            .path
            .as_deref()
            .ok_or_else(|| bad(format!("`{}` needs `path`", q.kind)))?;
        let (repo, file) = stack_paths(p).map_err(bad)?;
        if !repo.is_dir() {
            return Err(bad(format!("{} is not a folder", repo.display())));
        }
        Ok((repo, file))
    };
    let stack = || -> Result<StackEntry, Refusal> {
        let id = q
            .stack
            .as_deref()
            .ok_or_else(|| bad(format!("`{}` needs `stack`", q.kind)))?;
        entry(s, id).ok_or_else(|| (StatusCode::NOT_FOUND, "no such stack".to_string()))
    };
    let (text, repo, file) = match q.kind.as_str() {
        "setup" => {
            let (repo, file) = folder()?;
            let drafted = file.is_file();
            (
                prompts::text(&Prompt::Setup {
                    repo: &repo,
                    drafted,
                }),
                repo,
                file,
            )
        }
        "fix-file" => {
            let (repo, file) = folder()?;
            if !file.is_file() {
                return Err((
                    StatusCode::CONFLICT,
                    format!("no devstack.toml in {}", repo.display()),
                ));
            }
            let error = match devstack_config::load(&file) {
                Ok(_) => return Err((StatusCode::CONFLICT, "the stack file loads".to_string())),
                Err(e) => e.to_string(),
            };
            let text = prompts::text(&Prompt::FixFile {
                repo: &repo,
                file: &file,
                error: &error,
            });
            (text, repo, file)
        }
        "upgrade" => {
            let e = stack()?;
            let i = devstack_skills::installed(&e.file);
            let text = prompts::text(&Prompt::Upgrade {
                repo: &e.repo,
                file: &e.file,
                from: i.from.as_deref(),
                current: devstack_config::VERSION,
            });
            (text, e.repo, e.file)
        }
        "start-failed" => {
            let e = stack()?;
            let Some(output) = s.start_errors.lock().unwrap().get(&e.id).cloned() else {
                return Err((
                    StatusCode::CONFLICT,
                    "its last start did not fail".to_string(),
                ));
            };
            let log = state_dir().join(format!("up-{}.log", e.id));
            let text = prompts::text(&Prompt::StartFailed {
                name: &e.name,
                repo: &e.repo,
                file: &e.file,
                output: &output,
                log: &log,
            });
            (text, e.repo, e.file)
        }
        other => return Err(bad(format!("no prompt `{other}`"))),
    };
    Ok(Written { text, repo, file })
}

/// `GET /hub/prompt?kind=&stack=|path=`: the prompt, to show and copy.
async fn prompt(State(s): State<AppState>, Query(q): Query<PromptQuery>) -> Response {
    match tokio::task::spawn_blocking(move || write_prompt(&s, &q)).await {
        Ok(Ok(w)) => Json(serde_json::json!({ "text": w.text, "path": w.repo })).into_response(),
        Ok(Err((status, msg))) => err(status, msg),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[derive(Deserialize)]
struct AgentQuery {
    #[serde(flatten)]
    prompt: PromptQuery,
    with: String,
}

/// `POST /hub/agent {kind, stack | path, with}`: start the agent `with` in
/// the target's folder with the prompt (filled in, or, Orca, sent).
async fn agent(State(s): State<AppState>, Json(q): Json<AgentQuery>) -> Response {
    let started = tokio::task::spawn_blocking(move || {
        let w = write_prompt(&s, &q.prompt)?;
        open::agent(&q.with, &w.repo, &w.repo, &w.file, &w.text).map_err(|e| match e {
            open::OpenError::Unknown => (
                StatusCode::BAD_REQUEST,
                format!("{} is not a program this machine has", q.with),
            ),
            open::OpenError::NoPrompt => (
                StatusCode::BAD_REQUEST,
                format!("{} is not started with a prompt", q.with),
            ),
            open::OpenError::Failed(why) => (StatusCode::BAD_GATEWAY, why),
        })?;
        diag::info(
            "agent_started",
            serde_json::json!({ "with": q.with, "kind": q.prompt.kind, "repo": w.repo }),
        );
        Ok::<_, Refusal>(())
    })
    .await;
    match started {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err((status, msg))) => {
            diag::warn("agent_failed", serde_json::json!({ "error": msg }));
            err(status, msg)
        }
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
    s.start_errors.lock().unwrap().remove(&id);
    let stopping = s.phases.lock().unwrap().get(&id) == Some(&Phase::Stopping);
    if !stopping {
        s.phases.lock().unwrap().insert(id.clone(), Phase::Starting);
        return match start_up(&s, &id, &exe, &e) {
            Ok(()) => StatusCode::ACCEPTED.into_response(),
            Err(msg) => {
                end_phase(&s.phases, &id, Phase::Starting);
                err(StatusCode::INTERNAL_SERVER_ERROR, msg)
            }
        };
    }
    // `down`, then `up` at once: the daemon on its way out would take this
    // `up` for its own and exit under it, or the new one would give up on
    // the state lock while the old one stops its services. Start once the
    // old one is gone.
    tokio::spawn(async move {
        let t0 = Instant::now();
        while s.phases.lock().unwrap().get(&id) == Some(&Phase::Stopping)
            && t0.elapsed() < STOP_WAIT
        {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        s.phases.lock().unwrap().insert(id.clone(), Phase::Starting);
        if let Err(msg) = start_up(&s, &id, &exe, &e) {
            end_phase(&s.phases, &id, Phase::Starting);
            s.start_errors.lock().unwrap().insert(id, msg);
        }
    });
    StatusCode::ACCEPTED.into_response()
}

/// How long a start waits for a stop under way, and a stop for its daemon
/// to exit (the daemon gives its services 90 s, ADR-0009).
const STOP_WAIT: Duration = Duration::from_secs(120);

/// Forget `phase` of stack `id`, unless another one took its place.
fn end_phase(phases: &Phases, id: &str, phase: Phase) {
    let mut p = phases.lock().unwrap();
    if p.get(id) == Some(&phase) {
        p.remove(id);
    }
}

/// Run `devstack up` for stack `e` in the background; the stack is
/// `Starting` until it ends.
fn start_up(s: &AppState, id: &str, exe: &std::path::Path, e: &StackEntry) -> Result<(), String> {
    // What `up` says goes to a file: when it fails before the daemon runs
    // (a stack file that needs a newer devstack, a port taken), its last
    // lines are what the dashboard shows instead of "Starting…" (#11).
    let log = state_dir().join(format!("up-{id}.log"));
    let (out, out2) = std::fs::File::create(&log)
        .and_then(|f| Ok((f.try_clone()?, f)))
        .map_err(|e| e.to_string())?;
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
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let errors = s.start_errors.clone();
    let phases = s.phases.clone();
    let repo = e.repo.clone();
    let id = id.to_string();
    std::thread::spawn(move || {
        let ok = child.wait().is_ok_and(|st| st.success());
        end_phase(&phases, &id, Phase::Starting);
        if ok {
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
    Ok(())
}

#[derive(Deserialize)]
struct DownQuery {
    #[serde(default)]
    infra: bool,
    /// Its lanes, too: the daemon exits even while lanes run.
    #[serde(default)]
    all: bool,
    /// For a restart (ADR-0052): the daemon exits even while lanes run and
    /// leaves them to the next one (an older daemon stops them).
    #[serde(default)]
    restart: bool,
}

async fn stack_down(
    State(s): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<DownQuery>,
) -> Response {
    let Some(e) = entry(&s, &id) else {
        return err(StatusCode::SERVICE_UNAVAILABLE, "stack not running");
    };
    let Some(daemon) = e.daemon else {
        return err(StatusCode::SERVICE_UNAVAILABLE, "stack not running");
    };
    {
        let mut phases = s.phases.lock().unwrap();
        if phases.get(&id) == Some(&Phase::Stopping) {
            return StatusCode::ACCEPTED.into_response(); // under way already
        }
        phases.insert(id.clone(), Phase::Stopping);
    }
    let (token, phases, port) = (s.token.clone(), s.phases.clone(), daemon.port);
    let state = e.repo.join(".devstack");
    tokio::spawn(async move {
        // Without the infrastructure, the daemon's own shutdown stops the
        // services: it says `stopping` and stops taking connections at once,
        // so nothing (an `up`, a forwarded request) takes it for a daemon
        // that serves while its services stop.
        //
        // Lanes that still run keep their daemon (ADR-0024) and the
        // infrastructure they share (ADR-0030): then only the main tree's
        // services stop (`200` from `down`, `409` from `shutdown`). A daemon
        // from before this does not know `unless_lanes` and exits.
        let shutdown = || daemon_request(port, &token, Method::POST, "/v1/shutdown");
        let exits = if q.infra {
            // With `all`, the lanes stop first, their containers too, so the
            // main tree's go as well.
            let path = if q.all {
                "/v1/down?infra=true&all=true"
            } else {
                "/v1/down?infra=true"
            };
            let kept = daemon_request(port, &token, Method::POST, path).await == Ok(200);
            (q.all || !kept) && shutdown().await.is_ok()
        } else {
            let path = if q.restart {
                "/v1/shutdown?restart=true"
            } else if q.all {
                "/v1/shutdown"
            } else {
                "/v1/shutdown?unless_lanes=true"
            };
            match daemon_request(port, &token, Method::POST, path).await {
                Ok(409) => {
                    let _ = daemon_request(port, &token, Method::POST, "/v1/down").await;
                    false
                }
                r => r.is_ok(),
            }
        };
        // Over when that daemon is gone: its `daemon.json` goes last.
        let t0 = Instant::now();
        while exits
            && t0.elapsed() < STOP_WAIT
            && devstack_daemon::read_info(&state)
                .is_some_and(|i| i.pid == daemon.pid && devstack_proc::pid_alive(i.pid))
        {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        end_phase(&phases, &id, Phase::Stopping);
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
/// How long a request that never reached a daemon is sent again, to the
/// daemon `daemon.json` names then (one that replaced the first).
const RESEND_WINDOW: Duration = Duration::from_secs(3);
/// What a `503` for a stack that is starting, stopping or restarting asks
/// the client to wait (`Retry-After`).
const RETRY_AFTER_S: u64 = 2;

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
        header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", s.token)).unwrap(),
    );
    let req = hyper::Request::from_parts(parts, body);
    match daemon_send(|| daemon_now(&s, &id), req).await {
        Ok(resp) => {
            let (mut parts, body) = resp.into_parts();
            // The hub's origin is the browser's; a daemon sets no cookie on it.
            parts.headers.remove(header::SET_COOKIE);
            strip_hop_by_hop(&mut parts.headers);
            Response::from_parts(parts, Body::new(body))
        }
        Err(e) => e.into_response(),
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

/// A stack's daemon as its `daemon.json` says now. Not the scan of up to a
/// second ago: between a `down` and an `up` that names a daemon that is
/// gone, or one that is stopping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DaemonNow {
    Serves {
        pid: u32,
        port: u16,
        started: u128,
    },
    /// It was told to stop (`daemon.json` says `stopping`) and takes no
    /// more connections.
    Stopping,
    /// No `daemon.json`, or its process is gone; `Starting` while the hub
    /// runs `devstack up` for it.
    Absent {
        starting: bool,
    },
}

fn daemon_now(s: &AppState, id: &str) -> DaemonNow {
    let Some(e) = entry(s, id) else {
        return DaemonNow::Absent { starting: false };
    };
    let phase = s.phases.lock().unwrap().get(id).copied();
    match daemon_in(&e.repo.join(".devstack")) {
        DaemonNow::Absent { .. } if phase == Some(Phase::Stopping) => DaemonNow::Stopping,
        DaemonNow::Absent { .. } => DaemonNow::Absent {
            starting: phase == Some(Phase::Starting),
        },
        d => d,
    }
}

fn daemon_in(state_dir: &std::path::Path) -> DaemonNow {
    match devstack_daemon::read_info(state_dir) {
        Some(i) if !devstack_proc::pid_alive(i.pid) => DaemonNow::Absent { starting: false },
        Some(i) if i.stopping => DaemonNow::Stopping,
        Some(i) => DaemonNow::Serves {
            pid: i.pid,
            port: i.port,
            started: i.started,
        },
        None => DaemonNow::Absent { starting: false },
    }
}

/// Why the hub could not reach a daemon: the answer it gives instead.
#[derive(Debug)]
struct DaemonError {
    status: StatusCode,
    msg: String,
    /// `Retry-After` (seconds): the stack is starting, stopping or restarting.
    retry_after: Option<u64>,
}

impl DaemonError {
    fn new(status: StatusCode, msg: impl Into<String>) -> Self {
        Self {
            status,
            msg: msg.into(),
            retry_after: None,
        }
    }

    /// `503` for a daemon that is not there (`now`), with what it does.
    fn unavailable(now: DaemonNow, why: Option<String>) -> Self {
        let (msg, retry) = match now {
            DaemonNow::Absent { starting: false } => ("stack not running".to_string(), false),
            DaemonNow::Absent { starting: true } => (
                "stack is starting: its daemon does not answer yet; try again in a moment".into(),
                true,
            ),
            DaemonNow::Stopping => (
                "stack is stopping or restarting: its daemon takes no more requests; \
                 try again once it is up"
                    .into(),
                true,
            ),
            DaemonNow::Serves { .. } => (
                "stack is restarting: its daemon changed while the hub reached it; \
                 try again in a moment"
                    .into(),
                true,
            ),
        };
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            msg: match why {
                Some(why) => format!("{msg} ({why})"),
                None => msg,
            },
            retry_after: retry.then_some(RETRY_AFTER_S),
        }
    }

    fn into_response(self) -> Response {
        let mut r = err(self.status, self.msg);
        if let Some(s) = self.retry_after {
            r.headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from(s));
        }
        r
    }
}

/// `GET <path>` on the daemon of stack `id`, with the token (the `ws` bridge).
async fn daemon_get(
    s: &AppState,
    id: &str,
    path: &str,
) -> Result<hyper::Response<hyper::body::Incoming>, DaemonError> {
    let req = hyper::Request::get(path)
        .header(header::AUTHORIZATION, format!("Bearer {}", s.token))
        .header(header::ACCEPT, "text/event-stream")
        .body(Body::empty())
        .map_err(|_| DaemonError::new(StatusCode::BAD_REQUEST, "bad path"))?;
    daemon_send(|| daemon_now(s, id), req).await
}

/// Send `req` to the daemon `resolve` names, with its Host.
///
/// A daemon can go between the look at `daemon.json` and the request: it
/// was told to stop (`down`, a restart) and closes its port, and what it
/// had not taken yet is cut. A request that certainly never reached it
/// (no connection; hyper hands it back unsent; or one without a body that
/// is safe to repeat) is sent again for a moment, to the daemon
/// `daemon.json` names then. Otherwise, when the daemon is no longer the
/// one the request went to, the answer is `503` with `Retry-After` and
/// what the stack does; only a daemon that still serves and broke the
/// exchange is a `502`.
async fn daemon_send(
    resolve: impl Fn() -> DaemonNow,
    mut req: hyper::Request<Body>,
) -> Result<hyper::Response<hyper::body::Incoming>, DaemonError> {
    use hyper::body::Body as _;
    let t0 = Instant::now();
    // GET, HEAD, OPTIONS without a body: safe to repeat whatever happened.
    let repeatable = matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS)
        && req.body().size_hint().exact() == Some(0);
    let again = |req: &hyper::Request<Body>| {
        let mut r = hyper::Request::new(Body::empty());
        *r.method_mut() = req.method().clone();
        *r.uri_mut() = req.uri().clone();
        *r.headers_mut() = req.headers().clone();
        r
    };
    loop {
        let now = resolve();
        let DaemonNow::Serves { port, .. } = now else {
            return Err(DaemonError::unavailable(now, None));
        };
        req.headers_mut().insert(
            header::HOST,
            HeaderValue::from_str(&format!("127.0.0.1:{port}")).unwrap(),
        );
        let in_window = || t0.elapsed() < RESEND_WINDOW;
        let stream = match connect_daemon(port).await {
            Ok(s) => s,
            Err(e) => {
                // Nothing was sent. A daemon that replaced this one is
                // worth another try; otherwise it is not running.
                let then = resolve();
                if then != now && in_window() {
                    continue;
                }
                return Err(match then {
                    DaemonNow::Serves { .. } if then == now => {
                        DaemonError::unavailable(DaemonNow::Absent { starting: false }, Some(e))
                    }
                    then => DaemonError::unavailable(then, Some(e)),
                });
            }
        };
        let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .map_err(|e| DaemonError::new(StatusCode::BAD_GATEWAY, e.to_string()))?;
        tokio::spawn(conn);
        let copy = repeatable.then(|| again(&req));
        match sender.try_send_request(req).await {
            Ok(resp) => return Ok(resp),
            Err(mut e) => {
                let then = resolve();
                let unsent = e.take_message();
                let sent = unsent.is_none();
                let error = e.into_error().to_string();
                diag::warn(
                    "daemon_send_failed",
                    serde_json::json!({
                        "error": error,
                        "sent": sent,
                        "daemon_changed": then != now,
                    }),
                );
                if in_window()
                    && let Some(r) = unsent.or(copy)
                {
                    req = r;
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
                if then == now {
                    return Err(DaemonError::new(StatusCode::BAD_GATEWAY, error));
                }
                // A daemon told to stop gives the requests it holds a moment,
                // then cuts them: this one may have been carried out in part.
                let why = if sent {
                    format!(
                        "the daemon closed the connection before it answered, the request may have been carried out in part: {error}"
                    )
                } else {
                    error
                };
                return Err(DaemonError::unavailable(then, Some(why)));
            }
        }
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
    fn auth_sends_on_to_dashboard_routes_only() {
        let route = "/#/s/ab12/services/api?lane=feat-x";
        assert_eq!(dashboard_route(Some(route)), route);
        // What `next_param` makes of it comes back as it was.
        let uri: axum::http::Uri = format!("/auth?code=c&next={}", next_param(route))
            .parse()
            .unwrap();
        let Query(q) = Query::<CodeQuery>::try_from_uri(&uri).unwrap();
        assert_eq!(q.next.as_deref(), Some(route));
        for bad in [
            None,
            Some("https://evil.example/"),
            Some("//evil.example/#/"),
            Some(r"/#/s/x\evil"),
            Some("/#/s/<script>"),
            Some("/hub/shutdown"),
        ] {
            assert_eq!(dashboard_route(bad), "/", "{bad:?}");
        }
    }

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

#[cfg(test)]
mod update_tests {
    use super::*;

    fn write_pending(dir: &std::path::Path, from: &str, started: u64) {
        let p = PendingUpdate {
            from: from.into(),
            pid: 4242,
            started,
        };
        std::fs::write(dir.join(PENDING_UPDATE), serde_json::to_vec(&p).unwrap()).unwrap();
    }

    #[test]
    fn the_next_hub_reports_what_an_update_installed() {
        let d = tempfile::tempdir().unwrap();
        assert!(pending_update(d.path(), "1.0.0", 1000, |_| false).is_none());
        write_pending(d.path(), "0.9.8", 1000);
        let (state, watch) = pending_update(d.path(), "1.0.0", 2000, |_| true).unwrap();
        assert_eq!(state.installed.as_deref(), Some("1.0.0"));
        assert!(!state.running && state.error.is_none() && state.finished.is_some());
        assert!(watch.is_none());
        assert!(!d.path().join(PENDING_UPDATE).exists(), "reported once");
    }

    #[test]
    fn the_same_version_after_an_update_is_a_failure() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("update.log"),
            "installing\nerror: no network\n",
        )
        .unwrap();
        write_pending(d.path(), "0.9.8", 1000);
        // The updater still runs: wait for it.
        let (state, watch) = pending_update(d.path(), "0.9.8", 2000, |_| true).unwrap();
        assert!(state.running);
        assert_eq!(watch, Some(4242));
        assert!(d.path().join(PENDING_UPDATE).exists());
        // It ended: the log says why.
        let (state, _) = pending_update(d.path(), "0.9.8", 2000, |_| false).unwrap();
        assert!(!state.running);
        assert!(state.error.unwrap().contains("no network"));
        assert!(!d.path().join(PENDING_UPDATE).exists());
    }

    #[test]
    fn an_old_or_broken_update_file_is_ignored() {
        let d = tempfile::tempdir().unwrap();
        write_pending(d.path(), "0.9.8", 1000);
        let late = 1000 + PENDING_UPDATE_MAX_AGE_MS;
        assert!(pending_update(d.path(), "1.0.0", late, |_| true).is_none());
        assert!(!d.path().join(PENDING_UPDATE).exists());
        std::fs::write(d.path().join(PENDING_UPDATE), "{").unwrap();
        assert!(pending_update(d.path(), "1.0.0", 1000, |_| true).is_none());
        assert!(!d.path().join(PENDING_UPDATE).exists());
    }

    #[test]
    fn this_process_is_alive() {
        assert!(devstack_proc::pid_alive(std::process::id()));
    }

    #[test]
    fn a_failed_release_check_is_told_until_one_succeeds() {
        let mut r = Release::default();
        assert!(r.answer(Ok(Some("9.0.0".into()))));
        assert!(!r.answer(Ok(Some("9.0.0".into()))), "nothing new");
        // No answer: the newer release found before stays, with why.
        assert!(r.answer(Err("no network".into())));
        assert_eq!(r.newer.as_deref(), Some("9.0.0"));
        assert_eq!(r.error.as_deref(), Some("no network"));
        assert!(!r.answer(Err("no network".into())));
        assert!(r.answer(Ok(None)));
        assert_eq!(r, Release::default());
    }
}

/// Forwarding to a daemon that stops, restarts or moves (finding A2: a
/// request one second after `down` + `up` ended as `502`).
#[cfg(test)]
mod daemon_send_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Takes connections, reads what comes, closes them unanswered: a
    /// daemon told to stop, cutting what it had not served.
    async fn cutting() -> u16 {
        let l = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut c, _)) = l.accept().await {
                let mut buf = [0u8; 4096];
                let _ = tokio::time::timeout(Duration::from_millis(500), c.read(&mut buf)).await;
            }
        });
        port
    }

    /// Answers every request with `200 ok`; counts them.
    async fn answering() -> (u16, Arc<AtomicUsize>) {
        let l = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = l.local_addr().unwrap().port();
        let n = Arc::new(AtomicUsize::new(0));
        let count = n.clone();
        tokio::spawn(async move {
            while let Ok((mut c, _)) = l.accept().await {
                let count = count.clone();
                tokio::spawn(async move {
                    let mut got = Vec::new();
                    let mut buf = [0u8; 4096];
                    while !got.windows(4).any(|w| w == b"\r\n\r\n") {
                        match c.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(k) => got.extend_from_slice(&buf[..k]),
                        }
                    }
                    count.fetch_add(1, Ordering::SeqCst);
                    let _ = c
                        .write_all(
                            b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok",
                        )
                        .await;
                });
            }
        });
        (port, n)
    }

    /// A port nothing listens on.
    fn closed() -> u16 {
        std::net::TcpListener::bind(("127.0.0.1", 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    fn serves(port: u16) -> DaemonNow {
        DaemonNow::Serves {
            pid: 1,
            port,
            started: u128::from(port),
        }
    }

    /// What `daemon.json` says, one look after the other; the last stays.
    fn looks(seq: Vec<DaemonNow>) -> impl Fn() -> DaemonNow {
        let seq = Mutex::new(std::collections::VecDeque::from(seq));
        move || {
            let mut s = seq.lock().unwrap();
            if s.len() > 1 {
                s.pop_front().unwrap()
            } else {
                s[0]
            }
        }
    }

    fn post() -> hyper::Request<Body> {
        hyper::Request::post("/v1/lanes")
            .body(Body::from(r#"{"path":"x"}"#))
            .unwrap()
    }

    fn get() -> hyper::Request<Body> {
        hyper::Request::get("/v1/stack")
            .body(Body::empty())
            .unwrap()
    }

    #[tokio::test]
    async fn a_daemon_that_stops_under_a_request_is_a_503_to_retry() {
        let port = cutting().await;
        let e = daemon_send(looks(vec![serves(port), DaemonNow::Stopping]), post())
            .await
            .unwrap_err();
        assert_eq!(e.status, StatusCode::SERVICE_UNAVAILABLE, "{}", e.msg);
        assert_eq!(e.retry_after, Some(RETRY_AFTER_S));
        assert!(e.msg.contains("stopping"), "{}", e.msg);
        let r = e.into_response();
        assert_eq!(r.headers()[header::RETRY_AFTER], "2");
    }

    #[tokio::test]
    async fn a_post_that_may_have_reached_a_replaced_daemon_is_not_sent_again() {
        let (new, n) = answering().await;
        let old = cutting().await;
        let e = daemon_send(looks(vec![serves(old), serves(new)]), post())
            .await
            .unwrap_err();
        assert_eq!(e.status, StatusCode::SERVICE_UNAVAILABLE, "{}", e.msg);
        assert!(e.msg.contains("restarting"), "{}", e.msg);
        assert_eq!(e.retry_after, Some(RETRY_AFTER_S));
        assert_eq!(n.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_get_goes_to_the_daemon_that_replaced_the_one_that_cut_it() {
        let (new, n) = answering().await;
        let old = cutting().await;
        let r = daemon_send(looks(vec![serves(old), serves(new)]), get())
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(n.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_post_that_found_no_daemon_goes_to_the_one_named_next() {
        // Not connected: certainly not delivered, safe to send again.
        let (new, n) = answering().await;
        let r = daemon_send(looks(vec![serves(closed()), serves(new)]), post())
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(n.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_daemon_that_serves_and_breaks_the_exchange_is_a_502() {
        let port = cutting().await;
        let e = daemon_send(looks(vec![serves(port)]), post())
            .await
            .unwrap_err();
        assert_eq!(e.status, StatusCode::BAD_GATEWAY, "{}", e.msg);
        assert_eq!(e.retry_after, None);
    }

    #[tokio::test]
    async fn no_daemon_is_a_503_and_a_start_under_way_says_so() {
        let e = daemon_send(looks(vec![serves(closed())]), post())
            .await
            .unwrap_err();
        assert_eq!(e.status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(e.msg.starts_with("stack not running"), "{}", e.msg);
        assert_eq!(e.retry_after, None);
        let e = daemon_send(looks(vec![DaemonNow::Absent { starting: false }]), get())
            .await
            .unwrap_err();
        assert_eq!(e.msg, "stack not running");
        let e = daemon_send(looks(vec![DaemonNow::Absent { starting: true }]), get())
            .await
            .unwrap_err();
        assert_eq!(e.status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(e.msg.contains("starting"), "{}", e.msg);
        assert_eq!(e.retry_after, Some(RETRY_AFTER_S));
    }

    #[test]
    fn daemon_json_says_whether_a_daemon_serves() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(daemon_in(d.path()), DaemonNow::Absent { starting: false });
        let write = |pid: u32, stopping: bool| {
            let info = devstack_daemon::DaemonInfo {
                pid,
                port: 7000,
                started: 5,
                version: String::new(),
                stopping,
                carries_lanes: false,
            };
            std::fs::write(
                devstack_daemon::info_path(d.path()),
                serde_json::to_vec(&info).unwrap(),
            )
            .unwrap();
        };
        let me = std::process::id();
        write(me, false);
        assert_eq!(
            daemon_in(d.path()),
            DaemonNow::Serves {
                pid: me,
                port: 7000,
                started: 5
            }
        );
        write(me, true);
        assert_eq!(daemon_in(d.path()), DaemonNow::Stopping);
        // Left behind by a daemon that was killed: its port says nothing.
        write(u32::MAX - 1, false);
        assert_eq!(daemon_in(d.path()), DaemonNow::Absent { starting: false });
    }
}
