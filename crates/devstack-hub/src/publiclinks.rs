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
use devstack_daemon::namedtunnel::{self, Config};
use devstack_node::linkrouter::{self, Connector, Dial};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The keychain entry of the tunnel's credentials.
const ACCOUNT: &str = "credentials";
/// The hosts are reported to the pool at least this often.
const REPORT_EVERY: Duration = Duration::from_secs(60);

/// What the hub knows of one tree (a stack's main tree, or a lane).
#[derive(Default)]
struct Tree {
    /// Service -> its port in this tree, and whether it speaks TLS.
    ports: BTreeMap<String, (u16, bool)>,
    /// Service -> the host of its open (or starting) link.
    open: BTreeMap<String, String>,
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
        let log = dir.join("cloudflared.log");
        let me = Arc::new(Self {
            secrets: Secrets::open_as("devstack-tunnel", dir.clone()),
            dir,
            config: Mutex::new(namedtunnel::read()),
            trees: Mutex::new(BTreeMap::new()),
            connector: Mutex::new(Connector::new(log)),
            router_port,
            pool: Mutex::default(),
        });
        // The router asks this hub where a host goes: a port here.
        let dialer = Arc::downgrade(&me);
        let dial: Dial = Arc::new(move |host: String| {
            let me = dialer.upgrade();
            Box::pin(async move {
                let me = me.ok_or((503, "the hub is stopping".to_string()))?;
                let (port, tls) = me.route(&host).map_err(|w| (404, w.to_string()))?;
                linkrouter::local(port, tls).await
            })
        });
        tokio::spawn(linkrouter::serve(listener, dial));
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
        let off_now = self.config.lock().unwrap().is_none();
        self.connector
            .lock()
            .unwrap()
            .tick(wanted, off_now, || self.spawn());
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
        linkrouter::spawn_connector(
            &self.dir,
            &config.tunnel_id,
            self.router_port,
            credentials,
            &self.dir.join("cloudflared.log"),
            &linkrouter::proc_options(),
        )
    }

    fn view(&self) -> Value {
        let config = self.config.lock().unwrap().clone();
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
        let connector = self.connector.lock().unwrap().view();
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
        self.connector.lock().unwrap().stop();
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
            connector: Mutex::new(Connector::new(
                std::env::temp_dir().join("public-links-test.log"),
            )),
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
