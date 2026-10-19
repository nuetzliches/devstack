//! A lane on another machine of the pool (ADR-0025, ADR-0029, ADR-0031): the
//! worktree stays here, everything of the lane runs on a node. This side
//! holds the lease, keeps the node's mirror in step with the worktree, renews
//! the lease while the lane runs, and passes the lane's API through under
//! `/v1/lanes/<name>/v1/...`, so CLI, MCP and dashboard treat it like a
//! local lane (ADR-0025 point 6).

use devstack_node::coordinator::{LeaseRequest, NodeRef};
use devstack_node::member::Local;
use devstack_node::nodeclient::{Elsewhere, HashCache, NodeClient};
use devstack_node::share::Amount;
use devstack_node::version::{self, Compat, Release};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

/// How often the file watcher's changes are synced (at most).
const SYNC_EVERY: Duration = Duration::from_secs(2);
/// How often everything is compared regardless of the watcher.
const FULL_SYNC_EVERY: Duration = Duration::from_secs(30);
/// How often the lease is renewed (the node's TTL is two minutes).
const RENEW_EVERY: Duration = Duration::from_secs(30);
/// How long a lease lives without a renewal (the node's TTL), and a margin.
const LEASE_LIVES: Duration = Duration::from_secs(150);
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
    /// Its services that run now (healthy or running), by name.
    pub active: Vec<String>,
}

pub struct RemoteLane {
    pub name: String,
    /// The pool it runs in (ADR-0034).
    pub pool: String,
    pub node: NodeRef,
    pub lease: String,
    /// The node or the coordinator runs another devstack than this
    /// machine, or does not say which: shown with the lane while it runs.
    pub warning: Option<String>,
    worktree: PathBuf,
    /// Where `[lanes] sync` files come from when the worktree has none.
    main: PathBuf,
    /// `[lanes] sync`: ignored files the lane takes along.
    extra: Vec<String>,
    /// Whether it holds the main slot here (ADR-0025 point 4), told the
    /// lane again with each renewal (a lane daemon that restarted forgets).
    owns_main: std::sync::atomic::AtomicBool,
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
    /// The lease is over or lost: its open tunnels end too.
    gone: tokio::sync::watch::Sender<bool>,
    /// The worktree changed since the last sync (set by the file watcher;
    /// always, when there is none).
    dirty: Arc<std::sync::atomic::AtomicBool>,
    /// Watches the worktree while the lane runs.
    _watcher: Option<notify::RecommendedWatcher>,
    /// The hashes the first sync found, for the syncs that follow (A-4):
    /// an unchanged file is not read again.
    hashes: Mutex<HashCache>,
}

/// devstack's per-user state, where this machine's memberships live.
pub(crate) fn state_dir() -> PathBuf {
    crate::token::token_path()
        .parent()
        .expect("token path has a parent")
        .to_path_buf()
}

/// This machine's membership of `pool`, in devstack's per-user state.
/// A node a lane ran on (ADR-0038 point 4): the lane's kept data there
/// goes when the lane ends for good or is reset.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RanOn {
    pub pool: String,
    pub node: devstack_node::coordinator::NodeRef,
}

const RAN_ON: &str = "nodes.json";

/// The nodes the lane whose state is `state` (`lane-<name>/`) ran on.
pub fn ran_on(state: &Path) -> Vec<RanOn> {
    std::fs::read(state.join(RAN_ON))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Note that the lane ran on `node` of `pool`.
fn note_ran_on(state: &Path, pool: &str, node: &devstack_node::coordinator::NodeRef) {
    let mut all = ran_on(state);
    all.retain(|r| !(r.pool == pool && r.node.id == node.id));
    all.push(RanOn {
        pool: pool.to_string(),
        node: node.clone(),
    });
    let _ = std::fs::create_dir_all(state);
    let _ = std::fs::write(
        state.join(RAN_ON),
        serde_json::to_vec_pretty(&all).unwrap_or_default(),
    );
}

/// Remove lane `lane` of `repo`'s kept data on `on` (`DELETE /v1/slots`).
pub fn purge_slot(
    rt: &tokio::runtime::Handle,
    on: &RanOn,
    repo: &str,
    lane: &str,
) -> Result<(), String> {
    let client =
        NodeClient::new(&local_member(&on.pool), on.node.clone()).map_err(|e| e.to_string())?;
    rt.block_on(client.purge_slot(repo, lane))
        .map_err(|e| e.to_string())
}

/// This member's kept lanes on `node` of `pool` (`GET /v1/slots`).
pub fn slots_on(
    rt: &tokio::runtime::Handle,
    pool: &str,
    node: devstack_node::coordinator::NodeRef,
) -> Result<Vec<devstack_node::node::SlotView>, String> {
    let client = NodeClient::new(&local_member(pool), node).map_err(|e| e.to_string())?;
    rt.block_on(client.slots()).map_err(|e| e.to_string())
}

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

/// Directories a `[lanes] sync` pattern never reaches into: git's,
/// devstack's own state (artifacts, snapshots, logs: a `.pem` in a snapshot
/// is no source) and installed packages, the largest trees of a checkout.
const NOT_WALKED: [&str; 3] = [".git", ".devstack", "node_modules"];

/// Files below `root/base`, relative to `root`, `/`-separated.
fn walk(root: &Path, base: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut dirs = vec![root.join(base)];
    while let Some(dir) = dirs.pop() {
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = e.path();
            match e.file_type() {
                Ok(t) if t.is_dir() && !NOT_WALKED.iter().any(|n| e.file_name() == *n) => {
                    dirs.push(p)
                }
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
    /// listens on the stack's ports moved by its offset, there and here, as
    /// a local lane does (ADR-0029 point 4).
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
    /// The profile it runs with (ADR-0041).
    pub profile: Option<&'a str>,
}

/// A lane leased on its node and admitted there, not yet synced or started
/// ([`RemoteLane::lease`]). Dropped without [`RemoteLane::launch`], the lease
/// expires on the node by itself.
pub struct Leased {
    pool: String,
    offer: devstack_node::coordinator::LeaseOffer,
    client: NodeClient,
    list: Vec<String>,
    elsewhere: Elsewhere,
    /// When the coordinator granted the lease.
    t0: Instant,
    /// The node or the coordinator runs another devstack than this
    /// machine, or does not say which (ADR-0031 point 8).
    warning: Option<String>,
}

impl Leased {
    /// The node's name.
    pub fn node(&self) -> &str {
        &self.offer.node.name
    }

    /// Whether the node or the coordinator runs another devstack.
    pub fn warning(&self) -> Option<&str> {
        self.warning.as_deref()
    }
}

/// A phase of a lane's start on a node is over: `remote_lane_phase` with
/// its duration, so each phase shows up as a duration of its own (B1: a
/// start of minutes could not be broken down).
fn phase_over(lane: &str, node: &str, phase: &str, ms: u64) {
    crate::diag::info(
        "remote_lane_phase",
        serde_json::json!({ "lane": lane, "node": node, "phase": phase, "ms": ms }),
    );
}

impl RemoteLane {
    /// Lease the lane on its node, sync it, start it there, and follow it.
    pub fn start(rt: &tokio::runtime::Handle, spec: RemoteSpec<'_>) -> Result<Arc<Self>, String> {
        let leased = Self::lease(rt, &spec)?;
        Self::launch(rt, spec, leased)
    }

    /// The part of a start that answers quickly and says whether the lane
    /// can go there at all: the coordinator's lease and the node's
    /// admission (step 1). Its files are listed before, so a failure there
    /// holds no capacity.
    pub fn lease(rt: &tokio::runtime::Handle, spec: &RemoteSpec<'_>) -> Result<Leased, String> {
        let t = Instant::now();
        (spec.step)(1, &format!("lease on {}", spec.on));
        let (pool, node) = resolve_on(spec.on)?;
        let local = local_member(&pool);
        let (list, elsewhere) = files(spec.worktree, spec.main, spec.extra, spec.scratch)?;
        let offer = local
            .lease(&LeaseRequest {
                node,
                repo: spec.repo.into(),
                lane: spec.name.into(),
                need: spec.need,
                devstack: Some(Release::this()),
            })
            .map_err(|e| format!("lease on `{}`: {e}", spec.on))?;
        // Another pool protocol misreads this machine (a newer coordinator
        // refused it already): refused here, before anything is admitted.
        // Another release, or one that does not say, is a warning
        // (ADR-0031 point 8).
        let warning = release_warning(&offer).map_err(|e| format!("lane `{}`: {e}", spec.name))?;
        if let Some(w) = &warning {
            crate::diag::warn(
                "pool_version_mismatch",
                serde_json::json!({ "lane": spec.name, "node": offer.node.name, "warning": w,
                    "version": devstack_node::version::VERSION,
                    "node_version": offer.node.devstack.as_ref().map(|r| r.version.clone()),
                    "coordinator_version": offer.coordinator.as_ref().map(|r| r.version.clone()) }),
            );
        }
        // Without a client the node cannot be told: the offer expires.
        let client = NodeClient::new(&local, offer.node.clone()).map_err(|e| e.to_string())?;
        let t0 = Instant::now();
        let admitted = match rt.block_on(client.admit(&offer)) {
            Ok(l) => l,
            Err(e) => {
                let _ = rt.block_on(client.release(&offer.id));
                return Err(format!("lane `{}` on {}: {e}", spec.name, offer.node.name));
            }
        };
        // It starts from what it kept there (ADR-0038): said, with its age,
        // since its data may be from another branch.
        if let Some(used) = admitted.kept {
            let ago = (devstack_node::store::now().saturating_sub(used) / 60).max(1);
            let age = if ago < 120 {
                format!("{ago} min")
            } else if ago < 48 * 60 {
                format!("{} h", ago / 60)
            } else {
                format!("{} days", ago / (24 * 60))
            };
            (spec.step)(
                1,
                &format!(
                    "lease on {}: starts from its data kept there, last used {age} ago (`devstack lane reset {}` starts fresh)",
                    offer.node.name, spec.name
                ),
            );
            crate::diag::info(
                "remote_lane_kept",
                serde_json::json!({ "lane": spec.name, "node": offer.node.name, "used": used }),
            );
        }
        // It keeps data there from now on (ADR-0038).
        if let Some(state) = spec.scratch.parent() {
            note_ran_on(state, &pool, &offer.node);
        }
        phase_over(
            spec.name,
            &offer.node.name,
            "lease",
            t.elapsed().as_millis() as u64,
        );
        Ok(Leased {
            pool,
            offer,
            client,
            list,
            elsewhere,
            t0,
            warning,
        })
    }

    /// The rest of a start, which may take minutes (a first start pulls and
    /// builds images on the node): sync (step 2), start there (step 3),
    /// follow it. On failure the lease is released.
    pub fn launch(
        rt: &tokio::runtime::Handle,
        spec: RemoteSpec<'_>,
        leased: Leased,
    ) -> Result<Arc<Self>, String> {
        let RemoteSpec {
            name,
            worktree,
            main,
            extra,
            sharing,
            stack_file,
            scratch,
            ports,
            port_offset,
            forward,
            step,
            profile,
            ..
        } = spec;
        let Leased {
            pool,
            offer,
            client,
            list,
            elsewhere,
            t0,
            warning,
        } = leased;
        let mut hashes = HashCache::default();
        let started = rt.block_on(async {
            let t = Instant::now();
            step(2, &format!("sync: comparing {} files", list.len()));
            let s = client
                .sync_cached(
                    &offer.id,
                    worktree,
                    &list,
                    &elsewhere,
                    &mut hashes,
                    &|n, of, bytes| {
                        step(
                            2,
                            &format!("sync: {n} of {of} changed files sent ({} KiB)", bytes / 1024),
                        );
                    },
                )
                .await?;
            crate::diag::info(
                "remote_lane_synced",
                serde_json::json!({ "lane": name, "node": offer.node.name, "files": s.files,
                    "uploaded": s.uploaded, "bytes": s.bytes, "ms": t0.elapsed().as_millis() as u64 }),
            );
            phase_over(name, &offer.node.name, "sync", t.elapsed().as_millis() as u64);
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
                    profile,
                    Duration::from_secs(1800),
                    &|p| {
                        step(3, &format!("on {node}: {p}"));
                        crate::diag::info(
                            "remote_lane_starting",
                            serde_json::json!({ "lane": lane, "node": node, "phase": p }),
                        );
                    },
                    // Measured on the node: exact, however short.
                    &|p, ms| phase_over(&lane, &node, p, ms),
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
        // The worktree's changes are synced when they happen (ADR-0029
        // point 2); without a watcher, every few seconds.
        let dirty = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let watcher = {
            let (dirty, root) = (dirty.clone(), worktree.to_path_buf());
            let w = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                let relevant = match res {
                    Ok(ev) => {
                        ev.need_rescan()
                            || ev.paths.iter().any(|p| {
                                p.strip_prefix(&root).is_ok_and(|r| {
                                    !crate::watch::in_generated(
                                        &r.to_string_lossy().replace('\\', "/"),
                                    )
                                })
                            })
                    }
                    Err(_) => true,
                };
                if relevant {
                    dirty.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            })
            .and_then(|mut w| {
                use notify::Watcher as _;
                w.watch(worktree, notify::RecursiveMode::Recursive)
                    .map(|()| w)
            });
            match w {
                Ok(w) => Some(w),
                Err(e) => {
                    crate::diag::warn(
                        "remote_lane_unwatched",
                        serde_json::json!({ "lane": name, "error": e.to_string(),
                            "why": "the worktree is compared every few seconds instead" }),
                    );
                    None
                }
            }
        };
        let lane = Arc::new(Self {
            name: name.into(),
            pool,
            node: offer.node,
            lease: offer.id,
            warning,
            worktree: worktree.to_path_buf(),
            main: main.to_path_buf(),
            extra: extra.to_vec(),
            sharing: Mutex::new(None),
            owns_main: std::sync::atomic::AtomicBool::new(false),
            client,
            state: Mutex::new(RemoteState::default()),
            stopped: tokio::sync::watch::channel(false).0,
            ports,
            listening: tokio::sync::watch::channel(false).0,
            gone: tokio::sync::watch::channel(false).0,
            dirty: dirty.clone(),
            _watcher: watcher,
            hashes: Mutex::new(hashes),
        });
        if lane._watcher.is_none() {
            // No watcher: every tick counts as a change.
            let dirty = dirty.clone();
            let mut over = lane.stopped.subscribe();
            rt.spawn(async move {
                loop {
                    dirty.store(true, std::sync::atomic::Ordering::SeqCst);
                    tokio::select! {
                        _ = tokio::time::sleep(SYNC_EVERY) => {}
                        _ = over.wait_for(|s| *s) => return,
                    }
                }
            });
        }
        if sharing.is_some()
            && let Err(e) = lane.share(sharing)
        {
            lane.trouble(Some(format!("sharing: {e}")));
        }
        if forward {
            lane.start_forwarding(rt);
        }
        lane.follow(rt, scratch.to_path_buf());
        Ok(lane)
    }

    pub fn state(&self) -> RemoteState {
        self.state.lock().unwrap().clone()
    }

    /// Give the lane the main slot, or take it back: its workers run on the
    /// node, or no longer. Blocking.
    pub fn set_main(&self, rt: &tokio::runtime::Handle, owns: bool) -> Result<(), String> {
        self.owns_main
            .store(owns, std::sync::atomic::Ordering::SeqCst);
        match rt.block_on(self.client.api(
            &self.lease,
            "POST",
            "/v1/main-slot",
            Some(serde_json::json!({ "owns": owns })),
        )) {
            Ok((200, _)) => Ok(()),
            Ok((code, body)) => Err(format!("the lane answered {code}: {body}")),
            Err(e) => Err(e.to_string()),
        }
    }

    /// Its services that run now (healthy or running), read from the lane
    /// at once rather than from the last poll. Blocking.
    pub fn active_now(&self, rt: &tokio::runtime::Handle) -> Vec<String> {
        match rt.block_on(self.client.api(&self.lease, "GET", "/v1/stack", None)) {
            Ok((200, body)) => serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v["services"].as_array().cloned())
                .unwrap_or_default()
                .iter()
                .filter(|s| matches!(s["status"]["state"].as_str(), Some("healthy" | "running")))
                .filter_map(|s| s["name"].as_str().map(str::to_string))
                .collect(),
            _ => self.state().active,
        }
    }

    /// Whether it holds the main slot here.
    pub fn owns_main(&self) -> bool {
        self.owns_main.load(std::sync::atomic::Ordering::SeqCst)
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
    /// state. Three loops of their own: a node that does not answer stalls
    /// one of them, not the others (a renewal waits for no sync).
    fn follow(self: &Arc<Self>, rt: &tokio::runtime::Handle, scratch: PathBuf) {
        let me = self.clone();
        rt.spawn(async move { me.follow_sync(scratch).await });
        let me = self.clone();
        rt.spawn(async move { me.follow_renew().await });
        let me = self.clone();
        rt.spawn(async move { me.follow_state().await });
    }

    /// Resolves when the lane is stopped or gone.
    async fn over(&self) {
        let mut stopped = self.stopped.subscribe();
        let mut gone = self.gone.subscribe();
        tokio::select! {
            _ = stopped.wait_for(|s| *s) => {}
            _ = gone.wait_for(|g| *g) => {}
        }
    }

    /// Sleep `d`; `false` when the lane is over meanwhile.
    async fn tick(&self, d: Duration) -> bool {
        tokio::select! {
            _ = tokio::time::sleep(d) => true,
            _ = self.over() => false,
        }
    }

    /// Sync what changed in the worktree (ADR-0029 point 2): when the file
    /// watcher saw a change, and in full every [`FULL_SYNC_EVERY`] (what it
    /// does not see: the main tree's `[lanes] sync` files, lost events).
    /// The first look syncs once more: the fingerprint of what the first
    /// sync sent is not known, and the listing is all it costs.
    async fn follow_sync(self: Arc<Self>, scratch: PathBuf) {
        let mut seen: Option<Fingerprint> = None;
        let mut hashes = std::mem::take(&mut *self.hashes.lock().unwrap());
        let mut full = Instant::now() - FULL_SYNC_EVERY;
        loop {
            if !self.tick(SYNC_EVERY).await {
                return;
            }
            let changed = self.dirty.swap(false, std::sync::atomic::Ordering::SeqCst);
            if !changed && full.elapsed() < FULL_SYNC_EVERY {
                continue;
            }
            if full.elapsed() >= FULL_SYNC_EVERY {
                full = Instant::now();
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
                            .sync_cached(
                                &self.lease,
                                &self.worktree,
                                &list,
                                &elsewhere,
                                &mut hashes,
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
                            Err(e) => {
                                // Tried again at the next tick.
                                self.dirty.store(true, std::sync::atomic::Ordering::SeqCst);
                                self.trouble(Some(format!("sync: {e}")));
                            }
                        }
                    }
                }
                Ok(Err(e)) => self.trouble(Some(format!("listing the worktree: {e}"))),
                Err(_) => {}
            }
        }
    }

    /// The lane is over for this machine: say why, stop listening, end
    /// its tunnels.
    fn end(&self, why: String, event: &str) {
        {
            let mut st = self.state.lock().unwrap();
            st.ended = true;
            st.error = Some(why);
        }
        self.listening.send_replace(false);
        self.gone.send_replace(true);
        crate::diag::warn(
            event,
            serde_json::json!({ "lane": self.name, "node": self.node.name, "lease": self.lease }),
        );
    }

    /// Renew the lease every [`RENEW_EVERY`]; tell the lane again that it
    /// holds the main slot and the coordinator whom it is shared with.
    async fn follow_renew(self: Arc<Self>) {
        let mut renewed = Instant::now();
        loop {
            if !self.tick(RENEW_EVERY).await {
                return;
            }
            match self.client.renew(&self.lease).await {
                Ok(_) => {
                    renewed = Instant::now();
                    if self.owns_main() {
                        let _ = self
                            .client
                            .api(
                                &self.lease,
                                "POST",
                                "/v1/main-slot",
                                Some(serde_json::json!({ "owns": true })),
                            )
                            .await;
                    }
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
                    return self.end(
                        format!(
                            "the lease on {} is over (an admin ended it, or it expired): \
                             `devstack up --on {}` starts the lane again",
                            self.node.name, self.node.name
                        ),
                        "remote_lane_ended",
                    );
                }
                Err(e) => {
                    self.trouble(Some(format!("renewing the lease: {e}")));
                    // Not renewed for longer than a lease lives: the node let
                    // it go, or cannot be reached; either way the lane is
                    // gone for this machine.
                    if renewed.elapsed() > LEASE_LIVES {
                        return self.end(
                            format!(
                                "{} has not answered for {} s: the lane counts as gone",
                                self.node.name,
                                renewed.elapsed().as_secs()
                            ),
                            "remote_lane_lost",
                        );
                    }
                }
            }
        }
    }

    /// Read the lane's state for the lists every [`STATE_EVERY`].
    async fn follow_state(self: Arc<Self>) {
        loop {
            if let Ok((200, body)) = self.client.api(&self.lease, "GET", "/v1/stack", None).await
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
                let active = svcs
                    .iter()
                    .filter(|s| {
                        matches!(s["status"]["state"].as_str(), Some("healthy" | "running"))
                    })
                    .filter_map(|s| s["name"].as_str().map(str::to_string))
                    .collect();
                let mut st = self.state.lock().unwrap();
                st.running = running;
                st.services = svcs.len();
                st.active = active;
            }
            if !self.tick(STATE_EVERY).await {
                return;
            }
        }
    }

    /// Listen on the lane's ports here (again): each on loopback, IPv4 and,
    /// where there is one, IPv6, every connection carried to its port of the
    /// lane on its node, bytes unchanged, so WebSockets, hot module
    /// replacement and TLS dev servers work as they do locally.
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
                        // A lease that is over ends its open connections too
                        // (a dev server's WebSocket would hang until TCP
                        // gives up).
                        let r = tokio::select! {
                            r = me.tunnel(tcp, there) => r,
                            _ = me.over() => Ok(()),
                        };
                        if let Err(e) = r {
                            crate::diag::warn_limited(
                                &format!("remote_lane_tunnel:{}:{there}", me.name),
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
            use http_body_util::BodyExt;
            let status = res.status();
            let body = res
                .into_body()
                .collect()
                .await
                .map(|b| String::from_utf8_lossy(&b.to_bytes()).trim().to_string())
                .unwrap_or_default();
            return Err(tunnel_error(
                &format!("the node answered {status}"),
                &body,
                &self.node,
            ));
        }
        let up = hyper::upgrade::on(&mut res)
            .await
            .map_err(|e| e.to_string())?;
        let mut up = hyper_util::rt::TokioIo::new(up);
        let open = crate::telemetry::Tunnel::open(&self.name, &self.node.name);
        tokio::io::copy_bidirectional(&mut up, &mut tcp)
            .await
            .map(|(a, b)| open.closed(a + b))
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

/// What a lease offer says of the node's and the coordinator's devstack:
/// `Err` for a node with another pool protocol, a warning for another
/// release or one that does not say.
fn release_warning(
    offer: &devstack_node::coordinator::LeaseOffer,
) -> Result<Option<String>, String> {
    let node = format!("the node {}", offer.node.name);
    if version::compat(offer.node.devstack.as_ref()) == Compat::Incompatible {
        return Err(version::note(&node, offer.node.devstack.as_ref()).unwrap_or_default());
    }
    let mut notes = Vec::new();
    if offer.coordinator.is_none() {
        // An older coordinator passes on no node's release either.
        notes.push(format!(
            "the pool's coordinator runs {}, so the node's is not known either; \
             if the lane fails there, compare `devstack --version` on both",
            version::describe(None)
        ));
    } else {
        notes.extend(version::note(&node, offer.node.devstack.as_ref()));
        notes.extend(version::note("the coordinator", offer.coordinator.as_ref()));
    }
    Ok((!notes.is_empty()).then(|| notes.join("; ")))
}

/// A tunnel the node or the lane did not open: what it said, and the
/// releases when they differ (an older lane daemon may serve other ports).
pub fn tunnel_error(what: &str, body: &str, node: &NodeRef) -> String {
    let said = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v["error"].as_str().map(str::to_string))
        .unwrap_or_else(|| body.lines().next().unwrap_or("").to_string());
    let mut out = what.to_string();
    if !said.is_empty() {
        out.push_str(": ");
        out.push_str(&said);
    }
    if let Some(n) = version::note(&format!("the node {}", node.name), node.devstack.as_ref()) {
        out.push_str(&format!(" ({n})"));
    }
    out
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
    use super::{NodeRef, Release, release_warning, tunnel_error, version};

    fn offer(
        node: Option<Release>,
        coordinator: Option<Release>,
    ) -> devstack_node::coordinator::LeaseOffer {
        devstack_node::coordinator::LeaseOffer {
            id: "l".into(),
            token: "t".into(),
            node: NodeRef {
                id: "n".into(),
                name: "build-box".into(),
                api: "https://x:1".into(),
                devstack: node,
            },
            coordinator,
        }
    }

    #[test]
    fn a_node_of_another_release_is_warned_about_or_refused() {
        let this = Release::this();
        let older = Release {
            version: "0.0.1".into(),
            ..this.clone()
        };
        let other_protocol = Release {
            protocol: this.protocol + 1,
            ..older.clone()
        };
        assert_eq!(
            release_warning(&offer(Some(this.clone()), Some(this.clone()))),
            Ok(None)
        );
        let w = release_warning(&offer(Some(older), Some(this.clone())))
            .unwrap()
            .unwrap();
        assert!(
            w.contains("build-box") && w.contains("0.0.1") && w.contains(version::VERSION),
            "{w}"
        );
        let w = release_warning(&offer(None, Some(this.clone())))
            .unwrap()
            .unwrap();
        assert!(w.contains("does not say"), "{w}");
        let w = release_warning(&offer(None, None)).unwrap().unwrap();
        assert!(w.contains("coordinator"), "{w}");
        let e = release_warning(&offer(Some(other_protocol), Some(this))).unwrap_err();
        assert!(e.contains("do not work together"), "{e}");
    }

    #[test]
    fn a_failed_tunnel_names_both_releases_when_they_differ() {
        let body = r#"{"error":"port 8091 is no service's or container's port in this stack"}"#;
        let node = offer(
            Some(Release {
                version: "0.0.1".into(),
                protocol: version::PROTOCOL,
            }),
            None,
        )
        .node;
        let e = tunnel_error("the node answered 403 Forbidden", body, &node);
        assert!(e.contains("port 8091 is no service's"), "{e}");
        assert!(e.contains("0.0.1") && e.contains(version::VERSION), "{e}");
        let same = NodeRef {
            devstack: Some(Release::this()),
            ..node
        };
        let e = tunnel_error("the node answered 403 Forbidden", body, &same);
        assert!(
            !e.contains(version::VERSION),
            "the same release is not named: {e}"
        );
    }

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
