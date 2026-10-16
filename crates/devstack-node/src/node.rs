//! The node agent (ADR-0025 point 2, ADR-0026, ADR-0031): a member that runs
//! other members' lanes. It admits a lease the coordinator created when it
//! fits what the node shares, keeps the lane's source mirror (ADR-0029), lets
//! a lease expire unless its requester renews it, and reports what it shares
//! and holds to the coordinator.
//!
//! API (mutual TLS; the caller is the approved member its certificate names):
//! - `GET /v1/node`
//! - `POST /v1/leases` `{id, token}`: admit a lease
//! - `POST /v1/leases/{id}/renew`, `DELETE /v1/leases/{id}` (the requester)
//! - `POST /v1/leases/{id}/sync/missing` `{hashes}`: the blobs it lacks
//! - `PUT /v1/blobs/{hash}`: one blob, by its BLAKE3 hash
//! - `POST /v1/leases/{id}/sync/apply` `{files}`: make the mirror exactly this
//! - `POST /v1/leases/{id}/start` `{stack_file}`, `POST /v1/leases/{id}/stop`,
//!   `GET /v1/leases/{id}/lane`: the lane itself (`lanerun`)
//! - `ANY /v1/leases/{id}/api/{*rest}`: the lane daemon's API, streamed

use crate::coordinator::{Lease, LeaseReport, Report};
use crate::lanerun::{DaemonAddr, LaneRun};
use crate::member::Local;
use crate::share::{Amount, Settings, idle_cpus};
use crate::tls::Peer;
use axum::extract::{DefaultBodyLimit, Path as UrlPath, State as AxState};
use axum::http::StatusCode;
use axum::routing::{any, delete, get, post, put};
use axum::{Extension, Json, Router};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long a lease lives without a renewal.
pub const TTL: Duration = Duration::from_secs(120);
/// How often the node measures, reaps and reports.
const EVERY: Duration = Duration::from_secs(15);
/// A lease admitted this recently is not in the measured usage yet, so its
/// need still counts against what the node offers.
const SETTLING: Duration = Duration::from_secs(180);
/// The largest blob a requester may upload.
const MAX_BLOB: usize = 512 << 20;

pub struct NodeConfig {
    /// This machine's membership (`member::default_dir`); the node keeps its
    /// own state in `node/` under it.
    pub dir: PathBuf,
    /// Where requesters reach the node API: `https://host:port`.
    pub api: String,
    /// What the node shares; `enabled` is implied.
    pub share: Settings,
    /// A server its owner never reclaims (ADR-0026).
    pub dedicated: bool,
}

/// A lease the node admitted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeLease {
    pub id: String,
    /// The requester.
    pub member: String,
    pub repo: String,
    pub lane: String,
    pub need: Amount,
    /// Unix seconds.
    pub expires: u64,
    /// Unix seconds.
    pub admitted: u64,
    /// The stack file the lane runs, relative to its root, once started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack_file: Option<String>,
}

impl NodeLease {
    fn expires_in(&self) -> u64 {
        self.expires.saturating_sub(crate::store::now())
    }
}

/// `GET /v1/node`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeView {
    pub id: String,
    pub name: String,
    pub offer: Amount,
    pub leases: Vec<NodeLease>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Admit {
    pub id: String,
    pub token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hashes {
    pub hashes: Vec<String>,
}

/// One file of a mirror: its path relative to the lane's root (with `/`),
/// the BLAKE3 hash of its content, whether it is executable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: String,
    pub hash: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub exec: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Apply {
    /// Every file the mirror is to hold; what is not listed goes.
    pub files: Vec<FileEntry>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Applied {
    pub written: usize,
    pub removed: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum NodeError {
    #[error("{0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("{0}")]
    Member(#[from] crate::member::MemberError),
    #[error("{0}")]
    Other(String),
}

struct Inner {
    local: Local,
    me: String,
    name: String,
    root: PathBuf,
    api: String,
    share: Settings,
    dedicated: bool,
    leases: Mutex<HashMap<String, NodeLease>>,
    /// The approved members, as the coordinator last listed them.
    /// With their tags, which this node's policy may ask for (ADR-0033).
    members: Mutex<(Option<Instant>, MemberTags)>,
    /// What this node lets run on it, as the coordinator last said.
    policy: Mutex<Option<crate::policy::NodePolicy>>,
    /// What the node offers now, before what its leases need.
    offer: Mutex<Amount>,
    /// A static Linux `devstack` the lane containers mount (ADR-0028 point
    /// 2); `None` where the agent is not one (a node on Windows cannot run
    /// lanes yet).
    bin: Option<PathBuf>,
    /// The agent's own container, when it runs in one
    /// (`DEVSTACK_NODE_CONTAINER`): it joins each lane's network.
    container: Option<String>,
    /// Where each running lane's daemon is.
    daemons: Mutex<HashMap<String, DaemonAddr>>,
    /// Lanes starting now, or whose start failed: the phase, or the error.
    starting: Mutex<HashMap<String, Result<String, String>>>,
}

type Shared = Arc<Inner>;
/// The approved members, each with its tags.
type MemberTags = HashMap<String, Vec<String>>;
type ApiResult<T> = std::result::Result<T, (StatusCode, String)>;

fn err(status: StatusCode, msg: impl Into<String>) -> (StatusCode, String) {
    (status, msg.into())
}

fn internal(e: impl std::fmt::Display) -> (StatusCode, String) {
    err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

impl Inner {
    fn lanes(&self) -> PathBuf {
        self.root.join("lanes")
    }

    fn blobs(&self) -> PathBuf {
        self.root.join("blobs")
    }

    fn save(&self, leases: &HashMap<String, NodeLease>) {
        let v: Vec<&NodeLease> = leases.values().collect();
        let _ = std::fs::write(
            self.root.join("leases.json"),
            serde_json::to_vec_pretty(&v).expect("leases serialise"),
        );
    }

    /// What is left for a new lease: the offer, less what recently admitted
    /// leases need (they are not in the measured usage yet).
    fn available(&self) -> Amount {
        let offer = *self.offer.lock().unwrap();
        let now = crate::store::now();
        let leases = self.leases.lock().unwrap();
        let settling = leases
            .values()
            .filter(|l| now.saturating_sub(l.admitted) < SETTLING.as_secs());
        let (cpus, memory) = settling.fold((0.0, 0u64), |(c, m), l| {
            (c + l.need.cpus, m + l.need.memory)
        });
        Amount {
            cpus: (offer.cpus - cpus).max(0.0),
            memory: offer.memory.saturating_sub(memory),
        }
    }

    fn run(&self, l: &NodeLease) -> LaneRun {
        LaneRun {
            id: l.id.clone(),
            lane: l.lane.clone(),
            dir: self.lanes().join(&l.id),
            stack_file: l
                .stack_file
                .clone()
                .unwrap_or_else(|| "devstack.toml".into()),
            display_root: None,
            port_offset: 0,
            memory_limit: None,
            network: crate::policy::Network::Lan,
        }
    }

    /// A lease is over: its lane stops, its mirror goes. Off the caller's
    /// thread, Docker takes its time.
    fn drop_lease(self: &Arc<Self>, l: NodeLease) {
        self.daemons.lock().unwrap().remove(&l.id);
        let s = self.clone();
        std::thread::spawn(move || {
            let run = s.run(&l);
            if l.stack_file.is_some() {
                run.stop(s.container.as_deref());
            }
            let _ = std::fs::remove_dir_all(&run.dir);
        });
    }
}

/// The approved member behind a request; asks the coordinator again when it
/// does not know the caller or its list is old. When the coordinator is
/// down the last list holds (ADR-0031 point 6).
async fn caller(s: &Shared, peer: &Peer) -> ApiResult<String> {
    let Some(id) = peer.member.clone() else {
        return Err(err(StatusCode::UNAUTHORIZED, "no member certificate"));
    };
    let fresh = {
        let m = s.members.lock().unwrap();
        m.0.is_some_and(|at| at.elapsed() < Duration::from_secs(30)) && m.1.contains_key(&id)
    };
    if !fresh {
        let s2 = s.clone();
        let listed = tokio::task::spawn_blocking(move || s2.local.members())
            .await
            .map_err(internal)?;
        if let Ok(list) = listed {
            let ids = list
                .into_iter()
                .filter(|m| m.state == crate::store::State::Approved)
                .map(|m| (m.id, m.tags))
                .collect();
            *s.members.lock().unwrap() = (Some(Instant::now()), ids);
        }
    }
    if s.members.lock().unwrap().1.contains_key(&id) {
        Ok(id)
    } else {
        Err(err(
            StatusCode::FORBIDDEN,
            "this certificate is not an approved member of the pool",
        ))
    }
}

/// The lease `id`, if `member` holds it.
fn lease_of(s: &Inner, id: &str, member: &str) -> ApiResult<NodeLease> {
    match s.leases.lock().unwrap().get(id) {
        Some(l) if l.member == member => Ok(l.clone()),
        _ => Err(err(
            StatusCode::NOT_FOUND,
            format!("no lease `{id}` of yours here"),
        )),
    }
}

fn router(s: Shared) -> Router {
    Router::new()
        .route("/v1/node", get(node))
        .route("/v1/leases", post(admit))
        .route("/v1/leases/{id}", delete(release))
        .route("/v1/leases/{id}/renew", post(renew))
        .route("/v1/leases/{id}/sync/missing", post(missing))
        .route("/v1/leases/{id}/sync/apply", post(apply))
        .route("/v1/leases/{id}/start", post(start))
        .route("/v1/leases/{id}/stop", post(stop))
        .route("/v1/leases/{id}/lane", get(lane_state))
        .route("/v1/leases/{id}/api/{*rest}", any(proxy))
        .route("/v1/leases/{id}/shared/{port}", get(shared_tunnel))
        .route(
            "/v1/blobs/{hash}",
            put(blob).layer(DefaultBodyLimit::max(MAX_BLOB)),
        )
        .with_state(s)
}

async fn node(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
) -> ApiResult<Json<NodeView>> {
    caller(&s, &peer).await?;
    let mut leases: Vec<NodeLease> = s.leases.lock().unwrap().values().cloned().collect();
    leases.sort_by_key(|l| l.admitted);
    Ok(Json(NodeView {
        id: s.me.clone(),
        name: s.name.clone(),
        offer: s.available(),
        leases,
    }))
}

async fn admit(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    Json(a): Json<Admit>,
) -> ApiResult<(StatusCode, Json<NodeLease>)> {
    let who = caller(&s, &peer).await?;
    if let Some(l) = s.leases.lock().unwrap().get(&a.id) {
        return if l.member == who {
            Ok((StatusCode::OK, Json(l.clone())))
        } else {
            Err(err(
                StatusCode::CONFLICT,
                "the lease belongs to someone else",
            ))
        };
    }
    // The coordinator created it, for this node and this requester.
    let s2 = s.clone();
    let (id, token) = (a.id.clone(), a.token.clone());
    let lease: Lease = tokio::task::spawn_blocking(move || {
        s2.local.get(&format!("/v1/leases/{id}?token={token}"))
    })
    .await
    .map_err(internal)?
    .map_err(|e| match e {
        crate::member::MemberError::Status(404, m) => err(StatusCode::FORBIDDEN, m),
        e => err(
            StatusCode::SERVICE_UNAVAILABLE,
            format!("cannot confirm the lease with the coordinator: {e}"),
        ),
    })?;
    if lease.node != s.me || lease.member != who {
        return Err(err(
            StatusCode::FORBIDDEN,
            "the lease is not for this node and you",
        ));
    }
    // What this node lets run on it (ADR-0033), whatever the coordinator
    // checked already.
    let tags = s
        .members
        .lock()
        .unwrap()
        .1
        .get(&who)
        .cloned()
        .unwrap_or_default();
    if let Some(p) = s.policy.lock().unwrap().as_ref()
        && let Err(why) = p.admits(&who, &tags, &lease.repo)
    {
        return Err(err(StatusCode::FORBIDDEN, format!("{}: {why}", s.name)));
    }
    // The node admits against what it has now (ADR-0031 point 4).
    let free = s.available();
    if lease.need.cpus > free.cpus || lease.need.memory > free.memory {
        return Err(err(
            StatusCode::CONFLICT,
            format!(
                "{} offers {:.1} CPU and {} MiB now; the lane needs {:.1} CPU and {} MiB",
                s.name,
                free.cpus,
                free.memory >> 20,
                lease.need.cpus,
                lease.need.memory >> 20
            ),
        ));
    }
    let now = crate::store::now();
    let l = NodeLease {
        id: lease.id,
        member: who,
        repo: lease.repo,
        lane: lease.lane,
        need: lease.need,
        expires: now + TTL.as_secs(),
        admitted: now,
        stack_file: None,
    };
    std::fs::create_dir_all(s.lanes().join(&l.id).join("src")).map_err(internal)?;
    let mut leases = s.leases.lock().unwrap();
    leases.insert(l.id.clone(), l.clone());
    s.save(&leases);
    Ok((StatusCode::CREATED, Json(l)))
}

async fn renew(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    UrlPath(id): UrlPath<String>,
) -> ApiResult<Json<NodeLease>> {
    let who = caller(&s, &peer).await?;
    lease_of(&s, &id, &who)?;
    let mut leases = s.leases.lock().unwrap();
    let l = leases.get_mut(&id).expect("checked above");
    l.expires = crate::store::now() + TTL.as_secs();
    let out = l.clone();
    s.save(&leases);
    Ok(Json(out))
}

async fn release(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    UrlPath(id): UrlPath<String>,
) -> ApiResult<StatusCode> {
    let who = caller(&s, &peer).await?;
    lease_of(&s, &id, &who)?;
    let gone = {
        let mut leases = s.leases.lock().unwrap();
        let l = leases.remove(&id);
        s.save(&leases);
        l
    };
    if let Some(l) = gone {
        s.drop_lease(l);
    }
    Ok(StatusCode::NO_CONTENT)
}

fn valid_hash(h: &str) -> bool {
    h.len() == 64
        && h.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

async fn missing(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    UrlPath(id): UrlPath<String>,
    Json(h): Json<Hashes>,
) -> ApiResult<Json<Hashes>> {
    let who = caller(&s, &peer).await?;
    lease_of(&s, &id, &who)?;
    let blobs = s.blobs();
    let mut seen = HashSet::new();
    let hashes = h
        .hashes
        .into_iter()
        .filter(|x| valid_hash(x) && seen.insert(x.clone()) && !blobs.join(x).exists())
        .collect();
    Ok(Json(Hashes { hashes }))
}

async fn blob(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    UrlPath(hash): UrlPath<String>,
    body: bytes::Bytes,
) -> ApiResult<StatusCode> {
    let who = caller(&s, &peer).await?;
    // Only a member with a lease here may store anything.
    if !s.leases.lock().unwrap().values().any(|l| l.member == who) {
        return Err(err(StatusCode::FORBIDDEN, "no lease of yours here"));
    }
    if !valid_hash(&hash) {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "a blob is named by its BLAKE3 hash",
        ));
    }
    if blake3::hash(&body).to_hex().as_str() != hash {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "the content does not have that hash",
        ));
    }
    let dir = s.blobs();
    let path = dir.join(&hash);
    if path.exists() {
        return Ok(StatusCode::NO_CONTENT);
    }
    std::fs::create_dir_all(&dir).map_err(internal)?;
    let tmp = dir.join(format!("{hash}.{}", crate::store::now()));
    std::fs::write(&tmp, &body).map_err(internal)?;
    std::fs::rename(&tmp, &path).map_err(internal)?;
    Ok(StatusCode::CREATED)
}

/// A mirror path from a requester, made safe: relative, no `..`, no drive or
/// root, `/`-separated.
fn safe_path(p: &str) -> Option<PathBuf> {
    let p = p.replace('\\', "/");
    if p.contains(':') {
        return None;
    }
    let mut out = PathBuf::new();
    for seg in p.split('/') {
        if seg.is_empty() || seg == "." || seg == ".." {
            return None;
        }
        out.push(seg);
    }
    out.components()
        .all(|c| matches!(c, Component::Normal(_)))
        .then_some(out)
}

/// What the mirror holds, by path: the hash it was written from.
fn manifest(dir: &Path) -> HashMap<String, String> {
    std::fs::read(dir.join("manifest.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Make the mirror of lane `lane_dir` hold exactly `files`, from `blobs`.
pub fn apply_files(
    blobs: &Path,
    lane_dir: &Path,
    files: &[FileEntry],
) -> Result<Applied, NodeError> {
    let io = |p: &Path| {
        let p = p.to_path_buf();
        move |e| NodeError::Io(p, e)
    };
    let src = lane_dir.join("src");
    let mut have = manifest(lane_dir);
    let mut out = Applied::default();
    let mut wanted = HashSet::new();
    for f in files {
        let rel = safe_path(&f.path)
            .ok_or_else(|| NodeError::Other(format!("unsafe path `{}`", f.path)))?;
        if !valid_hash(&f.hash) {
            return Err(NodeError::Other(format!("`{}`: no hash", f.path)));
        }
        wanted.insert(f.path.clone());
        let target = src.join(&rel);
        if have.get(&f.path) == Some(&f.hash) && target.exists() {
            continue;
        }
        let blob = blobs.join(&f.hash);
        if !blob.exists() {
            return Err(NodeError::Other(format!(
                "`{}`: blob {} not uploaded",
                f.path, f.hash
            )));
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(io(parent))?;
        }
        let tmp = target.with_file_name(format!(
            ".{}.devstack-tmp",
            target
                .file_name()
                .map(|n| n.to_string_lossy())
                .unwrap_or_default()
        ));
        std::fs::copy(&blob, &tmp).map_err(io(&tmp))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = if f.exec { 0o755 } else { 0o644 };
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))
                .map_err(io(&tmp))?;
        }
        std::fs::rename(&tmp, &target).map_err(io(&target))?;
        have.insert(f.path.clone(), f.hash.clone());
        out.written += 1;
    }
    let gone: Vec<String> = have
        .keys()
        .filter(|p| !wanted.contains(*p))
        .cloned()
        .collect();
    for p in gone {
        have.remove(&p);
        if let Some(rel) = safe_path(&p) {
            let target = src.join(rel);
            if std::fs::remove_file(&target).is_ok() {
                out.removed += 1;
            }
            // Directories left empty go too.
            let mut dir = target.parent().map(Path::to_path_buf);
            while let Some(d) = dir {
                if d == src || std::fs::remove_dir(&d).is_err() {
                    break;
                }
                dir = d.parent().map(Path::to_path_buf);
            }
        }
    }
    std::fs::write(
        lane_dir.join("manifest.json"),
        serde_json::to_vec(&have).expect("a manifest serialises"),
    )
    .map_err(io(lane_dir))?;
    Ok(out)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Start {
    /// The stack file, relative to the lane's root.
    #[serde(default = "default_stack_file")]
    pub stack_file: String,
    /// Where the requester has the lane's files (its worktree): paths in the
    /// lane's logs point there (ADR-0029 point 5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_root: Option<String>,
    /// What the lane's ports are moved by on its requester's machine: it
    /// runs on those ports here too.
    #[serde(default)]
    pub port_offset: u16,
}

fn default_stack_file() -> String {
    "devstack.toml".into()
}

/// `GET /v1/leases/{id}/lane`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LaneState {
    pub started: bool,
    pub running: bool,
    /// While it starts: what it does now. Absent once it is ready.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    /// Why its start failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Started and its daemon reached: the lane's API answers.
    #[serde(default)]
    pub ready: bool,
    /// After a start: the ports its containers are published on at the
    /// requester's machine.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub infra_ports: Vec<u16>,
}

/// Start the lane from its mirror: infrastructure, the lane container, its
/// daemon (ADR-0025 point 2). Again on a running lane: it starts afresh.
async fn start(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    UrlPath(id): UrlPath<String>,
    body: Option<Json<Start>>,
) -> ApiResult<Json<LaneState>> {
    let who = caller(&s, &peer).await?;
    let mut l = lease_of(&s, &id, &who)?;
    let Some(bin) = s.bin.clone() else {
        return Err(err(
            StatusCode::CONFLICT,
            format!("{} cannot run lanes: its agent is no Linux build", s.name),
        ));
    };
    let (stack_file, display_root, port_offset) = body.map_or_else(
        || (default_stack_file(), None, 0),
        |Json(b)| (b.stack_file, b.display_root, b.port_offset),
    );
    if safe_path(&stack_file).is_none() {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "the stack file must be inside the lane",
        ));
    }
    l.stack_file = Some(stack_file);
    {
        let mut leases = s.leases.lock().unwrap();
        if let Some(x) = leases.get_mut(&id) {
            x.stack_file = l.stack_file.clone();
        }
        s.save(&leases);
    }
    // A first start pulls or builds the lane image and the infrastructure's
    // images: minutes. It runs on, and `GET .../lane` says how far it got.
    if matches!(s.starting.lock().unwrap().get(&id), Some(Ok(_))) {
        return Err(err(StatusCode::CONFLICT, "the lane is starting"));
    }
    s.daemons.lock().unwrap().remove(&id);
    s.starting
        .lock()
        .unwrap()
        .insert(id.clone(), Ok("starting".into()));
    let s2 = s.clone();
    let lid = id.clone();
    tokio::task::spawn_blocking(move || {
        let mut run = s2.run(&l);
        run.display_root = display_root;
        run.port_offset = port_offset;
        if let Some(p) = s2.policy.lock().unwrap().as_ref() {
            run.memory_limit = p.memory_limit(l.need.memory);
            run.network = p.network;
        }
        let s3 = s2.clone();
        let pid = lid.clone();
        let phase = move |p: &str| {
            s3.starting
                .lock()
                .unwrap()
                .insert(pid.clone(), Ok(p.to_string()));
        };
        match run.start(&bin, s2.container.as_deref(), &phase) {
            Ok(addr) => {
                s2.daemons.lock().unwrap().insert(lid.clone(), addr);
                s2.starting.lock().unwrap().remove(&lid);
            }
            Err(e) => {
                s2.starting.lock().unwrap().insert(lid, Err(e.to_string()));
            }
        }
    });
    Ok(Json(LaneState {
        started: true,
        running: false,
        phase: Some("starting".into()),
        ..LaneState::default()
    }))
}

async fn stop(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    UrlPath(id): UrlPath<String>,
) -> ApiResult<Json<LaneState>> {
    let who = caller(&s, &peer).await?;
    let l = lease_of(&s, &id, &who)?;
    s.daemons.lock().unwrap().remove(&id);
    let s2 = s.clone();
    tokio::task::spawn_blocking(move || s2.run(&l).stop(s2.container.as_deref()))
        .await
        .map_err(internal)?;
    Ok(Json(LaneState {
        started: true,
        ..LaneState::default()
    }))
}

async fn lane_state(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    UrlPath(id): UrlPath<String>,
) -> ApiResult<Json<LaneState>> {
    let who = caller(&s, &peer).await?;
    let l = lease_of(&s, &id, &who)?;
    if l.stack_file.is_none() {
        return Ok(Json(LaneState::default()));
    }
    match s.starting.lock().unwrap().get(&id).cloned() {
        Some(Ok(phase)) => {
            return Ok(Json(LaneState {
                started: true,
                phase: Some(phase),
                ..LaneState::default()
            }));
        }
        Some(Err(e)) => {
            return Ok(Json(LaneState {
                started: true,
                error: Some(e),
                ..LaneState::default()
            }));
        }
        None => {}
    }
    let infra_ports = s
        .daemons
        .lock()
        .unwrap()
        .get(&id)
        .map(|d| d.infra_ports.clone());
    let s2 = s.clone();
    let running = tokio::task::spawn_blocking(move || s2.run(&l).running())
        .await
        .map_err(internal)?;
    Ok(Json(LaneState {
        started: true,
        running,
        ready: running && infra_ports.is_some(),
        infra_ports: infra_ports.unwrap_or_default(),
        ..LaneState::default()
    }))
}

/// Where the lane's daemon is: known since its start, or found again after
/// the agent restarted while the lane kept running.
async fn daemon_of(s: &Shared, l: NodeLease) -> ApiResult<DaemonAddr> {
    if let Some(d) = s.daemons.lock().unwrap().get(&l.id) {
        return Ok(d.clone());
    }
    if l.stack_file.is_none() {
        return Err(err(StatusCode::CONFLICT, "the lane is not started"));
    }
    let s2 = s.clone();
    let id = l.id.clone();
    let d = tokio::task::spawn_blocking(move || s2.run(&l).attach(s2.container.as_deref()))
        .await
        .map_err(internal)?
        .map_err(|e| err(StatusCode::CONFLICT, e.to_string()))?;
    s.daemons.lock().unwrap().insert(id, d.clone());
    Ok(d)
}

/// The lane daemon's API (ADR-0025 point 6), passed through as it is:
/// streams stay streams. The agent adds the daemon's token and the loopback
/// `Host` its API expects (ADR-0019).
async fn proxy(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    UrlPath((id, rest)): UrlPath<(String, String)>,
    mut req: axum::extract::Request,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    // A tunnel (`Upgrade`, ADR-0029 point 4) is passed through as well: once
    // the lane's daemon switched protocols, the two connections are spliced.
    let upgrade_in = req
        .headers()
        .contains_key(hyper::header::UPGRADE)
        .then(|| hyper::upgrade::on(&mut req));
    let target = async {
        let who = caller(&s, &peer).await?;
        let l = lease_of(&s, &id, &who)?;
        daemon_of(&s, l).await
    };
    let d = match target.await {
        Ok(d) => d,
        Err(e) => return e.into_response(),
    };
    let query = req
        .uri()
        .query()
        .map(|q| format!("?{q}"))
        .unwrap_or_default();
    pass(d, format!("/{rest}{query}"), upgrade_in, req).await
}

/// `GET /v1/leases/{id}/shared/{port}` with `Upgrade`: a tunnel to a port of
/// a lane its owner shares (ADR-0035 point 3), for a member it is shared
/// with. Asked of the coordinator at every connection: unsharing, or
/// removing the member, holds from the next one.
async fn shared_tunnel(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    UrlPath((id, port)): UrlPath<(String, u16)>,
    mut req: axum::extract::Request,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let upgrade_in = req
        .headers()
        .contains_key(hyper::header::UPGRADE)
        .then(|| hyper::upgrade::on(&mut req));
    let target = async {
        let who = caller(&s, &peer).await?;
        let l = s
            .leases
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .ok_or_else(|| err(StatusCode::NOT_FOUND, format!("no lane `{id}` here")))?;
        if l.member != who {
            let s2 = s.clone();
            let lease = id.clone();
            let sharing = tokio::task::spawn_blocking(move || s2.local.lease_shared(&lease))
                .await
                .map_err(internal)?
                .map_err(|e| err(StatusCode::BAD_GATEWAY, format!("the coordinator: {e}")))?;
            let tags = s
                .members
                .lock()
                .unwrap()
                .1
                .get(&who)
                .cloned()
                .unwrap_or_default();
            let ok = sharing.is_some_and(|sh| {
                sh.admits(&who, &tags) && sh.services.iter().any(|x| x.port == port)
            });
            if !ok {
                return Err(err(
                    StatusCode::FORBIDDEN,
                    "that lane is not shared with you, or not that port",
                ));
            }
        }
        daemon_of(&s, l).await
    };
    match target.await {
        Ok(d) => pass(d, format!("/v1/tunnel/{port}"), upgrade_in, req).await,
        Err(e) => e.into_response(),
    }
}

/// Pass `req` to the lane's daemon at `path`, with its token; a tunnel is
/// spliced once the daemon switched protocols.
async fn pass(
    d: DaemonAddr,
    path: String,
    upgrade_in: Option<hyper::upgrade::OnUpgrade>,
    req: axum::extract::Request,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let (parts, body) = req.into_parts();
    let tcp = match tokio::net::TcpStream::connect(&d.addr).await {
        Ok(t) => {
            let _ = t.set_nodelay(true);
            t
        }
        Err(e) => {
            return err(StatusCode::BAD_GATEWAY, format!("the lane's daemon: {e}")).into_response();
        }
    };
    let (mut send, conn) =
        match hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(tcp)).await {
            Ok(x) => x,
            Err(e) => return err(StatusCode::BAD_GATEWAY, e.to_string()).into_response(),
        };
    tokio::spawn(conn.with_upgrades());
    let mut out = hyper::Request::builder()
        .method(parts.method)
        .uri(path)
        .header(
            hyper::header::HOST,
            format!("127.0.0.1:{}", crate::lanerun::DAEMON_PORT),
        )
        .header(hyper::header::AUTHORIZATION, format!("Bearer {}", d.token));
    for (k, v) in &parts.headers {
        if k != hyper::header::HOST
            && k != hyper::header::AUTHORIZATION
            && k != hyper::header::ORIGIN
        {
            out = out.header(k, v);
        }
    }
    let Ok(out) = out.body(body) else {
        return err(StatusCode::BAD_REQUEST, "bad request").into_response();
    };
    match send.send_request(out).await {
        Ok(mut res) if res.status() == StatusCode::SWITCHING_PROTOCOLS => {
            let Some(upgrade_in) = upgrade_in else {
                return err(
                    StatusCode::BAD_GATEWAY,
                    "the lane's daemon switched protocols unasked",
                )
                .into_response();
            };
            let upgrade_out = hyper::upgrade::on(&mut res);
            tokio::spawn(async move {
                let (Ok(a), Ok(b)) = (upgrade_in.await, upgrade_out.await) else {
                    return;
                };
                let mut a = hyper_util::rt::TokioIo::new(a);
                let mut b = hyper_util::rt::TokioIo::new(b);
                let _ = tokio::io::copy_bidirectional(&mut a, &mut b).await;
            });
            let (parts, _) = res.into_parts();
            axum::response::Response::from_parts(parts, axum::body::Body::empty())
        }
        Ok(res) => res.map(axum::body::Body::new).into_response(),
        Err(e) => err(StatusCode::BAD_GATEWAY, format!("the lane's daemon: {e}")).into_response(),
    }
}

async fn apply(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    UrlPath(id): UrlPath<String>,
    Json(a): Json<Apply>,
) -> ApiResult<Json<Applied>> {
    let who = caller(&s, &peer).await?;
    lease_of(&s, &id, &who)?;
    let (blobs, lane) = (s.blobs(), s.lanes().join(&id));
    let r = tokio::task::spawn_blocking(move || apply_files(&blobs, &lane, &a.files))
        .await
        .map_err(internal)?;
    r.map(Json)
        .map_err(|e| err(StatusCode::BAD_REQUEST, e.to_string()))
}

/// Measure, let expired leases go, and report, every `EVERY`.
async fn background(s: Shared) {
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    let mut before = devstack_proc::cpu_times();
    let mut wait = Duration::from_secs(2);
    loop {
        tokio::time::sleep(wait).await;
        wait = EVERY;
        let after = devstack_proc::cpu_times();
        let free = Amount {
            cpus: match (before, after) {
                (Some(b), Some(a)) => idle_cpus(b, a, cores),
                _ => 0.0,
            },
            memory: devstack_proc::memory_available().unwrap_or(0),
        };
        if after.is_some() {
            before = after;
        }
        let offer = Settings {
            enabled: true,
            ..s.share.clone()
        }
        .offer(free);
        *s.offer.lock().unwrap() = offer;

        let now = crate::store::now();
        let expired: Vec<NodeLease> = {
            let mut leases = s.leases.lock().unwrap();
            let gone: Vec<String> = leases
                .values()
                .filter(|l| l.expires <= now)
                .map(|l| l.id.clone())
                .collect();
            let out: Vec<NodeLease> = gone.iter().filter_map(|id| leases.remove(id)).collect();
            if !out.is_empty() {
                s.save(&leases);
            }
            out
        };
        for l in expired {
            s.drop_lease(l);
        }

        let available = s.available();
        let report = Report {
            sharing: true,
            cpus: available.cpus,
            memory: available.memory,
            api: Some(s.api.clone()),
            dedicated: s.dedicated,
            leases: s
                .leases
                .lock()
                .unwrap()
                .values()
                .map(|l| LeaseReport {
                    id: l.id.clone(),
                    member: l.member.clone(),
                    repo: l.repo.clone(),
                    lane: l.lane.clone(),
                    expires_in: l.expires_in(),
                })
                .collect(),
        };
        let s2 = s.clone();
        let answer = tokio::task::spawn_blocking(move || s2.local.report(&report)).await;
        // What an admin set for this node (ADR-0033); the last known holds
        // while the coordinator does not answer.
        let s3 = s.clone();
        if let Ok(Ok(me)) = tokio::task::spawn_blocking(move || s3.local.me()).await {
            *s.policy.lock().unwrap() = me.policy;
        }
        // Leases an admin ended: their lanes stop like expired ones.
        if let Ok(Ok(a)) = answer
            && !a.end.is_empty()
        {
            let ended: Vec<NodeLease> = {
                let mut leases = s.leases.lock().unwrap();
                let out: Vec<NodeLease> = a.end.iter().filter_map(|id| leases.remove(id)).collect();
                if !out.is_empty() {
                    s.save(&leases);
                }
                out
            };
            for l in ended {
                s.drop_lease(l);
            }
        }
    }
}

/// Run the node agent of the member in `config.dir` on `listener` until
/// `stop` resolves.
pub async fn serve(
    config: NodeConfig,
    listener: tokio::net::TcpListener,
    stop: impl std::future::Future<Output = ()>,
) -> Result<(), NodeError> {
    let local = Local::new(&config.dir);
    let m = local
        .membership()?
        .filter(|m| m.state == crate::store::State::Approved)
        .ok_or_else(|| {
            NodeError::Other(
                "this machine is no approved member of a pool: `devstack join` first".into(),
            )
        })?;
    let read = |f: &str| {
        let p = config.dir.join(f);
        std::fs::read_to_string(&p).map_err(|e| NodeError::Io(p, e))
    };
    let tls = crate::tls::server_config(
        &read("ca.pem")?,
        &read("member.pem")?,
        &read("member.key")?,
        true,
    )
    .map_err(|e| NodeError::Other(e.to_string()))?;
    let root = config.dir.join("node");
    std::fs::create_dir_all(root.join("lanes")).map_err(|e| NodeError::Io(root.clone(), e))?;
    let leases: HashMap<String, NodeLease> = std::fs::read(root.join("leases.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<Vec<NodeLease>>(&b).ok())
        .unwrap_or_default()
        .into_iter()
        .map(|l| (l.id.clone(), l))
        .collect();
    let bin = linux_binary(&root);
    let s = Arc::new(Inner {
        local,
        me: m.id,
        name: m.name,
        root,
        api: config.api,
        share: config.share,
        dedicated: config.dedicated,
        leases: Mutex::new(leases),
        members: Mutex::new((None, HashMap::new())),
        policy: Mutex::new(None),
        offer: Mutex::new(Amount::default()),
        bin,
        container: std::env::var("DEVSTACK_NODE_CONTAINER")
            .ok()
            .filter(|c| !c.trim().is_empty()),
        daemons: Mutex::new(HashMap::new()),
        starting: Mutex::new(HashMap::new()),
    });
    let bg = tokio::spawn(background(s.clone()));
    crate::tls::serve(tls, listener, router(s), stop).await;
    bg.abort();
    Ok(())
}

/// A copy of the agent's own binary that lane containers mount, when it is
/// a Linux build (the release's is static, ADR-0028 point 2). A copy per
/// version, so a running lane keeps its binary when the agent updates.
///
/// `DEVSTACK_LANE_BIN` names another one (a test whose own build is not
/// static).
fn linux_binary(root: &Path) -> Option<PathBuf> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let exe = std::env::var_os("DEVSTACK_LANE_BIN")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::current_exe().ok())?;
    let dir = root.join("bin");
    let target = dir.join(format!("devstack-{}", env!("CARGO_PKG_VERSION")));
    std::fs::create_dir_all(&dir).ok()?;
    // Always fresh: one version number covers many builds from main.
    let tmp = dir.join(".devstack.tmp");
    std::fs::copy(&exe, &tmp).ok()?;
    std::fs::rename(&tmp, &target).ok()?;
    Some(target)
}

/// Run the node agent until Ctrl-C (or SIGTERM).
pub fn serve_blocking(config: NodeConfig, listen: std::net::SocketAddr) -> Result<(), NodeError> {
    let rt = tokio::runtime::Runtime::new().map_err(|e| NodeError::Other(e.to_string()))?;
    rt.block_on(async {
        let listener = tokio::net::TcpListener::bind(listen)
            .await
            .map_err(|e| NodeError::Other(format!("listen on {listen}: {e}")))?;
        serve(config, listener, crate::coordinator::stop_signal()).await
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_from_a_requester_stays_in_the_mirror() {
        assert!(safe_path("src/Api/Program.cs").is_some());
        assert!(safe_path("src\\Api\\Program.cs").is_some());
        for bad in ["", "/etc/passwd", "../x", "a/../../x", "C:/x", "a/./b"] {
            assert!(safe_path(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn apply_writes_what_changed_and_removes_what_went() {
        let d = tempfile::tempdir().unwrap();
        let (blobs, lane) = (d.path().join("blobs"), d.path().join("lane"));
        std::fs::create_dir_all(&blobs).unwrap();
        let put = |text: &str| {
            let h = blake3::hash(text.as_bytes()).to_hex().to_string();
            std::fs::write(blobs.join(&h), text).unwrap();
            h
        };
        let (a, b) = (put("a"), put("b"));
        let f = |p: &str, h: &str| FileEntry {
            path: p.into(),
            hash: h.into(),
            exec: false,
        };
        let r = apply_files(&blobs, &lane, &[f("x/a.txt", &a), f("b.txt", &b)]).unwrap();
        assert_eq!((r.written, r.removed), (2, 0));
        assert_eq!(
            std::fs::read_to_string(lane.join("src/x/a.txt")).unwrap(),
            "a"
        );
        let r = apply_files(&blobs, &lane, &[f("b.txt", &a)]).unwrap();
        assert_eq!((r.written, r.removed), (1, 1));
        assert_eq!(
            std::fs::read_to_string(lane.join("src/b.txt")).unwrap(),
            "a"
        );
        assert!(!lane.join("src/x").exists(), "an emptied directory goes");
        let missing = "0".repeat(64);
        assert!(apply_files(&blobs, &lane, &[f("c.txt", &missing)]).is_err());
    }
}
