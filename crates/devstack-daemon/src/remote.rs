//! A lane on another machine of the pool (ADR-0025, ADR-0029, ADR-0031): the
//! worktree stays here, everything of the lane runs on a node. This side
//! holds the lease, keeps the node's mirror in step with the worktree, renews
//! the lease while the lane runs, and passes the lane's API through under
//! `/v1/lanes/<name>/v1/...`, so CLI, MCP and dashboard treat it like a
//! local lane (ADR-0025 point 6).

use devstack_node::coordinator::{LeaseRequest, NodeRef};
use devstack_node::member::Local;
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
    pub node: NodeRef,
    pub lease: String,
    worktree: PathBuf,
    client: NodeClient,
    state: Mutex<RemoteState>,
    stopped: tokio::sync::watch::Sender<bool>,
    /// The lane's ports, `(on the node, here)`.
    ports: Vec<(u16, u16)>,
    /// Whether this machine listens on them for the lane now: a lane that
    /// moves here takes them over before its old placement goes.
    listening: tokio::sync::watch::Sender<bool>,
}

/// This machine's membership of the pool, in devstack's per-user state.
pub fn local_member() -> Local {
    let state = crate::token::token_path()
        .parent()
        .expect("token path has a parent")
        .to_path_buf();
    Local::new(&devstack_node::member::default_dir(&state))
}

/// What the worktree's files look like now: path, size, modification time.
type Fingerprint = BTreeMap<String, (u64, Option<SystemTime>)>;

fn files(worktree: &Path, scratch: &Path) -> Result<Vec<String>, String> {
    devstack_build::snapshot::Snapshot::open(worktree, scratch)
        .and_then(|s| s.list())
        .map_err(|e| e.to_string())
}

fn fingerprint(worktree: &Path, list: &[String]) -> Fingerprint {
    list.iter()
        .filter_map(|rel| {
            let m = std::fs::metadata(worktree.join(rel)).ok()?;
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
}

impl RemoteLane {
    /// Lease the lane on its node, sync it, start it there, and follow it.
    pub fn start(rt: &tokio::runtime::Handle, spec: RemoteSpec<'_>) -> Result<Arc<Self>, String> {
        let RemoteSpec {
            name,
            repo,
            worktree,
            stack_file,
            on,
            need,
            scratch,
            ports,
            port_offset,
            forward,
        } = spec;
        let local = local_member();
        let offer = local
            .lease(&LeaseRequest {
                node: on.into(),
                repo: repo.into(),
                lane: name.into(),
                need,
            })
            .map_err(|e| format!("lease on `{on}`: {e}"))?;
        let client = NodeClient::new(&local, offer.node.clone()).map_err(|e| e.to_string())?;
        let list = files(worktree, scratch)?;
        let t0 = Instant::now();
        let started = rt.block_on(async {
            client.admit(&offer).await?;
            let s = client.sync(&offer.id, worktree, &list).await?;
            crate::diag::info(
                "remote_lane_synced",
                serde_json::json!({ "lane": name, "node": offer.node.name, "files": s.files,
                    "uploaded": s.uploaded, "bytes": s.bytes, "ms": t0.elapsed().as_millis() as u64 }),
            );
            // Paths in its logs point into the worktree, `/` as compilers
            // on the node write them.
            let display = worktree.to_string_lossy().replace('\\', "/");
            client
                .start(&offer.id, stack_file, Some(&display), port_offset)
                .await
        });
        if let Err(e) = started {
            let _ = rt.block_on(client.release(&offer.id));
            return Err(format!("lane `{name}` on {}: {e}", offer.node.name));
        }
        crate::diag::info(
            "remote_lane_started",
            serde_json::json!({ "lane": name, "node": offer.node.name, "lease": offer.id,
                "ms": t0.elapsed().as_millis() as u64 }),
        );
        let lane = Arc::new(Self {
            name: name.into(),
            node: offer.node,
            lease: offer.id,
            worktree: worktree.to_path_buf(),
            client,
            state: Mutex::new(RemoteState::default()),
            stopped: tokio::sync::watch::channel(false).0,
            ports,
            listening: tokio::sync::watch::channel(false).0,
        });
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
            let wt = self.worktree.clone();
            let sc = scratch.clone();
            let listed = tokio::task::spawn_blocking(move || {
                let list = files(&wt, &sc)?;
                let fp = fingerprint(&wt, &list);
                Ok::<_, String>((list, fp))
            })
            .await;
            match listed {
                Ok(Ok((list, fp))) => {
                    if seen.as_ref() != Some(&fp) {
                        match self.client.sync(&self.lease, &self.worktree, &list).await {
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
                    Ok(_) => renewed = Instant::now(),
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
    pub async fn up_and_wait(&self, expected: &[String], within: Duration) -> Result<(), String> {
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
                && let Some(done) = readiness(&v, expected)
            {
                return done;
            }
            if Instant::now() > until {
                return Err(format!("not ready within {} s", within.as_secs()));
            }
        }
    }

    async fn forward_port(self: Arc<Self>, there: u16, here: u16) {
        let mut listeners = Vec::new();
        match tokio::net::TcpListener::bind(("127.0.0.1", here)).await {
            Ok(l) => listeners.push(l),
            Err(e) => {
                self.trouble(Some(format!("port {here} here: {e}")));
                return;
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
