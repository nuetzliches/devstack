//! The coordinator (ADR-0031): its certificate authority, the members and
//! what the nodes report, served over TLS. A request is the member whose
//! client certificate it presents; joining needs none.
//!
//! - `GET /v1/ca`: the pool's CA (a joining member compares its fingerprint)
//! - `POST /v1/join` `{name, csr}`: ask to join; `GET /v1/join/{id}`: the answer
//! - members only: `GET /v1/me`, `GET /v1/members`, `POST /v1/nodes/report`,
//!   `GET /v1/nodes`
//! - leases (ADR-0031 point 4): `POST /v1/leases` `{node, repo, lane, need}`
//!   (a requester), `GET /v1/leases/{id}?token=` (the node, to admit it),
//!   `GET /v1/leases`
//! - admins only (ADR-0032): `POST /v1/admin/nodes/{id}/pause|resume`,
//!   `POST /v1/admin/leases/{id}/end`, `GET /v1/admin/members`,
//!   `POST /v1/admin/members/{id}/approve` `{role?}`,
//!   `POST /v1/admin/members/{id}/remove` (also rejects a request),
//!   `POST /v1/admin/members/{id}/role` `{role}`, `GET /v1/admin/audit`

use crate::pki::{self, Pair};
use crate::store::{self, AuditEntry, Member, Role, State, Store};
use axum::extract::{Path as UrlPath, State as AxState};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A node that has not reported for this long is offline.
pub const ONLINE_FOR: Duration = Duration::from_secs(45);

#[derive(Debug, thiserror::Error)]
pub enum CoordError {
    #[error("{0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("{0}")]
    Pki(#[from] pki::PkiError),
    #[error("{0}")]
    Store(#[from] store::StoreError),
    #[error("tls: {0}")]
    Tls(#[from] rustls::Error),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, CoordError>;

fn read(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).map_err(|e| CoordError::Io(path.into(), e))
}

fn write_private(path: &Path, text: &str) -> Result<()> {
    std::fs::write(path, text).map_err(|e| CoordError::Io(path.into(), e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| CoordError::Io(path.into(), e))?;
    }
    Ok(())
}

/// Create a coordinator in `dir`: a new CA and a server certificate for
/// `names` (the addresses and host names members reach it at). Returns the
/// CA's fingerprint, which a member passes to `devstack join`.
pub fn init(dir: &Path, names: &[String]) -> Result<String> {
    if names.is_empty() {
        return Err(CoordError::Other(
            "name at least one address or host name members reach the coordinator at".into(),
        ));
    }
    std::fs::create_dir_all(dir).map_err(|e| CoordError::Io(dir.into(), e))?;
    if dir.join("ca.key").exists() {
        return Err(CoordError::Other(format!(
            "{} already holds a coordinator; its CA stays (a new one would lock every member out)",
            dir.display()
        )));
    }
    let ca = pki::new_ca()?;
    let server = pki::server_cert(&ca, names)?;
    write_private(&dir.join("ca.key"), &ca.key)?;
    write_private(&dir.join("ca.pem"), &ca.cert)?;
    write_private(&dir.join("server.key"), &server.key)?;
    write_private(&dir.join("server.pem"), &server.cert)?;
    let names = serde_json::to_string_pretty(&serde_json::json!({ "names": names }))
        .expect("names serialise");
    write_private(&dir.join("coordinator.json"), &names)?;
    Ok(pki::fingerprint(&ca.cert)?)
}

/// The addresses and host names the coordinator in `dir` was made for.
pub fn names(dir: &Path) -> Result<Vec<String>> {
    let v: serde_json::Value = serde_json::from_str(&read(&dir.join("coordinator.json"))?)
        .map_err(|e| CoordError::Other(format!("coordinator.json: {e}")))?;
    Ok(v["names"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|n| n.as_str().map(str::to_string))
        .collect())
}

/// The CA of the coordinator in `dir`.
pub fn ca(dir: &Path) -> Result<Pair> {
    Ok(Pair {
        cert: read(&dir.join("ca.pem"))?,
        key: read(&dir.join("ca.key"))?,
    })
}

/// What a node reports about itself (ADR-0031 point 3).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Report {
    /// Whether its owner shares it (the switch in the dashboard).
    #[serde(default)]
    pub sharing: bool,
    /// CPU cores it offers now.
    #[serde(default)]
    pub cpus: f64,
    /// Memory it offers now, bytes.
    #[serde(default)]
    pub memory: u64,
    /// Where its node API listens (an address on the pool's network).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<String>,
    /// A server its owner never reclaims (ADR-0026): it may hold long
    /// leases; a developer's machine that runs lanes is not.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dedicated: bool,
    /// The leases it holds now.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub leases: Vec<LeaseReport>,
    /// What the machine has in all (ADR-0036).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<crate::share::Amount>,
    /// What its lanes for the pool use now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used: Option<crate::share::Amount>,
    /// Its GPUs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gpus: Vec<crate::share::Gpu>,
}

/// One moment of a node's capacity, or the pool's (ADR-0036): what it has,
/// offers and its lanes use, and its GPUs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Point {
    /// Unix seconds.
    pub at: u64,
    pub total_cpus: f64,
    pub total_memory: u64,
    pub offered_cpus: f64,
    pub offered_memory: u64,
    pub used_cpus: f64,
    pub used_memory: u64,
    pub gpus: u32,
    pub gpus_offered: u32,
    /// Summed over the GPUs, percent (two at full load: 200).
    pub gpu_load: f64,
    pub gpu_memory: u64,
    pub gpu_memory_used: u64,
}

impl Point {
    fn of(at: u64, r: &Report) -> Self {
        let total = r.total.unwrap_or_default();
        let used = r.used.unwrap_or_default();
        Self {
            at,
            total_cpus: total.cpus,
            total_memory: total.memory,
            offered_cpus: if r.sharing { r.cpus } else { 0.0 },
            offered_memory: if r.sharing { r.memory } else { 0 },
            used_cpus: used.cpus,
            used_memory: used.memory,
            gpus: r.gpus.len() as u32,
            gpus_offered: r.gpus.iter().filter(|g| g.offered).count() as u32,
            gpu_load: r.gpus.iter().map(|g| g.load).sum(),
            gpu_memory: r.gpus.iter().map(|g| g.memory).sum(),
            gpu_memory_used: r.gpus.iter().map(|g| g.memory_used).sum(),
        }
    }

    fn add(&mut self, o: &Self) {
        self.total_cpus += o.total_cpus;
        self.total_memory += o.total_memory;
        self.offered_cpus += o.offered_cpus;
        self.offered_memory += o.offered_memory;
        self.used_cpus += o.used_cpus;
        self.used_memory += o.used_memory;
        self.gpus += o.gpus;
        self.gpus_offered += o.gpus_offered;
        self.gpu_load += o.gpu_load;
        self.gpu_memory += o.gpu_memory;
        self.gpu_memory_used += o.gpu_memory_used;
    }

    /// The average of `ps` (at the last one's time).
    fn mean(ps: &[Self]) -> Self {
        let n = ps.len().max(1) as f64;
        let mut sum = Self::default();
        for p in ps {
            sum.add(p);
        }
        let avg = |x: u64| (x as f64 / n) as u64;
        Self {
            at: ps.last().map_or(0, |p| p.at),
            total_cpus: sum.total_cpus / n,
            total_memory: avg(sum.total_memory),
            offered_cpus: sum.offered_cpus / n,
            offered_memory: avg(sum.offered_memory),
            used_cpus: sum.used_cpus / n,
            used_memory: avg(sum.used_memory),
            gpus: ps.last().map_or(0, |p| p.gpus),
            gpus_offered: ps.last().map_or(0, |p| p.gpus_offered),
            gpu_load: sum.gpu_load / n,
            gpu_memory: avg(sum.gpu_memory),
            gpu_memory_used: avg(sum.gpu_memory_used),
        }
    }
}

/// A node's history (ADR-0036 point 2): every report of the last hour, then
/// a point per minute for a day. In memory: a restart starts it again.
#[derive(Debug, Default)]
pub struct History {
    raw: std::collections::VecDeque<Point>,
    minutes: std::collections::VecDeque<Point>,
}

const HOUR: u64 = 3600;
const DAY: u64 = 24 * HOUR;

impl History {
    pub fn push(&mut self, p: Point) {
        // A minute that is over becomes one point.
        if let Some(last) = self.raw.back()
            && p.at / 60 != last.at / 60
        {
            let minute = last.at / 60;
            let done: Vec<Point> = self
                .raw
                .iter()
                .filter(|x| x.at / 60 == minute)
                .copied()
                .collect();
            if !done.is_empty() {
                self.minutes.push_back(Point::mean(&done));
            }
        }
        self.raw.push_back(p);
        while self.raw.front().is_some_and(|x| x.at + HOUR < p.at) {
            self.raw.pop_front();
        }
        while self.minutes.front().is_some_and(|x| x.at + DAY < p.at) {
            self.minutes.pop_front();
        }
    }

    /// The last hour as reported, or the last day by minute.
    pub fn series(&self, day: bool) -> Vec<Point> {
        if day {
            self.minutes.iter().copied().collect()
        } else {
            self.raw.iter().copied().collect()
        }
    }
}

/// The pool's history from its nodes': summed per `step` seconds, each node
/// with its last point in a step.
pub fn pool_series(nodes: &[Vec<Point>], step: u64) -> Vec<Point> {
    let mut buckets: std::collections::BTreeMap<u64, Point> = std::collections::BTreeMap::new();
    for series in nodes {
        let mut last: std::collections::BTreeMap<u64, Point> = std::collections::BTreeMap::new();
        for p in series {
            last.insert(p.at / step, *p);
        }
        for (b, p) in last {
            let e = buckets.entry(b).or_insert(Point {
                at: b * step,
                ..Point::default()
            });
            e.add(&p);
        }
    }
    buckets.into_values().collect()
}

/// A lease as its node reports it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaseReport {
    pub id: String,
    /// The requester.
    pub member: String,
    pub repo: String,
    pub lane: String,
    /// Seconds until it expires unless renewed.
    pub expires_in: u64,
}

/// `POST /v1/leases`: a requester asks for a lane on `node`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaseRequest {
    /// The node's member id or name (`--on`).
    pub node: String,
    /// The repository, as the requester names it (its stack id).
    pub repo: String,
    pub lane: String,
    /// What the lane is expected to need.
    #[serde(default)]
    pub need: crate::share::Amount,
}

/// A member that runs lanes, as a requester reaches it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeRef {
    pub id: String,
    pub name: String,
    /// `https://host:port`
    pub api: String,
}

/// The coordinator's answer to a `LeaseRequest`: the requester takes it to
/// the node, which admits it (ADR-0031 point 4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaseOffer {
    pub id: String,
    /// Proves to the node that the coordinator created this lease for this
    /// requester.
    pub token: String,
    pub node: NodeRef,
}

/// A lease as the coordinator keeps it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lease {
    pub id: String,
    pub node: String,
    pub member: String,
    pub repo: String,
    pub lane: String,
    pub need: crate::share::Amount,
    /// `offered` (created, not yet admitted by the node) or `running`.
    pub state: String,
    /// Unix seconds.
    pub created: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in: Option<u64>,
    /// Whom its owner shares the lane with, and which services (ADR-0035).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared: Option<Sharing>,
}

/// A lane shared with others in the pool (ADR-0035).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Sharing {
    /// Member ids or tags; `*`: every member of the pool.
    pub with: Vec<String>,
    /// What they may open.
    pub services: Vec<SharedService>,
}

impl Sharing {
    /// May the member `id` with `tags` use it?
    pub fn admits(&self, id: &str, tags: &[String]) -> bool {
        self.with
            .iter()
            .any(|w| w == "*" || w == id || tags.iter().any(|t| t == w))
    }
}

/// A service of a shared lane: its port on the node (the stack's port).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SharedService {
    pub name: String,
    pub port: u16,
    /// `http` or `https`.
    #[serde(default = "http")]
    pub scheme: String,
    /// The port its owner reaches it on (the lane's port there): the one a
    /// member opening it gets too, if free, so links between its services
    /// work alike.
    #[serde(default)]
    pub here: u16,
}

fn http() -> String {
    "http".into()
}

/// A lane shared with the caller, as `GET /v1/shared` lists it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharedLane {
    pub lease: String,
    pub repo: String,
    pub lane: String,
    /// Its owner: member id and name.
    pub owner: String,
    pub owner_name: String,
    pub node: NodeRef,
    pub services: Vec<SharedService>,
}

/// A member as the pool shows it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeView {
    pub id: String,
    pub name: String,
    pub online: bool,
    /// Seconds since its last report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seen: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<Report>,
    /// Paused by an admin: no new lanes.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub paused: bool,
    /// `server` (a dedicated node), `node` (a machine that runs lanes and
    /// may reclaim them), `machine` (a developer's machine that asks for
    /// lanes), from its last report; `unknown` before one.
    #[serde(default)]
    pub kind: String,
}

/// What a member is, from its last report.
pub fn kind_of(report: Option<&Report>) -> &'static str {
    match report {
        Some(r) if r.api.is_some() && r.dedicated => "server",
        Some(r) if r.api.is_some() => "node",
        Some(_) => "machine",
        None => "unknown",
    }
}

/// The coordinator's answer to a node's report: what it is to do.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReportAnswer {
    /// Leases an admin ended: the node stops their lanes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub end: Vec<String>,
}

/// A member as `/v1/members` lists it (no request or certificate); the
/// address it asked from only for admins.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemberView {
    pub id: String,
    pub name: String,
    pub state: State,
    #[serde(default)]
    pub role: Role,
    pub requested_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// As a node: what it lets run on it (ADR-0033).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<crate::policy::NodePolicy>,
}

impl From<&Member> for MemberView {
    fn from(m: &Member) -> Self {
        Self {
            id: m.id.clone(),
            name: m.name.clone(),
            state: m.state,
            role: m.role,
            requested_at: m.requested_at,
            address: None,
            tags: m.tags.clone(),
            policy: m.policy.clone(),
        }
    }
}

/// `POST /v1/admin/members/{id}/tags`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TagsRequest {
    pub tags: Vec<String>,
}

impl MemberView {
    fn for_admin(m: &Member) -> Self {
        Self {
            address: m.address.clone(),
            ..Self::from(m)
        }
    }
}

/// `POST /v1/admin/members/{id}/approve`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ApproveRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<Role>,
}

/// `POST /v1/admin/members/{id}/role`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoleRequest {
    pub role: Role,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinRequest {
    pub name: String,
    pub csr: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinAnswer {
    pub id: String,
    pub state: State,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cert: Option<String>,
}

/// An offered lease not admitted within this time is dropped.
const OFFER_FOR: Duration = Duration::from_secs(300);

struct Inner {
    dir: PathBuf,
    /// Leases an admin ended, until their node no longer holds them.
    ending: Mutex<std::collections::HashSet<String>>,
    /// Leases by id, with their token and when they were offered.
    leases: Mutex<HashMap<String, (Lease, String, std::time::Instant)>>,
    store: Store,
    ca_pem: String,
    reports: Mutex<HashMap<String, (std::time::Instant, Report)>>,
    /// Each node's capacity over time (ADR-0036).
    history: Mutex<HashMap<String, History>>,
    /// Where telemetry goes (ADR-0037); `None`: nowhere.
    collector: Option<crate::telemetry::Collector>,
    /// The last failure passing telemetry on, and when it was logged.
    telemetry_error: Arc<Mutex<(Option<String>, Option<std::time::Instant>)>>,
}

use crate::tls::Peer;

type Shared = Arc<Inner>;
type ApiResult<T> = std::result::Result<T, (StatusCode, String)>;

fn internal(e: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

/// The approved member behind a request.
fn member(s: &Inner, peer: &Peer) -> ApiResult<Member> {
    let Some(fp) = &peer.fp else {
        return Err((
            StatusCode::UNAUTHORIZED,
            "no client certificate: join the pool first (`devstack join`)".into(),
        ));
    };
    s.store.by_cert(fp).map_err(internal)?.ok_or((
        StatusCode::FORBIDDEN,
        "this certificate is not an approved member of the pool".into(),
    ))
}

/// The approved admin behind a request, and how the audit log names them.
fn admin(s: &Inner, peer: &Peer) -> ApiResult<(Member, String)> {
    let m = member(s, peer)?;
    if m.role != Role::Admin {
        return Err((
            StatusCode::FORBIDDEN,
            "only an admin of the pool may do this".into(),
        ));
    }
    let by = format!("{} ({})", m.id, m.name);
    Ok((m, by))
}

/// A store error as an answer: unknown and ambiguous members are the
/// caller's mistake, a refused change a conflict.
fn store_err(e: store::StoreError) -> (StatusCode, String) {
    use store::StoreError as E;
    let status = match &e {
        E::Unknown(_) => StatusCode::NOT_FOUND,
        E::Ambiguous(..) => StatusCode::BAD_REQUEST,
        E::Refused(_) => StatusCode::CONFLICT,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    (status, e.to_string())
}

fn router(shared: Shared) -> Router {
    Router::new()
        .route(
            "/v1/ca",
            get(|AxState(s): AxState<Shared>| async move { s.ca_pem.clone() }),
        )
        .route("/v1/join", post(join))
        .route("/v1/join/{id}", get(join_state))
        .route("/v1/me", get(me))
        .route("/v1/members", get(members))
        .route("/v1/nodes/report", post(report))
        .route("/v1/nodes", get(nodes))
        .route("/v1/leases", get(leases).post(lease_create))
        .route("/v1/leases/{id}", get(lease_get))
        .route("/v1/leases/{id}/share", axum::routing::put(lease_share))
        .route("/v1/leases/{id}/shared", get(lease_shared))
        .route("/v1/shared", get(shared_list))
        .route("/v1/history", get(history))
        .route("/v1/telemetry", get(telemetry_info))
        .route(
            "/v1/telemetry/{kind}",
            post(telemetry_post).layer(axum::extract::DefaultBodyLimit::max(4 << 20)),
        )
        .route("/v1/admin/members", get(admin_members))
        .route("/v1/admin/members/{id}/approve", post(admin_approve))
        .route("/v1/admin/members/{id}/remove", post(admin_remove))
        .route("/v1/admin/members/{id}/role", post(admin_role))
        .route("/v1/admin/audit", get(admin_audit))
        .route("/v1/admin/nodes/{id}/pause", post(admin_pause))
        .route("/v1/admin/nodes/{id}/resume", post(admin_resume))
        .route("/v1/admin/leases/{id}/end", post(admin_end))
        .route("/v1/admin/members/{id}/tags", post(admin_tags))
        .route("/v1/admin/nodes/{id}/policy", post(admin_policy))
        .with_state(shared)
}

async fn join(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    Json(r): Json<JoinRequest>,
) -> ApiResult<Json<JoinAnswer>> {
    let id = pki::member_id(&r.csr).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    let name: String = r
        .name
        .chars()
        .filter(|c| !c.is_control())
        .take(64)
        .collect();
    let m = s
        .store
        .request(&id, name.trim(), &r.csr, Some(&peer.addr.ip().to_string()))
        .map_err(internal)?;
    Ok(Json(answer(&m)))
}

fn answer(m: &Member) -> JoinAnswer {
    JoinAnswer {
        id: m.id.clone(),
        state: m.state,
        cert: m.cert.clone().filter(|_| m.state == State::Approved),
    }
}

async fn join_state(
    AxState(s): AxState<Shared>,
    UrlPath(id): UrlPath<String>,
) -> ApiResult<Json<JoinAnswer>> {
    match s.store.get(&id).map_err(internal)? {
        Some(m) => Ok(Json(answer(&m))),
        None => Err((StatusCode::NOT_FOUND, format!("no request `{id}`"))),
    }
}

async fn me(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
) -> ApiResult<Json<MemberView>> {
    Ok(Json(MemberView::from(&member(&s, &peer)?)))
}

async fn members(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
) -> ApiResult<Json<Vec<MemberView>>> {
    member(&s, &peer)?;
    let ms = s.store.list().map_err(internal)?;
    Ok(Json(ms.iter().map(MemberView::from).collect()))
}

async fn report(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    Json(r): Json<Report>,
) -> ApiResult<Json<ReportAnswer>> {
    let m = member(&s, &peer)?;
    // What an admin ended and the node still holds: it is to stop them.
    let end: Vec<String> = {
        let mut ending = s.ending.lock().unwrap();
        ending.retain(|id| r.leases.iter().any(|l| &l.id == id));
        r.leases
            .iter()
            .filter(|l| ending.contains(&l.id))
            .map(|l| l.id.clone())
            .collect()
    };
    {
        let mut leases = s.leases.lock().unwrap();
        // What the node holds is running; what it no longer holds is over.
        leases.retain(|id, (l, _, at)| {
            if l.node != m.id {
                return l.state != "offered" || at.elapsed() < OFFER_FOR;
            }
            match r.leases.iter().find(|x| &x.id == id) {
                Some(x) => {
                    l.state = "running".into();
                    l.expires_in = Some(x.expires_in);
                    true
                }
                None => l.state == "offered" && at.elapsed() < OFFER_FOR,
            }
        });
    }
    s.history
        .lock()
        .unwrap()
        .entry(m.id.clone())
        .or_default()
        .push(Point::of(store::now(), &r));
    s.reports
        .lock()
        .unwrap()
        .insert(m.id, (std::time::Instant::now(), r));
    Ok(Json(ReportAnswer { end }))
}

async fn nodes(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
) -> ApiResult<Json<Vec<NodeView>>> {
    member(&s, &peer)?;
    let ms = s.store.list().map_err(internal)?;
    let reports = s.reports.lock().unwrap();
    Ok(Json(
        ms.iter()
            .filter(|m| m.state == State::Approved)
            .map(|m| {
                let r = reports.get(&m.id);
                let age = r.map(|(at, _)| at.elapsed());
                NodeView {
                    id: m.id.clone(),
                    name: m.name.clone(),
                    online: age.is_some_and(|a| a < ONLINE_FOR),
                    seen: age.map(|a| a.as_secs()),
                    report: r.map(|(_, r)| r.clone()),
                    paused: m.paused,
                    kind: kind_of(r.map(|(_, r)| r)).into(),
                }
            })
            .collect(),
    ))
}

async fn lease_create(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    Json(mut r): Json<LeaseRequest>,
) -> ApiResult<Json<LeaseOffer>> {
    let me = member(&s, &peer)?;
    let ms = s.store.list().map_err(internal)?;
    let pick = |f: &dyn Fn(&Member) -> bool| -> Vec<&Member> {
        ms.iter()
            .filter(|m| m.state == State::Approved && f(m))
            .collect()
    };
    // No node named (`pool`): the coordinator places the lane (ADR-0031
    // point 4): of the nodes that are online, run lanes, share, are not
    // paused and offer enough, the one with the most memory free, not the
    // requester itself.
    if r.node.is_empty() || r.node.eq_ignore_ascii_case("pool") {
        let reports = s.reports.lock().unwrap();
        let best = ms
            .iter()
            .filter(|m| m.state == State::Approved && !m.paused && m.id != me.id)
            .filter(|m| {
                m.policy
                    .as_ref()
                    .is_none_or(|p| p.admits(&me.id, &me.tags, &r.repo).is_ok())
            })
            .filter_map(|m| {
                let (at, rep) = reports.get(&m.id)?;
                (at.elapsed() < ONLINE_FOR
                    && rep.api.is_some()
                    && rep.sharing
                    && rep.cpus >= r.need.cpus
                    && rep.memory >= r.need.memory)
                    .then_some((m, rep.memory))
            })
            .max_by_key(|(_, mem)| *mem)
            .map(|(m, _)| m.id.clone());
        drop(reports);
        let Some(id) = best else {
            return Err((
                StatusCode::CONFLICT,
                format!(
                    "no node of the pool can take the lane now ({:.1} CPU, {} MiB): none online, sharing and with that much free",
                    r.need.cpus,
                    r.need.memory >> 20
                ),
            ));
        };
        r.node = id;
    }
    let mut hits = pick(&|m| m.id == r.node || m.id.starts_with(&r.node));
    if hits.is_empty() {
        hits = pick(&|m| m.name.eq_ignore_ascii_case(&r.node));
    }
    let node = match hits.as_slice() {
        [n] => (*n).clone(),
        [] => {
            return Err((
                StatusCode::NOT_FOUND,
                format!("no node `{}` in the pool", r.node),
            ));
        }
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("`{}` names more than one member", r.node),
            ));
        }
    };
    let (api, offer) = {
        let reports = s.reports.lock().unwrap();
        let Some((at, rep)) = reports
            .get(&node.id)
            .filter(|(at, _)| at.elapsed() < ONLINE_FOR)
        else {
            return Err((StatusCode::CONFLICT, format!("{} is offline", node.name)));
        };
        if node.paused {
            return Err((
                StatusCode::CONFLICT,
                format!("{} is paused by an admin: it takes no new lanes", node.name),
            ));
        }
        if let Some(p) = &node.policy
            && let Err(why) = p.admits(&me.id, &me.tags, &r.repo)
        {
            return Err((StatusCode::FORBIDDEN, format!("{}: {why}", node.name)));
        }
        let _ = at;
        let Some(api) = rep.api.clone() else {
            return Err((
                StatusCode::CONFLICT,
                format!("{} runs no node agent: it cannot run lanes", node.name),
            ));
        };
        if !rep.sharing {
            return Err((
                StatusCode::CONFLICT,
                format!("{} does not share right now", node.name),
            ));
        }
        (
            api,
            crate::share::Amount {
                cpus: rep.cpus,
                memory: rep.memory,
            },
        )
    };
    if r.need.cpus > offer.cpus || r.need.memory > offer.memory {
        return Err((
            StatusCode::CONFLICT,
            format!(
                "{} offers {:.1} CPU and {} MiB now; the lane needs {:.1} CPU and {} MiB",
                node.name,
                offer.cpus,
                offer.memory >> 20,
                r.need.cpus,
                r.need.memory >> 20
            ),
        ));
    }
    let id = random_hex(8);
    let token = random_hex(24);
    let lease = Lease {
        id: id.clone(),
        node: node.id.clone(),
        member: me.id,
        repo: r.repo,
        lane: r.lane,
        need: r.need,
        state: "offered".into(),
        created: store::now(),
        expires_in: None,
        shared: None,
    };
    s.leases.lock().unwrap().insert(
        id.clone(),
        (lease, token.clone(), std::time::Instant::now()),
    );
    Ok(Json(LeaseOffer {
        id,
        token,
        node: NodeRef {
            id: node.id,
            name: node.name,
            api,
        },
    }))
}

#[derive(Deserialize)]
struct TokenQuery {
    token: String,
}

/// The node asks about a lease a requester brought it: only the node it
/// was created for, with its token.
async fn lease_get(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    UrlPath(id): UrlPath<String>,
    axum::extract::Query(q): axum::extract::Query<TokenQuery>,
) -> ApiResult<Json<Lease>> {
    let me = member(&s, &peer)?;
    let leases = s.leases.lock().unwrap();
    match leases.get(&id) {
        Some((l, token, _)) if l.node == me.id && crate::tls::same(token, &q.token) => {
            Ok(Json(l.clone()))
        }
        _ => Err((
            StatusCode::NOT_FOUND,
            format!("no lease `{id}` for this node"),
        )),
    }
}

async fn leases(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
) -> ApiResult<Json<Vec<Lease>>> {
    member(&s, &peer)?;
    let mut v: Vec<Lease> = s
        .leases
        .lock()
        .unwrap()
        .values()
        .map(|(l, _, _)| l.clone())
        .collect();
    v.sort_by_key(|l| l.created);
    Ok(Json(v))
}

/// `PUT /v1/leases/{id}/share` (its owner): share the lane, or stop
/// sharing it with `null` (ADR-0035).
async fn lease_share(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    UrlPath(id): UrlPath<String>,
    Json(sharing): Json<Option<Sharing>>,
) -> ApiResult<StatusCode> {
    let me = member(&s, &peer)?;
    let mut leases = s.leases.lock().unwrap();
    match leases.get_mut(&id) {
        Some((l, _, _)) if l.member == me.id => {
            l.shared = sharing.filter(|x| !x.with.is_empty() && !x.services.is_empty());
            Ok(StatusCode::NO_CONTENT)
        }
        _ => Err((StatusCode::NOT_FOUND, format!("no lease `{id}` of yours"))),
    }
}

/// `GET /v1/leases/{id}/shared` (the lease's node): whom it is shared with,
/// so the node can check a tunnel of someone else (ADR-0035 point 3).
async fn lease_shared(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    UrlPath(id): UrlPath<String>,
) -> ApiResult<Json<Option<Sharing>>> {
    let me = member(&s, &peer)?;
    let leases = s.leases.lock().unwrap();
    match leases.get(&id) {
        Some((l, _, _)) if l.node == me.id => Ok(Json(l.shared.clone())),
        _ => Err((
            StatusCode::NOT_FOUND,
            format!("no lease `{id}` on this node"),
        )),
    }
}

/// `GET /v1/shared`: the lanes shared with the caller, but its own.
async fn shared_list(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
) -> ApiResult<Json<Vec<SharedLane>>> {
    let me = member(&s, &peer)?;
    let all = s.store.list().map_err(internal)?;
    let name = |id: &str| all.iter().find(|m| m.id == id).map(|m| m.name.clone());
    let reports = s.reports.lock().unwrap();
    let mut out: Vec<SharedLane> = s
        .leases
        .lock()
        .unwrap()
        .values()
        .filter_map(|(l, _, _)| {
            let sh = l.shared.as_ref()?;
            if l.member == me.id || l.state != "running" || !sh.admits(&me.id, &me.tags) {
                return None;
            }
            let api = reports.get(&l.node)?.1.api.clone()?;
            Some(SharedLane {
                lease: l.id.clone(),
                repo: l.repo.clone(),
                lane: l.lane.clone(),
                owner: l.member.clone(),
                owner_name: name(&l.member).unwrap_or_else(|| l.member.clone()),
                node: NodeRef {
                    id: l.node.clone(),
                    name: name(&l.node).unwrap_or_else(|| l.node.clone()),
                    api,
                },
                services: sh.services.clone(),
            })
        })
        .collect();
    out.sort_by(|a, b| (&a.node.name, &a.repo, &a.lane).cmp(&(&b.node.name, &b.repo, &b.lane)));
    Ok(Json(out))
}

#[derive(Deserialize)]
pub struct HistoryQuery {
    /// A node's member id; none: the pool.
    #[serde(default)]
    pub node: Option<String>,
    /// `1h` (default) or `24h`.
    #[serde(default)]
    pub range: Option<String>,
}

/// `GET /v1/history?node=&range=1h|24h`: a node's capacity over time, or
/// the pool's (ADR-0036).
async fn history(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    axum::extract::Query(q): axum::extract::Query<HistoryQuery>,
) -> ApiResult<Json<Vec<Point>>> {
    member(&s, &peer)?;
    let day = q.range.as_deref() == Some("24h");
    let h = s.history.lock().unwrap();
    Ok(Json(match q.node {
        Some(n) => h.get(&n).map(|x| x.series(day)).unwrap_or_default(),
        None => {
            let all: Vec<Vec<Point>> = h.values().map(|x| x.series(day)).collect();
            pool_series(&all, if day { 60 } else { 30 })
        }
    }))
}

/// `GET /v1/telemetry`: whether this pool collects telemetry, and what
/// (ADR-0037).
async fn telemetry_info(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
) -> ApiResult<Json<crate::telemetry::Info>> {
    member(&s, &peer)?;
    Ok(Json(crate::telemetry::Info {
        enabled: s.collector.is_some(),
        last_error: s.telemetry_error.lock().unwrap().0.clone(),
        collects: crate::telemetry::collects(),
    }))
}

/// `POST /v1/telemetry/{logs|metrics}`: OTLP/JSON from a member, passed on
/// with its identity set from its certificate. Without a collector: `404`,
/// and the member stops sending. Lossy: answered at once, dropped if the
/// collector does not take it.
async fn telemetry_post(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    UrlPath(kind): UrlPath<String>,
    Json(mut body): Json<serde_json::Value>,
) -> ApiResult<StatusCode> {
    let m = member(&s, &peer)?;
    let Some(collector) = s.collector.clone() else {
        return Err((
            StatusCode::NOT_FOUND,
            "this pool collects no telemetry".into(),
        ));
    };
    if kind != "logs" && kind != "metrics" {
        return Err((StatusCode::NOT_FOUND, format!("no telemetry `{kind}`")));
    }
    let pool = names(&s.dir)
        .ok()
        .and_then(|n| n.first().cloned())
        .unwrap_or_default();
    crate::telemetry::stamp(
        &mut body,
        &[
            ("devstack.member.id", m.id.clone()),
            ("devstack.member.name", m.name.clone()),
            ("devstack.pool.coordinator", pool),
        ],
    );
    let errors = s.telemetry_error.clone();
    tokio::task::spawn_blocking(move || {
        // A panic in here would vanish with the task: catch it and say so.
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            collector.send(&kind, &body)
        }))
        .unwrap_or_else(|p| {
            Err(format!(
                "panicked: {}",
                p.downcast_ref::<String>()
                    .map(String::as_str)
                    .or_else(|| p.downcast_ref::<&str>().copied())
                    .unwrap_or("?")
            ))
        });
        let mut e = errors.lock().unwrap();
        match r {
            Ok(()) => e.0 = None,
            Err(why) => {
                // Logged at most once a minute: a collector that is down
                // would otherwise fill the log.
                if e.1
                    .is_none_or(|at| at.elapsed() > std::time::Duration::from_secs(60))
                {
                    eprintln!("telemetry: passing {kind} on failed: {why}");
                    e.1 = Some(std::time::Instant::now());
                }
                e.0 = Some(why);
            }
        }
    });
    Ok(StatusCode::ACCEPTED)
}

fn random_hex(bytes: usize) -> String {
    use rand::RngCore;
    let mut b = vec![0u8; bytes];
    rand::rng().fill_bytes(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

async fn admin_tags(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    UrlPath(id): UrlPath<String>,
    Json(t): Json<TagsRequest>,
) -> ApiResult<Json<MemberView>> {
    let (_, by) = admin(&s, &peer)?;
    let m = s.store.set_tags(&id, &t.tags, &by).map_err(store_err)?;
    Ok(Json(MemberView::for_admin(&m)))
}

/// Set what a node lets run on it; `null` lifts its policy.
async fn admin_policy(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    UrlPath(id): UrlPath<String>,
    Json(p): Json<Option<crate::policy::NodePolicy>>,
) -> ApiResult<Json<MemberView>> {
    let (_, by) = admin(&s, &peer)?;
    let m = s.store.set_policy(&id, p, &by).map_err(store_err)?;
    Ok(Json(MemberView::for_admin(&m)))
}

async fn admin_pause(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    UrlPath(id): UrlPath<String>,
) -> ApiResult<Json<MemberView>> {
    let (_, by) = admin(&s, &peer)?;
    let m = s.store.set_paused(&id, true, &by).map_err(store_err)?;
    Ok(Json(MemberView::for_admin(&m)))
}

async fn admin_resume(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    UrlPath(id): UrlPath<String>,
) -> ApiResult<Json<MemberView>> {
    let (_, by) = admin(&s, &peer)?;
    let m = s.store.set_paused(&id, false, &by).map_err(store_err)?;
    Ok(Json(MemberView::for_admin(&m)))
}

/// End a lease (a lane someone forgot): its node stops the lane at its next
/// report, and its requester sees the lease gone.
async fn admin_end(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    UrlPath(id): UrlPath<String>,
) -> ApiResult<StatusCode> {
    let (_, by) = admin(&s, &peer)?;
    let lease = s
        .leases
        .lock()
        .unwrap()
        .get(&id)
        .map(|(l, _, _)| l.clone())
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("no lease `{id}`")))?;
    s.ending.lock().unwrap().insert(id.clone());
    s.store
        .record(
            &by,
            "end",
            &lease.node,
            Some(format!("{}/{} ({id})", lease.repo, lease.lane)),
        )
        .map_err(store_err)?;
    Ok(StatusCode::ACCEPTED)
}

async fn admin_members(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
) -> ApiResult<Json<Vec<MemberView>>> {
    admin(&s, &peer)?;
    let ms = s.store.list().map_err(internal)?;
    Ok(Json(ms.iter().map(MemberView::for_admin).collect()))
}

async fn admin_approve(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    UrlPath(id): UrlPath<String>,
    body: Option<Json<ApproveRequest>>,
) -> ApiResult<Json<MemberView>> {
    let (_, by) = admin(&s, &peer)?;
    let role = body.and_then(|Json(b)| b.role);
    let ca = ca(&s.dir).map_err(internal)?;
    let m = s.store.approve(&id, &ca, role, &by).map_err(store_err)?;
    Ok(Json(MemberView::for_admin(&m)))
}

async fn admin_remove(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    UrlPath(id): UrlPath<String>,
) -> ApiResult<Json<MemberView>> {
    let (_, by) = admin(&s, &peer)?;
    let m = s.store.remove(&id, &by).map_err(store_err)?;
    s.reports.lock().unwrap().remove(&m.id);
    Ok(Json(MemberView::for_admin(&m)))
}

async fn admin_role(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    UrlPath(id): UrlPath<String>,
    Json(r): Json<RoleRequest>,
) -> ApiResult<Json<MemberView>> {
    let (_, by) = admin(&s, &peer)?;
    let m = s.store.set_role(&id, r.role, &by).map_err(store_err)?;
    Ok(Json(MemberView::for_admin(&m)))
}

async fn admin_audit(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
) -> ApiResult<Json<Vec<AuditEntry>>> {
    admin(&s, &peer)?;
    Ok(Json(s.store.audit(200).map_err(internal)?))
}

/// Serve the coordinator in `dir` on `listener` until `stop` resolves.
pub async fn serve(
    dir: &Path,
    listener: tokio::net::TcpListener,
    stop: impl std::future::Future<Output = ()>,
) -> Result<()> {
    let ca_pem = read(&dir.join("ca.pem"))?;
    let config = crate::tls::server_config(
        &ca_pem,
        &read(&dir.join("server.pem"))?,
        &read(&dir.join("server.key"))?,
        false,
    )
    .map_err(|e| CoordError::Other(e.to_string()))?;
    let shared = Arc::new(Inner {
        dir: dir.to_path_buf(),
        leases: Mutex::new(HashMap::new()),
        history: Mutex::new(HashMap::new()),
        collector: crate::telemetry::Collector::of(dir),
        telemetry_error: Arc::default(),
        ending: Mutex::new(std::collections::HashSet::new()),
        store: Store::new(dir),
        ca_pem,
        reports: Mutex::new(HashMap::new()),
    });
    crate::tls::serve(config, listener, router(shared), stop).await;
    Ok(())
}

/// Serve the coordinator in `dir` on `listen` until Ctrl-C (or SIGTERM).
pub fn serve_blocking(dir: &Path, listen: std::net::SocketAddr) -> Result<()> {
    let rt = tokio::runtime::Runtime::new().map_err(|e| CoordError::Other(e.to_string()))?;
    rt.block_on(async {
        let listener = tokio::net::TcpListener::bind(listen)
            .await
            .map_err(|e| CoordError::Other(format!("listen on {listen}: {e}")))?;
        serve(dir, listener, stop_signal()).await
    })
}

pub async fn stop_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let Ok(mut term) = signal(SignalKind::terminate()) else {
            let _ = tokio::signal::ctrl_c().await;
            return;
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(test)]
mod history_tests {
    use super::*;

    fn p(at: u64, used: f64) -> Point {
        Point {
            at,
            total_cpus: 8.0,
            used_cpus: used,
            ..Point::default()
        }
    }

    #[test]
    fn a_node_keeps_an_hour_as_reported_and_a_day_by_minute() {
        let mut h = History::default();
        // Two reports in minute 0, one in minute 1: minute 0 is averaged.
        h.push(p(0, 1.0));
        h.push(p(30, 3.0));
        h.push(p(60, 5.0));
        assert_eq!(h.series(false).len(), 3);
        let day = h.series(true);
        assert_eq!(day.len(), 1);
        assert_eq!(day[0].used_cpus, 2.0);
        // An hour later the first reports are gone from the hour.
        h.push(p(2 * HOUR, 0.0));
        assert_eq!(h.series(false).len(), 1);
        assert_eq!(h.series(true).len(), 2);
    }

    #[test]
    fn the_pool_sums_its_nodes_per_step() {
        let a = vec![p(0, 1.0), p(15, 2.0), p(31, 4.0)];
        let b = vec![p(5, 10.0), p(40, 20.0)];
        let s = pool_series(&[a, b], 30);
        assert_eq!(s.len(), 2);
        // Step 0: a's last (2) + b's (10); step 1: 4 + 20.
        assert_eq!((s[0].used_cpus, s[1].used_cpus), (12.0, 24.0));
        assert_eq!(s[0].total_cpus, 16.0);
    }
}
