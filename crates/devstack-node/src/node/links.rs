//! A node carries its lanes' public links itself (ADR-0092 points 6 and 7),
//! when its own `policy.json` says `public_links: true`: it takes a tunnel
//! of the pool for itself, reads the open links of its lanes from their
//! daemons, reports their hosts to the coordinator (which points their
//! records at this node's tunnel, the owners' suffixes checked against the
//! leases it runs), and runs the tunnel's connector and a router that
//! reaches each lane's port through its daemon's tunnel. The links answer
//! while the owner's machine is off. Without the policy, or the pool's
//! offer, nothing runs and the owners' machines carry the links.

use super::{Shared, daemon_of};
use crate::linkrouter::{self, Connector, Dial, Upstream};
use crate::publiclinks::{HostsAnswer, HostsReport, Issued, Offer};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How often the lanes' links are read.
const EVERY: Duration = Duration::from_secs(2);
/// The hosts are reported at least this often.
const REPORT_EVERY: Duration = Duration::from_secs(60);
/// What the lane's daemon's tunnel speaks (`devstack-daemon`'s
/// `TUNNEL_PROTOCOL`).
const TUNNEL_PROTOCOL: &str = "devstack-tcp";

/// A link of a lane here: its lease, and the port and TLS of its service.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Route {
    lease: String,
    port: u16,
    tls: bool,
}

struct State {
    connector: Connector,
    issued: Option<Issued>,
    routes: BTreeMap<String, Route>,
    registered: BTreeSet<String>,
    reported: Option<Vec<String>>,
    at: Option<Instant>,
}

/// Follow the lanes' links, for the agent's life.
pub(super) fn start(s: Shared) {
    if s.local_policy.as_ref().and_then(|p| p.public_links) != Some(true) {
        return;
    }
    let dir = s.root.join("public-links");
    let state = Arc::new(Mutex::new(State {
        connector: Connector::new(dir.join("cloudflared.log")),
        issued: None,
        routes: BTreeMap::new(),
        registered: BTreeSet::new(),
        reported: None,
        at: None,
    }));
    tokio::spawn(async move {
        let listener = match tokio::net::TcpListener::bind(("127.0.0.1", 0)).await {
            Ok(l) => l,
            Err(e) => {
                eprintln!("node: the public links' router: {e}");
                return;
            }
        };
        let Ok(router_port) = listener.local_addr().map(|a| a.port()) else {
            return;
        };
        let dial = dialer(Arc::downgrade(&s), state.clone());
        tokio::spawn(linkrouter::serve(listener, dial));
        loop {
            tokio::time::sleep(EVERY).await;
            let routes = links_of(&s).await;
            let report = {
                let mut st = state.lock().unwrap();
                st.routes = routes;
                let hosts: Vec<String> = st.routes.keys().cloned().collect();
                let due = st.reported.as_ref() != Some(&hosts)
                    || st.at.is_none_or(|t| t.elapsed() > REPORT_EVERY);
                (due && (st.issued.is_some() || !hosts.is_empty())).then_some(hosts)
            };
            if let Some(hosts) = report {
                let s2 = s.clone();
                let st2 = state.clone();
                let _ = tokio::task::spawn_blocking(move || sync(&s2, &st2, hosts)).await;
            }
            let mut st = state.lock().unwrap();
            let wanted = !st.registered.is_empty();
            let issued = st.issued.clone();
            st.connector.tick(wanted, false, || {
                let i = issued.ok_or("the pool gave this node no tunnel")?;
                linkrouter::spawn_connector(
                    &dir,
                    &i.tunnel_id,
                    router_port,
                    i.credentials,
                    &dir.join("cloudflared.log"),
                    &linkrouter::proc_options(),
                )
            });
        }
    });
}

/// The pool's tunnel for this node (taken at the first link), then the
/// report of the hosts below its domain; what the coordinator gave a
/// record is routed.
fn sync(s: &Shared, state: &Arc<Mutex<State>>, hosts: Vec<String>) {
    let issued = state.lock().unwrap().issued.clone();
    let issued = match issued {
        Some(i) => i,
        None => {
            let offer: Result<Offer, _> = s.local.get("/v1/public-links");
            if !offer.is_ok_and(|o| o.domain.is_some()) {
                return;
            }
            match s
                .local
                .post::<Issued>("/v1/public-links/tunnel", &serde_json::json!({}))
            {
                Ok(i) => {
                    state.lock().unwrap().issued = Some(i.clone());
                    i
                }
                Err(e) => {
                    eprintln!("node: no tunnel of the pool for public links: {e}");
                    return;
                }
            }
        }
    };
    let below: Vec<String> = hosts
        .iter()
        .filter(|h| h.ends_with(&format!(".{}", issued.domain)))
        .cloned()
        .collect();
    let answer: Result<HostsAnswer, _> = s
        .local
        .put_json("/v1/public-links/hosts", &HostsReport { hosts: below });
    let mut st = state.lock().unwrap();
    st.at = Some(Instant::now());
    if let Ok(a) = answer {
        st.reported = Some(hosts);
        st.registered = a.hosts.into_iter().collect();
    }
}

/// The open links of the lanes here, by host, as their daemons say.
async fn links_of(s: &Shared) -> BTreeMap<String, Route> {
    let leases: Vec<super::NodeLease> = s
        .leases
        .lock()
        .unwrap()
        .values()
        .filter(|l| l.stack_file.is_some())
        .cloned()
        .collect();
    let mut out = BTreeMap::new();
    for l in leases {
        let id = l.id.clone();
        let Ok(d) = daemon_of(s, l).await else {
            continue;
        };
        let Ok(Ok(view)) = tokio::task::spawn_blocking(move || stack_of(&d)).await else {
            continue;
        };
        for svc in view["services"].as_array().into_iter().flatten() {
            let link = &svc["public_link"];
            if !matches!(link["state"].as_str(), Some("open" | "starting")) {
                continue;
            }
            let (Some(host), Some(port)) = (
                link["url"]
                    .as_str()
                    .and_then(|u| u.strip_prefix("https://")),
                svc["port"].as_u64().and_then(|p| u16::try_from(p).ok()),
            ) else {
                continue;
            };
            let tls = svc["url"].as_str().is_some_and(|u| u.starts_with("https:"));
            out.insert(
                host.to_string(),
                Route {
                    lease: id.clone(),
                    port,
                    tls,
                },
            );
        }
    }
    out
}

/// `GET /v1/stack` of a lane's daemon.
fn stack_of(d: &crate::lanerun::DaemonAddr) -> Result<Value, String> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(5)))
        .http_status_as_error(false)
        .build()
        .new_agent();
    let mut r = agent
        .get(&format!("http://{}/v1/stack", d.addr))
        .header(
            "Host",
            &format!("127.0.0.1:{}", crate::lanerun::DAEMON_PORT),
        )
        .header("Authorization", &format!("Bearer {}", d.token))
        .call()
        .map_err(|e| e.to_string())?;
    let text = r.body_mut().read_to_string().map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

/// Where the router sends a host: the lane's port, through its daemon's
/// tunnel, for the hosts the coordinator gave a record only.
fn dialer(s: std::sync::Weak<super::Inner>, state: Arc<Mutex<State>>) -> Dial {
    Arc::new(move |host: String| {
        let s = s.upgrade();
        let route = {
            let st = state.lock().unwrap();
            st.registered
                .contains(&host)
                .then(|| st.routes.get(&host).cloned())
                .flatten()
        };
        Box::pin(async move {
            let s = s.ok_or((503, "the node is stopping".to_string()))?;
            let r = route.ok_or((404, "no public link is open at this address".to_string()))?;
            let lease = s
                .leases
                .lock()
                .unwrap()
                .get(&r.lease)
                .cloned()
                .ok_or((404, "its lane is gone".to_string()))?;
            let d = daemon_of(&s, lease)
                .await
                .map_err(|_| (502, "its lane's daemon does not answer".to_string()))?;
            tunnel_to(&d, r.port, r.tls).await
        })
    })
}

/// A connection to `port` of a lane, through its daemon's tunnel.
async fn tunnel_to(
    d: &crate::lanerun::DaemonAddr,
    port: u16,
    tls: bool,
) -> Result<Upstream, (u16, String)> {
    let bad = |e: String| (502, format!("the lane's daemon: {e}"));
    let tcp = tokio::net::TcpStream::connect(&d.addr)
        .await
        .map_err(|e| bad(e.to_string()))?;
    let _ = tcp.set_nodelay(true);
    let (mut send, conn) = hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(tcp))
        .await
        .map_err(|e| bad(e.to_string()))?;
    tokio::spawn(conn.with_upgrades());
    let req = hyper::Request::builder()
        .method("GET")
        .uri(format!("/v1/tunnel/{port}"))
        .header(
            hyper::header::HOST,
            format!("127.0.0.1:{}", crate::lanerun::DAEMON_PORT),
        )
        .header(hyper::header::AUTHORIZATION, format!("Bearer {}", d.token))
        .header(hyper::header::CONNECTION, "upgrade")
        .header(hyper::header::UPGRADE, TUNNEL_PROTOCOL)
        .body(http_body_util::Empty::<bytes::Bytes>::new())
        .map_err(|e| bad(e.to_string()))?;
    let resp = send
        .send_request(req)
        .await
        .map_err(|e| bad(e.to_string()))?;
    if resp.status() != hyper::StatusCode::SWITCHING_PROTOCOLS {
        return Err(bad(format!("answered {}", resp.status())));
    }
    let up = hyper::upgrade::on(resp)
        .await
        .map_err(|e| bad(e.to_string()))?;
    Ok(Upstream {
        io: Box::new(hyper_util::rt::TokioIo::new(up)),
        port,
        tls,
    })
}
