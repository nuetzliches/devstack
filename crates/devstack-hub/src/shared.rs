//! Lanes others in the pool share with this machine (ADR-0035): listed from
//! the coordinator, opened through a forward on this machine's loopback to
//! the lane's node, the same tunnel the lane's owner uses, checked by the
//! node against the owner's sharing at every connection.
//!
//! - `GET /hub/pool/shared`: the lanes shared with this machine
//! - `POST /hub/pool/shared/{lease}/open` `{service}`: a forward to that
//!   service, on the port its owner has it on if that is free here, else
//!   another one: `{url, port}`. It stays while the hub runs.
//!
//! Both take `?pool=<name>` (ADR-0034).

use axum::Json;
use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use devstack_node::coordinator::SharedLane;
use devstack_node::member::Local;
use devstack_node::nodeclient::NodeClient;
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

#[derive(Deserialize, Default)]
pub struct PoolQuery {
    #[serde(default)]
    pool: Option<String>,
}

impl PoolQuery {
    fn local(&self) -> Result<Local, Box<Response>> {
        let p = self
            .pool
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .unwrap_or(devstack_node::member::DEFAULT_POOL);
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
    let key_dir = format!("{:?}", q.pool);
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
    let key = (key_dir, lease.clone(), svc.port);
    if let Some(port) = forwards().lock().unwrap().get(&key).copied() {
        return Json(json!({ "url": url(port), "port": port })).into_response();
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
    tokio::spawn(async move {
        loop {
            let Ok((mut tcp, _)) = listener.accept().await else {
                break;
            };
            let _ = tcp.set_nodelay(true);
            let (client, lease) = (client.clone(), lease.clone());
            tokio::spawn(async move {
                match client.open_shared(&lease, there).await {
                    Ok(up) => {
                        let mut up = hyper_util::rt::TokioIo::new(up);
                        let _ = tokio::io::copy_bidirectional(&mut up, &mut tcp).await;
                    }
                    Err(e) => devstack_daemon::diag::warn(
                        "shared_lane_tunnel",
                        json!({ "lease": lease, "port": there, "error": e.to_string() }),
                    ),
                }
            });
        }
        forwards().lock().unwrap().remove(&key);
    });
    devstack_daemon::diag::info(
        "shared_lane_opened",
        json!({ "lane": lane.lane, "owner": lane.owner_name, "service": svc.name, "port": port }),
    );
    Json(json!({ "url": url(port), "port": port })).into_response()
}
