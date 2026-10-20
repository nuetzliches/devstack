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
use crate::version::Release;
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
/// The largest certificate request a joining machine sends (a P-256
/// request is well under 1 KiB as PEM).
const MAX_CSR: usize = 4096;
/// Requests that may wait for an admin at once.
const MAX_PENDING: usize = 64;
/// Join requests one address may send per minute.
const JOINS_PER_MINUTE: usize = 10;
/// Leases a member may have offered and not yet admitted.
const MAX_OFFERS: usize = 16;
/// Telemetry bodies passed on at once; more are refused until they are.
const TELEMETRY_IN_FLIGHT: usize = 8;
/// Reports a node's history keeps as they came (an hour at one per 15 s,
/// with room for a node that reports faster).
const MAX_RAW: usize = 720;

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
    pki::write_private(path, text).map_err(|e| CoordError::Io(path.into(), e))
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
    /// The devstack it runs; absent from a release before version
    /// reporting (`crate::version`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub devstack: Option<Release>,
    /// The stopped lanes it keeps for their next start (ADR-0038), as last
    /// counted; absent from an older node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kept: Option<Kept>,
    /// How the last update an admin ordered goes (ADR-0039).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update: Option<UpdateReport>,
    /// Why it would refuse an update an admin orders, if it would (in a
    /// container its image is updated instead): the dashboard says so
    /// rather than offer the order. Absent: it updates itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub no_update: Option<String>,
    /// The lanes it has a slot for (ADR-0038: mirror, builds, volumes),
    /// running or kept: where a lane starts warm (ADR-0026 point 3).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub slots: Vec<SlotRef>,
    /// Bytes free where it keeps lanes; absent from an older node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_free: Option<u64>,
}

/// A lane a node has a slot for: whose, of which repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotRef {
    pub member: String,
    pub repo: String,
    pub lane: String,
}

/// Less free disk than this where a node keeps lanes: it takes none (a
/// lane's images, mirror and volumes need room).
pub const MIN_DISK_FREE: u64 = 5 << 30;

/// Where a lane would go, best first (ADR-0026 point 3, ADR-0031 point 4):
/// `GET /v1/placement`, and the coordinator's own choice.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Placement {
    pub node: String,
    pub name: String,
    /// It can take the lane now.
    pub eligible: bool,
    /// Why it ranks where it does, or why it cannot take the lane.
    pub why: String,
    /// What it offers now.
    pub cpus: f64,
    pub memory: u64,
}

/// Rank the nodes for a lane of `member`'s `repo`/`lane` needing `need`:
/// first the hard conditions (each node's own reason when it fails one),
/// then, with room to spare (half as much again as the need), a node that
/// keeps this lane's data, then one the repository ran on (its images and
/// builds are there), then the most free capacity for the need (the lower
/// of free memory and free CPU, each over the need). `nodes` are the
/// candidates already granted, online and admitted by their policy, with
/// their last report.
pub fn rank(
    nodes: &[(&str, &str, &Report)],
    member: &str,
    repo: &str,
    lane: &str,
    need: crate::share::Amount,
    requester: Option<&Release>,
) -> Vec<Placement> {
    let mut out: Vec<(u8, f64, Placement)> = nodes
        .iter()
        .map(|(id, name, rep)| {
            let refuse = if rep.api.is_none() {
                Some("runs no node agent".to_string())
            } else if !rep.sharing {
                Some("does not share now".into())
            } else if !same_protocol(requester, rep.devstack.as_ref()) {
                Some("runs a devstack that does not work with the requester's".into())
            } else if rep.cpus < need.cpus || rep.memory < need.memory {
                Some(format!(
                    "offers {:.1} CPU and {} MiB, the lane needs {:.1} CPU and {} MiB",
                    rep.cpus,
                    rep.memory >> 20,
                    need.cpus,
                    need.memory >> 20
                ))
            } else if rep.disk_free.is_some_and(|d| d < MIN_DISK_FREE) {
                Some(format!(
                    "has {} MiB of disk free, less than {} MiB",
                    rep.disk_free.unwrap_or(0) >> 20,
                    MIN_DISK_FREE >> 20
                ))
            } else {
                None
            };
            let ratio = {
                let m = rep.memory as f64 / need.memory.max(1) as f64;
                let c = rep.cpus / need.cpus.max(0.01);
                m.min(c)
            };
            let roomy =
                rep.memory as f64 >= need.memory as f64 * 1.5 && rep.cpus >= need.cpus * 1.5;
            let kept = rep
                .slots
                .iter()
                .any(|s| s.member == member && s.repo == repo && s.lane == lane);
            let warm = rep.slots.iter().any(|s| s.repo == repo);
            let (tier, why) = match &refuse {
                Some(r) => (9, r.clone()),
                None if roomy && kept => {
                    (0, "keeps this lane's data (mirror, builds, volumes)".into())
                }
                None if roomy && warm => (
                    1,
                    "the repository ran there (its images and builds are there)".into(),
                ),
                None => (2, format!("free capacity: {:.1}× the lane's need", ratio)),
            };
            (
                tier,
                ratio,
                Placement {
                    node: id.to_string(),
                    name: name.to_string(),
                    eligible: refuse.is_none(),
                    why,
                    cpus: rep.cpus,
                    memory: rep.memory,
                },
            )
        })
        .collect();
    out.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then(b.1.total_cmp(&a.1))
            .then(a.2.name.cmp(&b.2.name))
    });
    out.into_iter().map(|(_, _, p)| p).collect()
}

/// An admin's order that a server update its devstack (ADR-0039).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateOrder {
    /// A release (`0.11.0`); `None`: the latest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Take no new lane and wait until its lanes have ended (the default).
    #[serde(default = "drain_default")]
    pub drain: bool,
    /// An older release than it runs, on purpose.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub downgrade: bool,
    /// The admin's member id.
    pub by: String,
    /// When it was ordered (Unix seconds): tells one order from the next.
    pub at: u64,
}

fn drain_default() -> bool {
    true
}

/// `POST /v1/admin/nodes/{id}/update`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// `false`: update at once, ending the lanes it runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drain: Option<bool>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub downgrade: bool,
}

/// What a node says of the update it was ordered (ADR-0039).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateReport {
    /// The order's `at`.
    pub at: u64,
    /// `draining`, `installing`, `restarting`, `done`, `refused`, `failed`,
    /// `cancelled` (an admin withdrew it while the node waited for its
    /// lanes).
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl UpdateReport {
    /// Over: the order can go.
    pub fn over(&self) -> bool {
        matches!(
            self.state.as_str(),
            "done" | "refused" | "failed" | "cancelled"
        )
    }
}

/// A release version as the releases name them: `0.11.0`.
pub fn valid_version(v: &str) -> bool {
    let parts: Vec<&str> = v.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 5 && p.bytes().all(|b| b.is_ascii_digit()))
}

/// What a node keeps of stopped lanes (ADR-0038 point 3).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Kept {
    pub lanes: usize,
    /// Their directories (mirrors, builds, caches); volumes not counted.
    pub bytes: u64,
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
        while self.raw.front().is_some_and(|x| x.at + HOUR < p.at) || self.raw.len() > MAX_RAW {
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
    /// What it was admitted with: the coordinator knows the lease again
    /// from it after a restart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub need: Option<crate::share::Amount>,
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
    /// The requester's devstack: the coordinator places the lane on a
    /// node with the same pool protocol only. Absent from older requesters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub devstack: Option<Release>,
}

/// A member that runs lanes, as a requester reaches it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeRef {
    pub id: String,
    pub name: String,
    /// `https://host:port`
    pub api: String,
    /// The devstack it runs, from its last report; absent when it does not
    /// say, or the coordinator does not pass it on (both older).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub devstack: Option<Release>,
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
    /// The coordinator's devstack; absent from an older one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinator: Option<Release>,
    /// Why the coordinator chose this node (`pool`); absent when the
    /// requester named it, and from an older coordinator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
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
    /// Only for devstack 0.11, which kept a separate list of who may view a
    /// lane (ADR-0040 before its amendment of 2026-10-20). Whoever a lane is
    /// shared with may view it now: an owner sends `with` here as well, so a
    /// 0.11 coordinator or node lets them view; read, it only widens `with`
    /// (a 0.11 owner's viewers could use the lane too).
    #[serde(default, rename = "view", skip_serializing_if = "Vec::is_empty")]
    pub legacy_view: Vec<String>,
}

fn listed(list: &[String], id: &str, tags: &[String]) -> bool {
    list.iter()
        .any(|w| w == "*" || w == id || tags.iter().any(|t| t == w))
}

impl Sharing {
    /// The lane shared `with` those, for `services`, as an owner tells it.
    pub fn new(with: Vec<String>, services: Vec<SharedService>) -> Self {
        Self {
            legacy_view: with.clone(),
            with,
            services,
        }
    }

    /// May the member `id` with `tags` use it, and view it (ADR-0040,
    /// amended 2026-10-20: sharing a lane includes viewing it)?
    pub fn admits(&self, id: &str, tags: &[String]) -> bool {
        listed(&self.with, id, tags) || listed(&self.legacy_view, id, tags)
    }
}

/// A service of a shared lane: its port on the node (the lane's port, the
/// stack's moved by the lane's offset).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SharedService {
    pub name: String,
    pub port: u16,
    /// `http` or `https`; nothing else is taken (a member opens it with the
    /// system's handler for the scheme).
    #[serde(default = "http", deserialize_with = "web_scheme")]
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

/// A shared service's scheme: `http` or `https`, any case.
fn web_scheme<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<String, D::Error> {
    let s = String::deserialize(d)?.to_ascii_lowercase();
    match s.as_str() {
        "http" | "https" => Ok(s),
        _ => Err(serde::de::Error::custom(format!(
            "a shared service is `http` or `https`, not `{s}`"
        ))),
    }
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
    /// The owner's devstack, from its last report: what its sharing means
    /// (`crate::version::PROTOCOL`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_devstack: Option<Release>,
    /// Only for devstack 0.11, whose hub and dashboard view a shared lane
    /// only where this is `true`: always `true` now, as whoever a lane is
    /// shared with may view it (ADR-0040, amended 2026-10-20). Not read.
    #[serde(default, rename = "view")]
    pub legacy_view: bool,
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

/// Whether a requester and a node work together: the same pool protocol,
/// or either does not say (an older release: placed, the requester warns).
fn same_protocol(requester: Option<&Release>, node: Option<&Release>) -> bool {
    match (requester, node) {
        (Some(a), Some(b)) => a.protocol == b.protocol,
        _ => true,
    }
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
    /// Why the coordinator did not take the node's API (no admin granted
    /// it to run lanes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused: Option<String>,
    /// The coordinator's devstack; absent from an older one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinator: Option<Release>,
    /// An update an admin ordered (ADR-0039), until the node says it is over.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update: Option<UpdateOrder>,
    /// An admin asked the node to remove the stopped lanes it keeps
    /// (ADR-0038 point 5).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub prune: bool,
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
    /// An admin granted it to run the pool's lanes.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub node: bool,
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
            node: m.node,
        }
    }
}

/// `POST /v1/admin/members/{id}/node`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NodeRequest {
    pub node: bool,
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
/// How long an offered lease holds what it needs on its node against other
/// offers: a requester admits it at once, and the node's next report
/// (brought forward by the admission) counts it from then on.
const OFFER_HOLDS: Duration = Duration::from_secs(60);

/// What leases offered for `node` and not admitted there yet need: the
/// node's last report does not count them.
fn offered_need(
    leases: &HashMap<String, (Lease, String, std::time::Instant)>,
    node: &str,
) -> crate::share::Amount {
    leases
        .values()
        .filter(|(l, _, at)| l.node == node && l.state == "offered" && at.elapsed() < OFFER_HOLDS)
        .fold(crate::share::Amount::default(), |a, (l, _, _)| {
            crate::share::Amount {
                cpus: a.cpus + l.need.cpus,
                memory: a.memory + l.need.memory,
            }
        })
}

/// A node's report with what offered leases hold taken off its offer.
fn less_offered(rep: &Report, held: crate::share::Amount) -> Report {
    Report {
        cpus: (rep.cpus - held.cpus).max(0.0),
        memory: rep.memory.saturating_sub(held.memory),
        ..rep.clone()
    }
}

struct Inner {
    dir: PathBuf,
    /// Leases an admin ended, until their node no longer holds them.
    ending: Mutex<std::collections::HashSet<String>>,
    /// Updates admins ordered, by node (ADR-0039).
    updates: Mutex<HashMap<String, UpdateOrder>>,
    /// Nodes an admin asked to remove their kept lanes (ADR-0038).
    prunes: Mutex<std::collections::HashSet<String>>,
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
    /// Telemetry bodies being passed on now.
    telemetry_slots: Arc<tokio::sync::Semaphore>,
    /// Join requests by address, in the last minute.
    joins: Mutex<HashMap<std::net::IpAddr, std::collections::VecDeque<std::time::Instant>>>,
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
        .route("/v1/placement", get(placement))
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
        .route(
            "/v1/admin/nodes/{id}/update",
            post(admin_update).delete(admin_update_cancel),
        )
        .route("/v1/admin/nodes/{id}/prune", post(admin_prune))
        .route("/v1/admin/members/{id}/tags", post(admin_tags))
        .route("/v1/admin/members/{id}/node", post(admin_node))
        .route("/v1/admin/nodes/{id}/policy", post(admin_policy))
        .with_state(shared)
}

async fn join(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    Json(r): Json<JoinRequest>,
) -> ApiResult<Json<JoinAnswer>> {
    // Anyone may ask: by address, a few times a minute; a request of
    // reasonable size; a bounded number waiting.
    {
        let mut joins = s.joins.lock().unwrap();
        joins.retain(|_, at| {
            at.retain(|t| t.elapsed() < Duration::from_secs(60));
            !at.is_empty()
        });
        let at = joins.entry(peer.addr.ip()).or_default();
        if at.len() >= JOINS_PER_MINUTE {
            return Err((
                StatusCode::TOO_MANY_REQUESTS,
                "too many join requests from this address: wait a minute".into(),
            ));
        }
        at.push_back(std::time::Instant::now());
    }
    if r.csr.len() > MAX_CSR {
        return Err((
            StatusCode::BAD_REQUEST,
            "the certificate request is too large".into(),
        ));
    }
    let id = pki::member_id(&r.csr).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    if s.store.get(&id).map_err(internal)?.is_none()
        && s.store.pending().map_err(internal)? >= MAX_PENDING
    {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            "too many requests wait for an admin already".into(),
        ));
    }
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
    Json(mut r): Json<Report>,
) -> ApiResult<Json<ReportAnswer>> {
    let m = member(&s, &peer)?;
    // Only a node an admin granted is one: anyone else's API is not taken,
    // so nothing is placed there (ADR-0032, addendum 2026-10-17).
    let mut refused = None;
    if r.api.is_some() && !m.node {
        r.api = None;
        refused = Some(format!(
            "the pool does not run lanes on {} ({}): an admin grants it with `devstack node members node {} on`",
            m.name, m.id, m.id
        ));
    }
    // What an admin ended and the node still holds: it is to stop them.
    // Only this node's leases are its to settle; others wait for theirs.
    let end: Vec<String> = {
        let leases = s.leases.lock().unwrap();
        let mut ending = s.ending.lock().unwrap();
        ending.retain(|id| match leases.get(id) {
            Some((l, _, _)) if l.node != m.id => true,
            Some(_) => r.leases.iter().any(|x| &x.id == id),
            None => false,
        });
        r.leases
            .iter()
            .filter(|l| ending.contains(&l.id))
            .map(|l| l.id.clone())
            .collect()
    };
    // Leases the coordinator does not know but the node holds (the
    // coordinator restarted): known again, from what the node says.
    let approved: std::collections::HashSet<String> = s
        .store
        .list()
        .map_err(internal)?
        .into_iter()
        .filter(|x| x.state == State::Approved)
        .map(|x| x.id)
        .collect();
    {
        let mut leases = s.leases.lock().unwrap();
        for x in &r.leases {
            if !m.node || leases.contains_key(&x.id) || !approved.contains(&x.member) {
                continue;
            }
            leases.insert(
                x.id.clone(),
                (
                    Lease {
                        id: x.id.clone(),
                        node: m.id.clone(),
                        member: x.member.clone(),
                        repo: x.repo.clone(),
                        lane: x.lane.clone(),
                        need: x.need.unwrap_or_default(),
                        state: "running".into(),
                        created: store::now(),
                        expires_in: Some(x.expires_in),
                        shared: None,
                    },
                    // Admitted already: no node asks for it by token again.
                    random_hex(24),
                    std::time::Instant::now(),
                ),
            );
        }
    }
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
    // An update an admin ordered goes to the node until the node says it is
    // over, or runs the release it was to run.
    let update = {
        let mut updates = s.updates.lock().unwrap();
        let over = updates.get(&m.id).is_some_and(|o| {
            r.update.as_ref().is_some_and(|u| u.at == o.at && u.over())
                || o.version
                    .as_ref()
                    .is_some_and(|v| r.devstack.as_ref().is_some_and(|d| &d.version == v))
        });
        if over {
            updates.remove(&m.id);
        }
        updates.get(&m.id).cloned()
    };
    let prune = s.prunes.lock().unwrap().remove(&m.id);
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
    Ok(Json(ReportAnswer {
        end,
        refused,
        coordinator: Some(Release::this()),
        update,
        prune,
    }))
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

/// The nodes that may take `r` for `me` at all (granted, online, not
/// paused, not the requester, admitted by their policy), ranked.
fn placement_for(s: &Shared, ms: &[Member], me: &Member, r: &LeaseRequest) -> Vec<Placement> {
    let leases = s.leases.lock().unwrap().clone();
    // What each node offers less what offers not admitted yet hold there.
    let reports: HashMap<String, (std::time::Instant, Report)> = s
        .reports
        .lock()
        .unwrap()
        .iter()
        .map(|(id, (at, rep))| {
            (
                id.clone(),
                (*at, less_offered(rep, offered_need(&leases, id))),
            )
        })
        .collect();
    let nodes: Vec<(&str, &str, &Report)> = ms
        .iter()
        .filter(|m| m.state == State::Approved && m.node && !m.paused && m.id != me.id)
        .filter(|m| {
            m.policy
                .as_ref()
                .is_none_or(|p| p.admits(&me.id, &me.tags, &r.repo).is_ok())
        })
        .filter_map(|m| {
            let (at, rep) = reports.get(&m.id)?;
            (at.elapsed() < ONLINE_FOR).then_some((m.id.as_str(), m.name.as_str(), rep))
        })
        .collect();
    rank(
        &nodes,
        &me.id,
        &r.repo,
        &r.lane,
        crate::policy::floor_need(r.need),
        r.devstack.as_ref(),
    )
}

/// `GET /v1/placement?repo=&lane=&cpus=&memory=`: where a lane of the
/// caller's would go, best first, with why (what the dashboard shows
/// beside the nodes to start it on).
async fn placement(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    axum::extract::Query(q): axum::extract::Query<PlacementQuery>,
) -> ApiResult<Json<Vec<Placement>>> {
    let me = member(&s, &peer)?;
    let ms = s.store.list().map_err(internal)?;
    let r = LeaseRequest {
        node: String::new(),
        repo: q.repo,
        lane: q.lane,
        need: crate::share::Amount {
            cpus: q.cpus.unwrap_or(0.0),
            memory: q.memory.unwrap_or(0),
        },
        devstack: Some(Release::this()),
    };
    Ok(Json(placement_for(&s, &ms, &me, &r)))
}

#[derive(Debug, Deserialize)]
struct PlacementQuery {
    repo: String,
    lane: String,
    #[serde(default)]
    cpus: Option<f64>,
    #[serde(default)]
    memory: Option<u64>,
}

async fn lease_create(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    Json(mut r): Json<LeaseRequest>,
) -> ApiResult<Json<LeaseOffer>> {
    let me = member(&s, &peer)?;
    crate::policy::valid_lease_names(&r.repo, &r.lane).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    // A lane needs at least the least, whatever it claims (ADR-0033 point 4).
    r.need = crate::policy::floor_need(r.need);
    let offered = s
        .leases
        .lock()
        .unwrap()
        .values()
        .filter(|(l, _, at)| l.member == me.id && l.state == "offered" && at.elapsed() < OFFER_FOR)
        .count();
    if offered >= MAX_OFFERS {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            format!("{offered} leases of yours wait to be admitted already"),
        ));
    }
    let ms = s.store.list().map_err(internal)?;
    let pick = |f: &dyn Fn(&Member) -> bool| -> Vec<&Member> {
        ms.iter()
            .filter(|m| m.state == State::Approved && f(m))
            .collect()
    };
    // No node named (`pool`): the coordinator places the lane (ADR-0031
    // point 4, in the order of ADR-0026 point 3): of the nodes an admin
    // granted that are online, run lanes, share, are not paused and offer
    // enough, not the requester itself, warm ones first (`rank`).
    let mut why = None;
    if r.node.is_empty() || r.node.eq_ignore_ascii_case("pool") {
        let ranked = placement_for(&s, &ms, &me, &r);
        let best = ranked.into_iter().find(|p| p.eligible).map(|p| {
            why = Some(p.why);
            p.node
        });
        let Some(id) = best else {
            return Err((
                StatusCode::CONFLICT,
                format!(
                    "no node of the pool can take the lane now ({:.1} CPU, {} MiB): none online, sharing, with that much free and a devstack that works with the requester's",
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
    let (api, offer, node_release) = {
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
        if !node.node {
            return Err((
                StatusCode::CONFLICT,
                format!(
                    "{} is not granted to run the pool's lanes (an admin: `devstack node members node {} on`)",
                    node.name, node.id
                ),
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
        if let (Some(theirs), Some(mine)) = (&rep.devstack, &r.devstack)
            && !same_protocol(Some(mine), Some(theirs))
        {
            return Err((
                StatusCode::CONFLICT,
                format!(
                    "{} runs {theirs}, the requester {mine}: they do not work together; update the older one",
                    node.name
                ),
            ));
        }
        (
            api,
            crate::share::Amount {
                cpus: rep.cpus,
                memory: rep.memory,
            },
            rep.devstack.clone(),
        )
    };
    // Checked and offered in one step, less what other offers for the node
    // hold until it admits them: two requests between two reports cannot
    // both be offered the same room (ADR-0026 point 4).
    let id = random_hex(8);
    let token = random_hex(24);
    {
        let mut leases = s.leases.lock().unwrap();
        let held = offered_need(&leases, &node.id);
        let offer = crate::share::Amount {
            cpus: (offer.cpus - held.cpus).max(0.0),
            memory: offer.memory.saturating_sub(held.memory),
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
        leases.insert(
            id.clone(),
            (lease, token.clone(), std::time::Instant::now()),
        );
    }
    Ok(Json(LeaseOffer {
        why,
        id,
        token,
        node: NodeRef {
            id: node.id,
            name: node.name,
            api,
            devstack: node_release,
        },
        coordinator: Some(Release::this()),
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
            let rep = &reports.get(&l.node)?.1;
            let api = rep.api.clone()?;
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
                    devstack: rep.devstack.clone(),
                },
                services: sh.services.clone(),
                owner_devstack: reports.get(&l.member).and_then(|(_, r)| r.devstack.clone()),
                legacy_view: true,
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
    let m = member(&s, &peer)?;
    Ok(Json(crate::telemetry::Info {
        enabled: s.collector.is_some(),
        // The collector's failures are the pool's admins' to see.
        last_error: if m.role == Role::Admin {
            s.telemetry_error.lock().unwrap().0.clone()
        } else {
            None
        },
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
    let Ok(slot) = s.telemetry_slots.clone().try_acquire_owned() else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "the coordinator passes on enough telemetry right now".into(),
        ));
    };
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
        let _slot = slot;
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

/// `bytes` random bytes as hex.
pub fn random_hex(bytes: usize) -> String {
    let mut b = vec![0u8; bytes];
    rand::fill(&mut b[..]);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Grant a member to run the pool's lanes, or take it back.
async fn admin_node(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    UrlPath(id): UrlPath<String>,
    Json(n): Json<NodeRequest>,
) -> ApiResult<Json<MemberView>> {
    let (_, by) = admin(&s, &peer)?;
    let m = s.store.set_node(&id, n.node, &by).map_err(store_err)?;
    if !m.node
        && let Some((_, r)) = s.reports.lock().unwrap().get_mut(&m.id)
    {
        r.api = None;
    }
    Ok(Json(MemberView::for_admin(&m)))
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

/// A node an admin may order: granted, online, and (for an update) a
/// server.
fn orderable(s: &Inner, id: &str, server: bool) -> ApiResult<Member> {
    let m = s
        .store
        .list()
        .map_err(internal)?
        .into_iter()
        .find(|m| m.id == id && m.state == State::Approved)
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("no member `{id}`")))?;
    let reports = s.reports.lock().unwrap();
    let report = reports.get(id);
    if !m.node || report.is_none_or(|(at, _)| at.elapsed() > Duration::from_secs(60)) {
        return Err((
            StatusCode::CONFLICT,
            format!("{} is no node of the pool that is online", m.name),
        ));
    }
    if server && kind_of(report.map(|(_, r)| r)) != "server" {
        return Err((
            StatusCode::CONFLICT,
            format!(
                "{} is no server: a developer's machine is updated by its owner (`devstack update`)",
                m.name
            ),
        ));
    }
    Ok(m)
}

/// `POST /v1/admin/nodes/{id}/update` (ADR-0039): a server updates its
/// devstack at its next report; it may refuse (its own policy, a container).
async fn admin_update(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    UrlPath(id): UrlPath<String>,
    Json(q): Json<UpdateRequest>,
) -> ApiResult<StatusCode> {
    let (_, by) = admin(&s, &peer)?;
    if let Some(v) = &q.version
        && !valid_version(v)
    {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("`{v}` is no release version"),
        ));
    }
    let m = orderable(&s, &id, true)?;
    let order = UpdateOrder {
        version: q.version.clone(),
        drain: q.drain.unwrap_or(true),
        downgrade: q.downgrade,
        by: by.clone(),
        at: store::now(),
    };
    s.updates.lock().unwrap().insert(m.id.clone(), order);
    s.store
        .record(
            &by,
            "update",
            &m.id,
            Some(format!(
                "to {}{}",
                q.version.as_deref().unwrap_or("the latest release"),
                if q.drain == Some(false) {
                    ", at once"
                } else {
                    ""
                }
            )),
        )
        .map_err(store_err)?;
    Ok(StatusCode::ACCEPTED)
}

/// `DELETE /v1/admin/nodes/{id}/update`: withdraw an update a node has not
/// begun to install (it waits for its lanes): the order is no longer sent,
/// and the node takes lanes again (ADR-0039). `404` without an order.
async fn admin_update_cancel(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    UrlPath(id): UrlPath<String>,
) -> ApiResult<StatusCode> {
    let (_, by) = admin(&s, &peer)?;
    let installing = s.reports.lock().unwrap().get(&id).is_some_and(|(_, r)| {
        r.update
            .as_ref()
            .is_some_and(|u| matches!(u.state.as_str(), "installing" | "restarting"))
    });
    if installing {
        return Err((
            StatusCode::CONFLICT,
            "the node installs the update already: it cannot be withdrawn now".into(),
        ));
    }
    if s.updates.lock().unwrap().remove(&id).is_none() {
        return Err((
            StatusCode::NOT_FOUND,
            format!("no update ordered for `{id}`"),
        ));
    }
    s.store
        .record(&by, "update cancelled", &id, None)
        .map_err(store_err)?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /v1/admin/nodes/{id}/prune` (ADR-0038 point 5): the node removes
/// the stopped lanes it keeps, at its next report.
async fn admin_prune(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    UrlPath(id): UrlPath<String>,
) -> ApiResult<StatusCode> {
    let (_, by) = admin(&s, &peer)?;
    let m = orderable(&s, &id, false)?;
    s.prunes.lock().unwrap().insert(m.id.clone());
    s.store
        .record(&by, "prune", &m.id, Some("its kept lanes".into()))
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
        telemetry_slots: Arc::new(tokio::sync::Semaphore::new(TELEMETRY_IN_FLIGHT)),
        joins: Mutex::new(HashMap::new()),
        ending: Mutex::new(std::collections::HashSet::new()),
        updates: Mutex::new(HashMap::new()),
        prunes: Mutex::new(std::collections::HashSet::new()),
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

#[cfg(test)]
mod placement_tests {
    use super::*;
    use crate::share::Amount;

    const GIB: u64 = 1 << 30;

    fn node(cpus: f64, mem_gib: u64, slots: &[(&str, &str, &str)]) -> Report {
        Report {
            sharing: true,
            cpus,
            memory: mem_gib * GIB,
            api: Some("https://n:1".into()),
            devstack: Some(Release::this()),
            slots: slots
                .iter()
                .map(|(m, r, l)| SlotRef {
                    member: m.to_string(),
                    repo: r.to_string(),
                    lane: l.to_string(),
                })
                .collect(),
            disk_free: Some(100 * GIB),
            ..Report::default()
        }
    }

    fn order(nodes: &[(&str, &str, &Report)], need: Amount) -> Vec<(String, bool, String)> {
        rank(nodes, "me", "shop", "fix", need, Some(&Release::this()))
            .into_iter()
            .map(|p| (p.name, p.eligible, p.why))
            .collect()
    }

    #[test]
    fn a_lane_goes_where_it_starts_warm_with_room_to_spare() {
        let need = Amount {
            cpus: 1.0,
            memory: 4 * GIB,
        };
        let big = node(16.0, 64, &[]);
        let warm = node(4.0, 8, &[("someone", "shop", "other")]);
        let kept = node(4.0, 8, &[("me", "shop", "fix")]);
        let o = order(
            &[
                ("a", "big", &big),
                ("b", "warm", &warm),
                ("c", "kept", &kept),
            ],
            need,
        );
        let names: Vec<&str> = o.iter().map(|x| x.0.as_str()).collect();
        assert_eq!(names, ["kept", "warm", "big"], "{o:?}");
        assert!(o[0].2.contains("keeps this lane's data"), "{o:?}");
        assert!(o[1].2.contains("the repository ran there"), "{o:?}");

        // Warm, but without room to spare (less than 1.5× the need): by
        // capacity like any other.
        let tight = node(1.2, 5, &[("me", "shop", "fix")]);
        let o = order(&[("a", "big", &big), ("c", "tight", &tight)], need);
        assert_eq!(o[0].0, "big", "{o:?}");
    }

    #[test]
    fn free_capacity_counts_cpu_and_memory_against_the_need() {
        let need = Amount {
            cpus: 2.0,
            memory: 2 * GIB,
        };
        // Much memory, little CPU: 1.25× the need in CPU.
        let ram = node(2.5, 64, &[]);
        let both = node(8.0, 8, &[]);
        let o = order(&[("a", "ram", &ram), ("b", "both", &both)], need);
        assert_eq!(o[0].0, "both", "{o:?}");
    }

    #[test]
    fn what_cannot_take_the_lane_says_why_and_comes_last() {
        let need = Amount {
            cpus: 1.0,
            memory: 4 * GIB,
        };
        let small = node(0.5, 2, &[("me", "shop", "fix")]);
        let mut full = node(8.0, 32, &[]);
        full.disk_free = Some(GIB);
        let mut off = node(8.0, 32, &[]);
        off.sharing = false;
        let ok = node(2.0, 8, &[]);
        let o = order(
            &[
                ("a", "small", &small),
                ("b", "full", &full),
                ("c", "off", &off),
                ("d", "ok", &ok),
            ],
            need,
        );
        assert_eq!((o[0].0.as_str(), o[0].1), ("ok", true), "{o:?}");
        assert!(o[1..].iter().all(|x| !x.1), "{o:?}");
        let why = |n: &str| o.iter().find(|x| x.0 == n).unwrap().2.clone();
        assert!(why("small").contains("offers 0.5 CPU"), "{o:?}");
        assert!(why("full").contains("disk free"), "{o:?}");
        assert!(why("off").contains("does not share"), "{o:?}");
    }

    #[test]
    fn offers_not_admitted_yet_hold_their_room() {
        let lease = |id: &str, node: &str, state: &str, gib: u64| {
            (
                id.to_string(),
                (
                    Lease {
                        id: id.into(),
                        node: node.into(),
                        member: "me".into(),
                        repo: "shop".into(),
                        lane: id.into(),
                        need: Amount {
                            cpus: 1.0,
                            memory: gib * GIB,
                        },
                        state: state.into(),
                        created: 0,
                        expires_in: None,
                        shared: None,
                    },
                    String::new(),
                    std::time::Instant::now(),
                ),
            )
        };
        let leases: HashMap<_, _> = [
            lease("a", "n1", "offered", 4),
            lease("b", "n1", "offered", 2),
            // Admitted: the node's report counts it already.
            lease("c", "n1", "running", 8),
            lease("d", "n2", "offered", 16),
        ]
        .into_iter()
        .collect();
        let held = offered_need(&leases, "n1");
        assert_eq!((held.cpus, held.memory), (2.0, 6 * GIB));
        let rep = less_offered(&node(4.0, 8, &[]), held);
        assert_eq!((rep.cpus, rep.memory), (2.0, 2 * GIB));
        // A second lane of 4 GiB no longer fits there.
        let o = order(
            &[("n1", "n1", &rep)],
            Amount {
                cpus: 1.0,
                memory: 4 * GIB,
            },
        );
        assert!(!o[0].1, "{o:?}");
        // More held than offered: nothing left, never less.
        let rep = less_offered(&node(1.0, 1, &[]), held);
        assert_eq!((rep.cpus, rep.memory), (0.0, 0));
        assert!(
            UpdateReport {
                at: 1,
                state: "cancelled".into(),
                target: None,
                error: None
            }
            .over()
        );
    }
}

#[cfg(test)]
mod share_tests {
    use super::*;

    /// Whoever a lane is shared with may view it (ADR-0040, amended
    /// 2026-10-20); nobody else may.
    #[test]
    fn a_lane_is_viewed_by_whom_it_is_shared_with() {
        let qa = vec!["team:qa".to_string()];
        let sh = Sharing::new(qa.clone(), Vec::new());
        assert!(sh.admits("bob", &qa));
        assert!(sh.admits("ann", &["x".into(), "team:qa".into()]));
        assert!(!sh.admits("eve", &[]));
        assert!(!Sharing::default().admits("bob", &qa));
        // Told as devstack 0.11 reads it: the same members may view it.
        let wire = serde_json::to_value(&sh).unwrap();
        assert_eq!(wire["view"], wire["with"]);
    }

    /// A sharing from devstack 0.11, with viewers of its own, still reads;
    /// they could use the lane, and may still.
    #[test]
    fn an_older_sharing_still_reads() {
        let old: Sharing =
            serde_json::from_str(r#"{"with":["team:dev"],"services":[],"view":["bob"]}"#).unwrap();
        assert!(old.admits("bob", &[]) && old.admits("ann", &["team:dev".into()]));
        let plain: Sharing = serde_json::from_str(r#"{"with":["*"],"services":[]}"#).unwrap();
        assert!(plain.admits("bob", &[]));
        let listed: SharedLane = serde_json::from_str(
            r#"{"lease":"l","repo":"shop","lane":"fix","owner":"o","owner_name":"o",
                "node":{"id":"n","name":"n","api":"https://n"},"services":[]}"#,
        )
        .unwrap();
        assert_eq!(listed.lane, "fix");
    }

    #[test]
    fn a_shared_service_is_a_web_page_only() {
        let ok: SharedService =
            serde_json::from_str(r#"{"name":"web","port":5173,"scheme":"HTTPS"}"#).unwrap();
        assert_eq!(ok.scheme, "https");
        let default: SharedService = serde_json::from_str(r#"{"name":"web","port":1}"#).unwrap();
        assert_eq!(default.scheme, "http");
        for bad in ["ms-msdt", "search-ms", "file", "javascript", ""] {
            let j = format!(r#"{{"name":"web","port":1,"scheme":"{bad}"}}"#);
            assert!(serde_json::from_str::<SharedService>(&j).is_err(), "{bad}");
        }
    }
}
