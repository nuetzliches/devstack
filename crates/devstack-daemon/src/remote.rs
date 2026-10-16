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
}

pub struct RemoteLane {
    pub name: String,
    pub node: NodeRef,
    pub lease: String,
    worktree: PathBuf,
    client: NodeClient,
    state: Mutex<RemoteState>,
    stopped: tokio::sync::watch::Sender<bool>,
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
            client.start(&offer.id, stack_file).await
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
        });
        let l = lane.clone();
        let scratch = scratch.to_path_buf();
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
