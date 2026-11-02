//! Lanes others in the pool share with this machine (ADR-0035): listed from
//! the coordinator, opened through a forward on this machine's loopback to
//! the lane's node, the same tunnel the lane's owner uses, checked by the
//! node against the owner's sharing at every connection.
//!
//! A member connects to a lane and disconnects again (ADR-0053): connected,
//! each service it shares has a forward, on the port its owner has it on if
//! that is free here, else another one; nothing is forwarded before.
//!
//! A connection is to the lane, not to a lease of it (ADR-0085): its owner,
//! repository and lane name, kept in `connected.json` in the pool's
//! directory. The lane started again under a new lease is connected again
//! by itself; one missing from the coordinator's list keeps its ports for
//! [`GRACE`]; after a restart of the hub its lanes are connected again.
//!
//! - `GET /hub/pool/shared`: the lanes shared with this machine, each
//!   service with `forwarded`, its port here, while the lane is connected
//! - `POST /hub/pool/shared/{lease}/connect`: connect to the lane:
//!   `{services: [{name, port, url}], warning?}`. It stays connected until
//!   it is disconnected, or its owner stops sharing it with this machine
//!   while it runs (ADR-0085).
//! - `POST /hub/pool/shared/{lease}/disconnect`: its forwards close, the
//!   connections through them too, and it is no longer connected: `{closed}`.
//! - `POST /hub/pool/shared/{lease}/open` `{service}`: connect the lane if it
//!   is not, `{url, port, warning?}` of that service. `warning`: the node,
//!   or the lane's owner, runs another devstack than this machine, or does
//!   not say which (ADR-0031, amended 2026-10-19).
//!
//! - `GET /hub/pool/shared/{lease}/api/<path>`: a lane shared with this
//!   machine, viewed (ADR-0040), read through its node: the lane daemon's
//!   `<path>` (state, events, logs, usage), streams passed through. The node
//!   checks that the lane is shared with this machine, and that `<path>`
//!   only reads.
//!
//! All take `?pool=<name>` (ADR-0034).

use axum::Json;
use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use devstack_node::coordinator::SharedLane;
use devstack_node::member::Local;
use devstack_node::nodeclient::{ClientError, NodeClient};
use devstack_node::version::{self, Compat};
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::watch;

#[derive(Deserialize, Default)]
pub struct PoolQuery {
    #[serde(default)]
    pub(crate) pool: Option<String>,
}

impl PoolQuery {
    /// The pool meant: no `pool`, an empty one and `default` are the same
    /// pool, and key the caches and forwards alike.
    fn name(&self) -> &str {
        self.pool
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .unwrap_or(devstack_node::member::DEFAULT_POOL)
    }

    fn local(&self) -> Result<Local, Box<Response>> {
        let p = self.name();
        if !devstack_node::member::valid_pool_name(p) {
            return Err(Box::new(error(
                StatusCode::BAD_REQUEST,
                format!("pool `{p}`"),
            )));
        }
        Ok(Local::new(&pool_path(p)))
    }
}

/// This machine's state directory.
fn state_dir() -> std::path::PathBuf {
    devstack_daemon::token::token_path()
        .parent()
        .expect("token path has a parent")
        .to_path_buf()
}

/// The directory of pool `pool` on this machine.
fn pool_path(pool: &str) -> std::path::PathBuf {
    devstack_node::member::pool_dir(&state_dir(), pool)
}

fn error(status: StatusCode, msg: String) -> Response {
    (status, Json(json!({ "error": msg }))).into_response()
}

/// A forward: (pool, lease, port on the node).
type Key = (String, String, u16);

/// A forward open now: its port here, and what ends it. Dropped, its
/// listener and every connection through it stop (ADR-0053 point 1).
struct Forward {
    port: u16,
    _stop: watch::Sender<()>,
}

/// The forwards open now: the lanes connected to (ADR-0053).
type Forwards = Mutex<HashMap<Key, Forward>>;

fn forwards() -> &'static Forwards {
    static F: OnceLock<Forwards> = OnceLock::new();
    F.get_or_init(Default::default)
}

/// The port here the service on node port `port` of lane `lease` is
/// forwarded to, if it is.
fn forwarded(pool: &str, lease: &str, port: u16) -> Option<u16> {
    forwards()
        .lock()
        .unwrap()
        .get(&(pool.to_string(), lease.to_string(), port))
        .map(|f| f.port)
}

/// Disconnect lane `lease`: its forwards close, connections included.
/// How many there were.
fn close(pool: &str, lease: &str) -> usize {
    let mut f = forwards().lock().unwrap();
    let before = f.len();
    f.retain(|(p, l, _), _| p != pool || l != lease);
    before - f.len()
}

/// How long a connected lane missing from the coordinator's list keeps its
/// ports here (ADR-0085 point 2): the coordinator restarting, or its owner's
/// daemon telling it the sharing again, takes about half a minute.
const GRACE: std::time::Duration = std::time::Duration::from_secs(120);

/// How long a connected lane may be gone before it is forgotten: one
/// stopped for good is not connected to the next lane of its name.
const FORGET: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

/// A lane connected to (ADR-0085 point 1): the lane, whatever its lease.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, Deserialize)]
struct LaneId {
    owner: String,
    repo: String,
    lane: String,
}

impl LaneId {
    fn of(l: &SharedLane) -> Self {
        Self {
            owner: l.owner.clone(),
            repo: l.repo.clone(),
            lane: l.lane.clone(),
        }
    }
}

/// A connection: the lease its forwards are to (empty before the hub saw
/// the lane after a restart), and since when the lane is missing.
#[derive(Debug, Clone, Default)]
struct Conn {
    lease: String,
    missing: Option<std::time::Instant>,
    /// Its forwards closed after [`GRACE`]; it connects again once listed.
    closed: bool,
}

type Conns = Mutex<HashMap<String, HashMap<LaneId, Conn>>>;

/// The connections, per pool; a pool's read from its `connected.json` the
/// first time it is needed.
fn conns() -> &'static Conns {
    static C: OnceLock<Conns> = OnceLock::new();
    C.get_or_init(Default::default)
}

fn conns_file(pool: &str) -> std::path::PathBuf {
    pool_path(pool).join("connected.json")
}

/// Pool `pool`'s connections, loaded from its file if they are not yet.
fn with_conns<T>(pool: &str, f: impl FnOnce(&mut HashMap<LaneId, Conn>) -> T) -> T {
    let mut all = conns().lock().unwrap();
    let c = all.entry(pool.to_string()).or_insert_with(|| {
        let ids: Vec<LaneId> = std::fs::read(conns_file(pool))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        ids.into_iter().map(|id| (id, Conn::default())).collect()
    });
    f(c)
}

/// Write pool `pool`'s connections: what a restarted hub connects again.
fn save_conns(pool: &str) {
    let mut ids: Vec<LaneId> = with_conns(pool, |c| c.keys().cloned().collect());
    ids.sort_by(|a, b| (&a.owner, &a.repo, &a.lane).cmp(&(&b.owner, &b.repo, &b.lane)));
    let file = conns_file(pool);
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(
        &file,
        serde_json::to_vec_pretty(&ids).expect("lane ids serialise"),
    );
}

/// Remember `lane` as connected (ADR-0085 point 1).
fn remember(pool: &str, lane: &SharedLane) {
    let added = with_conns(pool, |c| {
        let fresh = !c.contains_key(&LaneId::of(lane));
        c.insert(
            LaneId::of(lane),
            Conn {
                lease: lane.lease.clone(),
                ..Conn::default()
            },
        );
        fresh
    });
    if added {
        save_conns(pool);
    }
}

/// Forget the lane of lease `lease` as connected: disconnected.
fn forget(pool: &str, lease: &str) {
    let removed = with_conns(pool, |c| {
        let before = c.len();
        c.retain(|_, x| x.lease != lease);
        before != c.len()
    });
    if removed {
        save_conns(pool);
    }
}

/// What to do about one connection, `lanes` listed now.
#[derive(Debug, PartialEq)]
enum Step {
    /// Listed: (re)connect it; the forwards of the lease it had close first
    /// when it has a new one.
    Connect { old: Option<String> },
    /// Missing, within [`GRACE`]: its ports stay.
    Wait,
    /// Missing for longer: its forwards close, it stays connected.
    Close,
    /// Gone for longer than [`FORGET`]: no longer connected.
    Forget,
    /// Missing, its forwards closed already.
    Nothing,
}

fn step(
    id: &LaneId,
    c: &mut Conn,
    lanes: &[SharedLane],
    now: std::time::Instant,
) -> (Step, Option<SharedLane>) {
    if let Some(l) = lanes.iter().find(|l| &LaneId::of(l) == id) {
        let old = (!c.lease.is_empty() && c.lease != l.lease).then(|| c.lease.clone());
        c.lease = l.lease.clone();
        c.missing = None;
        c.closed = false;
        return (Step::Connect { old }, Some(l.clone()));
    }
    let since = *c.missing.get_or_insert(now);
    let gone = now.saturating_duration_since(since);
    let s = if gone > FORGET {
        Step::Forget
    } else if c.closed {
        Step::Nothing
    } else if gone > GRACE {
        c.closed = true;
        Step::Close
    } else {
        Step::Wait
    };
    (s, None)
}

/// Bring pool `pool`'s forwards in line with `lanes`, the lanes shared with
/// this machine as the coordinator listed them just now (ADR-0085): a
/// connected lane listed is connected, under its new lease if it has one,
/// a service it no longer shares closed; one missing keeps its ports for
/// [`GRACE`]. Past that, its forwards close, and a lane that runs but is no
/// longer shared with this machine is no longer connected.
async fn reconcile(pool: &str, local: &Local, lanes: &[SharedLane]) {
    let now = std::time::Instant::now();
    let steps: Vec<(LaneId, Step, Option<SharedLane>, String)> = with_conns(pool, |c| {
        c.iter_mut()
            .map(|(id, conn)| {
                let lease = conn.lease.clone();
                let (s, l) = step(id, conn, lanes, now);
                (id.clone(), s, l, lease)
            })
            .collect()
    });
    // The forwards of a lease no connection holds, and of a service no
    // longer shared, close.
    {
        let held: Vec<&str> = steps
            .iter()
            .filter(|(_, s, _, _)| *s == Step::Wait)
            .map(|(_, _, _, lease)| lease.as_str())
            .collect();
        let listed: Vec<&SharedLane> = steps.iter().filter_map(|(_, _, l, _)| l.as_ref()).collect();
        forwards().lock().unwrap().retain(|(p, l, port), _| {
            p != pool
                || held.contains(&l.as_str())
                || listed
                    .iter()
                    .any(|x| &x.lease == l && x.services.iter().any(|s| s.port == *port))
        });
    }
    let mut changed = false;
    let mut withdrawn: Vec<LaneId> = Vec::new();
    for (id, s, lane, lease) in steps {
        match s {
            Step::Connect { old } => {
                let lane = lane.expect("listed");
                if let Some(old) = &old {
                    close(pool, old);
                    devstack_daemon::diag::info(
                        "shared_lane_reconnected",
                        json!({ "lane": lane.lane, "owner": lane.owner_name, "lease": lane.lease, "was": old }),
                    );
                }
                if let Err(r) = connect_lane(pool, local, &lane) {
                    devstack_daemon::diag::warn(
                        "shared_lane_reconnect_failed",
                        json!({ "lane": lane.lane, "owner": lane.owner_name, "status": r.status().as_u16() }),
                    );
                }
            }
            Step::Close => {
                close(pool, &lease);
                withdrawn.push(id);
            }
            Step::Forget => {
                close(pool, &lease);
                with_conns(pool, |c| c.remove(&id));
                changed = true;
            }
            Step::Wait | Step::Nothing => {}
        }
    }
    // Past the grace: a lane that runs but is not listed was taken back
    // by its owner (ADR-0085 point 4); one that does not run may come back.
    if !withdrawn.is_empty() {
        let dir = pool_path(pool);
        if let Ok(Ok(leases)) = tokio::task::spawn_blocking(move || Local::new(&dir).leases()).await
        {
            for id in withdrawn {
                let runs = leases.iter().any(|x| {
                    x.member == id.owner
                        && x.repo == id.repo
                        && x.lane == id.lane
                        && x.state == "running"
                });
                if runs {
                    with_conns(pool, |c| c.remove(&id));
                    changed = true;
                    devstack_daemon::diag::info(
                        "shared_lane_unshared",
                        json!({ "lane": id.lane, "repo": id.repo }),
                    );
                }
            }
        }
    }
    if changed {
        save_conns(pool);
    }
}

/// Every [`FOLLOW_EVERY`], the connections of every pool brought in line
/// with what the coordinator lists: a lane started again, or a hub started
/// again, is connected without a dashboard open (ADR-0085 point 3).
const FOLLOW_EVERY: std::time::Duration = std::time::Duration::from_secs(10);

pub fn follow() {
    tokio::spawn(async move {
        loop {
            for (pool, dir) in devstack_node::member::pools(&state_dir()) {
                if with_conns(&pool, |c| c.is_empty()) {
                    continue;
                }
                let d2 = dir.clone();
                // The coordinator not reached: nothing is known missing.
                let Ok(Ok(lanes)) =
                    tokio::task::spawn_blocking(move || Local::new(&d2).shared()).await
                else {
                    continue;
                };
                reconcile(&pool, &Local::new(&dir), &lanes).await;
            }
            tokio::time::sleep(FOLLOW_EVERY).await;
        }
    });
}

/// The shared lanes as `GET /hub/pool/shared` answers them: each service
/// with `forwarded`, its port here while the lane is connected.
fn with_forwarded(pool: &str, lanes: &[SharedLane]) -> serde_json::Value {
    let mut v = json!(lanes);
    for (lane, out) in lanes.iter().zip(v.as_array_mut().into_iter().flatten()) {
        for (s, o) in lane
            .services
            .iter()
            .zip(out["services"].as_array_mut().into_iter().flatten())
        {
            if let Some(port) = forwarded(pool, &lane.lease, s.port) {
                o["forwarded"] = json!(port);
            }
        }
    }
    v
}

/// Accept connections on `listener`, the forward `key` on port `port` here,
/// each one handled by `handle` with the forward's end, until it ends:
/// `stopped` sees its `Forward` dropped.
fn serve<F, Fut>(
    listener: tokio::net::TcpListener,
    key: Key,
    port: u16,
    mut stopped: watch::Receiver<()>,
    handle: F,
) where
    F: Fn(tokio::net::TcpStream, watch::Receiver<()>) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    tokio::spawn(async move {
        loop {
            let accepted = tokio::select! {
                a = listener.accept() => a,
                _ = stopped.changed() => break,
            };
            let Ok((tcp, _)) = accepted else {
                break;
            };
            let _ = tcp.set_nodelay(true);
            tokio::spawn(handle(tcp, stopped.clone()));
        }
        // Closed for another reason than a disconnect: forget it, unless a
        // newer forward took its place.
        let mut f = forwards().lock().unwrap();
        if f.get(&key).is_some_and(|x| x.port == port) {
            f.remove(&key);
        }
    });
}

/// The lanes shared with this machine, per pool, as the coordinator said
/// lately: a dashboard viewing one reads it many times a minute.
type Listed = Mutex<HashMap<String, (std::time::Instant, Vec<SharedLane>)>>;

fn listed() -> &'static Listed {
    static L: OnceLock<Listed> = OnceLock::new();
    L.get_or_init(Default::default)
}

/// The lane `lease` shared with this machine, and the membership
/// to reach its node with; the answer to give otherwise.
#[allow(clippy::result_large_err)] // the error is the answer itself
async fn viewable(q: &PoolQuery, lease: &str) -> Result<(Local, SharedLane), Response> {
    let local = q.local().map_err(|r| *r)?;
    let key = q.name().to_string();
    let cached = listed()
        .lock()
        .unwrap()
        .get(&key)
        .filter(|(at, _)| at.elapsed() < std::time::Duration::from_secs(10))
        .map(|(_, l)| l.clone());
    let lanes = match cached {
        Some(l) => l,
        None => {
            let l2 = q.local().map_err(|r| *r)?;
            let got = tokio::task::spawn_blocking(move || l2.shared()).await;
            let lanes = match got {
                Ok(Ok(v)) => v,
                Ok(Err(e)) => return Err(error(StatusCode::BAD_GATEWAY, e.to_string())),
                Err(e) => return Err(error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
            };
            listed()
                .lock()
                .unwrap()
                .insert(key, (std::time::Instant::now(), lanes.clone()));
            lanes
        }
    };
    let Some(lane) = lanes.into_iter().find(|l| l.lease == lease) else {
        return Err(error(
            StatusCode::NOT_FOUND,
            format!("no lane `{lease}` is shared with this machine"),
        ));
    };
    Ok((local, lane))
}

/// A read of the shared lane `lease` through its node (ADR-0040):
/// `path_and_query` is the lane daemon's (`/v1/stack`), without `pool`.
#[allow(clippy::result_large_err)] // the error is the answer itself
pub(crate) async fn view_get(
    q: &PoolQuery,
    lease: &str,
    path_and_query: &str,
) -> Result<hyper::Response<hyper::body::Incoming>, Response> {
    let (local, lane) = viewable(q, lease).await?;
    let client = NodeClient::new(&local, lane.node.clone())
        .map_err(|e| error(StatusCode::BAD_GATEWAY, e.to_string()))?;
    let req = hyper::Request::get(path_and_query)
        .header(hyper::header::ACCEPT, "text/event-stream, */*")
        .body(http_body_util::Empty::<bytes::Bytes>::new())
        .map_err(|_| error(StatusCode::BAD_REQUEST, "bad path".into()))?;
    client
        .view(lease, req)
        .await
        .map_err(|e| error(StatusCode::BAD_GATEWAY, format!("{}: {e}", lane.node.name)))
}

/// The query of a request without `pool`, which the hub keeps.
pub(crate) fn without_pool(query: Option<&str>) -> String {
    let rest: Vec<&str> = query
        .unwrap_or("")
        .split('&')
        .filter(|kv| !kv.is_empty() && !kv.starts_with("pool="))
        .collect();
    if rest.is_empty() {
        String::new()
    } else {
        format!("?{}", rest.join("&"))
    }
}

/// `GET /hub/pool/shared/{lease}/api/{*rest}`.
pub async fn view(
    Path((lease, rest)): Path<(String, String)>,
    Query(q): Query<PoolQuery>,
    req: axum::extract::Request,
) -> Response {
    let path = format!("/{rest}{}", without_pool(req.uri().query()));
    match view_get(&q, &lease, &path).await {
        Ok(resp) => {
            let (parts, body) = resp.into_parts();
            let mut out = Response::new(axum::body::Body::new(body));
            *out.status_mut() = parts.status;
            for h in [hyper::header::CONTENT_TYPE, hyper::header::CACHE_CONTROL] {
                if let Some(v) = parts.headers.get(&h) {
                    out.headers_mut().insert(h, v.clone());
                }
            }
            out
        }
        Err(r) => r,
    }
}

pub async fn list(Query(q): Query<PoolQuery>) -> Response {
    let local = match q.local() {
        Ok(l) => l,
        Err(r) => return *r,
    };
    let lanes = tokio::task::spawn_blocking(move || {
        if local.membership().ok().flatten().is_none() {
            return Ok(Vec::new());
        }
        local.shared()
    })
    .await;
    match lanes {
        Ok(Ok(v)) => {
            let pool = q.name();
            if let Ok(local) = q.local() {
                reconcile(pool, &local, &v).await;
            }
            Json(with_forwarded(pool, &v)).into_response()
        }
        Ok(Err(e)) => error(StatusCode::BAD_GATEWAY, e.to_string()),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[derive(Deserialize)]
pub struct OpenBody {
    service: String,
}

/// The lane `lease` shared with this machine, as the coordinator says now
/// (a service shared since is in it), and the membership to reach its node
/// with; the answer to give otherwise.
#[allow(clippy::result_large_err)] // the error is the answer itself
async fn shared_now(q: &PoolQuery, lease: &str) -> Result<(Local, SharedLane), Response> {
    let local = q.local().map_err(|r| *r)?;
    let found = tokio::task::spawn_blocking(move || {
        let lanes: Vec<SharedLane> = local.shared()?;
        Ok::<_, devstack_node::member::MemberError>((local, lanes))
    })
    .await;
    let (local, lanes) = match found {
        Ok(Ok(x)) => x,
        Ok(Err(e)) => return Err(error(StatusCode::BAD_GATEWAY, e.to_string())),
        Err(e) => return Err(error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
    };
    let pool = q.name();
    reconcile(pool, &local, &lanes).await;
    listed()
        .lock()
        .unwrap()
        .insert(pool.to_string(), (std::time::Instant::now(), lanes.clone()));
    let Some(lane) = lanes.into_iter().find(|l| l.lease == lease) else {
        return Err(error(
            StatusCode::NOT_FOUND,
            format!("no lane `{lease}` is shared with this machine"),
        ));
    };
    Ok((local, lane))
}

/// A web page's scheme: the one a shared service is opened with. The scheme
/// comes from the lane owner's daemon, through the coordinator; anything
/// else would reach a protocol handler of this machine (`ms-msdt:`,
/// `search-ms:`) when opened.
fn web(scheme: &str) -> Option<String> {
    let s = scheme.to_ascii_lowercase();
    (s == "http" || s == "https").then_some(s)
}

/// Connect to `lane` (ADR-0053 point 1): a forward for each service it
/// shares with a web scheme that has none yet, on the port its owner has it
/// on if that is free here, else another one. The services' names and
/// ports here.
#[allow(clippy::result_large_err)] // the error is the answer itself
fn connect_lane(
    pool: &str,
    local: &Local,
    lane: &SharedLane,
) -> Result<Vec<(String, u16)>, Response> {
    let mut client: Option<Arc<NodeClient>> = None;
    let mut opened = Vec::new();
    let mut ports = Vec::new();
    for svc in &lane.services {
        if web(&svc.scheme).is_none() {
            continue;
        }
        let key: Key = (pool.to_string(), lane.lease.clone(), svc.port);
        if let Some(port) = forwarded(pool, &lane.lease, svc.port) {
            ports.push((svc.name.clone(), port));
            continue;
        }
        let client = match &client {
            Some(c) => c.clone(),
            None => {
                let c = Arc::new(
                    NodeClient::new(local, lane.node.clone())
                        .map_err(|e| error(StatusCode::BAD_GATEWAY, e.to_string()))?,
                );
                client = Some(c.clone());
                c
            }
        };
        // The owner's port if it is free here: links between its services
        // work alike. Else any.
        let listener = match std::net::TcpListener::bind(("127.0.0.1", svc.here)) {
            Ok(l) if svc.here != 0 => l,
            _ => std::net::TcpListener::bind(("127.0.0.1", 0))
                .map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?,
        };
        let listener = listener
            .set_nonblocking(true)
            .and_then(|()| tokio::net::TcpListener::from_std(listener))
            .map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        let port = listener.local_addr().map_or(0, |a| a.port());
        let (stop, stopped) = watch::channel(());
        {
            let mut f = forwards().lock().unwrap();
            // Connected meanwhile, by another request: that forward stays.
            if let Some(other) = f.get(&key) {
                ports.push((svc.name.clone(), other.port));
                continue;
            }
            f.insert(key.clone(), Forward { port, _stop: stop });
        }
        let (there, lease, node) = (svc.port, lane.lease.clone(), lane.node.clone());
        serve(listener, key, port, stopped, move |mut tcp, mut stopped| {
            let (client, lease, node) = (client.clone(), lease.clone(), node.clone());
            async move {
                match client.open_shared(&lease, there).await {
                    Ok(up) => {
                        let mut up = hyper_util::rt::TokioIo::new(up);
                        tokio::select! {
                            _ = tokio::io::copy_bidirectional(&mut up, &mut tcp) => {}
                            _ = stopped.changed() => {}
                        }
                    }
                    Err(e) => {
                        let error = match &e {
                            ClientError::Status(code, body) => {
                                devstack_daemon::remote::tunnel_error(
                                    &format!("the node answered {code}"),
                                    body,
                                    &node,
                                )
                            }
                            other => {
                                devstack_daemon::remote::tunnel_error(&other.to_string(), "", &node)
                            }
                        };
                        crate::notice::tell(
                            "error",
                            format!("The shared lane's port {there} could not be reached: {error}"),
                        );
                        devstack_daemon::diag::warn(
                            "shared_lane_tunnel",
                            json!({ "lease": lease, "port": there, "error": error,
                                "version": version::VERSION,
                                "node_version": node.devstack.as_ref().map(|r| r.version.clone()) }),
                        );
                    }
                }
            }
        });
        opened.push(json!({ "service": svc.name, "port": port, "owner_port": svc.here }));
        ports.push((svc.name.clone(), port));
    }
    if !opened.is_empty() {
        devstack_daemon::diag::info(
            "shared_lane_connected",
            json!({ "lane": lane.lane, "owner": lane.owner_name, "opened": opened }),
        );
        if let Some(w) = release_warning(lane) {
            devstack_daemon::diag::warn(
                "pool_version_mismatch",
                json!({ "lane": lane.lane, "node": lane.node.name, "warning": w,
                    "version": version::VERSION,
                    "node_version": lane.node.devstack.as_ref().map(|r| r.version.clone()),
                    "owner_version": lane.owner_devstack.as_ref().map(|r| r.version.clone()) }),
            );
        }
    }
    Ok(ports)
}

/// `POST /hub/pool/shared/{lease}/connect`: connect to the lane (ADR-0053),
/// `{services: [{name, port, url}], warning?}`.
pub async fn connect(Path(lease): Path<String>, Query(q): Query<PoolQuery>) -> Response {
    let (local, lane) = match shared_now(&q, &lease).await {
        Ok(x) => x,
        Err(r) => return r,
    };
    remember(q.name(), &lane);
    let ports = match connect_lane(q.name(), &local, &lane) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let services: Vec<_> = ports
        .iter()
        .filter_map(|(name, port)| {
            let svc = lane.services.iter().find(|s| &s.name == name)?;
            let scheme = web(&svc.scheme)?;
            Some(json!({ "name": name, "port": port, "url": format!("{scheme}://localhost:{port}") }))
        })
        .collect();
    let mut v = json!({ "services": services });
    if let Some(w) = release_warning(&lane) {
        v["warning"] = json!(w);
    }
    Json(v).into_response()
}

/// `POST /hub/pool/shared/{lease}/disconnect`: its forwards close, the
/// connections through them too (ADR-0053), `{closed}`: how many.
pub async fn disconnect(Path(lease): Path<String>, Query(q): Query<PoolQuery>) -> Response {
    if let Err(r) = q.local() {
        return *r;
    }
    forget(q.name(), &lease);
    let closed = close(q.name(), &lease);
    if closed > 0 {
        devstack_daemon::diag::info(
            "shared_lane_disconnected",
            json!({ "lease": lease, "closed": closed }),
        );
    }
    Json(json!({ "closed": closed })).into_response()
}

/// `POST /hub/pool/shared/{lease}/open` `{service}`: connect the lane if it
/// is not (ADR-0053 point 2), `{url, port, warning?}` of that service.
pub async fn open(
    Path(lease): Path<String>,
    Query(q): Query<PoolQuery>,
    Json(b): Json<OpenBody>,
) -> Response {
    let (local, lane) = match shared_now(&q, &lease).await {
        Ok(x) => x,
        Err(r) => return r,
    };
    let Some(svc) = lane.services.iter().find(|s| s.name == b.service).cloned() else {
        return error(
            StatusCode::NOT_FOUND,
            format!("lane `{}` shares no service `{}`", lane.lane, b.service),
        );
    };
    let Some(scheme) = web(&svc.scheme) else {
        return error(
            StatusCode::BAD_GATEWAY,
            format!(
                "lane `{}` shares `{}` with the scheme `{}`: only http and https are opened",
                lane.lane, svc.name, svc.scheme
            ),
        );
    };
    remember(q.name(), &lane);
    let ports = match connect_lane(q.name(), &local, &lane) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let Some(port) = ports.iter().find(|(n, _)| *n == svc.name).map(|(_, p)| *p) else {
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("`{}` was not forwarded", svc.name),
        );
    };
    devstack_daemon::diag::info(
        "shared_lane_opened",
        json!({ "lane": lane.lane, "owner": lane.owner_name, "service": svc.name, "port": port }),
    );
    let mut v = json!({ "url": format!("{scheme}://localhost:{port}"), "port": port });
    if let Some(w) = release_warning(&lane) {
        v["warning"] = json!(w);
    }
    Json(v).into_response()
}

/// What to tell a member opening `lane` about the releases involved: its
/// node's against this machine's, and its owner's against its node's (an
/// owner newer than the node shares ports the node's lane daemon does not
/// serve). `None` when all is the same.
fn release_warning(lane: &SharedLane) -> Option<String> {
    let node = format!("the node {}", lane.node.name);
    let mut notes: Vec<String> = version::note(&node, lane.node.devstack.as_ref())
        .into_iter()
        .collect();
    if let (Some(owner), Some(on)) = (&lane.owner_devstack, &lane.node.devstack)
        && version::compat_with(Some(owner), on) == Compat::Incompatible
    {
        notes.push(format!(
            "{} shares the lane from {owner}, {node} runs {on}: they do not work together",
            lane.owner_name
        ));
    }
    (!notes.is_empty()).then(|| notes.join("; "))
}

#[cfg(test)]
mod view_tests {
    #[test]
    fn the_pool_stays_with_the_hub() {
        assert_eq!(super::without_pool(Some("pool=acme&tail=50")), "?tail=50");
        assert_eq!(super::without_pool(Some("pool=acme")), "");
        assert_eq!(super::without_pool(None), "");
        assert_eq!(
            super::without_pool(Some("tail=5&history=true")),
            "?tail=5&history=true"
        );
    }

    #[test]
    fn no_pool_and_the_default_pool_are_one_pool() {
        let q = |p: Option<&str>| super::PoolQuery {
            pool: p.map(String::from),
        };
        let default = devstack_node::member::DEFAULT_POOL;
        assert_eq!(q(None).name(), default);
        assert_eq!(q(Some("")).name(), default);
        assert_eq!(q(Some(" ")).name(), default);
        assert_eq!(q(Some(default)).name(), default);
        assert_eq!(q(Some(" acme ")).name(), "acme");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use devstack_node::coordinator::{NodeRef, SharedService};
    use devstack_node::version::Release;

    fn lane(node: Option<Release>, owner: Option<Release>) -> SharedLane {
        SharedLane {
            lease: "l".into(),
            repo: "shop".into(),
            lane: "fix".into(),
            owner: "o".into(),
            owner_name: "owner-pc".into(),
            node: NodeRef {
                id: "n".into(),
                name: "build-box".into(),
                api: "https://x:1".into(),
                devstack: node,
            },
            services: Vec::new(),
            owner_devstack: owner,
            legacy_view: true,
        }
    }

    fn service(name: &str, port: u16) -> SharedService {
        SharedService {
            name: name.into(),
            port,
            scheme: "http".into(),
            here: port,
        }
    }

    /// A forward on a free port here that echoes, as `connect_lane` makes
    /// one to a node.
    async fn echo_forward(pool: &str, lease: &str, there: u16) -> u16 {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let key: Key = (pool.into(), lease.into(), there);
        let (stop, stopped) = watch::channel(());
        forwards()
            .lock()
            .unwrap()
            .insert(key.clone(), Forward { port, _stop: stop });
        serve(
            listener,
            key,
            port,
            stopped,
            |mut tcp, mut stopped| async move {
                let (mut r, mut w) = tcp.split();
                tokio::select! {
                    _ = tokio::io::copy(&mut r, &mut w) => {}
                    _ = stopped.changed() => {}
                }
            },
        );
        port
    }

    #[tokio::test]
    async fn disconnecting_closes_the_ports_and_the_connections() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let port = echo_forward("disconnect-test", "l1", 3000).await;
        let other = echo_forward("disconnect-test", "l2", 3000).await;
        assert_eq!(forwarded("disconnect-test", "l1", 3000), Some(port));
        let mut c = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        c.write_all(b"hi").await.unwrap();
        let mut buf = [0u8; 2];
        c.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"hi");

        assert_eq!(close("disconnect-test", "l1"), 1);
        assert_eq!(forwarded("disconnect-test", "l1", 3000), None);
        // The connection in flight ends.
        let n = tokio::time::timeout(std::time::Duration::from_secs(5), c.read(&mut buf))
            .await
            .expect("the connection is cut");
        assert!(matches!(n, Ok(0) | Err(_)), "{n:?}");
        // The port is free again.
        let mut free = false;
        for _ in 0..50 {
            if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() {
                free = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(free, "port {port} is still taken");
        // Another lane's forward stays.
        assert_eq!(forwarded("disconnect-test", "l2", 3000), Some(other));
        assert_eq!(close("disconnect-test", "l2"), 1);
        assert_eq!(close("disconnect-test", "l2"), 0);
    }

    #[tokio::test]
    async fn a_lane_no_longer_shared_is_disconnected() {
        let pool = "prune-test";
        echo_forward(pool, "kept", 3000).await;
        echo_forward(pool, "kept", 3001).await;
        echo_forward(pool, "gone", 3000).await;
        echo_forward("prune-other", "gone", 3000).await;
        let mut kept = lane(None, None);
        kept.lease = "kept".into();
        kept.services = vec![service("web", 3000), service("db", 5432)];
        conns().lock().unwrap().insert(
            pool.into(),
            HashMap::from([(
                LaneId::of(&kept),
                Conn {
                    lease: "kept".into(),
                    ..Conn::default()
                },
            )]),
        );
        let local = Local::new(&std::env::temp_dir().join("devstack-prune-test"));
        reconcile(pool, &local, std::slice::from_ref(&kept)).await;
        assert!(forwarded(pool, "kept", 3000).is_some());
        // A service no longer shared, a lane no connection holds: closed.
        assert_eq!(forwarded(pool, "kept", 3001), None);
        assert_eq!(forwarded(pool, "gone", 3000), None);
        // Another pool's are not this listing's to close.
        assert!(forwarded("prune-other", "gone", 3000).is_some());

        let v = with_forwarded(pool, &[kept]);
        assert!(v[0]["services"][0]["forwarded"].is_u64(), "{v}");
        assert!(v[0]["services"][1].get("forwarded").is_none(), "{v}");
        close(pool, "kept");
        close("prune-other", "gone");
        conns().lock().unwrap().remove(pool);
    }

    #[test]
    fn a_connection_is_to_the_lane_not_its_lease() {
        let t0 = std::time::Instant::now();
        let mut first = lane(None, None);
        first.lease = "a".into();
        let id = LaneId::of(&first);
        let mut c = Conn {
            lease: "a".into(),
            ..Conn::default()
        };
        // Listed: connected.
        let (s, l) = step(&id, &mut c, std::slice::from_ref(&first), t0);
        assert_eq!(s, Step::Connect { old: None });
        assert_eq!(l.unwrap().lease, "a");
        // Missing a while (the coordinator restarted): its ports stay.
        let later = t0 + std::time::Duration::from_secs(30);
        assert_eq!(step(&id, &mut c, &[], later).0, Step::Wait);
        assert_eq!(
            step(&id, &mut c, &[], later + std::time::Duration::from_secs(60)).0,
            Step::Wait
        );
        // Back under a new lease (started again): the old one's forwards go.
        let mut again = first.clone();
        again.lease = "b".into();
        let (s, _) = step(&id, &mut c, std::slice::from_ref(&again), later);
        assert_eq!(
            s,
            Step::Connect {
                old: Some("a".into())
            }
        );
        assert_eq!(c.lease, "b");
        assert_eq!(c.missing, None);
        // Another owner's lane of the same name is another lane.
        let mut other = again.clone();
        other.owner = "someone-else".into();
        let t1 = t0 + std::time::Duration::from_secs(100);
        assert_eq!(
            step(&id, &mut c, std::slice::from_ref(&other), t1).0,
            Step::Wait
        );
        // Missing past the grace: closed once, then kept until forgotten.
        let past = t1 + GRACE + std::time::Duration::from_secs(1);
        assert_eq!(step(&id, &mut c, &[], past).0, Step::Close);
        assert_eq!(step(&id, &mut c, &[], past).0, Step::Nothing);
        assert_eq!(
            step(
                &id,
                &mut c,
                &[],
                t1 + FORGET + std::time::Duration::from_secs(1)
            )
            .0,
            Step::Forget
        );
        // A hub started again knows no lease: listed, it connects, nothing to close.
        let mut fresh = Conn::default();
        assert_eq!(
            step(&id, &mut fresh, std::slice::from_ref(&again), t0).0,
            Step::Connect { old: None }
        );
    }

    #[test]
    fn only_web_pages_are_opened() {
        assert_eq!(web("HTTPS").as_deref(), Some("https"));
        assert_eq!(web("http").as_deref(), Some("http"));
        assert_eq!(web("ms-msdt"), None);
    }

    #[test]
    fn opening_a_lane_warns_about_other_releases() {
        let this = Release::this();
        assert_eq!(
            release_warning(&lane(Some(this.clone()), Some(this.clone()))),
            None
        );
        let w = release_warning(&lane(None, Some(this.clone()))).unwrap();
        assert!(w.contains("build-box") && w.contains("does not say"), "{w}");
        // The owner newer than the node (finding A1): both named.
        let old = Release {
            version: "0.0.1".into(),
            protocol: this.protocol + 1,
        };
        let w = release_warning(&lane(Some(old), Some(this.clone()))).unwrap();
        assert!(w.contains("owner-pc shares the lane from"), "{w}");
        assert!(w.contains("0.0.1") && w.contains(&this.version), "{w}");
    }
}
