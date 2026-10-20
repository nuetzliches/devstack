//! Lanes others in the pool share with this machine (ADR-0035): listed from
//! the coordinator, opened through a forward on this machine's loopback to
//! the lane's node, the same tunnel the lane's owner uses, checked by the
//! node against the owner's sharing at every connection.
//!
//! - `GET /hub/pool/shared`: the lanes shared with this machine
//! - `POST /hub/pool/shared/{lease}/open` `{service}`: a forward to that
//!   service, on the port its owner has it on if that is free here, else
//!   another one: `{url, port, warning?}`. It stays while the hub runs.
//!   `warning`: the node, or the lane's owner, runs another devstack than
//!   this machine, or does not say which (ADR-0031, amended 2026-10-19).
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

/// Forwards open now: (pool, lease, port on the node) -> port here.
type Forwards = Mutex<HashMap<(String, String, u16), u16>>;

fn forwards() -> &'static Forwards {
    static F: OnceLock<Forwards> = OnceLock::new();
    F.get_or_init(Default::default)
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
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => error(StatusCode::BAD_GATEWAY, e.to_string()),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[derive(Deserialize)]
pub struct OpenBody {
    service: String,
}

pub async fn open(
    Path(lease): Path<String>,
    Query(q): Query<PoolQuery>,
    Json(b): Json<OpenBody>,
) -> Response {
    let local = match q.local() {
        Ok(l) => l,
        Err(r) => return *r,
    };
    let key_dir = q.name().to_string();
    let found = tokio::task::spawn_blocking(move || {
        let lanes: Vec<SharedLane> = local.shared()?;
        Ok::<_, devstack_node::member::MemberError>((local, lanes))
    })
    .await;
    let (local, lanes) = match found {
        Ok(Ok(x)) => x,
        Ok(Err(e)) => return error(StatusCode::BAD_GATEWAY, e.to_string()),
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let Some(lane) = lanes.into_iter().find(|l| l.lease == lease) else {
        return error(
            StatusCode::NOT_FOUND,
            format!("no lane `{lease}` is shared with this machine"),
        );
    };
    let Some(svc) = lane.services.iter().find(|s| s.name == b.service).cloned() else {
        return error(
            StatusCode::NOT_FOUND,
            format!("lane `{}` shares no service `{}`", lane.lane, b.service),
        );
    };
    // The scheme comes from the lane owner's daemon, through the
    // coordinator: only a web page's. Anything else would reach a protocol
    // handler of this machine (`ms-msdt:`, `search-ms:`) when opened.
    let scheme = svc.scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return error(
            StatusCode::BAD_GATEWAY,
            format!(
                "lane `{}` shares `{}` with the scheme `{}`: only http and https are opened",
                lane.lane, svc.name, svc.scheme
            ),
        );
    }
    let url = |port: u16| format!("{scheme}://localhost:{port}");
    let warning = release_warning(&lane);
    let answer = |port: u16| {
        let mut v = json!({ "url": url(port), "port": port });
        if let Some(w) = &warning {
            v["warning"] = json!(w);
        }
        Json(v).into_response()
    };
    let key = (key_dir, lease.clone(), svc.port);
    if let Some(port) = forwards().lock().unwrap().get(&key).copied() {
        return answer(port);
    }
    let client = match NodeClient::new(&local, lane.node.clone()) {
        Ok(c) => Arc::new(c),
        Err(e) => return error(StatusCode::BAD_GATEWAY, e.to_string()),
    };
    // The owner's port if it is free here: links between its services work
    // alike. Else any.
    let listener = match tokio::net::TcpListener::bind(("127.0.0.1", svc.here)).await {
        Ok(l) if svc.here != 0 => l,
        _ => match tokio::net::TcpListener::bind(("127.0.0.1", 0)).await {
            Ok(l) => l,
            Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        },
    };
    let port = listener.local_addr().map_or(0, |a| a.port());
    forwards().lock().unwrap().insert(key.clone(), port);
    let there = svc.port;
    let node = lane.node.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut tcp, _)) = listener.accept().await else {
                break;
            };
            let _ = tcp.set_nodelay(true);
            let (client, lease, node) = (client.clone(), lease.clone(), node.clone());
            tokio::spawn(async move {
                match client.open_shared(&lease, there).await {
                    Ok(up) => {
                        let mut up = hyper_util::rt::TokioIo::new(up);
                        let _ = tokio::io::copy_bidirectional(&mut up, &mut tcp).await;
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
                        devstack_daemon::diag::warn(
                            "shared_lane_tunnel",
                            json!({ "lease": lease, "port": there, "error": error,
                                "version": version::VERSION,
                                "node_version": node.devstack.as_ref().map(|r| r.version.clone()) }),
                        );
                    }
                }
            });
        }
        forwards().lock().unwrap().remove(&key);
    });
    devstack_daemon::diag::info(
        "shared_lane_opened",
        json!({ "lane": lane.lane, "owner": lane.owner_name, "service": svc.name, "port": port }),
    );
    if let Some(w) = &warning {
        devstack_daemon::diag::warn(
            "pool_version_mismatch",
            json!({ "lane": lane.lane, "node": lane.node.name, "warning": w,
                "version": version::VERSION,
                "node_version": lane.node.devstack.as_ref().map(|r| r.version.clone()),
                "owner_version": lane.owner_devstack.as_ref().map(|r| r.version.clone()) }),
        );
    }
    answer(port)
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
    use devstack_node::coordinator::NodeRef;
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
