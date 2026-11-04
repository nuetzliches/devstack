//! Public links through the user's named tunnel (ADR-0090, step 2): a
//! Cloudflare Tunnel of their own account, set up once (`devstack tunnel
//! --setup`), its credentials in the keychain, run by the hub, one
//! connector per machine. Two connectors of one tunnel would share its
//! requests at random, so no daemon runs one.
//!
//! The tunnel sends every host below `*.<domain>` to the router here, on a
//! loopback port of its own. The router knows which links are open from
//! the events of every tree the hub follows anyway (`notify`): a service's
//! port from the snapshot, its link from `public_link`. A host no open link
//! has is answered 404, and so is one two trees would claim. The connector
//! runs while one link is open, and ends a while after the last closed.
//!
//! No route hands the credentials out, and the router serves nothing of
//! the hub's or a daemon's API: it only forwards to the services' ports.
//!
//! A tunnel a pool gave (ADR-0092, once the member accepted it) works the
//! same, but its hosts need a DNS record the pool's coordinator makes: the
//! hub reports the hosts it carries whenever they change and every minute,
//! and routes only those the coordinator gave a record; why it refused one
//! goes to the daemon, which fails the link with it.

use crate::secrets::Secrets;
use crate::{AppState, err};
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use devstack_daemon::namedtunnel::{self, Config};
use http_body_util::{BodyExt, Full, combinators::BoxBody};
use hyper::body::Incoming;
use hyper::{Request, header};
use hyper_util::rt::TokioIo;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The keychain entry of the tunnel's credentials.
const ACCOUNT: &str = "credentials";
/// How long the connector runs on after the last link closed: a link
/// closed and opened again (a restart of its lane) keeps it.
const GRACE: Duration = Duration::from_secs(20);
/// How long `cloudflared` gets to register a connection.
const OPEN_TIMEOUT: Duration = Duration::from_secs(45);
/// A failed connector is started again no sooner than this.
const RETRY: Duration = Duration::from_secs(30);
/// The hosts are reported to the pool at least this often.
const REPORT_EVERY: Duration = Duration::from_secs(60);

type Body = BoxBody<Bytes, hyper::Error>;

/// What the hub knows of one tree (a stack's main tree, or a lane).
#[derive(Default)]
struct Tree {
    /// Service -> its port in this tree, and whether it speaks TLS.
    ports: BTreeMap<String, (u16, bool)>,
    /// Service -> the host of its open (or starting) link.
    open: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "state")]
enum ConnState {
    Off,
    Starting,
    Running,
    Failed { error: String },
}

struct Connector {
    tree: Option<devstack_proc::Owned>,
    state: ConnState,
    /// Unix ms of the last change of `state`.
    since: u128,
    started: Option<Instant>,
    failed_at: Option<Instant>,
    /// No link open since then.
    idle_since: Option<Instant>,
}

/// What the pool's coordinator said of the hosts last.
#[derive(Default)]
struct PoolSync {
    reported: Option<Vec<String>>,
    at: Option<Instant>,
    /// Hosts with a record.
    registered: std::collections::BTreeSet<String>,
    /// Host -> why it has none.
    refused: BTreeMap<String, String>,
    error: Option<String>,
    busy: bool,
}

pub(crate) struct PublicLinks {
    dir: PathBuf,
    secrets: Secrets,
    config: Mutex<Option<Config>>,
    trees: Mutex<BTreeMap<(String, String), Tree>>,
    connector: Mutex<Connector>,
    router_port: u16,
    pool: Mutex<PoolSync>,
}

impl PublicLinks {
    /// Bind the router and follow the connector, for the hub's life.
    pub(crate) async fn start(dir: PathBuf) -> Result<Arc<Self>, String> {
        let dir = dir.join("tunnel");
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(|e| format!("the public links' router: {e}"))?;
        let router_port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let me = Arc::new(Self {
            secrets: Secrets::open_as("devstack-tunnel", dir.clone()),
            dir,
            config: Mutex::new(namedtunnel::read()),
            trees: Mutex::new(BTreeMap::new()),
            connector: Mutex::new(Connector {
                tree: None,
                state: ConnState::Off,
                since: crate::now_ms(),
                started: None,
                failed_at: None,
                idle_since: None,
            }),
            router_port,
            pool: Mutex::default(),
        });
        tokio::spawn(serve(me.clone(), listener));
        let watcher = me.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(500)).await;
                watcher.tick();
                watcher.sync_pool();
            }
        });
        Ok(me)
    }

    /// An event of tree (`id`, `lane`; `""` the main tree): a snapshot says
    /// its services' ports and open links, a `public_link` event one link.
    pub(crate) fn apply(&self, id: &str, lane: &str, ev: &Value) {
        let Some(domain) = self
            .config
            .lock()
            .unwrap()
            .as_ref()
            .map(|c| c.domain.clone())
        else {
            return;
        };
        let named = |link: &Value| -> Option<String> {
            if !matches!(link["state"].as_str(), Some("starting" | "open")) {
                return None;
            }
            let host = link["url"].as_str()?.strip_prefix("https://")?;
            host.strip_suffix(&domain)?
                .ends_with('.')
                .then(|| host.to_string())
        };
        {
            let mut trees = self.trees.lock().unwrap();
            match ev["type"].as_str() {
                Some("snapshot") => {
                    let mut t = Tree::default();
                    for s in ev["stack"]["services"].as_array().into_iter().flatten() {
                        let Some(name) = s["name"].as_str() else {
                            continue;
                        };
                        if let Some(port) = s["port"].as_u64().and_then(|p| u16::try_from(p).ok()) {
                            let tls = s["url"].as_str().is_some_and(|u| u.starts_with("https:"));
                            t.ports.insert(name.into(), (port, tls));
                        }
                        if let Some(host) = named(&s["public_link"]) {
                            t.open.insert(name.into(), host);
                        }
                    }
                    trees.insert((id.into(), lane.into()), t);
                }
                Some("public_link") => {
                    let Some(service) = ev["service"].as_str() else {
                        return;
                    };
                    let t = trees.entry((id.into(), lane.into())).or_default();
                    match named(&ev["link"]) {
                        Some(host) => t.open.insert(service.into(), host),
                        None => t.open.remove(service),
                    };
                }
                _ => return,
            }
        }
        self.tick();
    }

    /// The tree's stream ended: none of its links is carried any longer.
    pub(crate) fn clear(&self, id: &str, lane: &str) {
        let gone = self
            .trees
            .lock()
            .unwrap()
            .remove(&(id.to_string(), lane.to_string()));
        if gone.is_some() {
            self.tick();
        }
    }

    /// The hosts of the open links.
    fn open_hosts(&self) -> Vec<String> {
        let trees = self.trees.lock().unwrap();
        let mut h: Vec<String> = trees
            .values()
            .flat_map(|t| t.open.values().cloned())
            .collect();
        h.sort();
        h.dedup();
        h
    }

    /// The hosts the tunnel carries: the open ones, with a pool's tunnel
    /// those its coordinator gave a record.
    fn hosts(&self) -> Vec<String> {
        let mut h = self.open_hosts();
        if self.pool_name().is_some() {
            let p = self.pool.lock().unwrap();
            h.retain(|x| p.registered.contains(x));
        }
        h
    }

    /// The pool whose tunnel this is, if it is one.
    fn pool_name(&self) -> Option<String> {
        self.config
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|c| c.pool.clone())
    }

    /// Report the open hosts to the pool when they changed, and every
    /// minute; what it answers decides what is routed.
    fn sync_pool(self: &Arc<Self>) {
        let Some(pool) = self.pool_name() else {
            return;
        };
        let open = self.open_hosts();
        {
            let mut p = self.pool.lock().unwrap();
            let due = p.reported.as_ref() != Some(&open)
                || p.at.is_none_or(|t| t.elapsed() > REPORT_EVERY);
            if !due || p.busy {
                return;
            }
            p.busy = true;
        }
        let me = self.clone();
        tokio::task::spawn_blocking(move || {
            let answer = report_hosts(&pool, &open);
            let mut p = me.pool.lock().unwrap();
            p.busy = false;
            p.at = Some(Instant::now());
            match answer {
                Ok(a) => {
                    p.reported = Some(open);
                    p.registered = a.hosts.into_iter().collect();
                    p.refused = a.refused;
                    p.error = None;
                }
                // Tried again in a minute; what had a record keeps it.
                Err(e) => p.error = Some(e),
            }
        });
    }

    /// Where `host` goes: the port of the one open link that has it.
    fn route(&self, host: &str) -> Result<(u16, bool), &'static str> {
        if self.pool_name().is_some() && !self.pool.lock().unwrap().registered.contains(host) {
            return Err("no public link is open at this address");
        }
        let trees = self.trees.lock().unwrap();
        let mut found = None;
        for t in trees.values() {
            for (service, h) in &t.open {
                if h != host {
                    continue;
                }
                let Some(&target) = t.ports.get(service) else {
                    continue;
                };
                if found.is_some_and(|f| f != target) {
                    return Err("two stacks claim this address; close one of their links");
                }
                found = Some(target);
            }
        }
        found.ok_or("no public link is open at this address")
    }

    /// Start, follow and end the connector: it runs while the tunnel is
    /// set up and a link is open.
    fn tick(&self) {
        let wanted = self.config.lock().unwrap().is_some() && !self.open_hosts().is_empty();
        let mut c = self.connector.lock().unwrap();
        if let Some(tree) = c.tree.as_mut() {
            if let Some(code) = tree.try_exit_code() {
                let out =
                    std::fs::read_to_string(self.dir.join("cloudflared.log")).unwrap_or_default();
                let error = namedtunnel::cloudflared_error(&out)
                    .unwrap_or_else(|| format!("cloudflared ended (exit code {code})"));
                c.tree = None;
                set(&mut c, ConnState::Failed { error });
                c.failed_at = Some(Instant::now());
            } else if c.state == ConnState::Starting {
                let out =
                    std::fs::read_to_string(self.dir.join("cloudflared.log")).unwrap_or_default();
                if out.contains("Registered tunnel connection") {
                    set(&mut c, ConnState::Running);
                } else if c.started.is_some_and(|t| t.elapsed() > OPEN_TIMEOUT) {
                    if let Some(mut t) = c.tree.take() {
                        t.kill();
                    }
                    let error = namedtunnel::cloudflared_error(&out).unwrap_or_else(|| {
                        format!(
                            "no connection registered after {} s",
                            OPEN_TIMEOUT.as_secs()
                        )
                    });
                    set(&mut c, ConnState::Failed { error });
                    c.failed_at = Some(Instant::now());
                }
            }
        }
        if wanted {
            c.idle_since = None;
            let may_retry = c.failed_at.is_none_or(|t| t.elapsed() > RETRY);
            if c.tree.is_none() && may_retry {
                match self.spawn() {
                    Ok(tree) => {
                        c.tree = Some(tree);
                        c.started = Some(Instant::now());
                        c.failed_at = None;
                        set(&mut c, ConnState::Starting);
                    }
                    Err(error) => {
                        set(&mut c, ConnState::Failed { error });
                        c.failed_at = Some(Instant::now());
                    }
                }
            }
        } else if c.tree.is_some() {
            let idle = *c.idle_since.get_or_insert_with(Instant::now);
            if idle.elapsed() > GRACE || self.config.lock().unwrap().is_none() {
                if let Some(mut t) = c.tree.take() {
                    t.kill();
                }
                set(&mut c, ConnState::Off);
            }
        } else if !matches!(c.state, ConnState::Off) && !wanted && c.failed_at.is_none() {
            set(&mut c, ConnState::Off);
        }
    }

    /// `cloudflared tunnel run` for the tunnel, its credentials in the
    /// environment (not on its command line), every host to the router.
    fn spawn(&self) -> Result<devstack_proc::Owned, String> {
        let config = self
            .config
            .lock()
            .unwrap()
            .clone()
            .ok_or("no named tunnel is set up")?;
        let credentials = self
            .secrets
            .get(ACCOUNT)?
            .ok_or("the named tunnel's credentials are gone from the keychain: set it up again (`devstack tunnel --setup`)")?;
        let program = namedtunnel::cloudflared()?;
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        // Its own file: `cloudflared` would otherwise read the user's
        // `~/.cloudflared/config.yml`, and its ingress rules.
        let file = self.dir.join("cloudflared.yml");
        std::fs::write(
            &file,
            format!(
                "tunnel: {id}\nno-autoupdate: true\ningress:\n  - service: http://127.0.0.1:{port}\n",
                id = config.tunnel_id,
                port = self.router_port
            ),
        )
        .map_err(|e| e.to_string())?;
        let log =
            std::fs::File::create(self.dir.join("cloudflared.log")).map_err(|e| e.to_string())?;
        let mut env = BTreeMap::new();
        env.insert("TUNNEL_CRED_CONTENTS".to_string(), credentials);
        let (tree, _) = devstack_proc::spawn(
            devstack_proc::Spec {
                name: "public-links".into(),
                command: format!(
                    "{} tunnel --config {} run",
                    namedtunnel::quoted(&program),
                    namedtunnel::quoted(&file)
                ),
                cwd: self.dir.clone(),
                env,
                log,
            },
            &proc_options(),
        )
        .map_err(|e| e.to_string())?;
        Ok(tree)
    }

    fn view(&self) -> Value {
        let config = self.config.lock().unwrap().clone();
        let c = self.connector.lock().unwrap();
        let links: Vec<Value> = self
            .trees
            .lock()
            .unwrap()
            .iter()
            .flat_map(|((id, lane), t)| {
                t.open.iter().map(move |(service, host)| {
                    json!({ "stack": id, "lane": if lane.is_empty() { "main" } else { lane }, "service": service, "host": host })
                })
            })
            .collect();
        let mut connector = serde_json::to_value(&c.state).unwrap_or_default();
        connector["since"] = json!(c.since);
        connector["pid"] = json!(c.tree.as_ref().map(|t| t.root_pid()));
        drop(c);
        let (refused, pool_error) = {
            let p = self.pool.lock().unwrap();
            (p.refused.clone(), p.error.clone())
        };
        json!({
            "pool": config.as_ref().and_then(|c| c.pool.clone()),
            "suffix": config.as_ref().and_then(|c| c.suffix.clone()),
            "refused": refused,
            "pool_error": pool_error,
            "configured": config.is_some(),
            "domain": config.as_ref().map(|c| c.domain.clone()),
            "tunnel_id": config.as_ref().map(|c| c.tunnel_id.clone()),
            "credentials_in": match self.secrets.kind() {
                crate::secrets::Kind::Keychain => "keychain",
                crate::secrets::Kind::File => "file",
            },
            "connector": connector,
            "hosts": self.hosts(),
            "links": links,
            // Where the tunnel sends every host (loopback; for diagnosis).
            "router_port": self.router_port,
        })
    }

    fn stop_connector(&self) {
        let mut c = self.connector.lock().unwrap();
        if let Some(mut t) = c.tree.take() {
            t.kill();
        }
        c.failed_at = None;
        set(&mut c, ConnState::Off);
    }
}

fn set(c: &mut Connector, state: ConnState) {
    if c.state != state {
        c.state = state;
        c.since = crate::now_ms();
    }
}

/// The hub owns `cloudflared`'s tree as the daemon owns a service's
/// (ADR-0009): on Unix through devstack's shim.
fn proc_options() -> devstack_proc::Options {
    let shim = std::env::current_exe()
        .map(|exe| vec![exe.to_string_lossy().into_owned(), "shim".into()])
        .unwrap_or_default();
    devstack_proc::Options {
        cgroup_root: None,
        shim,
    }
}

// ------------------------------------------------------------- the API

/// `GET /hub/public-links`: the named tunnel (no secret), its connector,
/// the hosts it carries.
pub(crate) async fn get(State(s): State<AppState>) -> Response {
    let mut v = s.links.view();
    let names: BTreeMap<String, String> = s
        .stacks
        .borrow()
        .iter()
        .map(|e| (e.id.clone(), e.name.clone()))
        .collect();
    for l in v["links"].as_array_mut().into_iter().flatten() {
        if let Some(name) = l["stack"].as_str().and_then(|id| names.get(id)) {
            l["stack_name"] = json!(name);
        }
    }
    Json(v).into_response()
}

#[derive(Deserialize)]
pub(crate) struct Setup {
    /// The JSON `cloudflared tunnel create` wrote (`AccountTag`,
    /// `TunnelSecret`, `TunnelID`).
    credentials: String,
    /// `dev-example.com`: its DNS has `*.dev-example.com` routed to the
    /// tunnel (`cloudflared tunnel route dns <tunnel> "*.dev-example.com"`).
    domain: String,
}

/// `PUT /hub/public-links/tunnel`: set the named tunnel up, or anew.
pub(crate) async fn put(State(s): State<AppState>, Json(b): Json<Setup>) -> Response {
    let domain = b
        .domain
        .trim()
        .trim_start_matches("*.")
        .to_ascii_lowercase();
    if !namedtunnel::valid_domain(&domain) {
        return err(
            StatusCode::BAD_REQUEST,
            format!(
                "`{domain}` is no domain (`dev-example.com`, the parent of the tunnel's wildcard)"
            ),
        );
    }
    let creds: Value = match serde_json::from_str(&b.credentials) {
        Ok(v) => v,
        Err(e) => {
            return err(
                StatusCode::BAD_REQUEST,
                format!("the credentials are no JSON: {e}"),
            );
        }
    };
    let field = |k: &str| {
        creds[k]
            .as_str()
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    };
    let (Some(account), Some(secret), Some(id)) = (
        field("AccountTag"),
        field("TunnelSecret"),
        field("TunnelID"),
    ) else {
        return err(
            StatusCode::BAD_REQUEST,
            "the credentials need `AccountTag`, `TunnelSecret` and `TunnelID`, as the file `cloudflared tunnel create` writes has them (not `cert.pem`)",
        );
    };
    let compact =
        json!({ "AccountTag": account, "TunnelSecret": secret, "TunnelID": id }).to_string();
    let links = s.links.clone();
    let result = tokio::task::spawn_blocking(move || -> Result<(), String> {
        links.secrets.set(ACCOUNT, &compact)?;
        let config = Config {
            domain,
            tunnel_id: id,
            suffix: None,
            pool: None,
        };
        namedtunnel::write(&config).map_err(|e| e.to_string())?;
        *links.config.lock().unwrap() = Some(config);
        links.stop_connector();
        links.tick();
        Ok(())
    })
    .await;
    match result {
        Ok(Ok(())) => Json(s.links.view()).into_response(),
        Ok(Err(e)) => err(StatusCode::INTERNAL_SERVER_ERROR, e),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// `PUT /v1/public-links/hosts` to the pool: the hosts this machine carries.
fn report_hosts(
    pool: &str,
    hosts: &[String],
) -> Result<devstack_node::publiclinks::HostsAnswer, String> {
    let local = devstack_node::member::Local::new(&devstack_node::member::pool_dir(
        &crate::poolfeed::state(),
        pool,
    ));
    local
        .put_json(
            "/v1/public-links/hosts",
            &devstack_node::publiclinks::HostsReport {
                hosts: hosts.to_vec(),
            },
        )
        .map_err(|e| e.to_string())
}

/// `GET /hub/public-links/pools`: what each pool this machine is in offers
/// of public links (ADR-0092), and why not where it offers none.
pub(crate) async fn pools() -> Response {
    let found = tokio::task::spawn_blocking(|| {
        devstack_node::member::pools(&crate::poolfeed::state())
            .into_iter()
            .map(|(name, dir)| {
                let local = devstack_node::member::Local::new(&dir);
                match local.get::<devstack_node::publiclinks::Offer>("/v1/public-links") {
                    Ok(o) => json!({ "pool": name, "offer": o }),
                    Err(e) => json!({ "pool": name, "error": e.to_string() }),
                }
            })
            .collect::<Vec<Value>>()
    })
    .await;
    match found {
        Ok(v) => Json(v).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[derive(Deserialize)]
pub(crate) struct UsePool {
    pool: String,
}

/// `PUT /hub/public-links/pool`: take the tunnel a pool offers (the member
/// accepts it, ADR-0092 point 3): its credentials into the keychain, its
/// domain and the member's suffix beside the token. The user's own tunnel
/// goes first: one set up is not replaced.
pub(crate) async fn use_pool(State(s): State<AppState>, Json(b): Json<UsePool>) -> Response {
    let links = s.links.clone();
    let result = tokio::task::spawn_blocking(move || -> Result<(), (StatusCode, String)> {
        if let Some(c) = links.config.lock().unwrap().as_ref()
            && c.pool.as_deref() != Some(b.pool.as_str())
        {
            return Err((
                StatusCode::CONFLICT,
                match &c.pool {
                    None => "your own named tunnel is set up, and goes first: `devstack tunnel --remove-setup` to use the pool's instead".to_string(),
                    Some(p) => format!("the tunnel of pool `{p}` is in use: `devstack tunnel --remove-setup` first"),
                },
            ));
        }
        let dir = devstack_node::member::pools(&crate::poolfeed::state())
            .into_iter()
            .find(|(n, _)| *n == b.pool)
            .map(|(_, d)| d)
            .ok_or((StatusCode::NOT_FOUND, format!("this machine is in no pool `{}`", b.pool)))?;
        let issued: devstack_node::publiclinks::Issued = devstack_node::member::Local::new(&dir)
            .post("/v1/public-links/tunnel", &json!({}))
            .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
        links
            .secrets
            .set(ACCOUNT, &issued.credentials)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
        let config = Config {
            domain: issued.domain,
            tunnel_id: issued.tunnel_id,
            suffix: Some(issued.suffix),
            pool: Some(b.pool),
        };
        namedtunnel::write(&config).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        *links.config.lock().unwrap() = Some(config);
        *links.pool.lock().unwrap() = PoolSync::default();
        links.stop_connector();
        links.tick();
        Ok(())
    })
    .await;
    match result {
        Ok(Ok(())) => Json(s.links.view()).into_response(),
        Ok(Err((status, e))) => err(status, e),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// `DELETE /hub/public-links/tunnel`: forget the named tunnel: its
/// credentials, its domain, the connector. New links are Quick Tunnels. A
/// pool's tunnel is told first that this machine carries no hosts.
pub(crate) async fn delete(State(s): State<AppState>) -> Response {
    let links = s.links.clone();
    let result = tokio::task::spawn_blocking(move || {
        if let Some(pool) = links.pool_name() {
            let _ = report_hosts(&pool, &[]);
        }
        *links.pool.lock().unwrap() = PoolSync::default();
        *links.config.lock().unwrap() = None;
        namedtunnel::remove();
        links.stop_connector();
        links.secrets.delete(ACCOUNT)
    })
    .await;
    match result {
        Ok(Ok(())) => Json(s.links.view()).into_response(),
        Ok(Err(e)) => err(StatusCode::INTERNAL_SERVER_ERROR, e),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

// ---------------------------------------------------------- the router

async fn serve(links: Arc<PublicLinks>, listener: tokio::net::TcpListener) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            tokio::time::sleep(Duration::from_millis(50)).await;
            continue;
        };
        let _ = stream.set_nodelay(true);
        let links = links.clone();
        tokio::spawn(async move {
            let service = hyper::service::service_fn(move |r| handle(links.clone(), r));
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .with_upgrades()
                .await;
        });
    }
}

fn text(status: StatusCode, body: String) -> hyper::Response<Body> {
    let mut r = hyper::Response::new(Full::new(Bytes::from(body)).map_err(|n| match n {}).boxed());
    *r.status_mut() = status;
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    r
}

async fn handle(
    links: Arc<PublicLinks>,
    req: Request<Incoming>,
) -> Result<hyper::Response<Body>, std::convert::Infallible> {
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .map(|h| h.split(':').next().unwrap_or(h).to_ascii_lowercase())
        .unwrap_or_default();
    let (port, tls) = match links.route(&host) {
        Ok(t) => t,
        Err(why) => return Ok(text(StatusCode::NOT_FOUND, format!("devstack: {why}\n"))),
    };
    Ok(forward(req, &host, port, tls).await.unwrap_or_else(|e| {
        text(
            StatusCode::BAD_GATEWAY,
            format!("devstack: the service does not answer: {e}\n"),
        )
    }))
}

trait Io: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> Io for T {}

/// Forward a request to the service's port on this machine, with the Host
/// a browser here sends (as a Quick Tunnel's `--http-host-header` does);
/// WebSockets pass through.
async fn forward(
    mut req: Request<Incoming>,
    host: &str,
    port: u16,
    tls: bool,
) -> Result<hyper::Response<Body>, String> {
    let tcp = match tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
        Ok(s) => s,
        // A dev server may listen on IPv6 loopback only.
        Err(_) => tokio::net::TcpStream::connect(("::1", port))
            .await
            .map_err(|e| e.to_string())?,
    };
    let _ = tcp.set_nodelay(true);
    let io: Box<dyn Io> = if tls {
        let name =
            rustls_pki_types::ServerName::try_from("localhost").map_err(|e| e.to_string())?;
        Box::new(
            tokio_rustls::TlsConnector::from(any_certificate())
                .connect(name, tcp)
                .await
                .map_err(|e| e.to_string())?,
        )
    } else {
        Box::new(tcp)
    };
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(io))
        .await
        .map_err(|e| e.to_string())?;
    tokio::spawn(conn.with_upgrades());
    let client_upgrade = req
        .headers()
        .contains_key(header::UPGRADE)
        .then(|| hyper::upgrade::on(&mut req));
    {
        let h = req.headers_mut();
        devstack_daemon::proxy::strip_hop_by_hop(h, client_upgrade.is_some());
        if let Ok(v) = header::HeaderValue::from_str(&format!("localhost:{port}")) {
            h.insert(header::HOST, v);
        }
        if let Ok(v) = header::HeaderValue::from_str(host) {
            h.insert("x-forwarded-host", v);
        }
        h.insert(
            "x-forwarded-proto",
            header::HeaderValue::from_static("https"),
        );
    }
    let mut resp = sender.send_request(req).await.map_err(|e| e.to_string())?;
    let upgraded = resp.status() == StatusCode::SWITCHING_PROTOCOLS && client_upgrade.is_some();
    devstack_daemon::proxy::strip_hop_by_hop(resp.headers_mut(), upgraded);
    if upgraded && let Some(client) = client_upgrade {
        let upstream = hyper::upgrade::on(&mut resp);
        tokio::spawn(async move {
            if let (Ok(c), Ok(u)) = (client.await, upstream.await) {
                let (mut c, mut u) = (TokioIo::new(c), TokioIo::new(u));
                let _ = tokio::io::copy_bidirectional(&mut c, &mut u).await;
            }
        });
        let (parts, _) = resp.into_parts();
        return Ok(hyper::Response::from_parts(
            parts,
            Full::new(Bytes::new()).map_err(|n| match n {}).boxed(),
        ));
    }
    Ok(resp.map(BodyExt::boxed))
}

/// TLS to a service on this machine: its certificate is a development one
/// (`insecure_tls`, as its health check takes it), so any is accepted.
fn any_certificate() -> Arc<rustls::ClientConfig> {
    static CONFIG: std::sync::OnceLock<Arc<rustls::ClientConfig>> = std::sync::OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let config = rustls::ClientConfig::builder_with_provider(provider.clone())
                .with_safe_default_protocol_versions()
                .expect("ring supports the default versions")
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(AnyCertificate(provider)))
                .with_no_client_auth();
            Arc::new(config)
        })
        .clone()
}

#[derive(Debug)]
struct AnyCertificate(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for AnyCertificate {
    fn verify_server_cert(
        &self,
        _: &rustls_pki_types::CertificateDer<'_>,
        _: &[rustls_pki_types::CertificateDer<'_>],
        _: &rustls_pki_types::ServerName<'_>,
        _: &[u8],
        _: rustls_pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls_pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls_pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// Clears a tree's links when the task that follows its stream ends,
/// aborted included (a lane that stopped).
pub(crate) struct Following {
    links: Arc<PublicLinks>,
    id: String,
    lane: String,
}

impl Following {
    pub(crate) fn new(links: &Arc<PublicLinks>, id: &str, lane: &str) -> Self {
        Self {
            links: links.clone(),
            id: id.into(),
            lane: lane.into(),
        }
    }
}

impl Drop for Following {
    fn drop(&mut self) {
        self.links.clear(&self.id, &self.lane);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn links(domain: &str) -> PublicLinks {
        PublicLinks {
            dir: std::env::temp_dir(),
            secrets: Secrets::nothing(),
            config: Mutex::new(Some(Config {
                domain: domain.into(),
                tunnel_id: "t".into(),
                suffix: None,
                pool: None,
            })),
            trees: Mutex::new(BTreeMap::new()),
            connector: Mutex::new(Connector {
                tree: None,
                state: ConnState::Off,
                since: 0,
                started: None,
                failed_at: None,
                idle_since: None,
            }),
            router_port: 1,
            pool: Mutex::default(),
        }
    }

    fn snapshot(port: u16, link: Value) -> Value {
        json!({ "type": "snapshot", "stack": { "services": [
            { "name": "web", "port": port, "url": format!("http://localhost:{port}"), "public_link": link },
            { "name": "api", "port": port + 1, "url": format!("https://localhost:{}", port + 1) },
        ] } })
    }

    #[test]
    fn only_open_links_under_the_domain_are_routed() {
        let l = links("dev.io");
        l.trees
            .lock()
            .unwrap()
            .insert(("shop".into(), String::new()), Tree::default());
        // A Quick Tunnel's link is not the named tunnel's.
        l.apply(
            "shop",
            "",
            &snapshot(
                4200,
                json!({ "state": "open", "url": "https://x.trycloudflare.com" }),
            ),
        );
        assert!(l.route("x.trycloudflare.com").is_err());
        l.apply(
            "shop",
            "",
            &snapshot(
                4200,
                json!({ "state": "open", "url": "https://web-shop.dev.io" }),
            ),
        );
        assert_eq!(l.route("web-shop.dev.io"), Ok((4200, false)));
        assert!(l.route("api-shop.dev.io").is_err(), "not opened");
        l.apply("shop", "", &json!({ "type": "public_link", "service": "api", "link": { "state": "starting", "url": "https://api-shop.dev.io" } }));
        assert_eq!(l.route("api-shop.dev.io"), Ok((4201, true)));
        l.apply(
            "shop",
            "",
            &json!({ "type": "public_link", "service": "web", "link": null }),
        );
        assert!(l.route("web-shop.dev.io").is_err());
        // A failed link carries nothing; the tree's end takes the rest.
        l.apply("shop", "", &json!({ "type": "public_link", "service": "api", "link": { "state": "failed", "url": "https://api-shop.dev.io" } }));
        assert!(l.hosts().is_empty());
    }

    #[test]
    fn the_view_names_the_connectors_state_as_a_word() {
        // `watch_named` in the daemon reads `connector.state` as one.
        let v = links("dev.io").view();
        assert_eq!(v["connector"]["state"], "off", "{v}");
        assert_eq!(v["domain"], "dev.io");
    }

    #[test]
    fn a_pools_tunnel_routes_only_what_the_pool_gave_a_record() {
        let l = links("dev.io");
        l.config.lock().unwrap().as_mut().unwrap().pool = Some("team".into());
        let host = "web-shop--anna.dev.io";
        l.apply(
            "shop",
            "",
            &snapshot(
                4200,
                json!({ "state": "open", "url": format!("https://{host}") }),
            ),
        );
        assert!(l.route(host).is_err(), "no record yet");
        assert!(l.hosts().is_empty());
        assert_eq!(l.open_hosts(), vec![host.to_string()]);
        l.pool.lock().unwrap().registered.insert(host.into());
        assert_eq!(l.route(host), Ok((4200, false)));
        assert_eq!(l.view()["hosts"], json!([host]));
    }

    #[test]
    fn a_host_two_trees_claim_goes_nowhere() {
        let l = links("dev.io");
        let open = json!({ "state": "open", "url": "https://web-shop.dev.io" });
        l.apply("one", "", &snapshot(4200, open.clone()));
        l.apply("two", "", &snapshot(5200, open));
        assert!(l.route("web-shop.dev.io").is_err());
        l.clear("two", "");
        assert_eq!(l.route("web-shop.dev.io"), Ok((4200, false)));
    }
}
