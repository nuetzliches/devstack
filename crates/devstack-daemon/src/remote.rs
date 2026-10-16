//! A lane on another machine of the pool (ADR-0025, ADR-0029, ADR-0031): the
//! worktree stays here, everything of the lane runs on a node. This side
//! holds the lease, keeps the node's mirror in step with the worktree, renews
//! the lease while the lane runs, and passes the lane's API through under
//! `/v1/lanes/<name>/v1/...`, so CLI, MCP and dashboard treat it like a
//! local lane (ADR-0025 point 6).

use devstack_node::coordinator::{LeaseRequest, NodeRef};
use devstack_node::member::Local;
use devstack_node::nodeclient::Elsewhere;
use devstack_node::nodeclient::NodeClient;
use devstack_node::share::Amount;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

/// How often the worktree is checked for changes to sync.
const SYNC_EVERY: Duration = Duration::from_secs(2);
/// How often the lease is renewed (the node's TTL is two minutes).
const RENEW_EVERY: Duration = Duration::from_secs(30);
/// How often the lane's state is read for the lists.
const STATE_EVERY: Duration = Duration::from_secs(5);

/// What the lists show of a remote lane.
#[derive(Debug, Clone, Default)]
pub struct RemoteState {
    pub running: usize,
    pub services: usize,
    /// The last thing that went wrong (a failed sync or renewal), until it
    /// works again.
    pub error: Option<String>,
    /// The lease is over (an admin ended it, or it expired): the lane no
    /// longer runs there.
    pub ended: bool,
}

pub struct RemoteLane {
    pub name: String,
    /// The pool it runs in (ADR-0034).
    pub pool: String,
    pub node: NodeRef,
    pub lease: String,
    worktree: PathBuf,
    /// Where `[lanes] sync` files come from when the worktree has none.
    main: PathBuf,
    /// `[lanes] sync`: ignored files the lane takes along.
    extra: Vec<String>,
    /// Whom it is shared with (ADR-0035), told the coordinator again with
    /// each renewal (a coordinator that restarted forgets it).
    sharing: Mutex<Option<devstack_node::coordinator::Sharing>>,
    client: NodeClient,
    state: Mutex<RemoteState>,
    stopped: tokio::sync::watch::Sender<bool>,
    /// The lane's ports, `(on the node, here)`.
    ports: Vec<(u16, u16)>,
    /// Whether this machine listens on them for the lane now: a lane that
    /// moves here takes them over before its old placement goes.
    listening: tokio::sync::watch::Sender<bool>,
}

/// devstack's per-user state, where this machine's memberships live.
fn state_dir() -> PathBuf {
    crate::token::token_path()
        .parent()
        .expect("token path has a parent")
        .to_path_buf()
}

/// This machine's membership of `pool`, in devstack's per-user state.
pub fn local_member(pool: &str) -> Local {
    Local::new(&devstack_node::member::pool_dir(&state_dir(), pool))
}

/// `--on` split into the pool and the node asked for (ADR-0034 point 3).
pub fn resolve_on(on: &str) -> Result<(String, String), String> {
    devstack_node::member::resolve_on(&state_dir(), on)
}

impl RemoteLane {
    /// Is it where `on` asks for (`pool` or the pool alone: anywhere in it)?
    pub fn is_at(&self, on: &str) -> bool {
        resolve_on(on).is_ok_and(|(pool, node)| {
            pool == self.pool
                && (node.eq_ignore_ascii_case("pool")
                    || self.node.name.eq_ignore_ascii_case(&node)
                    || self.node.id.starts_with(&node))
        })
    }
}

/// What the worktree's files look like now: path, size, modification time.
type Fingerprint = BTreeMap<String, (u64, Option<SystemTime>)>;

/// The files to sync: what git does not ignore, and what `[lanes] sync`
/// names, from the worktree or else from the main tree.
fn files(
    worktree: &Path,
    main: &Path,
    extra: &[String],
    scratch: &Path,
) -> Result<(Vec<String>, Elsewhere), String> {
    let mut list = devstack_build::snapshot::Snapshot::open(worktree, scratch)
        .and_then(|s| s.list())
        .map_err(|e| e.to_string())?;
    let mut elsewhere = Elsewhere::new();
    let mut have: std::collections::HashSet<String> = list.iter().cloned().collect();
    for pattern in extra {
        let Ok(glob) = globset::GlobBuilder::new(pattern)
            .literal_separator(true)
            .build()
            .map(|g| g.compile_matcher())
        else {
            continue;
        };
        for root in [worktree, main] {
            for rel in walk(root, glob_base(pattern)) {
                if glob.is_match(&rel) && have.insert(rel.clone()) {
                    if root != worktree {
                        elsewhere.insert(rel.clone(), root.join(&rel));
                    }
                    list.push(rel);
                }
            }
        }
    }
    Ok((list, elsewhere))
}

/// The directory a glob starts in: its leading part without wildcards
/// (`certs/*.pem` -> `certs`, `a/b.txt` -> `a`).
fn glob_base(pattern: &str) -> &str {
    let wild = pattern.find(['*', '?', '[', '{']).unwrap_or(pattern.len());
    pattern[..wild].rfind('/').map_or("", |i| &pattern[..i])
}

/// Files below `root/base`, relative to `root`, `/`-separated.
fn walk(root: &Path, base: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut dirs = vec![root.join(base)];
    while let Some(dir) = dirs.pop() {
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = e.path();
            match e.file_type() {
                Ok(t) if t.is_dir() && e.file_name() != ".git" => dirs.push(p),
                Ok(t) if t.is_file() => {
                    if let Ok(rel) = p.strip_prefix(root) {
                        out.push(rel.to_string_lossy().replace('\\', "/"));
                    }
                }
                _ => {}
            }
        }
    }
    out
}

fn fingerprint(worktree: &Path, list: &[String], elsewhere: &Elsewhere) -> Fingerprint {
    list.iter()
        .filter_map(|rel| {
            let path = elsewhere
                .get(rel)
                .cloned()
                .unwrap_or_else(|| worktree.join(rel));
            let m = std::fs::metadata(path).ok()?;
            Some((rel.clone(), (m.len(), m.modified().ok())))
        })
        .collect()
}

/// What a lane on a node is to be.
pub struct RemoteSpec<'a> {
    /// The lane's name.
    pub name: &'a str,
    /// The repository, as the pool sees it (the stack's name).
    pub repo: &'a str,
    pub worktree: &'a Path,
    /// The main working tree, where `[lanes] sync` files come from when the
    /// worktree has none.
    pub main: &'a Path,
    /// `[lanes] sync`.
    pub extra: &'a [String],
    /// Whom it is shared with from the start (ADR-0035).
    pub sharing: Option<devstack_node::coordinator::Sharing>,
    /// The stack file, relative to the worktree's root.
    pub stack_file: &'a str,
    /// The node: its name or member id.
    pub on: &'a str,
    /// What the lane is expected to take.
    pub need: Amount,
    /// A directory of this daemon's for the lane.
    pub scratch: &'a Path,
    /// Each service port of the lane, as `(on the node, here)`: the lane
    /// listens on the stack's ports there, here on them moved by its offset,
    /// as a local lane does (ADR-0029 point 4).
    pub ports: Vec<(u16, u16)>,
    /// What the lane's ports are moved by here; it runs on those ports on
    /// the node too.
    pub port_offset: u16,
    /// Listen on them at once; else when [`RemoteLane::start_forwarding`]
    /// says so (a lane moving here, ADR-0025 point 10).
    pub forward: bool,
    /// Hears each step: its number (1 lease, 2 sync, 3 start on the node)
    /// and what it does now.
    pub step: &'a (dyn Fn(u8, &str) + Send + Sync),
}

impl RemoteLane {
    /// Lease the lane on its node, sync it, start it there, and follow it.
    pub fn start(rt: &tokio::runtime::Handle, spec: RemoteSpec<'_>) -> Result<Arc<Self>, String> {
        let RemoteSpec {
            name,
            repo,
            worktree,
            main,
            extra,
            sharing,
            stack_file,
            on,
            need,
            scratch,
            ports,
            port_offset,
            forward,
            step,
        } = spec;
        step(1, &format!("lease on {on}"));
        let (pool, node) = resolve_on(on)?;
        let local = local_member(&pool);
        let offer = local
            .lease(&LeaseRequest {
                node,
                repo: repo.into(),
                lane: name.into(),
                need,
            })
            .map_err(|e| format!("lease on `{on}`: {e}"))?;
        let client = NodeClient::new(&local, offer.node.clone()).map_err(|e| e.to_string())?;
        let (list, elsewhere) = files(worktree, main, extra, scratch)?;
        let t0 = Instant::now();
        let started = rt.block_on(async {
            client.admit(&offer).await?;
            step(2, &format!("sync: comparing {} files", list.len()));
            let s = client
                .sync_from(&offer.id, worktree, &list, &elsewhere, &|n, of, bytes| {
                    step(
                        2,
                        &format!("sync: {n} of {of} changed files sent ({} KiB)", bytes / 1024),
                    );
                })
                .await?;
            crate::diag::info(
                "remote_lane_synced",
                serde_json::json!({ "lane": name, "node": offer.node.name, "files": s.files,
                    "uploaded": s.uploaded, "bytes": s.bytes, "ms": t0.elapsed().as_millis() as u64 }),
            );
            // Paths in its logs point into the worktree, `/` as compilers
            // on the node write them.
            let display = worktree.to_string_lossy().replace('\\', "/");
            let lane = name.to_string();
            let node = offer.node.name.clone();
            step(3, &format!("on {node}: starting"));
            client
                .start(
                    &offer.id,
                    stack_file,
                    Some(&display),
                    port_offset,
                    Duration::from_secs(1800),
                    &move |p| {
                        step(3, &format!("on {node}: {p}"));
                        crate::diag::info(
                            "remote_lane_starting",
                            serde_json::json!({ "lane": lane, "node": node, "phase": p }),
                        );
                    },
                )
                .await
        });
        let started = match started {
            Ok(st) => st,
            Err(e) => {
                let _ = rt.block_on(client.release(&offer.id));
                return Err(format!("lane `{name}` on {}: {e}", offer.node.name));
            }
        };
        // Its containers' ports here too, where a local lane publishes them.
        let mut ports = ports;
        for p in started.infra_ports {
            if !ports.iter().any(|(_, here)| *here == p) {
                ports.push((p, p));
            }
        }
        crate::diag::info(
            "remote_lane_started",
            serde_json::json!({ "lane": name, "node": offer.node.name, "lease": offer.id,
                "ms": t0.elapsed().as_millis() as u64 }),
        );
        let lane = Arc::new(Self {
            name: name.into(),
            pool,
            node: offer.node,
            lease: offer.id,
            worktree: worktree.to_path_buf(),
            main: main.to_path_buf(),
            extra: extra.to_vec(),
            sharing: Mutex::new(None),
            client,
            state: Mutex::new(RemoteState::default()),
            stopped: tokio::sync::watch::channel(false).0,
            ports,
            listening: tokio::sync::watch::channel(false).0,
        });
        if sharing.is_some()
            && let Err(e) = lane.share(sharing)
        {
            lane.trouble(Some(format!("sharing: {e}")));
        }
        let l = lane.clone();
        let scratch = scratch.to_path_buf();
        if forward {
            lane.start_forwarding(rt);
        }
        // The first look syncs once more: the fingerprint of what the first
        // sync sent is not known, and the listing is all it costs.
        rt.spawn(async move { l.follow(scratch, None).await });
        Ok(lane)
    }

    pub fn state(&self) -> RemoteState {
        self.state.lock().unwrap().clone()
    }

    /// Share the lane, or stop sharing it (`None`); the coordinator hears
    /// it at once. Blocking.
    pub fn share(
        &self,
        sharing: Option<devstack_node::coordinator::Sharing>,
    ) -> Result<(), String> {
        let r = local_member(&self.pool)
            .share_lease(&self.lease, sharing.as_ref())
            .map_err(|e| e.to_string());
        *self.sharing.lock().unwrap() = sharing;
        r
    }

    /// Whom it is shared with.
    pub fn shared_with(&self) -> Vec<String> {
        self.sharing
            .lock()
            .unwrap()
            .as_ref()
            .map(|s| s.with.clone())
            .unwrap_or_default()
    }

    fn trouble(&self, what: Option<String>) {
        self.state.lock().unwrap().error = what;
    }

    /// While the lane runs: sync what changes, renew the lease, read its
    /// state.
    async fn follow(self: Arc<Self>, scratch: PathBuf, mut seen: Option<Fingerprint>) {
        let mut stopped = self.stopped.subscribe();
        let (mut renewed, mut read) = (Instant::now(), Instant::now() - STATE_EVERY);
        loop {
            tokio::select! {
                _ = tokio::time::sleep(SYNC_EVERY) => {}
                _ = stopped.wait_for(|s| *s) => return,
            }
            let (wt, main, extra) = (self.worktree.clone(), self.main.clone(), self.extra.clone());
            let sc = scratch.clone();
            let listed = tokio::task::spawn_blocking(move || {
                let (list, elsewhere) = files(&wt, &main, &extra, &sc)?;
                let fp = fingerprint(&wt, &list, &elsewhere);
                Ok::<_, String>((list, elsewhere, fp))
            })
            .await;
            match listed {
                Ok(Ok((list, elsewhere, fp))) => {
                    if seen.as_ref() != Some(&fp) {
                        match self
                            .client
                            .sync_from(
                                &self.lease,
                                &self.worktree,
                                &list,
                                &elsewhere,
                                &|_, _, _| {},
                            )
                            .await
                        {
                            Ok(s) => {
                                if s.written + s.removed > 0 {
                                    crate::diag::info(
                                        "remote_lane_synced",
                                        serde_json::json!({ "lane": self.name, "uploaded": s.uploaded,
                                            "written": s.written, "removed": s.removed }),
                                    );
                                }
                                seen = Some(fp);
                                self.trouble(None);
                            }
                            Err(e) => self.trouble(Some(format!("sync: {e}"))),
                        }
                    }
                }
                Ok(Err(e)) => self.trouble(Some(format!("listing the worktree: {e}"))),
                Err(_) => {}
            }
            if renewed.elapsed() >= RENEW_EVERY {
                match self.client.renew(&self.lease).await {
                    Ok(_) => {
                        renewed = Instant::now();
                        let sharing = self.sharing.lock().unwrap().clone();
                        if let Some(sh) = sharing {
                            let (pool, lease) = (self.pool.clone(), self.lease.clone());
                            let _ = tokio::task::spawn_blocking(move || {
                                local_member(&pool).share_lease(&lease, Some(&sh))
                            })
                            .await;
                        }
                    }
                    Err(devstack_node::nodeclient::ClientError::Status(404, _)) => {
                        {
                            let mut st = self.state.lock().unwrap();
                            st.ended = true;
                            st.error = Some(format!(
                                "the lease on {} is over (an admin ended it, or it expired): \
                                 `devstack up --on {}` starts the lane again",
                                self.node.name, self.node.name
                            ));
                        }
                        self.listening.send_replace(false);
                        crate::diag::warn(
                            "remote_lane_ended",
                            serde_json::json!({ "lane": self.name, "node": self.node.name, "lease": self.lease }),
                        );
                        return;
                    }
                    Err(e) => self.trouble(Some(format!("renewing the lease: {e}"))),
                }
            }
            if read.elapsed() >= STATE_EVERY {
                read = Instant::now();
                if let Ok((200, body)) =
                    self.client.api(&self.lease, "GET", "/v1/stack", None).await
                    && let Ok(v) = serde_json::from_str::<serde_json::Value>(&body)
                {
                    let svcs = v["services"].as_array().cloned().unwrap_or_default();
                    let running = svcs
                        .iter()
                        .filter(|s| {
                            matches!(
                                s["status"]["state"].as_str(),
                                Some("healthy" | "starting" | "building" | "running")
                            )
                        })
                        .count();
                    let mut st = self.state.lock().unwrap();
                    st.running = running;
                    st.services = svcs.len();
                }
            }
        }
    }

    /// Listen on `here` (loopback, IPv4 and, where there is one, IPv6) and
    /// carry every connection to port `there` of the lane on its node: bytes
    /// unchanged, so WebSockets, hot module replacement and TLS dev servers
    /// work as they do locally.
    /// Listen on the lane's ports here (again).
    pub fn start_forwarding(self: &Arc<Self>, rt: &tokio::runtime::Handle) {
        if self.listening.send_replace(true) {
            return;
        }
        for &(there, here) in &self.ports {
            let l = self.clone();
            rt.spawn(async move { l.forward_port(there, here).await });
        }
    }

    /// Stop listening on the lane's ports here; open connections stay.
    /// Returns once the ports are free (or after two seconds).
    pub fn stop_forwarding(&self) {
        self.listening.send_replace(false);
        let until = Instant::now() + Duration::from_secs(2);
        while Instant::now() < until
            && self
                .ports
                .iter()
                .any(|(_, here)| crate::supervisor::port_in_use(*here))
        {
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Start the lane's services (`POST /v1/up` there) and wait until every
    /// one of `expected` is ready, or one failed, or `within` ran out.
    /// `ready` hears how many of `expected` are ready so far.
    pub async fn up_and_wait(
        &self,
        expected: &[String],
        within: Duration,
        ready: &(dyn Fn(usize, usize) + Send + Sync),
    ) -> Result<(), String> {
        let (st, body) = self
            .client
            .api(&self.lease, "POST", "/v1/up", None)
            .await
            .map_err(|e| e.to_string())?;
        if st >= 300 {
            return Err(format!("starting its services: {st} {body}"));
        }
        let until = Instant::now() + within;
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            if let Ok((200, body)) = self.client.api(&self.lease, "GET", "/v1/stack", None).await
                && let Ok(v) = serde_json::from_str::<serde_json::Value>(&body)
            {
                ready(ready_count(&v, expected), expected.len());
                if let Some(done) = readiness(&v, expected) {
                    return done;
                }
            }
            if Instant::now() > until {
                return Err(format!("not ready within {} s", within.as_secs()));
            }
        }
    }

    async fn forward_port(self: Arc<Self>, there: u16, here: u16) {
        let mut listeners = Vec::new();
        // A port something else still holds (a container on its way out) is
        // tried again: every second for half a minute, then every 15 s, as
        // long as the lane forwards. The lane says so meanwhile.
        let t0 = Instant::now();
        let mut stopped = self.stopped.subscribe();
        let mut listening = self.listening.subscribe();
        loop {
            match tokio::net::TcpListener::bind(("127.0.0.1", here)).await {
                Ok(l) => {
                    listeners.push(l);
                    let mut st = self.state.lock().unwrap();
                    if st
                        .error
                        .as_deref()
                        .is_some_and(|e| e.starts_with(&format!("port {here} here")))
                    {
                        st.error = None;
                    }
                    break;
                }
                Err(e) => {
                    self.trouble(Some(format!(
                        "port {here} here: {e} (something else holds it; tried again until it is free)"
                    )));
                    let wait = if t0.elapsed() < Duration::from_secs(30) {
                        Duration::from_secs(1)
                    } else {
                        Duration::from_secs(15)
                    };
                    tokio::select! {
                        _ = tokio::time::sleep(wait) => {}
                        _ = stopped.wait_for(|s| *s) => return,
                        _ = listening.wait_for(|l| !*l) => return,
                    }
                }
            }
        }
        if let Ok(l) = tokio::net::TcpListener::bind(("::1", here)).await {
            listeners.push(l);
        }
        for listener in listeners {
            let me = self.clone();
            tokio::spawn(async move {
                let mut stopped = me.stopped.subscribe();
                let mut listening = me.listening.subscribe();
                loop {
                    let accepted = tokio::select! {
                        a = listener.accept() => a,
                        _ = stopped.wait_for(|s| *s) => return,
                        _ = listening.wait_for(|l| !*l) => return,
                    };
                    let Ok((tcp, _)) = accepted else {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        continue;
                    };
                    let me = me.clone();
                    tokio::spawn(async move {
                        if let Err(e) = me.tunnel(tcp, there).await {
                            crate::diag::warn(
                                "remote_lane_tunnel",
                                serde_json::json!({ "lane": me.name, "port": there, "error": e }),
                            );
                        }
                    });
                }
            });
        }
    }

    /// One connection, through the node agent to the lane daemon's tunnel.
    async fn tunnel(&self, mut tcp: tokio::net::TcpStream, port: u16) -> Result<(), String> {
        let _ = tcp.set_nodelay(true);
        let req = axum::http::Request::builder()
            .method("GET")
            .uri(format!("/v1/tunnel/{port}"))
            .header(axum::http::header::CONNECTION, "upgrade")
            .header(axum::http::header::UPGRADE, crate::api::TUNNEL_PROTOCOL)
            .body(http_body_util::Empty::<bytes::Bytes>::new())
            .map_err(|e| e.to_string())?;
        let mut res = self
            .client
            .forward(&self.lease, req)
            .await
            .map_err(|e| e.to_string())?;
        if res.status() != axum::http::StatusCode::SWITCHING_PROTOCOLS {
            return Err(format!("the node answered {}", res.status()));
        }
        let up = hyper::upgrade::on(&mut res)
            .await
            .map_err(|e| e.to_string())?;
        let mut up = hyper_util::rt::TokioIo::new(up);
        tokio::io::copy_bidirectional(&mut up, &mut tcp)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// The lane's API, passed through (`req`'s URI is the daemon's path).
    pub async fn forward(&self, req: axum::extract::Request) -> axum::response::Response {
        use axum::response::IntoResponse;
        match self.client.forward(&self.lease, req).await {
            Ok(res) => res.map(axum::body::Body::new).into_response(),
            Err(e) => (
                axum::http::StatusCode::BAD_GATEWAY,
                axum::Json(serde_json::json!({
                    "error": format!("lane `{}` on {}: {e}", self.name, self.node.name)
                })),
            )
                .into_response(),
        }
    }

    /// Stop the lane: the node stops it and removes it with its mirror.
    pub fn stop(&self, rt: &tokio::runtime::Handle) {
        self.stopped.send_replace(true);
        let _ = rt.block_on(self.client.release(&self.lease));
        crate::diag::info(
            "remote_lane_stopped",
            serde_json::json!({ "lane": self.name, "node": self.node.name, "lease": self.lease }),
        );
    }
}

/// How many of `expected` are ready in a stack view.
pub fn ready_count(view: &serde_json::Value, expected: &[String]) -> usize {
    view["services"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|s| {
            expected
                .iter()
                .any(|e| Some(e.as_str()) == s["name"].as_str())
                && matches!(s["status"]["state"].as_str(), Some("healthy" | "running"))
        })
        .count()
}

/// Whether the services in `expected` are all ready in a stack view
/// (`GET /v1/stack`): `None` while one still starts, else the outcome.
pub fn readiness(view: &serde_json::Value, expected: &[String]) -> Option<Result<(), String>> {
    let mut pending = false;
    for s in view["services"].as_array().into_iter().flatten() {
        let name = s["name"].as_str().unwrap_or("");
        if !expected.iter().any(|e| e == name) {
            continue;
        }
        match s["status"]["state"].as_str().unwrap_or("") {
            "healthy" | "running" => {}
            "completed" if s["status"]["exit_code"] == 0 => {}
            "failed" | "exited" | "completed" => {
                let why = s["status"]["reason"].as_str().unwrap_or("it stopped");
                return Some(Err(format!("{name}: {why}")));
            }
            _ => pending = true,
        }
    }
    (!pending).then_some(Ok(()))
}

#[cfg(test)]
mod tests {
    #[test]
    fn glob_base_is_the_part_before_wildcards() {
        assert_eq!(super::glob_base("app/certs/*.pem"), "app/certs");
        assert_eq!(super::glob_base("a/b.txt"), "a");
        assert_eq!(super::glob_base("*.pem"), "");
        assert_eq!(super::glob_base("**/x"), "");
    }

    #[test]
    fn sync_takes_ignored_files_from_the_worktree_or_else_the_main_tree() {
        let main = tempfile::tempdir().unwrap();
        let wt = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let put = |root: &std::path::Path, rel: &str| {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, rel).unwrap();
        };
        put(wt.path(), "src/main.rs");
        put(wt.path(), "certs/a.pem");
        put(main.path(), "certs/a.pem");
        put(main.path(), "certs/b.pem");
        put(main.path(), "certs/readme.txt");
        let extra = ["certs/*.pem".to_string()];
        let (mut list, elsewhere) =
            super::files(wt.path(), main.path(), &extra, scratch.path()).unwrap();
        list.sort();
        assert_eq!(list, ["certs/a.pem", "certs/b.pem", "src/main.rs"]);
        assert_eq!(elsewhere.len(), 1);
        assert_eq!(elsewhere["certs/b.pem"], main.path().join("certs/b.pem"));
    }

    #[test]
    fn ready_when_every_expected_service_is() {
        let v = |a: &str, b: &str| {
            serde_json::json!({ "services": [
                { "name": "api", "status": { "state": a } },
                { "name": "web", "status": { "state": b, "reason": "port taken" } },
                { "name": "tool", "status": { "state": "stopped" } },
            ]})
        };
        let exp = ["api".to_string(), "web".to_string()];
        assert_eq!(super::readiness(&v("healthy", "starting"), &exp), None);
        assert_eq!(
            super::readiness(&v("healthy", "running"), &exp),
            Some(Ok(()))
        );
        assert!(
            matches!(super::readiness(&v("healthy", "failed"), &exp), Some(Err(e)) if e.contains("port taken"))
        );
    }
}
