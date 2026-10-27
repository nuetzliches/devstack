//! A lane on another machine of the pool (ADR-0025, ADR-0029, ADR-0031): the
//! worktree stays here, everything of the lane runs on a node. This side
//! holds the lease, keeps the node's mirror in step with the worktree, renews
//! the lease while the lane runs, and passes the lane's API through under
//! `/v1/lanes/<name>/v1/...`, so CLI, MCP and dashboard treat it like a
//! local lane (ADR-0025 point 6).

use devstack_node::coordinator::{LeaseRequest, NodeRef};
use devstack_node::member::Local;
use devstack_node::nodeclient::{Elsewhere, HashCache, NodeClient};
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
/// How long a warm lease's `prepare` runs may take (ADR-0065 point 5).
const WARM_WITHIN: Duration = Duration::from_secs(30 * 60);
/// The first devstack whose node takes warm leases.
const WARM_SINCE_NODE: &str = "0.24.0";
/// How often the lane's state is read for the lists.
const STATE_EVERY: Duration = Duration::from_secs(5);
/// How often what the lane's daemon logged is taken for telemetry (#287):
/// within the minute this daemon sends in.
const TELEMETRY_EVERY: Duration = Duration::from_secs(30);

/// What the lists show of a remote lane.
#[derive(Debug, Clone, Default)]
pub struct RemoteState {
    pub running: usize,
    pub services: usize,
    /// Its containers that run, of all (as [`crate::state::containers_running`]).
    pub containers_running: usize,
    pub containers: usize,
    /// What goes wrong now, by what it concerns (`sync`, `renew`,
    /// `sharing`, `port 3000`, `lease`, `disk`), each until that works again: a
    /// sync that works does not hide a port that is not forwarded.
    pub errors: std::collections::BTreeMap<String, String>,
    /// The lease is over (an admin ended it, or it expired): the lane no
    /// longer runs there.
    pub ended: bool,
    /// Its services that run now (healthy or running), by name.
    pub active: Vec<String>,
    /// Its containers' published ports this machine does not forward
    /// (`5353/udp`): another protocol than TCP and UDP (#192), or UDP where
    /// the node, or the daemon that started the lane, does not relay it
    /// (ADR-0056).
    pub not_forwarded: Vec<String>,
    /// Why, when there are such ports.
    pub not_forwarded_reason: Option<String>,
}

impl RemoteState {
    /// What goes wrong now, in one text (`None`: nothing).
    pub fn error(&self) -> Option<String> {
        (!self.errors.is_empty())
            .then(|| self.errors.values().cloned().collect::<Vec<_>>().join("; "))
    }
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
    /// What the node runs, as the lease offer said (`None`: it does not say).
    node_release: Option<Release>,
    /// What the coordinator runs, as the lease offer said: carried over a
    /// restart, so the next daemon warns against its own release.
    coordinator_release: Option<Release>,
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
    /// Its containers' UDP ports, `(on the node, here)`, relayed (ADR-0056).
    udp_ports: Vec<(u16, u16)>,
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
    /// Syncs wait: the lane is being removed (its worktree went).
    sync_held: std::sync::atomic::AtomicBool,
    /// Where it says in the lane's state that a daemon here follows it
    /// ([`FOLLOWED`]), once it is followed.
    followed: Mutex<Option<PathBuf>>,
}

/// In a lane's state while a daemon here follows it on a node (ADR-0052
/// point 6): its [`Carried`]. Gone when the lane stops, or its end is seen
/// here; left when the daemon restarts (the next one follows it again) or
/// ends without stopping it (a crash, a reboot), when the next daemon finds
/// it and says the lane is gone.
const FOLLOWED: &str = "followed.json";

/// The lane on a node whose state is `state` (`lane-<name>/`), as the
/// daemon that followed it last noted it.
pub fn followed(state: &Path) -> Option<Carried> {
    std::fs::read(state.join(FOLLOWED))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
}

/// Remove the note of lease `lease` in `state`; a note of another lease (the
/// lane moved to another node meanwhile) stays.
pub fn forget_followed(state: &Path, lease: &str) {
    if followed(state).is_some_and(|c| c.lease == lease) {
        let _ = std::fs::remove_file(state.join(FOLLOWED));
    }
}

/// devstack's per-user state, where this machine's memberships live.
pub fn state_dir() -> PathBuf {
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

/// Where a lane starts, as a client asked (ADR-0025, amended 2026-10-21):
/// `on` as given, `local`/`here` this machine (`None`), no `on` the
/// machine's default (`devstack pool default-on`), here when it has none or
/// its pool is not joined.
pub fn effective_on(on: Option<&str>) -> Option<String> {
    match on.map(str::trim).filter(|o| !o.is_empty()) {
        Some(o) if devstack_node::member::is_here(o) || is_this_machine(o) => None,
        Some(o) => Some(o.to_string()),
        None => {
            let d = devstack_node::member::default_on(&state_dir())?;
            let (pool, _) = resolve_on(&d).ok()?;
            local_member(&pool)
                .membership()
                .ok()
                .flatten()
                .filter(|m| m.state == devstack_node::store::State::Approved)
                .map(|_| d)
        }
    }
}

/// `on` names this machine in its pool, by its member id or name: the
/// pool would refuse it as a node not granted lanes (#310); it means
/// `local`.
pub fn is_this_machine(on: &str) -> bool {
    let Ok((pool, node)) = resolve_on(on) else {
        return false;
    };
    local_member(&pool)
        .membership()
        .ok()
        .flatten()
        .is_some_and(|m| m.id.eq_ignore_ascii_case(&node) || m.name.eq_ignore_ascii_case(&node))
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

/// The files to sync: what git does not ignore (or `listed`), and what
/// `[lanes] sync` names, from the worktree or else from the main tree.
fn files(
    worktree: &Path,
    main: &Path,
    extra: &[String],
    scratch: &Path,
    listed: Option<&[String]>,
) -> Result<(Vec<String>, Elsewhere), String> {
    let mut list = match listed {
        Some(l) => l.to_vec(),
        None => devstack_build::snapshot::Snapshot::open(worktree, scratch)
            .and_then(|s| s.list())
            .map_err(|e| e.to_string())?,
    };
    let mut elsewhere = Elsewhere::new();
    let mut have: std::collections::HashSet<String> = list.iter().cloned().collect();
    for pattern in extra {
        // A path without a wildcard (the stack file, ADR-0055) is looked up,
        // not walked for: a walk from the root reads the whole tree.
        if !pattern.contains(['*', '?', '[', '{']) {
            let skipped = pattern.split('/').any(|c| NOT_WALKED.contains(&c));
            if skipped || have.contains(pattern.as_str()) {
                continue;
            }
            if let Some(root) = [worktree, main]
                .into_iter()
                .find(|r| r.join(pattern).is_file())
            {
                if root != worktree {
                    elsewhere.insert(pattern.clone(), root.join(pattern));
                }
                have.insert(pattern.clone());
                list.push(pattern.clone());
            }
            continue;
        }
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
    /// What the lane is expected to take, as this machine estimates it.
    pub need: &'a Expected,
    /// What its stack file declares it needs at least (ADR-0049 point 5).
    pub floor: Option<devstack_node::share::Need>,
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
    /// Its background workers run (`devstack up --workers`, #314).
    pub workers: bool,
    /// A warm lease (ADR-0065 point 5): only `prepare`, no infrastructure.
    pub warm: bool,
    /// The files of `worktree` to sync, when git cannot tell them: a ref's
    /// checkout under `.devstack/`, which git ignores (#298).
    pub files: Option<&'a [String]>,
}

/// What a lane is expected to need as this machine estimates it (ADR-0049
/// point 4), and from what; the coordinator takes what the lane's
/// repository took in the pool first.
#[derive(Debug, Clone, Default)]
pub struct Expected {
    pub need: devstack_node::share::Need,
    pub from: String,
}

/// What a stack file declares a lane needs at least, as the pool reads it;
/// `None`: nothing.
pub fn floor_of(n: &devstack_config::model::Need) -> Option<devstack_node::share::Need> {
    if n.is_empty() {
        return None;
    }
    Some(devstack_node::share::Need {
        cpus: n.cpus.unwrap_or(0.0),
        memory: n.memory.unwrap_or(0),
        disk_repo: 0,
        disk_lane: n.disk.unwrap_or(0),
        gpus: n.gpus.as_ref().map(|g| devstack_node::share::GpuNeed {
            count: g.count,
            memory: g.memory.unwrap_or(0),
        }),
    })
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

/// What a lane on a node is made of here, for [`RemoteLane::assemble`].
struct Parts<'a> {
    name: &'a str,
    pool: String,
    node: NodeRef,
    lease: String,
    warning: Option<String>,
    coordinator: Option<Release>,
    worktree: &'a Path,
    main: &'a Path,
    extra: &'a [String],
    client: NodeClient,
    ports: Vec<(u16, u16)>,
    udp_ports: Vec<(u16, u16)>,
    hashes: HashCache,
}

/// A lane on a node as a daemon that restarts leaves it for the next one
/// (ADR-0052): enough to renew its lease and follow it again.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Carried {
    pub pool: String,
    pub node: NodeRef,
    pub lease: String,
    /// Its ports, `(on the node, here)`, its containers' included.
    pub ports: Vec<(u16, u16)>,
    /// Its containers' UDP ports, relayed (ADR-0056); absent from a daemon
    /// before that, whose lane relays none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub udp_ports: Vec<(u16, u16)>,
    /// What the coordinator ran when it granted the lease; absent from a
    /// daemon before 0.20.1, which carried its warning as a text. The next
    /// daemon makes the warning again against its own release
    /// ([`carried_warning`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinator: Option<Release>,
}

/// Watch `worktree` for changes to sync (ADR-0029 point 2): `dirty` is set
/// by each one that is no generated file. `None`, said, when it cannot be
/// watched: the lane then compares it every few seconds.
fn watch_worktree(
    name: &str,
    worktree: &Path,
    dirty: &Arc<std::sync::atomic::AtomicBool>,
) -> Option<notify::RecommendedWatcher> {
    let (dirty, root) = (dirty.clone(), worktree.to_path_buf());
    let w = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let relevant = match res {
            Ok(ev) => {
                ev.need_rescan()
                    || ev.paths.iter().any(|p| {
                        p.strip_prefix(&root).is_ok_and(|r| {
                            !crate::watch::in_generated(&r.to_string_lossy().replace('\\', "/"))
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
        let (list, elsewhere) = files(
            spec.worktree,
            spec.main,
            spec.extra,
            spec.scratch,
            spec.files,
        )?;
        let offer = local
            .lease(&LeaseRequest {
                node,
                repo: spec.repo.into(),
                lane: spec.name.into(),
                need: spec.need.need,
                need_from: Some(spec.need.from.clone()),
                floor: spec.floor,
                profile: spec.profile.map(str::to_string),
                devstack: Some(Release::this()),
                warm: spec.warm,
            })
            .map_err(|e| format!("lease on `{}`: {e}", spec.on))?;
        // Another pool protocol misreads this machine (a newer coordinator
        // refused it already): refused here, before anything is admitted.
        // Another release, or one that does not say, is a warning
        // (ADR-0031 point 8).
        let mut warning =
            release_warning(&offer).map_err(|e| format!("lane `{}`: {e}", spec.name))?;
        // An agent before 0.26.0 does not pass `--workers` on (#314).
        if spec.workers && !takes_workers(offer.node.devstack.as_ref()) {
            let w = format!(
                "the node {} runs {}, which starts the lane without its background workers whatever `--workers` says; `devstack up --workers --on local` runs them here",
                offer.node.name,
                version::describe(offer.node.devstack.as_ref())
            );
            warning = Some(match warning {
                Some(o) => format!("{o}; {w}"),
                None => w,
            });
        }
        // What the lease holds, and from what (ADR-0049 point 10).
        if let Some(n) = &offer.need {
            let from = offer.need_from.as_deref().unwrap_or("");
            crate::diag::info(
                "remote_lane_need",
                serde_json::json!({ "lane": spec.name, "node": offer.node.name,
                    "cpus": n.cpus, "memory": n.memory, "disk_repo": n.disk_repo,
                    "disk_lane": n.disk_lane, "from": from }),
            );
            (spec.step)(
                1,
                &format!(
                    "lease on {}: {:.1} CPU, {} memory, {} disk ({from})",
                    offer.node.name,
                    n.cpus,
                    devstack_node::coordinator::gib(n.memory),
                    devstack_node::coordinator::gib(n.disk())
                ),
            );
        }
        // The coordinator chose the node (`pool`): said why (ADR-0026 point 3).
        if let Some(why) = &offer.why {
            crate::diag::info(
                "remote_lane_placed",
                serde_json::json!({ "lane": spec.name, "node": offer.node.name, "why": why }),
            );
            (spec.step)(1, &format!("lease on {}: {why}", offer.node.name));
        }
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

    /// A warm lease on a node (ADR-0065 point 5): leased, `spec.worktree` (a
    /// ref's checkout) synced as its mirror, started there (its container
    /// runs only the `prepare` the member's store lacks), asked until its
    /// runs are done, released. What each service's run did.
    pub fn warm(
        rt: &tokio::runtime::Handle,
        spec: RemoteSpec<'_>,
        cancel: &dyn Fn() -> bool,
    ) -> Result<Vec<serde_json::Value>, String> {
        let Leased {
            offer,
            client,
            list,
            elsewhere,
            ..
        } = Self::lease(rt, &spec)?;
        // A node that does not know warm leases would start a whole lane: it
        // runs this release or a later one, or exactly this devstack.
        let node_version = offer.node.devstack.as_ref().map(|r| r.version.as_str());
        let knows = node_version.and_then(devstack_config::version_order)
            >= devstack_config::version_order(WARM_SINCE_NODE)
            || node_version == Some(devstack_config::VERSION);
        if !knows {
            let _ = rt.block_on(client.release(&offer.id));
            return Err(format!(
                "{} runs a devstack before {WARM_SINCE_NODE}, which takes no warm leases",
                offer.node.name
            ));
        }
        let renewing = {
            let client = client.clone();
            let lease = offer.id.clone();
            rt.spawn(async move {
                loop {
                    tokio::time::sleep(RENEW_EVERY).await;
                    let _ = client.renew(&lease).await;
                }
            })
        };
        let ran = (|| {
            let mut hashes = HashCache::default();
            rt.block_on(client.sync_cached(
                &offer.id,
                spec.worktree,
                &list,
                &elsewhere,
                &mut hashes,
                &|_, _, _| {},
            ))
            .map_err(|e| format!("sync: {e}"))?;
            rt.block_on(client.start(
                &offer.id,
                spec.stack_file,
                None,
                0,
                None,
                false,
                false,
                Duration::from_secs(1800),
                &|_| {},
                &|_, _| {},
            ))
            .map_err(|e| format!("start: {e}"))?;
            let until = Instant::now() + WARM_WITHIN;
            loop {
                if cancel() {
                    return Err("stopped".to_string());
                }
                match rt.block_on(client.api(&offer.id, "GET", "/v1/warm", None)) {
                    Ok((200, body)) => {
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body)
                            && v["done"] == true
                        {
                            return Ok(v["results"].as_array().cloned().unwrap_or_default());
                        }
                    }
                    Ok((404, _)) => return Err("the lane's daemon runs no warm lease".into()),
                    _ => {}
                }
                if Instant::now() >= until {
                    return Err(format!(
                        "not done within {} min",
                        WARM_WITHIN.as_secs() / 60
                    ));
                }
                std::thread::sleep(Duration::from_secs(2));
            }
        })();
        renewing.abort();
        let _ = rt.block_on(client.release(&offer.id));
        ran
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
            workers,
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
        let unwritten: Mutex<Option<String>> = Mutex::new(None);
        // The lease lives two minutes unless renewed: a sync of a large tree
        // (90 s on a member whose virus scanner checks each file) and the
        // start after it take longer. Renewed meanwhile, until the lane is
        // followed (its own renewals take over).
        let renewing = {
            let client = client.clone();
            let lease = offer.id.clone();
            rt.spawn(async move {
                loop {
                    tokio::time::sleep(RENEW_EVERY).await;
                    let _ = client.renew(&lease).await;
                }
            })
        };
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
                            &format!(
                                "sync: {n} of {of} changed files sent ({} KiB)",
                                bytes / 1024
                            ),
                        );
                    },
                )
                .await?;
            crate::diag::info(
                "remote_lane_synced",
                serde_json::json!({ "lane": name, "node": offer.node.name, "files": s.files,
                    "uploaded": s.uploaded, "bytes": s.bytes, "ms": t0.elapsed().as_millis() as u64,
                    "hashed": s.hashed, "hash_ms": s.hash_ms, "compare_ms": s.compare_ms,
                    "upload_ms": s.upload_ms, "apply_ms": s.apply_ms }),
            );
            // Files the node could not write: the lane starts with the rest,
            // and says so (#128).
            if !s.failed.is_empty() {
                let e = not_written(&s.failed);
                step(2, &format!("sync: {e}"));
                crate::diag::warn(
                    "remote_lane_sync_failed",
                    serde_json::json!({ "lane": name, "node": offer.node.name,
                        "error": e.lines().next().unwrap_or_default() }),
                );
                *unwritten.lock().unwrap() = Some(e);
            }
            phase_over(
                name,
                &offer.node.name,
                "sync",
                t.elapsed().as_millis() as u64,
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
                    profile,
                    // This machine relays UDP (ADR-0056); an older node
                    // does not read it, and its lane names none.
                    true,
                    workers,
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
        renewing.abort();
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
        let udp_ports = started.infra_udp_ports.iter().map(|p| (*p, *p)).collect();
        crate::diag::info(
            "remote_lane_started",
            serde_json::json!({ "lane": name, "node": offer.node.name, "lease": offer.id,
                "ms": t0.elapsed().as_millis() as u64 }),
        );
        let lane = Self::assemble(
            rt,
            Parts {
                name,
                pool,
                node: offer.node,
                lease: offer.id,
                warning,
                coordinator: offer.coordinator,
                worktree,
                main,
                extra,
                client,
                ports,
                udp_ports,
                hashes,
            },
        );
        if let Some(e) = unwritten.into_inner().unwrap() {
            lane.trouble("sync", Some(format!("sync: {e}")));
        }
        lane.take_on(rt, sharing, forward, scratch);
        Ok(lane)
    }

    /// The lane as a lease on its node and what follows it here, not yet
    /// followed: its worktree watched (ADR-0029 point 2; without a watcher,
    /// compared every few seconds).
    fn assemble(rt: &tokio::runtime::Handle, p: Parts<'_>) -> Arc<Self> {
        let dirty = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let watcher = watch_worktree(p.name, p.worktree, &dirty);
        let node_release = p.node.devstack.clone();
        let lane = Arc::new(Self {
            name: p.name.into(),
            pool: p.pool,
            node: p.node,
            lease: p.lease,
            warning: p.warning,
            node_release,
            coordinator_release: p.coordinator,
            worktree: p.worktree.to_path_buf(),
            main: p.main.to_path_buf(),
            extra: p.extra.to_vec(),
            sharing: Mutex::new(None),
            owns_main: std::sync::atomic::AtomicBool::new(false),
            client: p.client,
            state: Mutex::new(RemoteState::default()),
            stopped: tokio::sync::watch::channel(false).0,
            ports: p.ports,
            udp_ports: p.udp_ports,
            listening: tokio::sync::watch::channel(false).0,
            gone: tokio::sync::watch::channel(false).0,
            dirty: dirty.clone(),
            _watcher: watcher,
            hashes: Mutex::new(p.hashes),
            followed: Mutex::new(None),
            sync_held: std::sync::atomic::AtomicBool::new(false),
        });
        if lane._watcher.is_none() {
            // No watcher: every tick counts as a change.
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
        lane
    }

    /// Follow the lane from here: shared as said, its ports listened on
    /// (`forward`), synced, renewed, its state read.
    fn take_on(
        self: &Arc<Self>,
        rt: &tokio::runtime::Handle,
        sharing: Option<devstack_node::coordinator::Sharing>,
        forward: bool,
        scratch: &Path,
    ) {
        if sharing.is_some()
            && let Err(e) = self.share(sharing)
        {
            self.trouble("sharing", Some(format!("sharing: {e}")));
        }
        if forward {
            self.start_forwarding(rt);
        }
        if let Some(state) = scratch.parent() {
            let path = state.join(FOLLOWED);
            let tmp = state.join(format!("{FOLLOWED}.tmp"));
            let written = serde_json::to_vec_pretty(&self.carried())
                .map_err(std::io::Error::other)
                .and_then(|b| std::fs::write(&tmp, b))
                .and_then(|()| std::fs::rename(&tmp, &path));
            if written.is_ok() {
                *self.followed.lock().unwrap() = Some(path);
            }
        }
        self.follow(rt, scratch.to_path_buf());
    }

    /// No daemon follows it from now on: the note goes.
    fn unfollow(&self) {
        if let Some(state) = self
            .followed
            .lock()
            .unwrap()
            .take()
            .and_then(|p| p.parent().map(Path::to_path_buf))
        {
            forget_followed(&state, &self.lease);
        }
    }

    /// What the next daemon needs to follow the lane after a restart
    /// (ADR-0052).
    pub fn carried(&self) -> Carried {
        Carried {
            pool: self.pool.clone(),
            node: self.node.clone(),
            lease: self.lease.clone(),
            ports: self.ports.clone(),
            udp_ports: self.udp_ports.clone(),
            coordinator: self.coordinator_release.clone(),
        }
    }

    /// The daemon stops for a restart (ADR-0052): the lane is no longer
    /// followed from here; its lease stays for the next daemon to renew.
    pub fn leave(&self) {
        self.listening.send_replace(false);
        self.stopped.send_replace(true);
        crate::diag::info(
            "remote_lane_left",
            serde_json::json!({ "lane": self.name, "node": self.node.name, "lease": self.lease }),
        );
    }

    /// Follow the lane `name` a daemon before this one followed (ADR-0052):
    /// its lease renewed first, then synced, renewed and its ports listened
    /// on here, as after a start. `Err` when the lease is over or the node
    /// does not answer. Blocking.
    #[allow(clippy::too_many_arguments)]
    pub fn adopt(
        rt: &tokio::runtime::Handle,
        name: &str,
        c: Carried,
        worktree: &Path,
        main: &Path,
        extra: &[String],
        sharing: Option<devstack_node::coordinator::Sharing>,
        scratch: &Path,
    ) -> Result<Arc<Self>, String> {
        let client =
            NodeClient::new(&local_member(&c.pool), c.node.clone()).map_err(|e| e.to_string())?;
        let renewed = match rt.block_on(client.renew(&c.lease)) {
            Ok(l) => l,
            Err(devstack_node::nodeclient::ClientError::Status(404, _)) => {
                return Err(format!(
                    "its lease on {} is over (it expired, or an admin ended it)",
                    c.node.name
                ));
            }
            Err(e) => return Err(format!("renewing its lease on {}: {e}", c.node.name)),
        };
        let warning = carried_warning(&c);
        let lane = Self::assemble(
            rt,
            Parts {
                name,
                pool: c.pool,
                node: c.node,
                lease: c.lease,
                warning,
                coordinator: c.coordinator,
                worktree,
                main,
                extra,
                client,
                ports: c.ports,
                udp_ports: c.udp_ports,
                hashes: HashCache::default(),
            },
        );
        lane.trouble("disk", renewed.warning);
        lane.take_on(rt, sharing, true, scratch);
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
        if r.is_ok() {
            self.trouble("sharing", None);
        }
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

    /// What goes wrong with `about` now (`None`: it works again).
    fn trouble(&self, about: &str, what: Option<String>) {
        let mut st = self.state.lock().unwrap();
        match what {
            Some(w) => st.errors.insert(about.to_string(), w),
            None => st.errors.remove(about),
        };
    }

    /// A sync that failed, in part or in full, goes to the diagnostic log
    /// (#128): at most one line per lane every ten seconds, with how many
    /// were left out.
    fn sync_failed(&self, error: &str) {
        crate::diag::warn_limited(
            &format!("remote_lane_sync_failed:{}", self.name),
            "remote_lane_sync_failed",
            serde_json::json!({ "lane": self.name, "node": self.node.name,
                "error": error.lines().next().unwrap_or_default() }),
        );
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
        let me = self.clone();
        rt.spawn(async move { me.follow_telemetry().await });
    }

    /// Take what the lane's daemon logged and send it with this daemon's
    /// lines, as lines of this lane on its node, every [`TELEMETRY_EVERY`]
    /// while this daemon sends any (#287): the lane's daemon has no
    /// membership and no consent of its own. A lane daemon that does not
    /// keep them (an older node's) answers 404, and nothing is taken.
    async fn follow_telemetry(self: Arc<Self>) {
        while self.tick(TELEMETRY_EVERY).await {
            if !crate::telemetry::active() {
                continue;
            }
            let Ok((200, body)) = self
                .client
                .api(&self.lease, "POST", "/v1/telemetry/take", None)
                .await
            else {
                continue;
            };
            let lines = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v["lines"].as_array().cloned())
                .unwrap_or_default();
            crate::telemetry::record_relayed(&self.name, &self.node.name, lines);
        }
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
            // Its worktree on its way out (git removes `.git` early), or the
            // lane being removed here: what the worktree loses meanwhile is
            // not sent, or the lane's builds on the node fail for a lane
            // about to go (seen 2026-10-23: six failure notices).
            if self.sync_held.load(std::sync::atomic::Ordering::SeqCst)
                || !self.worktree.join(".git").exists()
            {
                continue;
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
                let (list, elsewhere) = files(&wt, &main, &extra, &sc, None)?;
                let fp = fingerprint(&wt, &list, &elsewhere);
                Ok::<_, String>((list, elsewhere, fp))
            })
            .await;
            match listed {
                Ok(Ok((list, elsewhere, fp))) => {
                    if seen.as_ref() != Some(&fp) {
                        let synced = tokio::select! {
                            r = self.client.sync_cached(
                                &self.lease,
                                &self.worktree,
                                &list,
                                &elsewhere,
                                &mut hashes,
                                &|_, _, _| {},
                            ) => r,
                            // Stopped meanwhile: its lease is gone, and the
                            // sync's failure is no news (a 404 logged as a
                            // failed sync, seen 2026-10-23).
                            _ = self.over() => return,
                        };
                        match synced {
                            Ok(s) => {
                                if s.written + s.removed > 0 {
                                    crate::diag::info(
                                        "remote_lane_synced",
                                        serde_json::json!({ "lane": self.name, "uploaded": s.uploaded,
                                            "written": s.written, "removed": s.removed }),
                                    );
                                }
                                if s.failed.is_empty() {
                                    seen = Some(fp);
                                    self.trouble("sync", None);
                                } else {
                                    // The rest landed; these are tried
                                    // again with the next change or the
                                    // next full sync, not every tick.
                                    let e = not_written(&s.failed);
                                    self.sync_failed(&e);
                                    self.trouble("sync", Some(format!("sync: {e}")));
                                }
                            }
                            Err(e) => {
                                // Tried again at the next tick.
                                self.dirty.store(true, std::sync::atomic::Ordering::SeqCst);
                                self.sync_failed(&e.to_string());
                                self.trouble("sync", Some(format!("sync: {e}")));
                            }
                        }
                    }
                }
                Ok(Err(e)) => self.trouble("sync", Some(format!("listing the worktree: {e}"))),
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
            st.errors.insert("lease".into(), why);
        }
        self.unfollow();
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
                Ok(l) => {
                    renewed = Instant::now();
                    self.trouble("renew", None);
                    // The node is short of disk (ADR-0049 point 8): shown
                    // with the lane while it is, logged when it begins.
                    let short = self.state.lock().unwrap().errors.contains_key("disk");
                    if let Some(w) = &l.warning
                        && !short
                    {
                        crate::diag::warn(
                            "remote_lane_disk",
                            serde_json::json!({ "lane": self.name, "node": self.node.name, "warning": w }),
                        );
                    }
                    self.trouble("disk", l.warning);
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
                        let shared = tokio::task::spawn_blocking(move || {
                            local_member(&pool).share_lease(&lease, Some(&sh))
                        })
                        .await;
                        if matches!(shared, Ok(Ok(_))) {
                            self.trouble("sharing", None);
                        }
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
                    self.trouble("renew", Some(format!("renewing the lease: {e}")));
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
                // Read as JSON: the node may run another devstack.
                let all = v["infra"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .flat_map(|g| g["containers"].as_array().cloned().unwrap_or_default())
                    .filter(|c| c["managed"].as_bool().unwrap_or(true))
                    .collect::<Vec<_>>();
                let mut st = self.state.lock().unwrap();
                st.running = running;
                st.services = svcs.len();
                st.containers_running = all.iter().filter(|c| c["state"] == "running").count();
                st.containers = all.len();
                st.active = active;
                st.not_forwarded = not_forwarded(&all);
                st.not_forwarded_reason = self.not_forwarded_reason(&st.not_forwarded);
            }
            if !self.tick(STATE_EVERY).await {
                return;
            }
        }
    }

    /// Listen on the lane's ports here (again): each on loopback, IPv4 and,
    /// where there is one, IPv6, every connection carried to its port of the
    /// lane on its node, bytes unchanged, so WebSockets, hot module
    /// replacement and TLS dev servers work as they do locally. Its UDP
    /// ports are relayed alike, a tunnel per client (ADR-0056).
    pub fn start_forwarding(self: &Arc<Self>, rt: &tokio::runtime::Handle) {
        if self.listening.send_replace(true) {
            return;
        }
        for &(there, here) in &self.ports {
            let l = self.clone();
            rt.spawn(async move { l.forward_port(there, here).await });
        }
        for &(there, here) in &self.udp_ports {
            let l = self.clone();
            rt.spawn(async move { l.forward_udp(there, here).await });
        }
    }

    /// Stop listening on the lane's ports here; open connections stay.
    /// Returns once the ports are free (or after two seconds).
    pub fn stop_forwarding(&self) {
        self.listening.send_replace(false);
        let until = Instant::now() + Duration::from_secs(2);
        while Instant::now() < until
            && (self
                .ports
                .iter()
                .any(|(_, here)| crate::supervisor::port_in_use(*here))
                || self
                    .udp_ports
                    .iter()
                    .any(|(_, here)| std::net::UdpSocket::bind(("127.0.0.1", *here)).is_err()))
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

    /// Bind with `bind`, tried again while something else holds the port
    /// (a container on its way out): every second for half a minute, then
    /// every 15 s, as long as the lane forwards. The lane says so meanwhile,
    /// under `about` (`port 3000`). `None`: it no longer forwards.
    async fn bind_retrying<T, F, Fut>(&self, about: &str, bind: F) -> Option<T>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = std::io::Result<T>>,
    {
        let t0 = Instant::now();
        let mut stopped = self.stopped.subscribe();
        let mut listening = self.listening.subscribe();
        loop {
            match bind().await {
                Ok(l) => {
                    self.trouble(about, None);
                    return Some(l);
                }
                Err(e) => {
                    self.trouble(
                        about,
                        Some(format!(
                            "{about} here: {e} (something else holds it; tried again until it is free)"
                        )),
                    );
                    let wait = if t0.elapsed() < Duration::from_secs(30) {
                        Duration::from_secs(1)
                    } else {
                        Duration::from_secs(15)
                    };
                    tokio::select! {
                        _ = tokio::time::sleep(wait) => {}
                        _ = stopped.wait_for(|s| *s) => return None,
                        _ = listening.wait_for(|l| !*l) => return None,
                    }
                }
            }
        }
    }

    async fn forward_port(self: Arc<Self>, there: u16, here: u16) {
        let Some(l) = self
            .bind_retrying(&format!("port {here}"), || {
                tokio::net::TcpListener::bind(("127.0.0.1", here))
            })
            .await
        else {
            return;
        };
        let mut listeners = vec![l];
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

    /// Relay a UDP port of the lane's containers here (ADR-0056): each
    /// client's datagrams through a tunnel of its own to the lane daemon,
    /// which sends them on to the container, until the lane no longer
    /// forwards.
    async fn forward_udp(self: Arc<Self>, there: u16, here: u16) {
        let Some(s) = self
            .bind_retrying(&format!("port {here}/udp"), || {
                tokio::net::UdpSocket::bind(("127.0.0.1", here))
            })
            .await
        else {
            return;
        };
        let mut sockets = vec![s];
        if let Ok(s) = tokio::net::UdpSocket::bind(("::1", here)).await {
            sockets.push(s);
        }
        for socket in sockets {
            let me = self.clone();
            tokio::spawn(async move {
                let mut stopped = me.stopped.subscribe();
                let mut listening = me.listening.subscribe();
                let opener = me.clone();
                let open = move || {
                    let me = opener.clone();
                    async move { me.open_udp(there).await }
                };
                let (name, key) = (
                    me.name.clone(),
                    format!("remote_lane_udp:{}:{there}", me.name),
                );
                let report = Arc::new(move |e: String| {
                    crate::diag::warn_limited(
                        &key,
                        "remote_lane_udp",
                        serde_json::json!({ "lane": name, "port": there, "error": e }),
                    );
                });
                let limits = crate::udprelay::Limits::default();
                // Dropped, the relay ends its sessions and lets the port go.
                tokio::select! {
                    _ = crate::udprelay::relay(socket, open, limits, report) => {}
                    _ = stopped.wait_for(|s| *s) => {}
                    _ = listening.wait_for(|l| !*l) => {}
                    _ = me.over() => {}
                }
            });
        }
    }

    /// A UDP session's tunnel, through the node agent to the lane daemon.
    async fn open_udp(
        &self,
        port: u16,
    ) -> Result<crate::udprelay::Opened<hyper_util::rt::TokioIo<hyper::upgrade::Upgraded>>, String>
    {
        let stream = self.open_tunnel(port, crate::udprelay::PROTOCOL).await?;
        let open = crate::telemetry::Tunnel::open(&self.name, &self.node.name);
        Ok(crate::udprelay::Opened {
            stream,
            closed: Some(Box::new(move |bytes| open.closed(bytes))),
        })
    }

    /// One connection, through the node agent to the lane daemon's tunnel.
    async fn tunnel(&self, mut tcp: tokio::net::TcpStream, port: u16) -> Result<(), String> {
        let _ = tcp.set_nodelay(true);
        let mut up = self.open_tunnel(port, crate::api::TUNNEL_PROTOCOL).await?;
        let open = crate::telemetry::Tunnel::open(&self.name, &self.node.name);
        match tokio::io::copy_bidirectional(&mut up, &mut tcp).await {
            Ok((a, b)) => {
                open.closed(a + b);
                Ok(())
            }
            // One end hung up without a goodbye (a browser closing a tab or
            // a dev server's WebSocket): how connections end, not a fault.
            Err(e) if closed_by_a_peer(&e) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }

    /// A tunnel to `port` of the lane, through the node agent to the lane
    /// daemon's `/v1/tunnel/{port}`, upgraded to `protocol`.
    async fn open_tunnel(
        &self,
        port: u16,
        protocol: &str,
    ) -> Result<hyper_util::rt::TokioIo<hyper::upgrade::Upgraded>, String> {
        let req = axum::http::Request::builder()
            .method("GET")
            .uri(format!("/v1/tunnel/{port}"))
            .header(axum::http::header::CONNECTION, "upgrade")
            .header(axum::http::header::UPGRADE, protocol)
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
        Ok(hyper_util::rt::TokioIo::new(up))
    }

    /// Why its `not_forwarded` ports are not (ADR-0056 point 7).
    fn not_forwarded_reason(&self, ports: &[String]) -> Option<String> {
        not_forwarded_reason(ports, &self.node.name, self.node_release.as_ref())
    }

    /// Why the node cannot do what `method` on the lane daemon's `path`
    /// (`/v1/...`) asks of the lane's containers, as `stack` names them:
    /// its devstack is older than the release that does (#101, #128).
    /// `None` when it can, or the request asks nothing of them.
    pub fn too_old_for(
        &self,
        stack: &devstack_config::Stack,
        method: &axum::http::Method,
        path: &str,
    ) -> Option<String> {
        let min = containers_need(stack, method, path)?;
        if version::at_least(self.node_release.as_ref(), min) {
            return None;
        }
        Some(format!(
            "the node {} runs this lane's containers, and its devstack is too old to start, stop or restart them ({}; {min} or later does): update devstack on the node",
            self.node.name,
            version::describe(self.node_release.as_ref())
        ))
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

    /// No more syncs: the lane is being removed, and stops soon.
    pub fn hold_sync(&self) {
        self.sync_held
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Stop the lane: the node stops it and removes it with its mirror.
    pub fn stop(&self, rt: &tokio::runtime::Handle) {
        self.stopped.send_replace(true);
        self.unfollow();
        let _ = rt.block_on(self.client.release(&self.lease));
        crate::diag::info(
            "remote_lane_stopped",
            serde_json::json!({ "lane": self.name, "node": self.node.name, "lease": self.lease }),
        );
    }
}

/// The release a node needs to act on the containers `method path` names
/// (`/v1/...` of the lane daemon's API): `/v1/infra/{group}/{service}/…`
/// since #101, a service route that names containers (a group's restart,
/// a Compose service by its name) since #128. `None`: it names none.
fn containers_need(
    stack: &devstack_config::Stack,
    method: &axum::http::Method,
    path: &str,
) -> Option<&'static str> {
    use devstack_node::laneinfra::{Named, named};
    if method != axum::http::Method::POST {
        return None;
    }
    let rest = path.strip_prefix("/v1/")?;
    let parts: Vec<&str> = rest.split('/').collect();
    match parts.as_slice() {
        ["infra", _, _, a] if matches!(*a, "start" | "stop" | "restart") => Some("0.11.0"),
        [kind @ ("services" | "groups"), name, action] => {
            let route = if *kind == "services" {
                Named::Services
            } else {
                Named::Groups
            };
            (!named(stack, route, name, action).is_empty()).then_some("0.12.0")
        }
        _ => None,
    }
}

/// The files a node could not write in a lane's mirror, for its trouble
/// and the log: the first few by name, how many more.
fn not_written(failed: &[String]) -> String {
    const SHOWN: usize = 3;
    let mut s = format!(
        "not written on the node: {}",
        failed
            .iter()
            .take(SHOWN)
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("; ")
    );
    if failed.len() > SHOWN {
        s.push_str(&format!(" (and {} more)", failed.len() - SHOWN));
    }
    s
}

/// The published ports of a lane's containers (its view's, read as JSON)
/// that this machine does not forward, each once, in order (`5353/udp`).
/// A lane daemon from before #192 does not say.
fn not_forwarded(containers: &[serde_json::Value]) -> Vec<String> {
    let mut out: Vec<String> = containers
        .iter()
        .flat_map(|c| c["not_forwarded"].as_array().cloned().unwrap_or_default())
        .filter_map(|p| p.as_str().map(str::to_string))
        .collect();
    out.sort_by_key(|p| {
        let port = p.split('/').next().and_then(|n| n.parse::<u16>().ok());
        (port, p.clone())
    });
    out.dedup();
    out
}

/// The release from which a node relays a lane's UDP ports (ADR-0056).
const RELAYS_UDP: &str = "0.20.0";

/// Why a lane's ports `not_forwarded` (`5353/udp`) are not: `None` when
/// there are none. A UDP port is relayed unless the node is older than
/// [`RELAYS_UDP`], or the lane was started by a daemon that was.
fn not_forwarded_reason(ports: &[String], node: &str, release: Option<&Release>) -> Option<String> {
    if ports.is_empty() {
        return None;
    }
    if !ports.iter().any(|p| p.ends_with("/udp")) {
        return Some("devstack forwards TCP and UDP only".into());
    }
    if !version::at_least(release, RELAYS_UDP) {
        return Some(format!(
            "the node {node} runs {}, which forwards TCP only: devstack {RELAYS_UDP} or later there relays UDP",
            version::describe(release)
        ));
    }
    Some(
        "the lane was started by an earlier devstack here, which forwards TCP only: start it again to relay UDP"
            .into(),
    )
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

/// A node's agent passes `--workers` on to its lane (#314): 0.26.0 and
/// later. One that does not say counts as older.
fn takes_workers(node: Option<&devstack_node::version::Release>) -> bool {
    node.and_then(|r| devstack_config::version_triple(&r.version))
        .is_some_and(|v| v >= (0, 26, 0))
}

/// The warning of a lane a daemon before this one followed (ADR-0052), made
/// against this machine's release: the daemon that leased it compared with
/// its own (`this machine devstack 0.19.0` after an update to 0.20.0). The
/// coordinator's release is known from a daemon that carried it (0.20.1).
fn carried_warning(c: &Carried) -> Option<String> {
    let node = format!("the node {}", c.node.name);
    let notes: Vec<String> = version::note(&node, c.node.devstack.as_ref())
        .into_iter()
        .chain(
            c.coordinator
                .as_ref()
                .and_then(|r| version::note("the coordinator", Some(r))),
        )
        .collect();
    (!notes.is_empty()).then(|| notes.join("; "))
}

/// Did a connection of a tunnel end because one end hung up (reset, aborted,
/// a write to a closed socket)? Windows says "aborted" (10053) where a
/// browser closes a connection it kept alive.
fn closed_by_a_peer(e: &std::io::Error) -> bool {
    use std::io::ErrorKind::*;
    matches!(
        e.kind(),
        ConnectionReset | ConnectionAborted | BrokenPipe | UnexpectedEof
    )
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
    use super::{NodeRef, Release, RemoteState, release_warning, tunnel_error, version};

    #[test]
    fn the_note_of_a_followed_lane_goes_only_with_its_own_lease() {
        let state = tempfile::tempdir().unwrap();
        assert!(super::followed(state.path()).is_none());
        let carried = super::Carried {
            pool: "default".into(),
            node: NodeRef {
                id: "n".into(),
                name: "build-box".into(),
                api: "https://x:1".into(),
                devstack: None,
            },
            lease: "new".into(),
            ports: vec![(8080, 9080)],
            udp_ports: Vec::new(),
            coordinator: None,
        };
        std::fs::write(
            state.path().join(super::FOLLOWED),
            serde_json::to_vec(&carried).unwrap(),
        )
        .unwrap();
        // The lane moved to another lease: the old one's end leaves the note.
        super::forget_followed(state.path(), "old");
        let read = super::followed(state.path()).expect("still noted");
        assert_eq!(read.lease, "new");
        assert_eq!(read.node.name, "build-box");
        super::forget_followed(state.path(), "new");
        assert!(super::followed(state.path()).is_none());
    }

    #[test]
    fn a_peer_hanging_up_is_how_a_tunnel_ends() {
        use std::io::{Error, ErrorKind};
        for k in [
            ErrorKind::ConnectionReset,
            ErrorKind::ConnectionAborted,
            ErrorKind::BrokenPipe,
        ] {
            assert!(super::closed_by_a_peer(&Error::from(k)), "{k:?}");
        }
        assert!(!super::closed_by_a_peer(&Error::from(ErrorKind::TimedOut)));
        assert!(!super::closed_by_a_peer(&Error::other("tls")));
    }

    #[test]
    fn a_sync_that_works_does_not_hide_a_port_that_is_not_forwarded() {
        let mut st = RemoteState::default();
        assert_eq!(st.error(), None);
        st.errors
            .insert("port 3000".into(), "port 3000 here: in use".into());
        st.errors.insert("sync".into(), "sync: refused".into());
        assert_eq!(
            st.error().as_deref(),
            Some("port 3000 here: in use; sync: refused")
        );
        st.errors.remove("sync");
        assert_eq!(st.error().as_deref(), Some("port 3000 here: in use"));
    }

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
            why: None,
            need: None,
            need_from: None,
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
            super::files(wt.path(), main.path(), &extra, scratch.path(), None).unwrap();
        list.sort();
        assert_eq!(list, ["certs/a.pem", "certs/b.pem", "src/main.rs"]);
        assert_eq!(elsewhere.len(), 1);
        assert_eq!(elsewhere["certs/b.pem"], main.path().join("certs/b.pem"));
    }

    #[test]
    fn a_worktree_without_a_stack_file_is_synced_with_the_main_trees() {
        let main = tempfile::tempdir().unwrap();
        let wt = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let put = |root: &std::path::Path, rel: &str| {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, rel).unwrap();
        };
        put(wt.path(), "src/main.rs");
        put(main.path(), "config/devstack.toml");
        let extra = ["config/devstack.toml".to_string()];
        let (list, elsewhere) =
            super::files(wt.path(), main.path(), &extra, scratch.path(), None).unwrap();
        assert!(list.contains(&"config/devstack.toml".to_string()));
        assert_eq!(
            elsewhere["config/devstack.toml"],
            main.path().join("config/devstack.toml")
        );

        // Once it has its own, its own is synced.
        put(wt.path(), "config/devstack.toml");
        let (list, elsewhere) =
            super::files(wt.path(), main.path(), &extra, scratch.path(), None).unwrap();
        assert_eq!(
            list.iter().filter(|f| *f == "config/devstack.toml").count(),
            1
        );
        assert!(elsewhere.is_empty());
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

    #[test]
    fn a_request_that_names_containers_needs_a_node_that_acts_on_them() {
        use axum::http::Method;
        let stack = devstack_config::from_str(
            r#"
version = 1
[stack]
name = "shop"
[compose.app]
project = "shop"
files = ["compose.yml"]
services = ["db", "web"]
[services.api]
build = "dotnet build"
output = "bin"
run = "{artifact}/Api"
port = 5000
[groups.server]
members = ["compose.app.db", "api"]
[groups.front]
members = ["api"]
"#,
            std::path::Path::new("devstack.toml"),
        )
        .unwrap();
        let need = |m: &Method, p: &str| super::containers_need(&stack, m, p);
        assert_eq!(
            need(&Method::POST, "/v1/infra/app/db/restart"),
            Some("0.11.0")
        );
        for p in [
            "/v1/services/app.web/restart",
            "/v1/services/compose.app.web/stop",
            "/v1/services/web/start",
            "/v1/services/server/restart",
            "/v1/groups/server/restart",
        ] {
            assert_eq!(need(&Method::POST, p), Some("0.12.0"), "{p}");
        }
        for (m, p) in [
            (Method::POST, "/v1/services/api/restart"),
            (Method::POST, "/v1/services/server/stop"),
            (Method::POST, "/v1/groups/front/restart"),
            (Method::GET, "/v1/infra/app/db/logs"),
            (Method::GET, "/v1/services/web/restart"),
            (Method::POST, "/v1/up"),
        ] {
            assert_eq!(need(&m, p), None, "{m} {p}");
        }
    }

    #[test]
    fn files_not_written_are_named_a_few_at_a_time() {
        let failed: Vec<String> = (1..=5).map(|i| format!("f{i}: denied")).collect();
        let e = super::not_written(&failed);
        assert!(
            e.starts_with("not written on the node: f1: denied; f2: denied; f3: denied"),
            "{e}"
        );
        assert!(e.ends_with("(and 2 more)"), "{e}");
        assert_eq!(
            super::not_written(&failed[..1]),
            "not written on the node: f1: denied"
        );
    }

    #[test]
    fn a_lanes_ports_not_forwarded_are_listed_once_in_order() {
        let containers: Vec<serde_json::Value> = serde_json::from_str(
            r#"[{"service":"dns","not_forwarded":["10053/udp","5453/udp"]},
                {"service":"voice","not_forwarded":["5453/udp"]},
                {"service":"db","ports":["15432->db:5432"]}]"#,
        )
        .unwrap();
        assert_eq!(super::not_forwarded(&containers), ["5453/udp", "10053/udp"]);
        assert!(
            super::not_forwarded(&containers[2..]).is_empty(),
            "an older lane daemon's"
        );
    }

    #[test]
    fn a_udp_port_not_relayed_says_which_side_is_too_old() {
        use super::not_forwarded_reason as why;
        let at = |v: &str| Release {
            version: v.into(),
            protocol: version::PROTOCOL,
        };
        let udp = ["5453/udp".to_string()];
        assert_eq!(why(&[], "build-1", None), None, "nothing to say");
        let old = why(&udp, "build-1", Some(&at("0.18.2"))).unwrap();
        assert!(
            old.contains("the node build-1 runs devstack 0.18.2")
                && old.contains("0.20.0 or later there relays UDP"),
            "{old}"
        );
        let unknown = why(&udp, "build-1", None).unwrap();
        assert!(unknown.contains("does not say its version"), "{unknown}");
        // The node relays: the lane was started before this machine did.
        let here = why(&udp, "build-1", Some(&at("0.20.1"))).unwrap();
        assert!(here.contains("start it again"), "{here}");
        assert_eq!(
            why(&["9100/sctp".to_string()], "build-1", Some(&at("0.20.0"))).as_deref(),
            Some("devstack forwards TCP and UDP only")
        );
    }

    #[test]
    fn a_lane_carried_over_keeps_its_udp_ports() {
        let c: super::Carried = serde_json::from_str(
            r#"{"pool":"default","node":{"id":"n1","name":"build-1","api":"https://n"},
                "lease":"l1","ports":[[3000,3100]]}"#,
        )
        .unwrap();
        assert!(
            c.udp_ports.is_empty(),
            "a daemon before ADR-0056 relays none"
        );
        let c = super::Carried {
            udp_ports: vec![(5453, 5453)],
            ..c
        };
        let again: super::Carried =
            serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(again.udp_ports, [(5453, 5453)]);
    }

    #[test]
    fn a_lane_carried_over_is_warned_about_against_this_release() {
        use super::carried_warning;
        let this = Release::this();
        let older = Release {
            version: "0.0.1".into(),
            ..this.clone()
        };
        // A 0.20.0 daemon carried the text its 0.19.0 predecessor made; the
        // node and the coordinator run this release by now.
        let c: super::Carried = serde_json::from_str(&format!(
            r#"{{"pool":"default","lease":"l1","ports":[],
                "node":{{"id":"n1","name":"build-1","api":"https://n",
                         "devstack":{{"version":"{v}","protocol":{p}}}}},
                "warning":"the node build-1 runs devstack {v}, this machine devstack 0.0.1"}}"#,
            v = this.version,
            p = this.protocol
        ))
        .unwrap();
        assert_eq!(carried_warning(&c), None, "the same release everywhere");
        let c = super::Carried {
            coordinator: Some(older.clone()),
            ..c
        };
        let w = carried_warning(&c).unwrap();
        assert!(
            w.starts_with("the coordinator runs devstack 0.0.1") && w.contains(version::VERSION),
            "{w}"
        );
        let c = super::Carried {
            node: NodeRef {
                devstack: None,
                ..c.node
            },
            coordinator: Some(this),
            ..c
        };
        let w = carried_warning(&c).unwrap();
        assert!(w.contains("build-1") && w.contains("does not say"), "{w}");
        let again: super::Carried = serde_json::from_str(
            &serde_json::to_string(&super::Carried {
                coordinator: Some(older.clone()),
                ..c
            })
            .unwrap(),
        )
        .unwrap();
        assert_eq!(again.coordinator, Some(older));
    }
}
