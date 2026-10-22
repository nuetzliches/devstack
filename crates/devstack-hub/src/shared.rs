//! Lanes others in the pool share with this machine (ADR-0035): listed from
//! the coordinator, opened through a forward on this machine's loopback to
//! the lane's node, the same tunnel the lane's owner uses, checked by the
//! node against the owner's sharing at every connection.
//!
//! A member connects to a lane and disconnects again (ADR-0053): connected,
//! each service it shares has a forward, on the port its owner has it on if
//! that is free here, else another one; nothing is forwarded before.
//!
//! - `GET /hub/pool/shared`: the lanes shared with this machine, each
//!   service with `forwarded`, its port here, while the lane is connected
//! - `POST /hub/pool/shared/{lease}/connect`: connect to the lane:
//!   `{services: [{name, port, url}], warning?}`. It stays connected until
//!   it is disconnected, the hub stops, or it is no longer shared with this
//!   machine.
//! - `POST /hub/pool/shared/{lease}/disconnect`: its forwards close, the
//!   connections through them too: `{closed}`.
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
        let state = devstack_daemon::token::token_path()
            .parent()
            .expect("token path has a parent")
            .to_path_buf();
        Ok(Local::new(&devstack_node::member::pool_dir(&state, p)))
    }
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

/// Close the forwards of pool `pool` that `lanes` no longer share: a lane
/// gone or no longer shared with this machine, a service no longer shared.
fn prune(pool: &str, lanes: &[SharedLane]) {
    forwards().lock().unwrap().retain(|(p, l, port), _| {
        p != pool
            || lanes
                .iter()
                .any(|x| &x.lease == l && x.services.iter().any(|s| s.port == *port))
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
            prune(pool, &v);
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
    prune(pool, &lanes);
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
        prune(pool, &[kept.clone()]);
        assert!(forwarded(pool, "kept", 3000).is_some());
        // A service no longer shared, a lane no longer shared: closed.
        assert_eq!(forwarded(pool, "kept", 3001), None);
        assert_eq!(forwarded(pool, "gone", 3000), None);
        // Another pool's are not this listing's to close.
        assert!(forwarded("prune-other", "gone", 3000).is_some());

        let v = with_forwarded(pool, &[kept]);
        assert!(v[0]["services"][0]["forwarded"].is_u64(), "{v}");
        assert!(v[0]["services"][1].get("forwarded").is_none(), "{v}");
        close(pool, "kept");
        close("prune-other", "gone");
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
