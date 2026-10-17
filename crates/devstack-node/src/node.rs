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
/// What one member's blobs may take on this node, in all.
const MAX_MEMBER_BLOBS: u64 = 8 << 30;
/// The largest list of files (or hashes) one sync sends: about 250 000.
const MAX_LIST: usize = 32 << 20;
/// How long the node trusts its last list of members while the coordinator
/// does not answer (ADR-0031 point 6): renewals go on, a member removed in
/// the meantime is refused after this at the latest.
const MEMBERS_VALID_FOR: Duration = Duration::from_secs(3600);

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
    /// What this node lets run on it, as the coordinator last said; `false`
    /// until the coordinator said it once.
    policy: Mutex<(bool, Option<crate::policy::NodePolicy>)>,
    /// What the node's own configuration lets run on it (`policy.json` in
    /// its state, ADR-0033 point 6), on top of the coordinator's.
    local_policy: Option<crate::policy::NodePolicy>,
    /// A secret of this node: names of images it builds for a member.
    secret: String,
    /// What each member's blobs take here, once counted.
    blob_use: Mutex<HashMap<String, u64>>,
    /// Whether the coordinator refused this node's API (not granted to run
    /// lanes), said once.
    refused: Mutex<Option<String>>,
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
    /// A lease came or went: the next report is due now, not in `EVERY`.
    /// The coordinator admits a lease against the node's last report; one
    /// that still counts a released lease refuses a lane the node has room
    /// for.
    changed: tokio::sync::Notify,
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

    /// A member's blobs: its own, so no member learns or gets another's
    /// content by its hash.
    fn blobs(&self, member: &str) -> PathBuf {
        self.root.join("blobs").join(member)
    }

    /// The policies that hold here: the node's own and the coordinator's.
    fn policies(&self) -> Vec<crate::policy::NodePolicy> {
        let mut v: Vec<crate::policy::NodePolicy> = self.local_policy.iter().cloned().collect();
        v.extend(self.policy.lock().unwrap().1.clone());
        v
    }

    /// What `member`'s blobs take here (counted once, then kept up to date).
    fn blob_use(&self, member: &str) -> u64 {
        if let Some(u) = self.blob_use.lock().unwrap().get(member) {
            return *u;
        }
        let used = dir_size(&self.blobs(member));
        *self
            .blob_use
            .lock()
            .unwrap()
            .entry(member.to_string())
            .or_insert(used)
    }

    /// Remove the blobs of `member` no mirror of its leases here was
    /// written from; all of them when it holds none.
    fn gc_blobs(&self, member: &str) {
        let ids: Vec<String> = self
            .leases
            .lock()
            .unwrap()
            .values()
            .filter(|l| l.member == member)
            .map(|l| l.id.clone())
            .collect();
        let keep: HashSet<String> = ids
            .iter()
            .flat_map(|id| manifest(&self.lanes().join(id)).into_values())
            .collect();
        let dir = self.blobs(member);
        if ids.is_empty() {
            let _ = std::fs::remove_dir_all(&dir);
        } else if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                if !keep.contains(e.file_name().to_string_lossy().as_ref()) {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        self.blob_use.lock().unwrap().remove(member);
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
            member: l.member.clone(),
            secret: self.secret.clone(),
            lane: l.lane.clone(),
            dir: self.lanes().join(&l.id),
            stack_file: l
                .stack_file
                .clone()
                .unwrap_or_else(|| "devstack.toml".into()),
            display_root: None,
            port_offset: 0,
            memory_limit: None,
            cpu_limit: None,
            network: crate::policy::Network::Lan,
        }
    }

    /// A lease is over: its lane stops, its mirror goes. Off the caller's
    /// thread, Docker takes its time.
    fn drop_lease(self: &Arc<Self>, l: NodeLease) {
        self.daemons.lock().unwrap().remove(&l.id);
        // What it needed is on offer again: the coordinator hears it now.
        self.changed.notify_one();
        let s = self.clone();
        std::thread::spawn(move || {
            let run = s.run(&l);
            if l.stack_file.is_some() {
                run.stop(s.container.as_deref());
            }
            let _ = std::fs::remove_dir_all(&run.dir);
            s.gc_blobs(&l.member);
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
    let mut asked = false;
    if !fresh {
        let s2 = s.clone();
        let listed = tokio::task::spawn_blocking(move || s2.local.members())
            .await
            .map_err(internal)?;
        if let Ok(list) = listed {
            asked = true;
            let ids = list
                .into_iter()
                .filter(|m| m.state == crate::store::State::Approved)
                .map(|m| (m.id, m.tags))
                .collect();
            *s.members.lock().unwrap() = (Some(Instant::now()), ids);
        }
    }
    let (at, known) = {
        let m = s.members.lock().unwrap();
        (m.0, m.1.contains_key(&id))
    };
    if known && !asked && at.is_none_or(|at| at.elapsed() > MEMBERS_VALID_FOR) {
        return Err(err(
            StatusCode::SERVICE_UNAVAILABLE,
            "the coordinator has not confirmed the pool's members for too long",
        ));
    }
    if known {
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
        .route(
            "/v1/leases/{id}/sync/missing",
            post(missing).layer(DefaultBodyLimit::max(MAX_LIST)),
        )
        .route(
            "/v1/leases/{id}/sync/apply",
            post(apply).layer(DefaultBodyLimit::max(MAX_LIST)),
        )
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
    // Ids and tokens are the coordinator's hex: nothing else goes into its URL.
    if !hex_id(&a.id) || !hex_id(&a.token) {
        return Err(err(StatusCode::BAD_REQUEST, "not a lease of this pool"));
    }
    // What this node lets run on it: asked of the coordinator before the
    // first lease, never assumed open.
    if !s.policy.lock().unwrap().0 {
        let s2 = s.clone();
        let me = tokio::task::spawn_blocking(move || s2.local.me())
            .await
            .map_err(internal)?
            .map_err(|e| {
                err(
                    StatusCode::SERVICE_UNAVAILABLE,
                    format!("cannot ask the coordinator for this node's policy: {e}"),
                )
            })?;
        *s.policy.lock().unwrap() = (true, me.policy);
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
    let policies = s.policies();
    let all: Vec<&crate::policy::NodePolicy> = policies.iter().collect();
    if let Err(why) = crate::policy::NodePolicy::admits_all(&all, &who, &tags, &lease.repo) {
        return Err(err(StatusCode::FORBIDDEN, format!("{}: {why}", s.name)));
    }
    // The node admits against what it has now (ADR-0031 point 4), and takes
    // a lane to need at least the least.
    let mut lease = lease;
    lease.need = crate::policy::floor_need(lease.need);
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
    s.changed.notify_one();
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
    // It may have ended since it was looked up.
    let Some(l) = leases.get_mut(&id) else {
        return Err(err(
            StatusCode::NOT_FOUND,
            format!("no lease `{id}` of yours here"),
        ));
    };
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

/// A lease id or token as the coordinator makes them: lower-case hex.
fn hex_id(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 128
        && v.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// The bytes the files directly under `dir` take.
fn dir_size(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| e.metadata().ok())
                .filter(|m| m.is_file())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0)
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
    let blobs = s.blobs(&who);
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
    body: axum::body::Body,
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
    let dir = s.blobs(&who);
    let path = dir.join(&hash);
    if path.exists() {
        return Ok(StatusCode::NO_CONTENT);
    }
    // Written to disk as it arrives (a blob may be hundreds of MiB), hashed
    // on the way; it takes its name only once the hash matches.
    tokio::fs::create_dir_all(&dir).await.map_err(internal)?;
    let tmp = dir.join(format!("{hash}.{}.tmp", crate::coordinator::random_hex(6)));
    let received = receive_blob(body, &tmp).await;
    let fail = |e| {
        let tmp = tmp.clone();
        async move {
            let _ = tokio::fs::remove_file(&tmp).await;
            Err(e)
        }
    };
    let (got, size) = match received {
        Ok(r) => r,
        Err(e) => return fail(e).await,
    };
    if got != hash {
        return fail(err(
            StatusCode::BAD_REQUEST,
            "the content does not have that hash",
        ))
        .await;
    }
    // Room for it among this member's blobs: what its mirrors no longer
    // need goes first.
    if s.blob_use(&who) + size > MAX_MEMBER_BLOBS {
        let (s2, w) = (s.clone(), who.clone());
        tokio::task::spawn_blocking(move || s2.gc_blobs(&w))
            .await
            .map_err(internal)?;
        if s.blob_use(&who) + size > MAX_MEMBER_BLOBS {
            return fail(err(
                StatusCode::INSUFFICIENT_STORAGE,
                format!(
                    "your files on {} take {} MiB already, the most a member may store here",
                    s.name,
                    MAX_MEMBER_BLOBS >> 20
                ),
            ))
            .await;
        }
    }
    if let Err(e) = tokio::fs::rename(&tmp, &path).await {
        return fail(internal(e)).await;
    }
    *s.blob_use.lock().unwrap().entry(who).or_insert(0) += size;
    Ok(StatusCode::CREATED)
}

/// Write an uploaded blob to `tmp` as it arrives, at most [`MAX_BLOB`]
/// bytes; its BLAKE3 hash and size.
async fn receive_blob(
    body: axum::body::Body,
    tmp: &Path,
) -> Result<(String, u64), (StatusCode, String)> {
    use http_body_util::BodyExt;
    use tokio::io::AsyncWriteExt;
    let mut body = http_body_util::Limited::new(body, MAX_BLOB);
    let mut file = tokio::fs::File::create(tmp).await.map_err(internal)?;
    let mut hasher = blake3::Hasher::new();
    let mut size = 0u64;
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|e| {
            if e.is::<http_body_util::LengthLimitError>() {
                err(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    format!("a blob may have {} MiB at most", MAX_BLOB >> 20),
                )
            } else {
                err(
                    StatusCode::BAD_REQUEST,
                    format!("the upload broke off: {e}"),
                )
            }
        })?;
        if let Ok(data) = frame.into_data() {
            hasher.update(&data);
            size += data.len() as u64;
            file.write_all(&data).await.map_err(internal)?;
        }
    }
    file.flush().await.map_err(internal)?;
    // Closed now, not when tokio gets to it: Windows renames no open file.
    drop(file.into_std().await);
    Ok((hasher.finalize().to_hex().to_string(), size))
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

/// Is `rel` beneath `root` a plain file, reached without a link?
fn plain_file(root: &Path, rel: &Path) -> bool {
    let mut cur = root.to_path_buf();
    let mut comps = rel.components().peekable();
    while let Some(c) = comps.next() {
        cur.push(c);
        let Ok(m) = std::fs::symlink_metadata(&cur) else {
            return false;
        };
        let last = comps.peek().is_none();
        if m.file_type().is_symlink() || (last && !m.is_file()) || (!last && !m.is_dir()) {
            return false;
        }
    }
    true
}

/// Make the mirror of lane `lane_dir` hold exactly `files`, from `blobs`.
///
/// The lane's own code writes into the mirror too, symlinks included: no
/// path is followed through one ([`crate::safefs`]). Each blob's content is
/// hashed again as it is copied, so a changed blob never lands.
pub fn apply_files(
    blobs: &Path,
    lane_dir: &Path,
    files: &[FileEntry],
) -> Result<Applied, NodeError> {
    let src = lane_dir.join("src");
    std::fs::create_dir_all(&src).map_err(|e| NodeError::Io(src.clone(), e))?;
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
        if have.get(&f.path) == Some(&f.hash) && plain_file(&src, &rel) {
            continue;
        }
        let blob = blobs.join(&f.hash);
        let mut from = std::fs::File::open(&blob)
            .map_err(|_| NodeError::Other(format!("`{}`: blob {} not uploaded", f.path, f.hash)))?;
        let mode = if f.exec { 0o755 } else { 0o644 };
        crate::safefs::write_beneath(&src, &rel, mode, |to| {
            let h = crate::safefs::copy_hashing(&mut from, to)?;
            if h == f.hash {
                Ok(())
            } else {
                Err(std::io::Error::other(format!(
                    "blob {} does not have its hash",
                    f.hash
                )))
            }
        })
        .map_err(|e| NodeError::Other(format!("`{}`: {e}", f.path)))?;
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
        if let Some(rel) = safe_path(&p)
            && crate::safefs::remove_beneath(&src, &rel).unwrap_or(false)
        {
            out.removed += 1;
        }
    }
    let io = |p: &Path| {
        let p = p.to_path_buf();
        move |e| NodeError::Io(p, e)
    };
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
        let policies = s2.policies();
        let all: Vec<&crate::policy::NodePolicy> = policies.iter().collect();
        if let Some(p) = crate::policy::NodePolicy::strictest(&all) {
            run.memory_limit = p.memory_limit(l.need.memory);
            run.cpu_limit = p.max_cpus.filter(|c| *c > 0.0);
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
    let connect = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::net::TcpStream::connect(&d.addr),
    );
    let tcp = match connect.await {
        Ok(Ok(t)) => {
            let _ = t.set_nodelay(true);
            t
        }
        Ok(Err(e)) => {
            return err(StatusCode::BAD_GATEWAY, format!("the lane's daemon: {e}")).into_response();
        }
        Err(_) => {
            return err(
                StatusCode::GATEWAY_TIMEOUT,
                "the lane's daemon does not answer",
            )
            .into_response();
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
    let (blobs, lane) = (s.blobs(&who), s.lanes().join(&id));
    let r = tokio::task::spawn_blocking(move || apply_files(&blobs, &lane, &a.files))
        .await
        .map_err(internal)?;
    r.map(Json)
        .map_err(|e| err(StatusCode::BAD_REQUEST, e.to_string()))
}

/// A node's capacity as OTLP gauges (ADR-0037 point 4).
fn capacity_metrics(
    total: Amount,
    offered: Amount,
    used: Amount,
    gpus: &[crate::share::Gpu],
    lanes: usize,
) -> serde_json::Value {
    use crate::telemetry::Gauge;
    let g = |name: &str, unit: &str, value: f64, kind: Option<&str>| Gauge {
        name: name.into(),
        unit: unit.into(),
        value,
        labels: kind
            .map(|k| vec![("devstack.kind".to_string(), k.to_string())])
            .unwrap_or_default(),
    };
    let mut v = vec![
        g("devstack.node.cpus", "1", total.cpus, Some("total")),
        g("devstack.node.cpus", "1", offered.cpus, Some("offered")),
        g("devstack.node.cpus", "1", used.cpus, Some("used")),
        g(
            "devstack.node.memory",
            "By",
            total.memory as f64,
            Some("total"),
        ),
        g(
            "devstack.node.memory",
            "By",
            offered.memory as f64,
            Some("offered"),
        ),
        g(
            "devstack.node.memory",
            "By",
            used.memory as f64,
            Some("used"),
        ),
        g("devstack.node.lanes", "1", lanes as f64, None),
    ];
    if !gpus.is_empty() {
        v.push(g("devstack.node.gpus", "1", gpus.len() as f64, None));
        v.push(g(
            "devstack.node.gpu_load",
            "%",
            gpus.iter().map(|x| x.load).sum(),
            None,
        ));
    }
    crate::telemetry::metrics_body("devstack-node", &v, &[])
}

/// What the containers of the lanes `ids` use now (`docker stats`): their
/// names carry the lease's id.
fn lanes_use(ids: &[String]) -> Amount {
    if ids.is_empty() {
        return Amount::default();
    }
    let Ok(out) = std::process::Command::new("docker")
        .args(["ps", "--format", "{{.Names}}"])
        .output()
    else {
        return Amount::default();
    };
    let names: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|n| ids.iter().any(|id| n.contains(id.as_str())))
        .map(str::to_string)
        .collect();
    if names.is_empty() {
        return Amount::default();
    }
    let stats = devstack_compose::Docker::default()
        .stats(&names)
        .unwrap_or_default();
    Amount {
        cpus: stats.iter().map(|s| s.cpu / 100.0).sum(),
        memory: stats.iter().map(|s| s.memory).sum(),
    }
}

/// The shortest interval idle CPU is measured over: a round a lease change
/// brought forward keeps the last reading rather than take a noisy one.
const CPU_INTERVAL: Duration = Duration::from_secs(1);

/// Until the next round: `wait`, or less when a lease came or went.
async fn next_round(wait: Duration, changed: &tokio::sync::Notify) {
    tokio::select! {
        () = tokio::time::sleep(wait) => {}
        () = changed.notified() => {}
    }
}

/// Measure, let expired leases go, and report, every `EVERY`, and at once
/// when a lease came or went.
async fn background(s: Shared) {
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    let mut before = devstack_proc::cpu_times();
    let mut measured = Instant::now();
    let mut free_cpus = 0.0;
    let mut wait = Duration::from_secs(2);
    // Whether the pool collects telemetry, and when that was asked (ADR-0037).
    let mut collects: Option<(Instant, bool)> = None;
    let mut sent = Instant::now() - Duration::from_secs(60);
    loop {
        next_round(wait, &s.changed).await;
        wait = EVERY;
        if measured.elapsed() >= CPU_INTERVAL {
            let after = devstack_proc::cpu_times();
            free_cpus = match (before, after) {
                (Some(b), Some(a)) => idle_cpus(b, a, cores),
                _ => 0.0,
            };
            if after.is_some() {
                before = after;
                measured = Instant::now();
            }
        }
        let free = Amount {
            cpus: free_cpus,
            memory: devstack_proc::memory_available().unwrap_or(0),
        };
        // What the pool's lanes use here now, their infrastructure included
        // (their containers carry the lease's id).
        let ids: Vec<String> = s.leases.lock().unwrap().keys().cloned().collect();
        let used = tokio::task::spawn_blocking(move || lanes_use(&ids))
            .await
            .unwrap_or_default();
        let offer = Settings {
            enabled: true,
            ..s.share.clone()
        }
        .offer(free);
        // The caps hold for the lanes together: what they use already is
        // not on offer again.
        let offer = Amount {
            cpus: s
                .share
                .max_cpus
                .map_or(offer.cpus, |m| offer.cpus.min((m - used.cpus).max(0.0))),
            memory: s.share.max_memory.map_or(offer.memory, |m| {
                offer.memory.min(m.saturating_sub(used.memory))
            }),
        };
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
        // What the machine has, what its lanes use, its GPUs (ADR-0036).
        let total = Amount {
            cpus: cores as f64,
            memory: devstack_proc::memory_total().unwrap_or(0),
        };
        let gpus: Vec<crate::share::Gpu> = tokio::task::spawn_blocking(crate::share::gpus)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|g| crate::share::Gpu { offered: true, ..g })
            .collect();
        // A node sends what concerns the pool's lanes with the coordinator's
        // setting alone (ADR-0037 point 3): its capacity, every minute.
        if sent.elapsed() >= Duration::from_secs(60) {
            sent = Instant::now();
            if collects.is_none_or(|(at, _)| at.elapsed() > Duration::from_secs(600)) {
                let s2 = s.clone();
                // Only an answer is kept: a coordinator that does not answer
                // yet (it restarted with this node) is asked again next time.
                if let Some(info) = tokio::task::spawn_blocking(move || s2.local.telemetry_info())
                    .await
                    .ok()
                    .and_then(Result::ok)
                {
                    collects = Some((Instant::now(), info.enabled));
                }
            }
            if collects.is_some_and(|(_, on)| on) {
                let body = capacity_metrics(
                    total,
                    available,
                    used,
                    &gpus,
                    s.leases.lock().unwrap().len(),
                );
                let s2 = s.clone();
                let _ =
                    tokio::task::spawn_blocking(move || s2.local.telemetry_send("metrics", &body))
                        .await;
            }
        }
        let report = Report {
            total: Some(total),
            used: Some(used),
            gpus,
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
                    need: Some(l.need),
                })
                .collect(),
        };
        let s2 = s.clone();
        let answer = tokio::task::spawn_blocking(move || s2.local.report(&report)).await;
        // What an admin set for this node (ADR-0033); the last known holds
        // while the coordinator does not answer.
        let s3 = s.clone();
        if let Ok(Ok(me)) = tokio::task::spawn_blocking(move || s3.local.me()).await {
            *s.policy.lock().unwrap() = (true, me.policy);
        }
        // A node no admin granted (ADR-0032, addendum): said once.
        if let Ok(Ok(a)) = &answer {
            let mut refused = s.refused.lock().unwrap();
            if a.refused != *refused {
                if let Some(why) = &a.refused {
                    eprintln!("node agent: {why}");
                }
                refused.clone_from(&a.refused);
            }
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
    // Rules of lanes that ended while the agent was down (ADR-0033 point 5).
    let held: Vec<String> = leases.keys().cloned().collect();
    let state = root.clone();
    tokio::task::spawn_blocking(move || crate::egress::cleanup(&state, &held));
    let local_policy = local_policy(&root)?;
    let secret = node_secret(&root)?;
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
        policy: Mutex::new((false, None)),
        local_policy,
        secret,
        blob_use: Mutex::new(HashMap::new()),
        refused: Mutex::new(None),
        offer: Mutex::new(Amount::default()),
        bin,
        container: std::env::var("DEVSTACK_NODE_CONTAINER")
            .ok()
            .filter(|c| !c.trim().is_empty()),
        daemons: Mutex::new(HashMap::new()),
        starting: Mutex::new(HashMap::new()),
        changed: tokio::sync::Notify::new(),
    });
    let bg = tokio::spawn(background(s.clone()));
    crate::tls::serve(tls, listener, router(s), stop).await;
    bg.abort();
    Ok(())
}

/// What the node's own configuration lets run on it: `policy.json` in its
/// state (a [`crate::policy::NodePolicy`]), for a server deployed as code
/// (ADR-0033 point 6). It holds on top of what an admin sets.
fn local_policy(root: &Path) -> Result<Option<crate::policy::NodePolicy>, NodeError> {
    let p = root.join("policy.json");
    match std::fs::read(&p) {
        Ok(b) => serde_json::from_slice(&b)
            .map(Some)
            .map_err(|e| NodeError::Other(format!("{}: {e}", p.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(NodeError::Io(p, e)),
    }
}

/// The node's secret (`secret` in its state), made once.
fn node_secret(root: &Path) -> Result<String, NodeError> {
    let p = root.join("secret");
    if let Ok(s) = std::fs::read_to_string(&p)
        && s.trim().len() >= 32
    {
        return Ok(s.trim().to_string());
    }
    let s = crate::coordinator::random_hex(32);
    crate::pki::write_private(&p, &s).map_err(|e| NodeError::Other(e.to_string()))?;
    Ok(s)
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
    let tmp = dir.join(format!(
        ".devstack.{}.tmp",
        crate::coordinator::random_hex(6)
    ));
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
        // A blob whose content changed since it was stored does not land.
        let c = put("c");
        std::fs::write(blobs.join(&c), "not c").unwrap();
        let e = apply_files(&blobs, &lane, &[f("c.txt", &c)]).unwrap_err();
        assert!(e.to_string().contains("does not have its hash"), "{e}");
        assert!(!lane.join("src/c.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_sync_never_writes_through_a_link_the_lane_made() {
        let d = tempfile::tempdir().unwrap();
        let (blobs, lane, bin) = (
            d.path().join("blobs"),
            d.path().join("lanes/x"),
            d.path().join("bin"),
        );
        std::fs::create_dir_all(&blobs).unwrap();
        std::fs::create_dir_all(lane.join("src")).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("devstack"), "the agent's").unwrap();
        // The lane's code: `ln -s ../../../bin /src/evil`.
        std::os::unix::fs::symlink("../../../bin", lane.join("src/evil")).unwrap();
        let h = blake3::hash(b"attacker").to_hex().to_string();
        std::fs::write(blobs.join(&h), "attacker").unwrap();
        let entry = FileEntry {
            path: "evil/devstack".into(),
            hash: h,
            exec: true,
        };
        apply_files(&blobs, &lane, &[entry]).unwrap();
        assert_eq!(
            std::fs::read_to_string(bin.join("devstack")).unwrap(),
            "the agent's"
        );
        assert!(
            !lane
                .join("src/evil")
                .symlink_metadata()
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    /// A released lease reaches the coordinator with the next report: that
    /// one is due at once, not at the end of `EVERY`, or the coordinator
    /// refuses a lane against an offer that still counts the gone lease.
    #[tokio::test]
    async fn a_lease_change_brings_the_report_forward() {
        let changed = tokio::sync::Notify::new();
        // A change while the round runs is not lost: the next one is at once.
        changed.notify_one();
        tokio::time::timeout(
            Duration::from_secs(5),
            next_round(Duration::from_secs(3600), &changed),
        )
        .await
        .expect("a change made earlier brings the round forward");
        // A change while it waits.
        let t0 = Instant::now();
        let (_, ()) = tokio::join!(
            async {
                tokio::time::sleep(Duration::from_millis(50)).await;
                changed.notify_one();
            },
            async {
                tokio::time::timeout(
                    Duration::from_secs(5),
                    next_round(Duration::from_secs(3600), &changed),
                )
                .await
                .expect("a change brings the round forward");
            }
        );
        assert!(t0.elapsed() < Duration::from_secs(5));
        // Without one it waits its time.
        assert!(
            tokio::time::timeout(
                Duration::from_millis(100),
                next_round(Duration::from_secs(3600), &changed),
            )
            .await
            .is_err()
        );
    }

    #[test]
    fn a_lease_id_is_hex() {
        assert!(hex_id("0a1b2c"));
        for bad in ["", "AB", "x?token=", "../a", "a/b"] {
            assert!(!hex_id(bad), "{bad}");
        }
    }
}
