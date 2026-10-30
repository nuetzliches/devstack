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
//! - provisioned nodes (ADR-0051): `POST /v1/leases?wait=provision` may
//!   answer `202` with a machine on its way, `GET /v1/leases/{id}/wait` until
//!   it is offered; `POST /v1/enrol` `{name, csr, token}` (no certificate);
//!   `PUT /v1/me/provisioned` `{on}`; admins: `GET|POST
//!   /v1/admin/provisioning`, `POST /v1/admin/members/{id}/provision` `{on}`,
//!   `POST /v1/admin/nodes/{id}/confirm`, `POST /v1/admin/nodes/{id}/give-back`

use crate::pki::{self, Pair};
use crate::provision::{self, Confirm, Machine, MachineState, Per, Settings};
use crate::store::{self, AuditEntry, Member, Role, State, Store};
use crate::version::Release;
use axum::extract::{Path as UrlPath, State as AxState};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
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
    /// What a new lane can take of its disk (ADR-0049 point 6): free, and
    /// what it could free (kept lanes no lease uses), less what its
    /// running lanes are still expected to write. Absent from an older
    /// node: `disk_free` stands for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_room: Option<u64>,
    /// What its lanes took, each lease's run so far and those that ended
    /// since its last report (ADR-0049 point 2).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runs: Vec<crate::footprint::Run>,
}

impl Report {
    /// What a new lane can take of its disk, as far as it says.
    pub fn room(&self) -> Option<u64> {
        self.disk_room.or(self.disk_free)
    }
}

/// A lane a node has a slot for: whose, of which repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotRef {
    pub member: String,
    pub repo: String,
    pub lane: String,
}

/// Less free disk than this where a node keeps lanes: it takes none (a
/// lane's images, mirror and volumes need room). Beyond a lane's disk need
/// (ADR-0049), the margin that stays free.
pub const MIN_DISK_FREE: u64 = 5 << 30;

fn is_zero(v: &u64) -> bool {
    *v == 0
}

/// Bytes as GiB with one decimal (`12.5 GiB`), or MiB below one.
pub fn gib(b: u64) -> String {
    if b < 1 << 30 {
        format!("{} MiB", b >> 20)
    } else {
        format!("{:.1} GiB", b as f64 / f64::from(1u32 << 30))
    }
}

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
    /// The disk the lane needs there beyond what is there already
    /// (ADR-0049 point 6), and what it has for lanes; absent from an
    /// older coordinator.
    #[serde(default)]
    pub disk: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_room: Option<u64>,
    /// What the lane would hold there (ADR-0049 point 4), and where that
    /// comes from (its footprint, or the requester's estimate): the zone
    /// the dashboard shows on each node. Absent from an older coordinator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub need: Option<crate::share::Need>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub need_from: Option<String>,
    /// What the lane uses once up, beside its need (the peak, its build
    /// included): the most of its last runs' steady use. Shown, never
    /// admitted by; absent until a run measured it, and from an older
    /// coordinator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steady: Option<crate::share::Amount>,
}

/// Rank the nodes for a lane of `member`'s `repo`/`lane` needing `need`:
/// first the hard conditions (each node's own reason when it fails one),
/// then, with room to spare (half as much again as the need), a node that
/// keeps this lane's data, then one the repository ran on (its images and
/// builds are there), then the most free capacity for the need (the lower
/// of free memory and free CPU, each over the need). `nodes` are the
/// candidates already granted, online and admitted by their policy, with
/// their last report. The disk a node needs room for is what the lane
/// does not have there yet (ADR-0049 point 6), and GPUs it asks for are
/// GPUs the node offers.
pub fn rank(
    nodes: &[(&str, &str, &Report)],
    member: &str,
    repo: &str,
    lane: &str,
    need: crate::share::Need,
    requester: Option<&Release>,
) -> Vec<Placement> {
    rank_filling(
        nodes,
        &std::collections::HashSet::new(),
        member,
        repo,
        lane,
        need,
        requester,
    )
}

/// [`rank`], with `filled` the nodes a provisioner made (ADR-0051 point 4):
/// after the members' own nodes of the same tier, and among themselves the
/// one with the least free capacity that takes the lane first, so lanes
/// gather on few machines and the others empty.
pub fn rank_filling(
    nodes: &[(&str, &str, &Report)],
    filled: &std::collections::HashSet<String>,
    member: &str,
    repo: &str,
    lane: &str,
    need: crate::share::Need,
    requester: Option<&Release>,
) -> Vec<Placement> {
    let mut out: Vec<(u8, bool, f64, Placement)> = nodes
        .iter()
        .map(|(id, name, rep)| {
            let fill = filled.contains(*id);
            let kept = rep
                .slots
                .iter()
                .any(|s| s.member == member && s.repo == repo && s.lane == lane);
            let warm = rep.slots.iter().any(|s| s.repo == repo);
            let disk = need.disk_on(kept, warm);
            let room = rep.room();
            let gpus = need.gpus.filter(|g| g.count > 0).map(|g| {
                let fit = rep
                    .gpus
                    .iter()
                    .filter(|x| x.offered && x.memory >= g.memory)
                    .count();
                (g, fit)
            });
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
            } else if let Some(r) = room.filter(|r| *r < disk.saturating_add(MIN_DISK_FREE)) {
                Some(if disk == 0 {
                    format!(
                        "has {} for lanes on disk, less than {}",
                        gib(r),
                        gib(MIN_DISK_FREE)
                    )
                } else {
                    format!(
                        "has {} for lanes on disk; the lane needs {} there, and {} stay free",
                        gib(r),
                        gib(disk),
                        gib(MIN_DISK_FREE)
                    )
                })
            } else if let Some((g, fit)) = gpus.filter(|(g, fit)| (*fit as u32) < g.count) {
                Some(format!(
                    "offers {fit} GPU{} with {} or more, the lane asks for {}",
                    if fit == 1 { "" } else { "s" },
                    gib(g.memory),
                    g.count
                ))
            } else {
                None
            };
            let ratio = {
                let m = rep.memory as f64 / need.memory.max(1) as f64;
                let c = rep.cpus / need.cpus.max(0.01);
                m.min(c)
            };
            let roomy = rep.memory as f64 >= need.memory as f64 * 1.5
                && rep.cpus >= need.cpus * 1.5
                && room.is_none_or(|r| r as f64 >= disk as f64 * 1.5 + MIN_DISK_FREE as f64);
            let (tier, why) = match &refuse {
                Some(r) => (9, r.clone()),
                None if roomy && kept => {
                    (0, "keeps this lane's data (mirror, builds, volumes)".into())
                }
                None if roomy && warm => (
                    1,
                    "the repository ran there (its images and builds are there)".into(),
                ),
                None if fill => (
                    2,
                    format!(
                        "provisioned, filled before another is made: free capacity {:.1}× the lane's need",
                        ratio
                    ),
                ),
                None => (2, format!("free capacity: {:.1}× the lane's need", ratio)),
            };
            (
                tier,
                fill,
                ratio,
                Placement {
                    node: id.to_string(),
                    name: name.to_string(),
                    eligible: refuse.is_none(),
                    why,
                    cpus: rep.cpus,
                    memory: rep.memory,
                    disk,
                    disk_room: room,
                    need: Some(need),
                    need_from: None,
                    steady: None,
                },
            )
        })
        .collect();
    out.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then(a.1.cmp(&b.1))
            .then(if a.1 && b.1 {
                a.2.total_cmp(&b.2)
            } else {
                b.2.total_cmp(&a.2)
            })
            .then(a.3.name.cmp(&b.3.name))
    });
    out.into_iter().map(|(_, _, _, p)| p).collect()
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
    /// Their directories (mirrors, builds, caches) and the members' stores
    /// of what `prepare` makes; volumes not counted.
    pub bytes: u64,
    /// Of `bytes`, the members' stores of what `prepare` makes (ADR-0062).
    /// Older nodes do not say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_bytes: Option<u64>,
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
/// a point per minute for a day. Kept in `history.json` too, so a restart
/// loses a minute at most.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct History {
    #[serde(default)]
    raw: std::collections::VecDeque<Point>,
    #[serde(default)]
    minutes: std::collections::VecDeque<Point>,
}

const HOUR: u64 = 3600;
const DAY: u64 = 24 * HOUR;
/// Where the coordinator keeps its nodes' histories.
const HISTORY: &str = "history.json";
/// How often it writes them.
const HISTORY_EVERY: Duration = Duration::from_secs(60);

/// The histories `dir` kept, without what is too old by `now`; none when
/// the file is missing or unreadable.
pub fn load_history(dir: &Path, now: u64) -> HashMap<String, History> {
    let path = dir.join(HISTORY);
    let mut all: HashMap<String, History> = match std::fs::read(&path) {
        Ok(b) => serde_json::from_slice(&b).unwrap_or_else(|e| {
            eprintln!(
                "coordinator: {}: {e}; the history starts again",
                path.display()
            );
            HashMap::new()
        }),
        Err(_) => HashMap::new(),
    };
    for h in all.values_mut() {
        h.raw.retain(|x| x.at + HOUR >= now);
        h.minutes.retain(|x| x.at + DAY >= now);
    }
    all.retain(|_, h| !h.raw.is_empty() || !h.minutes.is_empty());
    all
}

fn save_history(dir: &Path, all: &HashMap<String, History>) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(all).expect("histories serialise");
    provision::write_whole(&dir.join(HISTORY), &bytes)
}

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
    pub need: Option<crate::share::Need>,
}

/// `POST /v1/leases`: a requester asks for a lane on `node`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeaseRequest {
    /// The node's member id or name (`--on`).
    pub node: String,
    /// The repository, as the requester names it (its stack id).
    pub repo: String,
    pub lane: String,
    /// What the lane is expected to need: the requester's estimate, or a
    /// default (ADR-0049 point 4); what the lane's repository took in the
    /// pool comes first.
    #[serde(default)]
    pub need: crate::share::Need,
    /// Where `need` comes from, as the requester says it ("estimated from
    /// the main tree"); absent from older requesters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub need_from: Option<String>,
    /// What the stack file declares (`[lanes.need]`, the profile's): a
    /// floor on each part of the need (ADR-0049 point 5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub floor: Option<crate::share::Need>,
    /// The profile the lane runs with (ADR-0041): its footprint is its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// The requester's devstack: the coordinator places the lane on a
    /// node with the same pool protocol only. Absent from older requesters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub devstack: Option<Release>,
    /// A warm lease (ADR-0065 point 5): the lane's container runs only the
    /// `prepare` its member's store lacks, no infrastructure, then ends.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub warm: bool,
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
    /// The need the lease holds, and where it comes from (ADR-0049 point
    /// 10); absent from an older coordinator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub need: Option<crate::share::Need>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub need_from: Option<String>,
    /// The node is one a provisioner made (ADR-0051 point 9): whoever runs
    /// the provisioner can read the lane there.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub provisioned: bool,
}

/// A lease that waits for a machine a provisioner makes (ADR-0051 point
/// 5): the answer `202` to `POST /v1/leases?wait=provision`, and to `GET
/// /v1/leases/{id}/wait` until the lease is offered.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provisioning {
    /// The lease.
    pub id: String,
    /// `provisioning`
    pub state: String,
    /// Seconds waited so far, and at most.
    pub waited: u64,
    pub limit: u64,
    /// What it waits for now: the machine, its enrolment, an admin.
    pub why: String,
    /// The provider's id of the machine, once it is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
}

/// What a lease request that may wait gets: an offer, or a machine on its
/// way.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Leasing {
    Offer(LeaseOffer),
    Provisioning(Provisioning),
}

/// `POST /v1/enrol`: a machine a provisioner made joins with its token.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrolRequest {
    pub name: String,
    pub csr: String,
    pub token: String,
}

/// `PUT /v1/me/provisioned`, `POST /v1/admin/members/{id}/provision`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnRequest {
    pub on: bool,
}

/// `GET /v1/admin/provisioning`: whether the coordinator has a
/// provisioner, the pool's settings, and the machines it asked for.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvisioningView {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provisioner: Option<String>,
    pub settings: Settings,
    pub machines: Vec<Machine>,
}

/// A provisioned node as `/v1/nodes` shows it (ADR-0051 point 9).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProvisionedView {
    pub machine: String,
    pub address: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<crate::share::Amount>,
    /// When it enrolled (unix seconds).
    pub since: u64,
    /// Seconds until it is given back, while it has no lease.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_left: Option<u64>,
    /// It admits no lease until an admin confirms it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unconfirmed: bool,
    /// Its certificate's SHA-256: the key it enrolled with, what an admin
    /// compares before confirming it (ADR-0051 point 6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// The repository it runs lanes of, when the pool provisions per
    /// repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
}

/// A lease as the coordinator keeps it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lease {
    pub id: String,
    pub node: String,
    pub member: String,
    pub repo: String,
    pub lane: String,
    pub need: crate::share::Need,
    /// The disk it needs on its node beyond what is there already
    /// (ADR-0049 point 6): held there while it is offered.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub disk: u64,
    /// `offered` (created, not yet admitted by the node) or `running`.
    pub state: String,
    /// Unix seconds.
    pub created: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in: Option<u64>,
    /// Whom its owner shares the lane with, and which services (ADR-0035).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared: Option<Sharing>,
    /// A warm lease (ADR-0065 point 5): the lane's container runs only the
    /// `prepare` its member's store lacks, no infrastructure, then ends.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub warm: bool,
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
    /// A node a provisioner made (ADR-0051 point 9).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provisioned: Option<ProvisionedView>,
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
    /// It may cause provisioning (ADR-0051 point 3).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub provision: bool,
    /// It keeps its lanes off provisioned nodes (ADR-0051 point 10).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub no_provisioned: bool,
    /// A node a provisioner made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provisioned: Option<store::Provisioned>,
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
            provision: m.provision,
            no_provisioned: m.no_provisioned,
            provisioned: m.provisioned.clone(),
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

/// What leases offered for `node` and not admitted there yet hold: the
/// node's last report does not count them. Their disk is what each needs
/// there (`Lease::disk`).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Held {
    cpus: f64,
    memory: u64,
    disk: u64,
}

fn offered_need(leases: &HashMap<String, (Lease, String, std::time::Instant)>, node: &str) -> Held {
    held_by(leases, node, |l, at| {
        l.state == "offered" && at.elapsed() < OFFER_HOLDS
    })
}

/// What the leases on `node` that `which` picks need there together.
fn held_by(
    leases: &HashMap<String, (Lease, String, std::time::Instant)>,
    node: &str,
    which: impl Fn(&Lease, &std::time::Instant) -> bool,
) -> Held {
    leases
        .values()
        .filter(|(l, _, at)| l.node == node && which(l, at))
        .fold(Held::default(), |a, (l, _, _)| Held {
            cpus: a.cpus + l.need.cpus,
            memory: a.memory + l.need.memory,
            disk: a.disk + l.disk,
        })
}

/// A node's report as a lane that holds leases there sees it: what its own
/// offered leases hold is not held against it, and what its running ones
/// take is room it has (the node counts them in its report: their need
/// while they settle, what they use after). Without this, a lane starting
/// on a node would read that the node cannot take it, its own lease taking
/// the room.
fn with_own(
    rep: &Report,
    leases: &HashMap<String, (Lease, String, std::time::Instant)>,
    node: &str,
    own: impl Fn(&Lease) -> bool,
) -> Report {
    let others = held_by(leases, node, |l, at| {
        l.state == "offered" && at.elapsed() < OFFER_HOLDS && !own(l)
    });
    let mine = held_by(leases, node, |l, _| l.state == "running" && own(l));
    let rep = less_offered(rep, others);
    Report {
        cpus: rep.cpus + mine.cpus,
        memory: rep.memory.saturating_add(mine.memory),
        ..rep
    }
}

/// A node's report with what offered leases hold taken off its offer.
fn less_offered(rep: &Report, held: Held) -> Report {
    Report {
        cpus: (rep.cpus - held.cpus).max(0.0),
        memory: rep.memory.saturating_sub(held.memory),
        disk_room: rep.room().map(|r| r.saturating_sub(held.disk)),
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
    /// What each repository's lanes took where they ran (ADR-0049).
    footprints: crate::footprint::Footprints,
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
    /// The provisioner, if the coordinator has one (ADR-0051).
    provisioner: Option<provision::Provisioner>,
    /// The machines it asked for, as `machines.json` keeps them.
    machines: Mutex<Vec<Machine>>,
    /// Leases that wait for a machine, by lease id.
    waits: Mutex<HashMap<String, Wait>>,
    /// Leases on provisioned nodes told to the provisioner as begun, by
    /// lease id: the machine's request and id, and the lease.
    begun: Mutex<HashMap<String, (String, String, provision::LeaseEvent)>>,
    /// Where members reach the coordinator (`https://host:port`), and its
    /// CA's fingerprint: what an enrolment names.
    public_url: String,
    fingerprint: String,
    /// One change of the pool's lists at a time: its MCP upstreams
    /// (ADR-0069 option B) and its skill sets (ADR-0074).
    upstreams: Mutex<()>,
    /// The skill sets to fetch again, and whether a fetch runs (ADR-0074).
    skill_fetch: Mutex<SkillFetch>,
    /// The version of each part of the pool its members read (ADR-0073).
    versions: tokio::sync::watch::Sender<crate::events::Versions>,
    /// Count the versions again: something may have changed.
    recount: Arc<tokio::sync::Notify>,
}

/// A lease that waits for a machine (ADR-0051 point 5).
#[derive(Debug, Clone)]
struct Wait {
    member: String,
    /// The request, its need settled.
    request: LeaseRequest,
    need_from: String,
    /// The machine's request id.
    machine: String,
    token: String,
    started: std::time::Instant,
    limit: Duration,
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
        .route("/v1/enrol", post(enrol))
        .route("/v1/me/provisioned", axum::routing::put(me_provisioned))
        .route("/v1/leases/{id}/wait", get(lease_wait))
        .route(
            "/v1/admin/provisioning",
            get(admin_provisioning).post(admin_provisioning_set),
        )
        .route("/v1/admin/members/{id}/provision", post(admin_provision))
        .route("/v1/admin/nodes/{id}/confirm", post(admin_confirm))
        .route("/v1/admin/nodes/{id}/give-back", post(admin_give_back))
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
        .route("/v1/upstreams", get(upstreams_list))
        .route("/v1/admin/upstreams", post(admin_upstream_put))
        .route(
            "/v1/admin/upstreams/{name}",
            axum::routing::delete(admin_upstream_remove),
        )
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
        .route("/v1/events", get(events))
        .route("/v1/skills", get(skills_list))
        .route("/v1/skills/refresh", post(skills_refresh_signed))
        .route("/v1/skills/{set}/{file}", get(skill_archive))
        .route("/v1/admin/skills", get(admin_skills).post(admin_skill_put))
        .route("/v1/admin/skills/refresh", post(admin_skills_refresh))
        .route(
            "/v1/admin/skills/{name}",
            axum::routing::delete(admin_skill_remove),
        )
        .layer(axum::middleware::from_fn_with_state(
            shared.clone(),
            recount_after,
        ))
        .with_state(shared)
}

async fn join(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    Json(r): Json<JoinRequest>,
) -> ApiResult<Json<JoinAnswer>> {
    // Anyone may ask: by address, a few times a minute; a request of
    // reasonable size; a bounded number waiting.
    throttle(&s, &peer)?;
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

/// A few joins (and enrolments) a minute per address.
fn throttle(s: &Inner, peer: &Peer) -> ApiResult<()> {
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
    Ok(())
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
    headers: axum::http::HeaderMap,
) -> ApiResult<Response> {
    member(&s, &peer)?;
    let ms = s.store.list().map_err(internal)?;
    let views: Vec<MemberView> = ms.iter().map(MemberView::from).collect();
    Ok(etagged(&headers, &views))
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
                        disk: 0,
                        state: "running".into(),
                        created: store::now(),
                        expires_in: Some(x.expires_in),
                        shared: None,
                        warm: false,
                    },
                    // Admitted already: no node asks for it by token again.
                    random_hex(24),
                    std::time::Instant::now(),
                ),
            );
        }
    }
    // What its lanes took (ADR-0049 point 2): runs of leases on this node
    // only, before those that ended are let go below.
    if m.node && !r.runs.is_empty() {
        let ours: HashMap<String, String> = s
            .leases
            .lock()
            .unwrap()
            .values()
            .filter(|(l, _, _)| l.node == m.id)
            .map(|(l, _, _)| (l.id.clone(), l.repo.clone()))
            .collect();
        let now = store::now();
        let mut changed = false;
        for run in r.runs.iter().filter(|x| ours.get(&x.id) == Some(&x.repo)) {
            if run.profile.as_ref().is_some_and(|p| p.len() > 64) {
                continue;
            }
            changed |= s.footprints.record(run, now);
        }
        if changed && let Err(e) = s.footprints.save() {
            eprintln!("coordinator: footprints.json: {e}");
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

/// What `/v1/nodes` says of a provisioned node.
fn provisioned_view(m: &Member, machines: &[Machine], s: &Settings) -> Option<ProvisionedView> {
    let p = m.provisioned.as_ref()?;
    let machine = machines.iter().find(|x| x.request == p.request);
    Some(ProvisionedView {
        machine: p.machine.clone(),
        address: p.address.clone(),
        size: machine.and_then(|x| x.size),
        since: m.requested_at,
        idle_left: machine
            .and_then(|x| x.idle_since)
            .map(|since| (s.idle_minutes * 60).saturating_sub(store::now().saturating_sub(since))),
        unconfirmed: s.confirm == Confirm::Admin && !p.confirmed,
        key: m.cert_fp.clone(),
        repo: machine.and_then(|x| x.repo.clone()),
    })
}

/// Whether a provisioned node `m` may take a lane of `me` now: the member
/// does not keep its lanes off them, and an admin confirmed it where the
/// pool asks for that.
fn provisioned_admits(m: &Member, me: &Member, s: &Settings) -> std::result::Result<(), String> {
    let Some(p) = &m.provisioned else {
        return Ok(());
    };
    if me.no_provisioned {
        return Err(format!(
            "{} is a provisioned node, and you keep your lanes off them (`devstack pool provisioned on`)",
            m.name
        ));
    }
    if s.confirm == Confirm::Admin && !p.confirmed {
        return Err(format!(
            "{} waits for an admin to confirm it (`devstack node confirm {}`)",
            m.name, m.id
        ));
    }
    Ok(())
}

async fn nodes(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    headers: axum::http::HeaderMap,
) -> ApiResult<Response> {
    member(&s, &peer)?;
    Ok(etagged(&headers, &node_views(&s)?))
}

/// Every approved member as a node: its last report, whether it is online.
fn node_views(s: &Inner) -> ApiResult<Vec<NodeView>> {
    let ms = s.store.list().map_err(internal)?;
    let settings = Settings::load(&s.dir);
    let machines = s.machines.lock().unwrap().clone();
    let reports = s.reports.lock().unwrap();
    Ok(ms
        .iter()
        .filter(|m| m.state == State::Approved)
        .map(|m| {
            let r = reports.get(&m.id);
            let age = r.map(|(at, _)| at.elapsed());
            NodeView {
                provisioned: provisioned_view(m, &machines, &settings),
                id: m.id.clone(),
                name: m.name.clone(),
                online: age.is_some_and(|a| a < ONLINE_FOR),
                seen: age.map(|a| a.as_secs()),
                report: r.map(|(_, r)| r.clone()),
                paused: m.paused,
                kind: kind_of(r.map(|(_, r)| r)).into(),
            }
        })
        .collect())
}

/// `body` as JSON with an `ETag` of its content, or `304` when the caller
/// has that already (`If-None-Match`).
fn etagged(headers: &axum::http::HeaderMap, body: &impl Serialize) -> Response {
    let tag = format!("\"{}\"", crate::events::version(body));
    let has = headers
        .get(axum::http::header::IF_NONE_MATCH)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|h| h.split(',').any(|t| t.trim() == tag));
    let mut r = if has {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        Json(body).into_response()
    };
    if let Ok(v) = axum::http::HeaderValue::from_str(&tag) {
        r.headers_mut().insert(axum::http::header::ETAG, v);
    }
    r
}

/// The version of each part of the pool (ADR-0073), as every member reads
/// it: what changes with time alone (how long ago a node reported, when a
/// lease expires) left out, whether a node is online kept.
fn versions(s: &Inner) -> crate::events::Versions {
    use crate::events::{self, version};
    let mut v = crate::events::Versions::new();
    if let Ok(ms) = s.store.list() {
        let views: Vec<MemberView> = ms.iter().map(MemberView::from).collect();
        v.insert(events::MEMBERS.into(), version(&views));
    }
    if let Ok(mut nodes) = node_views(s) {
        for n in &mut nodes {
            n.seen = None;
        }
        v.insert(events::NODES.into(), version(&nodes));
    }
    let mut leases: Vec<Lease> = s
        .leases
        .lock()
        .unwrap()
        .values()
        .map(|(l, _, _)| Lease {
            expires_in: None,
            ..l.clone()
        })
        .collect();
    leases.sort_by(|a, b| (a.created, &a.id).cmp(&(b.created, &b.id)));
    v.insert(events::LEASES.into(), version(&leases));
    v.insert(
        events::UPSTREAMS.into(),
        version(&crate::upstreams::load(&s.dir)),
    );
    v.insert(
        events::TELEMETRY.into(),
        version(&(s.collector.is_some(), crate::telemetry::collects())),
    );
    // Every set; each member's stream tells the version of its own.
    let sets: Vec<crate::skills::SetView> = crate::skills::admin_view(&s.dir).sets;
    v.insert(events::SKILLS.into(), version(&sets));
    v
}

/// Count the versions again after each change a request made
/// ([`recount_after`]), and every few seconds for what changes with time
/// alone: a node that stopped reporting goes offline, an offer expires.
/// Ends with the coordinator.
async fn count_versions(s: std::sync::Weak<Inner>, recount: Arc<tokio::sync::Notify>) {
    const EVERY: Duration = Duration::from_secs(5);
    loop {
        let _ = tokio::time::timeout(EVERY, recount.notified()).await;
        let Some(strong) = s.upgrade() else { return };
        let Ok(now) = tokio::task::spawn_blocking(move || {
            let v = versions(&strong);
            (strong, v)
        })
        .await
        else {
            return;
        };
        let (strong, v) = now;
        strong.versions.send_if_modified(|old| {
            if *old == v {
                false
            } else {
                *old = v;
                true
            }
        });
    }
}

/// After a request that may have changed something (anything but `GET`
/// and `HEAD`), have the versions counted again.
async fn recount_after(
    AxState(s): AxState<Shared>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let changes = !matches!(
        *req.method(),
        axum::http::Method::GET | axum::http::Method::HEAD
    );
    let res = next.run(req).await;
    if changes {
        s.recount.notify_one();
    }
    res
}

/// `GET /v1/events` (members, ADR-0073): which parts of the pool changed,
/// all of them first; changes within [`COALESCE`](crate::events::COALESCE)
/// in one event, a comment on a quiet stream every
/// [`PING_EVERY`](crate::events::PING_EVERY). It ends when the member is
/// removed (or its certificate no longer approved).
async fn events(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
) -> ApiResult<Response> {
    use crate::events::{COALESCE, Event, PING_EVERY, frame};
    member(&s, &peer)?;
    let (mut tx, body) = http_body_util::channel::Channel::<bytes::Bytes, std::io::Error>::new(8);
    let mut rx = s.versions.subscribe();
    tokio::spawn(async move {
        let mut sent = crate::events::Versions::new();
        loop {
            // Removed since: the stream ends, and with it what the member
            // follows of the pool.
            let Ok(me) = member(&s, &peer) else {
                return;
            };
            let mut now = rx.borrow_and_update().clone();
            // The sets given to this member: another's change is not told.
            if now.contains_key(crate::events::SKILLS) {
                let mine = crate::skills::for_member(&s.dir, &me.id, &me.tags);
                now.insert(crate::events::SKILLS.into(), crate::events::version(&mine));
            }
            let topics: crate::events::Versions = now
                .iter()
                .filter(|(t, v)| sent.get(*t) != Some(*v))
                .map(|(t, v)| (t.clone(), v.clone()))
                .collect();
            if !topics.is_empty() {
                if tx.send_data(frame(&Event { topics })).await.is_err() {
                    return;
                }
                sent = now;
            }
            loop {
                tokio::select! {
                    r = rx.changed() => {
                        if r.is_err() {
                            return;
                        }
                        break;
                    }
                    () = tokio::time::sleep(PING_EVERY) => {
                        if tx.send_data(bytes::Bytes::from_static(b": ping\n\n")).await.is_err() {
                            return;
                        }
                    }
                }
            }
            tokio::time::sleep(COALESCE).await;
        }
    });
    Ok((
        [
            (axum::http::header::CONTENT_TYPE, "text/event-stream"),
            (axum::http::header::CACHE_CONTROL, "no-cache"),
        ],
        axum::body::Body::new(body),
    )
        .into_response())
}

/// The need a lease of `r` holds (ADR-0049 point 4), and where it comes
/// from: what the repository's lanes with its profile took in the pool,
/// else the requester's estimate; each part at least what the stack file
/// declares, and at least the least (ADR-0033 point 4).
fn need_of(s: &Inner, r: &LeaseRequest) -> (crate::share::Need, String) {
    let (base, from) = match s.footprints.get(&r.repo, r.profile.as_deref()) {
        Some((n, runs)) => (
            n,
            format!(
                "measured: the most of its last {runs} run{} in the pool",
                if runs == 1 { "" } else { "s" }
            ),
        ),
        None => (
            r.need,
            r.need_from
                .clone()
                .unwrap_or_else(|| "as the requester expects it".into()),
        ),
    };
    let (need, raised) = match r.floor {
        Some(f) => base.at_least(f),
        None => (base, false),
    };
    let from = if raised {
        format!("{from}, raised to what its stack file declares")
    } else {
        from
    };
    (crate::policy::floor_need(need), from)
}

/// The nodes that may take `r` for `me` at all (granted, online, not
/// paused, not the requester, admitted by their policy), ranked. `own`:
/// the room the lane's own leases hold counts as its (what the dashboard
/// shows while it starts or runs), not when it asks for another lease.
/// Every node of `order` refuses a lane that cannot run on a node of the
/// pool at all, saying `why` (ADR-0028): the requester knows it, the
/// coordinator does not.
pub fn not_in_pool(order: &mut [Placement], why: &str) {
    for p in order {
        p.eligible = false;
        p.why = why.to_string();
    }
}

fn placement_for(
    s: &Shared,
    ms: &[Member],
    me: &Member,
    r: &LeaseRequest,
    own: bool,
) -> Vec<Placement> {
    let leases = s.leases.lock().unwrap().clone();
    let mine = |l: &Lease| l.member == me.id && l.repo == r.repo && l.lane == r.lane;
    // What each node offers less what offers not admitted yet hold there.
    let reports: HashMap<String, (std::time::Instant, Report)> = s
        .reports
        .lock()
        .unwrap()
        .iter()
        .map(|(id, (at, rep))| {
            let rep = if own {
                with_own(rep, &leases, id, mine)
            } else {
                less_offered(rep, offered_need(&leases, id))
            };
            (id.clone(), (*at, rep))
        })
        .collect();
    let settings = Settings::load(&s.dir);
    let nodes: Vec<(&str, &str, &Report)> = ms
        .iter()
        .filter(|m| m.state == State::Approved && m.node && !m.paused && m.id != me.id)
        .filter(|m| provisioned_admits(m, me, &settings).is_ok())
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
    let filled: std::collections::HashSet<String> = ms
        .iter()
        .filter(|m| m.provisioned.is_some())
        .map(|m| m.id.clone())
        .collect();
    let (need, from) = need_of(s, r);
    let steady = s.footprints.steady(&r.repo, r.profile.as_deref());
    let mut out = rank_filling(
        &nodes,
        &filled,
        &me.id,
        &r.repo,
        &r.lane,
        need,
        r.devstack.as_ref(),
    );
    for p in &mut out {
        p.need_from = Some(from.clone());
        p.steady = steady;
        // What offers for other lanes hold there is not in the node's own
        // figures the dashboard shows: say so where it decides.
        if !p.eligible && (p.cpus < need.cpus || p.memory < need.memory) {
            let held = held_by(&leases, &p.node, |l, at| {
                l.state == "offered" && at.elapsed() < OFFER_HOLDS && !(own && mine(l))
            });
            p.why = with_held(&p.why, held);
        }
    }
    out
}

/// A refusal for want of CPU or memory, with what offers for other lanes
/// hold of the node's offer, if they hold any.
fn with_held(why: &str, held: Held) -> String {
    if held.cpus <= 0.0 && held.memory == 0 {
        return why.to_string();
    }
    format!(
        "{why}; {:.1} CPU and {} MiB more are held for a lane starting there",
        held.cpus,
        held.memory >> 20
    )
}

/// `GET /v1/placement?repo=&lane=&cpus=&memory=&disk=&profile=`: where a
/// lane of the caller's would go, best first, with why (what the dashboard
/// shows beside the nodes to start it on). `disk` is the requester's
/// estimate of the lane's own part.
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
        need: crate::share::Need {
            cpus: q.cpus.unwrap_or(0.0),
            memory: q.memory.unwrap_or(0),
            disk_lane: q.disk.unwrap_or(0),
            ..Default::default()
        },
        need_from: None,
        floor: None,
        profile: q.profile.filter(|p| !p.is_empty()),
        devstack: Some(Release::this()),
        warm: false,
    };
    Ok(Json(placement_for(&s, &ms, &me, &r, true)))
}

#[derive(Debug, Deserialize)]
struct PlacementQuery {
    repo: String,
    lane: String,
    #[serde(default)]
    cpus: Option<f64>,
    #[serde(default)]
    memory: Option<u64>,
    #[serde(default)]
    disk: Option<u64>,
    #[serde(default)]
    profile: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct LeaseQuery {
    /// `provision`: the requester waits for a machine when no node can take
    /// the lane (ADR-0051 point 5); an older requester does not ask, and is
    /// refused as before.
    #[serde(default)]
    wait: Option<String>,
}

async fn lease_create(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    axum::extract::Query(q): axum::extract::Query<LeaseQuery>,
    Json(mut r): Json<LeaseRequest>,
) -> ApiResult<Response> {
    let me = member(&s, &peer)?;
    crate::policy::valid_lease_names(&r.repo, &r.lane).map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    // What the lane's repository took in the pool, at least what its stack
    // file declares, and at least the least, whatever the requester claims
    // (ADR-0049 point 4, ADR-0033 point 4).
    if r.profile.as_ref().is_some_and(|p| p.len() > 64) {
        return Err((StatusCode::BAD_REQUEST, "not a profile's name".into()));
    }
    let (need, need_from) = need_of(&s, &r);
    r.need = need;
    let offered = s
        .leases
        .lock()
        .unwrap()
        .values()
        .filter(|(l, _, at)| {
            l.member == me.id
                && (l.state == "offered" && at.elapsed() < OFFER_FOR || l.state == "provisioning")
        })
        .count();
    if offered >= MAX_OFFERS {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            format!("{offered} leases of yours wait to be admitted already"),
        ));
    }
    let ms = s.store.list().map_err(internal)?;
    // No node named (`pool`): the coordinator places the lane (ADR-0031
    // point 4, in the order of ADR-0026 point 3): of the nodes an admin
    // granted that are online, run lanes, share, are not paused and offer
    // enough, not the requester itself, warm ones first (`rank`).
    let mut why = None;
    if r.node.is_empty() || r.node.eq_ignore_ascii_case("pool") {
        let ranked = placement_for(&s, &ms, &me, &r, false);
        let best = ranked.into_iter().find(|p| p.eligible).map(|p| {
            why = Some(p.why);
            p.node
        });
        let Some(id) = best else {
            let refused = format!(
                "no node of the pool can take the lane now ({:.1} CPU, {} MiB, {} of disk; {need_from}): none online, sharing, with that much free and a devstack that works with the requester's",
                r.need.cpus,
                r.need.memory >> 20,
                gib(r.need.disk())
            );
            // A machine a provisioner makes, for a requester that waits
            // for one (ADR-0051 point 3); a warm lease never makes one.
            if q.wait.as_deref() == Some("provision") && !r.warm && s.provisioner.is_some() {
                return provision_for(&s, &me, r, need_from, refused).await;
            }
            return Err((StatusCode::CONFLICT, refused));
        };
        r.node = id;
    }
    let offer = offer(
        &s,
        &ms,
        &me,
        r,
        need_from,
        why,
        (random_hex(8), random_hex(24)),
    )?;
    Ok(Json(offer).into_response())
}

/// Offer `r` (its need settled) on the node it names, as lease `id` with
/// `token`: checked against the node's last report and what other offers
/// hold there, and recorded in the same step.
fn offer(
    s: &Shared,
    ms: &[Member],
    me: &Member,
    mut r: LeaseRequest,
    need_from: String,
    why: Option<String>,
    (id, token): (String, String),
) -> ApiResult<LeaseOffer> {
    let pick = |f: &dyn Fn(&Member) -> bool| -> Vec<&Member> {
        ms.iter()
            .filter(|m| m.state == State::Approved && f(m))
            .collect()
    };
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
    if let Err(why) = provisioned_admits(&node, me, &Settings::load(&s.dir)) {
        return Err((StatusCode::FORBIDDEN, why));
    }
    let (api, rep, node_release) = {
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
        (api, rep.clone(), rep.devstack.clone())
    };
    // Checked and offered in one step, less what other offers for the node
    // hold until it admits them: two requests between two reports cannot
    // both be offered the same room (ADR-0026 point 4).
    {
        let mut leases = s.leases.lock().unwrap();
        let rep = less_offered(&rep, offered_need(&leases, &node.id));
        // The same conditions as placement (`rank`), on this node alone.
        let [p] = rank(
            &[(&node.id, &node.name, &rep)],
            &me.id,
            &r.repo,
            &r.lane,
            r.need,
            r.devstack.as_ref(),
        )
        .try_into()
        .expect("one node ranked");
        if !p.eligible {
            return Err((
                StatusCode::CONFLICT,
                format!("{} {} (the lane's need: {need_from})", node.name, p.why),
            ));
        }
        let lease = Lease {
            id: id.clone(),
            node: node.id.clone(),
            member: me.id.clone(),
            repo: std::mem::take(&mut r.repo),
            lane: std::mem::take(&mut r.lane),
            need: r.need,
            disk: p.disk,
            state: "offered".into(),
            created: store::now(),
            expires_in: None,
            shared: None,
            warm: r.warm,
        };
        leases.insert(
            id.clone(),
            (lease, token.clone(), std::time::Instant::now()),
        );
    }
    Ok(LeaseOffer {
        why,
        id,
        token,
        provisioned: node.provisioned.is_some(),
        node: NodeRef {
            id: node.id,
            name: node.name,
            api,
            devstack: node_release,
        },
        coordinator: Some(Release::this()),
        need: Some(r.need),
        need_from: Some(need_from),
    })
}

/// No node can take `r`: have a machine made for it, or join one on its
/// way that holds it too, and answer `202` with the lease that waits for
/// it (ADR-0051 points 3 and 5). Refused, with `refused` and why, when the
/// member may not cause provisioning, keeps off provisioned nodes, a cap
/// holds, or the provisioner refuses.
async fn provision_for(
    s: &Shared,
    me: &Member,
    r: LeaseRequest,
    need_from: String,
    refused: String,
) -> ApiResult<Response> {
    let no = |why: String| {
        Err((
            StatusCode::CONFLICT,
            format!("{refused}; no node is provisioned for it: {why}"),
        ))
    };
    let Some(provisioner) = s.provisioner.clone() else {
        return Err((StatusCode::CONFLICT, refused));
    };
    if !me.provision {
        return no(format!(
            "{} ({}) may not cause provisioning (an admin: `devstack node members provision {} on`)",
            me.name, me.id, me.id
        ));
    }
    if me.no_provisioned {
        return no(
            "you keep your lanes off provisioned nodes (`devstack pool provisioned on`)".into(),
        );
    }
    let settings = Settings::load(&s.dir);
    let repo = (settings.per == Per::Repo).then(|| r.repo.clone());
    // A machine on its way that holds this lane too, with the lanes that
    // wait for it already: wait for it rather than make another.
    let joined = {
        let machines = s.machines.lock().unwrap();
        let waits = s.waits.lock().unwrap();
        machines
            .iter()
            .filter(|m| m.state == MachineState::Provisioning && m.repo == repo)
            .find(|m| {
                let Some(size) = m.size else { return false };
                let (cpus, memory) = waits.values().filter(|w| w.machine == m.request).fold(
                    (r.need.cpus, r.need.memory),
                    |(c, b), w| {
                        (
                            c + w.request.need.cpus,
                            b.saturating_add(w.request.need.memory),
                        )
                    },
                );
                size.cpus >= cpus && size.memory >= memory
            })
            .map(|m| (m.request.clone(), m.deadline))
    };
    let (request, deadline) = match joined {
        Some(j) => j,
        None => {
            let request = random_hex(8);
            let token = random_hex(24);
            let created = store::now();
            {
                let mut machines = s.machines.lock().unwrap();
                if let Err(e) = provision::within_caps(&settings, &machines, &r.need) {
                    return no(e);
                }
                machines.push(Machine {
                    request: request.clone(),
                    machine: String::new(),
                    address: String::new(),
                    size: None,
                    repo: repo.clone(),
                    state: MachineState::Provisioning,
                    requester: me.id.clone(),
                    created,
                    deadline: created + provision::MAX_WAIT,
                    token_hash: provision::token_hash(&token),
                    member: None,
                    idle_since: None,
                    alarm: None,
                    error: None,
                });
                save_machines(s, &machines);
            }
            let ask = provision::MachineRequest {
                request: request.clone(),
                need: r.need,
                repo: repo.clone(),
                devstack: Release::this(),
                enrol: provision::Enrolment {
                    command: format!(
                        "devstack join {} --fingerprint {} --enrol {token}",
                        s.public_url, s.fingerprint
                    ),
                    coordinator: s.public_url.clone(),
                    fingerprint: s.fingerprint.clone(),
                    token,
                    expires_in: provision::MAX_WAIT,
                },
            };
            let answer = tokio::task::spawn_blocking(move || provisioner.request(&ask))
                .await
                .map_err(internal)?;
            let mut machines = s.machines.lock().unwrap();
            let Some(m) = machines.iter_mut().find(|m| m.request == request) else {
                return no("the machine was given back meanwhile".into());
            };
            let address = match &answer {
                Ok(a) => a
                    .address
                    .as_deref()
                    .and_then(|x| x.parse::<std::net::IpAddr>().ok()),
                Err(_) => None,
            };
            let why = match (&answer, address) {
                (Err(e), _) => Some(e.clone()),
                (Ok(a), _) if !a.accepted => Some(format!(
                    "the provisioner refused: {}",
                    a.reason.as_deref().unwrap_or("it gave no reason")
                )),
                (Ok(_), None) => Some(
                    "the provisioner named no IP address for the machine, so it could not enrol"
                        .into(),
                ),
                _ => None,
            };
            let a = answer.unwrap_or_default();
            m.machine = a.machine.clone().unwrap_or_default();
            if let Some(why) = why {
                // Made, but of no use: given back by `tend`.
                if m.machine.is_empty() {
                    machines.retain(|m| m.request != request);
                } else {
                    m.state = MachineState::Returning;
                }
                save_machines(s, &machines);
                return no(why);
            }
            m.address = address.expect("checked").to_canonical().to_string();
            m.size = a.size;
            m.deadline = created + provision::wait_for(a.estimate);
            let deadline = m.deadline;
            save_machines(s, &machines);
            (request, deadline)
        }
    };
    let id = random_hex(8);
    let limit = Duration::from_secs(deadline.saturating_sub(store::now()));
    {
        let mut leases = s.leases.lock().unwrap();
        leases.insert(
            id.clone(),
            (
                Lease {
                    id: id.clone(),
                    node: String::new(),
                    member: me.id.clone(),
                    repo: r.repo.clone(),
                    lane: r.lane.clone(),
                    need: r.need,
                    disk: 0,
                    state: "provisioning".into(),
                    created: store::now(),
                    expires_in: None,
                    shared: None,
                    warm: false,
                },
                random_hex(24),
                std::time::Instant::now(),
            ),
        );
    }
    s.waits.lock().unwrap().insert(
        id.clone(),
        Wait {
            member: me.id.clone(),
            request: r,
            need_from,
            machine: request,
            token: random_hex(24),
            started: std::time::Instant::now(),
            limit,
        },
    );
    let p = waiting(s, &id).expect("just inserted");
    Ok((StatusCode::ACCEPTED, Json(Leasing::Provisioning(p))).into_response())
}

fn save_machines(s: &Inner, machines: &[Machine]) {
    if let Err(e) = provision::save_machines(&s.dir, machines) {
        eprintln!("coordinator: machines.json: {e}");
    }
}

/// How the lease `id` that waits for a machine stands, for its requester.
fn waiting(s: &Inner, id: &str) -> Option<Provisioning> {
    let w = s.waits.lock().unwrap().get(id).cloned()?;
    let m = s
        .machines
        .lock()
        .unwrap()
        .iter()
        .find(|m| m.request == w.machine)
        .cloned();
    let why = match &m {
        Some(m) if m.member.is_some() => "the machine has enrolled; its node agent reports".into(),
        Some(m) if !m.machine.is_empty() => format!(
            "the provisioner makes machine {}; it enrols from {}",
            m.machine, m.address
        ),
        _ => "the provisioner is asked for a machine".into(),
    };
    Some(Provisioning {
        id: id.into(),
        state: "provisioning".into(),
        waited: w.started.elapsed().as_secs(),
        limit: w.limit.as_secs(),
        why,
        machine: m.map(|m| m.machine).filter(|x| !x.is_empty()),
    })
}

/// `GET /v1/leases/{id}/wait` (the requester): the lease that waits for a
/// machine, offered once the machine has enrolled, reports and (where the
/// pool asks for it) an admin confirmed it; `409` with why once it can no
/// longer come (the limit passed, the machine was given back).
async fn lease_wait(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    UrlPath(id): UrlPath<String>,
) -> ApiResult<Json<Leasing>> {
    let me = member(&s, &peer)?;
    let Some(w) = s
        .waits
        .lock()
        .unwrap()
        .get(&id)
        .filter(|w| w.member == me.id)
        .cloned()
    else {
        return Err((
            StatusCode::NOT_FOUND,
            format!("no lease `{id}` of yours waits for a machine"),
        ));
    };
    let fail = |why: String| {
        s.waits.lock().unwrap().remove(&id);
        s.leases.lock().unwrap().remove(&id);
        Err((StatusCode::CONFLICT, why))
    };
    let machine = s
        .machines
        .lock()
        .unwrap()
        .iter()
        .find(|m| m.request == w.machine)
        .cloned();
    let Some(m) = machine.filter(|m| m.state != MachineState::Returning) else {
        return fail("the machine made for the lane was given back".into());
    };
    if w.started.elapsed() > w.limit {
        return fail(format!(
            "the machine did not come within {} s; it is given back",
            w.limit.as_secs()
        ));
    }
    let mut p = waiting(&s, &id).expect("waits");
    if let Some(node) = &m.member {
        let ms = s.store.list().map_err(internal)?;
        let mut r = w.request.clone();
        r.node = node.clone();
        match offer(
            &s,
            &ms,
            &me,
            r,
            w.need_from.clone(),
            Some(format!("provisioned for it: machine {}", m.machine)),
            (id.clone(), w.token.clone()),
        ) {
            Ok(o) => {
                s.waits.lock().unwrap().remove(&id);
                return Ok(Json(Leasing::Offer(o)));
            }
            Err((_, why)) => p.why = why,
        }
    }
    Ok(Json(Leasing::Provisioning(p)))
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
    headers: axum::http::HeaderMap,
) -> ApiResult<Response> {
    member(&s, &peer)?;
    let mut v: Vec<Lease> = s
        .leases
        .lock()
        .unwrap()
        .values()
        .map(|(l, _, _)| l.clone())
        .collect();
    v.sort_by_key(|l| l.created);
    Ok(etagged(&headers, &v))
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
    headers: axum::http::HeaderMap,
) -> ApiResult<Response> {
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
    Ok(etagged(&headers, &out))
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
    headers: axum::http::HeaderMap,
) -> ApiResult<Response> {
    let m = member(&s, &peer)?;
    Ok(etagged(
        &headers,
        &crate::telemetry::Info {
            enabled: s.collector.is_some(),
            // The collector's failures are the pool's admins' to see.
            last_error: if m.role == Role::Admin {
                s.telemetry_error.lock().unwrap().0.clone()
            } else {
                None
            },
            collects: crate::telemetry::collects(),
        },
    ))
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

/// `POST /v1/enrol` (no certificate): a machine a provisioner made joins
/// with the token of its request (ADR-0051 point 6), from the address the
/// provisioner named for it, once. It is approved, granted as a node and
/// marked provisioned in one step. A second use of the token, or one from
/// another address, is an alarm: the node enrolled with it is revoked, its
/// machine given back, and the pool's admins see both addresses.
async fn enrol(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    Json(r): Json<EnrolRequest>,
) -> ApiResult<Json<JoinAnswer>> {
    throttle(&s, &peer)?;
    if r.csr.len() > MAX_CSR || r.token.len() > 256 {
        return Err((StatusCode::BAD_REQUEST, "the request is too large".into()));
    }
    let id = pki::member_id(&r.csr).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    let from = peer.addr.ip().to_canonical();
    let hash = provision::token_hash(&r.token);
    let mut machines = s.machines.lock().unwrap();
    let Some(m) = machines
        .iter_mut()
        .find(|m| !m.token_hash.is_empty() && crate::tls::same(&m.token_hash, &hash))
    else {
        return Err((
            StatusCode::FORBIDDEN,
            "no enrolment with this token: it is used up, expired or never was".into(),
        ));
    };
    let expected = m.address.parse::<std::net::IpAddr>().ok();
    let second = m.member.is_some() || m.alarm.is_some();
    let elsewhere = expected.is_some_and(|a| a != from);
    if second || elsewhere {
        let why = if second {
            format!(
                "the enrolment token of machine {} was used again, from {from} (it enrolled from {})",
                if m.machine.is_empty() {
                    &m.request
                } else {
                    &m.machine
                },
                if m.address.is_empty() {
                    "-"
                } else {
                    &m.address
                }
            )
        } else {
            format!(
                "an enrolment for machine {} came from {from}, not from {}, the address the provisioner named",
                m.machine, m.address
            )
        };
        alarm(&s, m, why);
        save_machines(&s, &machines);
        return Err((
            StatusCode::FORBIDDEN,
            "this enrolment is refused; the pool's admins are told".into(),
        ));
    }
    if expected.is_none() {
        return Err((
            StatusCode::CONFLICT,
            "not yet: the provisioner has not named this machine; try again in a moment".into(),
        ));
    }
    // Too late: the machine is given back already, or is past its time.
    if m.state != MachineState::Provisioning || store::now() > m.deadline {
        return Err((StatusCode::FORBIDDEN, "this enrolment has expired".into()));
    }
    let name: String = r
        .name
        .chars()
        .filter(|c| !c.is_control())
        .take(64)
        .collect();
    let ca = ca(&s.dir).map_err(internal)?;
    let joined = s
        .store
        .enrol(
            &id,
            name.trim(),
            &r.csr,
            &from.to_string(),
            &ca,
            store::Provisioned {
                request: m.request.clone(),
                machine: m.machine.clone(),
                address: m.address.clone(),
                confirmed: false,
            },
            m.repo.as_deref(),
        )
        .map_err(store_err)?;
    m.member = Some(joined.id.clone());
    m.state = MachineState::Ready;
    let ready = provision::Event {
        kind: "node_ready".into(),
        request: m.request.clone(),
        machine: m.machine.clone(),
        at: store::now(),
        size: m.size,
        lease: None,
    };
    save_machines(&s, &machines);
    drop(machines);
    tell(&s, vec![ready]);
    Ok(Json(answer(&joined)))
}

/// Raise an alarm on machine `m`: its node (if one enrolled) is revoked, it
/// is given back, and the leases that wait for it fail.
fn alarm(s: &Inner, m: &mut Machine, why: String) {
    eprintln!("coordinator: ALARM: {why}");
    if let Some(node) = &m.member {
        let by = "coordinator (enrolment alarm)";
        let _ = s.store.record(by, "alarm", node, Some(why.clone()));
        let _ = s.store.remove(node, by);
        s.reports.lock().unwrap().remove(node);
    }
    m.alarm = Some(why);
    m.state = MachineState::Returning;
}

/// Tell the provisioner what happened, off the request: a failure is
/// logged, nothing waits for it.
fn tell(s: &Inner, events: Vec<provision::Event>) {
    let Some(p) = s.provisioner.clone() else {
        return;
    };
    if events.is_empty() {
        return;
    }
    tokio::task::spawn_blocking(move || {
        for e in events {
            if let Err(err) = p.event(&e) {
                eprintln!("coordinator: event {} of {}: {err}", e.kind, e.machine);
            }
        }
    });
}

/// `PUT /v1/me/provisioned` `{on}`: the caller lets its lanes go to
/// provisioned nodes, or keeps them off (ADR-0051 point 10).
async fn me_provisioned(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    Json(o): Json<OnRequest>,
) -> ApiResult<Json<MemberView>> {
    let me = member(&s, &peer)?;
    let m = s
        .store
        .set_no_provisioned(&me.id, !o.on)
        .map_err(store_err)?;
    Ok(Json(MemberView::from(&m)))
}

async fn admin_provisioning(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
) -> ApiResult<Json<ProvisioningView>> {
    admin(&s, &peer)?;
    Ok(Json(ProvisioningView {
        provisioner: s.provisioner.as_ref().map(|p| p.url.clone()),
        settings: Settings::load(&s.dir),
        machines: s.machines.lock().unwrap().clone(),
    }))
}

/// `POST /v1/admin/provisioning`: the pool's settings for provisioning.
async fn admin_provisioning_set(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    Json(settings): Json<Settings>,
) -> ApiResult<Json<Settings>> {
    let (me, by) = admin(&s, &peer)?;
    settings.save(&s.dir).map_err(internal)?;
    s.store
        .record(
            &by,
            "provisioning",
            &me.id,
            Some(serde_json::to_string(&settings).unwrap_or_default()),
        )
        .map_err(store_err)?;
    Ok(Json(settings))
}

/// `POST /v1/admin/members/{id}/provision` `{on}` (ADR-0051 point 3).
async fn admin_provision(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    UrlPath(id): UrlPath<String>,
    Json(o): Json<OnRequest>,
) -> ApiResult<Json<MemberView>> {
    let (_, by) = admin(&s, &peer)?;
    let m = s.store.set_provision(&id, o.on, &by).map_err(store_err)?;
    Ok(Json(MemberView::for_admin(&m)))
}

/// `POST /v1/admin/nodes/{id}/confirm` (ADR-0051 point 6).
async fn admin_confirm(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    UrlPath(id): UrlPath<String>,
) -> ApiResult<Json<MemberView>> {
    let (_, by) = admin(&s, &peer)?;
    let m = s.store.confirm(&id, &by).map_err(store_err)?;
    Ok(Json(MemberView::for_admin(&m)))
}

/// `POST /v1/admin/nodes/{id}/give-back` (ADR-0051 point 9): the node's
/// membership is revoked and its machine given back, its lanes with it.
async fn admin_give_back(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    UrlPath(id): UrlPath<String>,
) -> ApiResult<StatusCode> {
    let (_, by) = admin(&s, &peer)?;
    let ms = s.store.list().map_err(internal)?;
    let Some(m) = ms
        .iter()
        .find(|m| (m.id == id || m.name == id) && m.provisioned.is_some())
    else {
        return Err((StatusCode::NOT_FOUND, format!("no provisioned node `{id}`")));
    };
    if m.state == State::Approved {
        s.store.remove(&m.id, &by).map_err(store_err)?;
    }
    s.reports.lock().unwrap().remove(&m.id);
    let mut machines = s.machines.lock().unwrap();
    for x in machines
        .iter_mut()
        .filter(|x| x.member.as_deref() == Some(m.id.as_str()))
    {
        x.state = MachineState::Returning;
    }
    save_machines(&s, &machines);
    Ok(StatusCode::ACCEPTED)
}

/// Every `every`: provisioned machines that did not enrol in time, whose
/// node was removed, or that had no lease for the pool's idle time are given
/// back (ADR-0051 points 5 and 7), deletions retried until the provisioner
/// confirms them; leases on them are told as begun and ended (point 8);
/// leases whose requester stopped waiting are dropped.
async fn tend(s: Shared) {
    let Some(p) = s.provisioner.clone() else {
        return;
    };
    let settings = Settings::load(&s.dir);
    let now = store::now();
    let ms = s.store.list().unwrap_or_default();
    // A requester that stopped asking: its wait goes a minute past the limit.
    let gone: Vec<String> = s
        .waits
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, w)| w.started.elapsed() > w.limit + Duration::from_secs(60))
        .map(|(id, _)| id.clone())
        .collect();
    for id in gone {
        s.waits.lock().unwrap().remove(&id);
        s.leases.lock().unwrap().remove(&id);
    }
    let leases: Vec<Lease> = s
        .leases
        .lock()
        .unwrap()
        .values()
        .map(|(l, _, _)| l.clone())
        .collect();
    let waited: std::collections::HashSet<String> = s
        .waits
        .lock()
        .unwrap()
        .values()
        .map(|w| w.machine.clone())
        .collect();
    let mut events = Vec::new();
    let delete: Vec<(String, String)> = {
        let mut machines = s.machines.lock().unwrap();
        for m in machines.iter_mut() {
            match m.state {
                MachineState::Provisioning if now > m.deadline => {
                    m.state = MachineState::Returning;
                }
                MachineState::Ready => {
                    let node = m
                        .member
                        .as_ref()
                        .and_then(|id| ms.iter().find(|x| &x.id == id));
                    let Some(node) = node.filter(|n| n.state == State::Approved) else {
                        m.state = MachineState::Returning;
                        continue;
                    };
                    let busy =
                        waited.contains(&m.request) || leases.iter().any(|l| l.node == node.id);
                    if busy {
                        m.idle_since = None;
                    } else {
                        let since = *m.idle_since.get_or_insert(now);
                        if now.saturating_sub(since) >= settings.idle_minutes * 60 {
                            let by = "coordinator (idle)";
                            let _ = s.store.remove(&node.id, by);
                            s.reports.lock().unwrap().remove(&node.id);
                            m.state = MachineState::Returning;
                        }
                    }
                }
                _ => {}
            }
        }
        // A machine the provisioner never named cannot be deleted by name.
        machines.retain(|m| !(m.state == MachineState::Returning && m.machine.is_empty()));
        save_machines(&s, &machines);
        machines
            .iter()
            .filter(|m| m.state == MachineState::Returning)
            .map(|m| (m.request.clone(), m.machine.clone()))
            .collect()
    };
    for (request, machine) in delete {
        let p2 = p.clone();
        let id = machine.clone();
        let done = tokio::task::spawn_blocking(move || p2.delete(&id))
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
        let mut machines = s.machines.lock().unwrap();
        match done {
            Ok(()) => {
                let size = machines
                    .iter()
                    .find(|m| m.request == request)
                    .and_then(|m| m.size);
                machines.retain(|m| m.request != request);
                events.push(provision::Event {
                    kind: "node_returned".into(),
                    request,
                    machine,
                    at: now,
                    size,
                    lease: None,
                });
            }
            Err(e) => {
                if let Some(m) = machines.iter_mut().find(|m| m.request == request) {
                    m.error = Some(e);
                }
            }
        }
        save_machines(&s, &machines);
    }
    // Leases on provisioned nodes, begun and ended (ADR-0051 point 8).
    let on: HashMap<String, (String, String)> = s
        .machines
        .lock()
        .unwrap()
        .iter()
        .filter_map(|m| Some((m.member.clone()?, (m.request.clone(), m.machine.clone()))))
        .chain(ms.iter().filter_map(|m| {
            let p = m.provisioned.as_ref()?;
            Some((m.id.clone(), (p.request.clone(), p.machine.clone())))
        }))
        .collect();
    {
        let mut begun = s.begun.lock().unwrap();
        for l in leases.iter().filter(|l| l.state == "running") {
            let Some((request, machine)) = on.get(&l.node) else {
                continue;
            };
            if begun.contains_key(&l.id) {
                continue;
            }
            let e = provision::LeaseEvent {
                id: l.id.clone(),
                member: l.member.clone(),
                repo: l.repo.clone(),
                lane: l.lane.clone(),
                need: l.need,
                began: now,
                ended: None,
            };
            events.push(provision::Event {
                kind: "lease_begun".into(),
                request: request.clone(),
                machine: machine.clone(),
                at: now,
                size: None,
                lease: Some(e.clone()),
            });
            begun.insert(l.id.clone(), (request.clone(), machine.clone(), e));
        }
        let ended: Vec<String> = begun
            .keys()
            .filter(|id| !leases.iter().any(|l| &&l.id == id && l.state == "running"))
            .cloned()
            .collect();
        for id in ended {
            let (request, machine, mut e) = begun.remove(&id).expect("listed");
            e.ended = Some(now);
            events.push(provision::Event {
                kind: "lease_ended".into(),
                request,
                machine,
                at: now,
                size: None,
                lease: Some(e),
            });
        }
    }
    tell(&s, events);
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

/// `GET /v1/upstreams`: the MCP upstreams this pool names for its members
/// (ADR-0069 option B), with an OAuth app's client secret where one is set
/// (the members' hubs log in with it; never a user's credential).
async fn upstreams_list(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    headers: axum::http::HeaderMap,
) -> ApiResult<Response> {
    member(&s, &peer)?;
    Ok(etagged(&headers, &crate::upstreams::load(&s.dir)))
}

/// `POST /v1/admin/upstreams`: add an upstream to the pool's list, or
/// change the one of its name (admins only; audited).
async fn admin_upstream_put(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    Json(u): Json<crate::upstreams::PoolUpstream>,
) -> ApiResult<Json<Vec<crate::upstreams::PoolUpstream>>> {
    let (me, by) = admin(&s, &peer)?;
    let detail = format!("{} {}", u.name, u.url);
    {
        let _one = s.upstreams.lock().unwrap();
        crate::upstreams::put(&s.dir, u)
            .map_err(|e| (StatusCode::BAD_REQUEST, e))?
            .map_err(internal)?;
    }
    s.store
        .record(&by, "upstream", &me.id, Some(detail))
        .map_err(store_err)?;
    Ok(Json(crate::upstreams::load(&s.dir)))
}

/// `DELETE /v1/admin/upstreams/{name}`: take an upstream off the pool's
/// list (admins only; audited).
async fn admin_upstream_remove(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    UrlPath(name): UrlPath<String>,
) -> ApiResult<Json<Vec<crate::upstreams::PoolUpstream>>> {
    let (me, by) = admin(&s, &peer)?;
    let removed = {
        let _one = s.upstreams.lock().unwrap();
        crate::upstreams::remove(&s.dir, &name).map_err(internal)?
    };
    if !removed {
        return Err((
            StatusCode::NOT_FOUND,
            format!("the pool names no upstream `{name}`"),
        ));
    }
    s.store
        .record(&by, "upstream-remove", &me.id, Some(name))
        .map_err(store_err)?;
    Ok(Json(crate::upstreams::load(&s.dir)))
}

async fn admin_audit(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
) -> ApiResult<Json<Vec<AuditEntry>>> {
    admin(&s, &peer)?;
    Ok(Json(s.store.audit(200).map_err(internal)?))
}

/// What a coordinator runs with beyond its directory.
#[derive(Debug, Clone)]
pub struct Options {
    /// The provisioner it asks for machines (ADR-0051).
    pub provisioner: Option<provision::Provisioner>,
    /// How often it looks after provisioned machines.
    pub tend_every: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            provisioner: None,
            tend_every: Duration::from_secs(15),
        }
    }
}

/// Serve the coordinator in `dir` on `listener` until `stop` resolves.
pub async fn serve(
    dir: &Path,
    listener: tokio::net::TcpListener,
    stop: impl std::future::Future<Output = ()>,
) -> Result<()> {
    serve_with(dir, listener, stop, Options::default()).await
}

/// [`serve`], with `options`.
pub async fn serve_with(
    dir: &Path,
    listener: tokio::net::TcpListener,
    stop: impl std::future::Future<Output = ()>,
    options: Options,
) -> Result<()> {
    let ca_pem = read(&dir.join("ca.pem"))?;
    let port = listener
        .local_addr()
        .map_err(|e| CoordError::Other(e.to_string()))?
        .port();
    let host = names(dir)?
        .into_iter()
        .next()
        .unwrap_or_else(|| "127.0.0.1".into());
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host
    };
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
        history: Mutex::new(load_history(dir, store::now())),
        collector: crate::telemetry::Collector::of(dir),
        telemetry_error: Arc::default(),
        telemetry_slots: Arc::new(tokio::sync::Semaphore::new(TELEMETRY_IN_FLIGHT)),
        joins: Mutex::new(HashMap::new()),
        ending: Mutex::new(std::collections::HashSet::new()),
        updates: Mutex::new(HashMap::new()),
        prunes: Mutex::new(std::collections::HashSet::new()),
        store: Store::new(dir),
        footprints: crate::footprint::Footprints::load(dir),
        reports: Mutex::new(HashMap::new()),
        machines: Mutex::new(provision::load_machines(dir)),
        waits: Mutex::new(HashMap::new()),
        begun: Mutex::new(HashMap::new()),
        public_url: format!("https://{host}:{port}"),
        fingerprint: pki::fingerprint(&ca_pem)?,
        provisioner: options.provisioner,
        upstreams: Mutex::new(()),
        skill_fetch: Mutex::new(SkillFetch::default()),
        versions: tokio::sync::watch::Sender::new(crate::events::Versions::new()),
        recount: Arc::new(tokio::sync::Notify::new()),
        ca_pem,
    });
    shared.versions.send_replace(versions(&shared));
    tokio::spawn(count_versions(
        Arc::downgrade(&shared),
        shared.recount.clone(),
    ));
    // The nodes' histories to disk, so a restart keeps them (ADR-0036).
    {
        let s = Arc::downgrade(&shared);
        tokio::spawn(async move {
            let mut every = tokio::time::interval(HISTORY_EVERY);
            every.tick().await;
            loop {
                every.tick().await;
                let Some(s) = s.upgrade() else { return };
                let all = s.history.lock().unwrap().clone();
                let _ = tokio::task::spawn_blocking(move || {
                    if let Err(e) = save_history(&s.dir, &all) {
                        eprintln!("coordinator: {HISTORY}: {e}");
                    }
                })
                .await;
            }
        });
    }
    // The skill sets as their sources hold them now (ADR-0074).
    if !crate::skills::load(&shared.dir).is_empty() {
        fetch_skills(&shared, None);
    }
    if shared.provisioner.is_some() {
        let s = shared.clone();
        tokio::spawn(async move {
            let mut every = tokio::time::interval(options.tend_every);
            loop {
                every.tick().await;
                tend(s.clone()).await;
            }
        });
    }
    crate::tls::serve(config, listener, router(shared), stop).await;
    Ok(())
}

/// Serve the coordinator in `dir` on `listen` until Ctrl-C (or SIGTERM).
pub fn serve_blocking(dir: &Path, listen: std::net::SocketAddr, options: Options) -> Result<()> {
    let rt = tokio::runtime::Runtime::new().map_err(|e| CoordError::Other(e.to_string()))?;
    rt.block_on(async {
        let listener = tokio::net::TcpListener::bind(listen)
            .await
            .map_err(|e| CoordError::Other(format!("listen on {listen}: {e}")))?;
        serve_with(dir, listener, stop_signal(), options).await
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
mod kept_tests {
    use super::*;

    #[test]
    fn what_a_node_keeps_names_its_store_and_older_nodes_need_not() {
        let old: Kept = serde_json::from_str(r#"{"lanes":2,"bytes":100}"#).unwrap();
        assert_eq!(old.store_bytes, None);
        assert_eq!(
            serde_json::to_string(&old).unwrap(),
            r#"{"lanes":2,"bytes":100}"#
        );
        let new = Kept {
            lanes: 0,
            bytes: 40,
            store_bytes: Some(40),
        };
        let back: Kept = serde_json::from_str(&serde_json::to_string(&new).unwrap()).unwrap();
        assert_eq!(back, new);
    }
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
    fn the_history_survives_a_restart_without_what_is_too_old() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = History::default();
        a.push(p(0, 1.0));
        a.push(p(60, 2.0));
        a.push(p(2 * HOUR, 3.0));
        let mut old = History::default();
        old.push(p(0, 1.0));
        let all = HashMap::from([("a".to_string(), a), ("old".to_string(), old)]);
        save_history(dir.path(), &all).unwrap();
        // Two days on, `old` is gone; `a` keeps what is younger than an hour
        // (raw) and a day (minutes) at `now`.
        let back = load_history(dir.path(), DAY + 30);
        assert_eq!(back.len(), 1);
        assert_eq!(back["a"].series(false).len(), 0);
        assert_eq!(back["a"].series(true).len(), 1);
        let back = load_history(dir.path(), 2 * HOUR);
        assert_eq!(back["a"].series(false), vec![p(2 * HOUR, 3.0)]);
        assert!(!back.contains_key("old"));
        // No file, or a broken one: it starts again.
        std::fs::write(dir.path().join(HISTORY), "{").unwrap();
        assert!(load_history(dir.path(), 0).is_empty());
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
    use crate::share::{Gpu, GpuNeed, Need};

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

    fn order(nodes: &[(&str, &str, &Report)], need: Need) -> Vec<(String, bool, String)> {
        rank(nodes, "me", "shop", "fix", need, Some(&Release::this()))
            .into_iter()
            .map(|p| (p.name, p.eligible, p.why))
            .collect()
    }

    #[test]
    fn each_place_says_what_the_lane_would_hold_there() {
        let need = Need {
            cpus: 2.5,
            memory: 6 * GIB,
            ..Need::default()
        };
        let (a, b) = (node(4.0, 8, &[]), node(0.5, 1, &[]));
        let nodes = [("a", "a", &a), ("b", "b", &b)];
        let got = rank(&nodes, "me", "shop", "fix", need, Some(&Release::this()));
        // Eligible or not: the dashboard draws the need on every node.
        assert_eq!(got.len(), 2);
        assert!(got.iter().all(|p| p.need == Some(need)));
    }

    #[test]
    fn provisioned_nodes_are_filled_after_the_members_own() {
        let need = Need {
            cpus: 1.0,
            memory: 2 * GIB,
            ..Need::default()
        };
        let (own, emptier, fuller) = (node(4.0, 8, &[]), node(8.0, 16, &[]), node(2.0, 4, &[]));
        let nodes = [
            ("p1", "emptier", &emptier),
            ("own", "own", &own),
            ("p2", "fuller", &fuller),
        ];
        let filled = ["p1".to_string(), "p2".to_string()].into_iter().collect();
        let got: Vec<String> = rank_filling(&nodes, &filled, "me", "shop", "fix", need, None)
            .into_iter()
            .map(|p| p.name)
            .collect();
        // The member's own first; of the provisioned, the fuller one.
        assert_eq!(got, ["own", "fuller", "emptier"]);
        // Without provisioned nodes, the most free capacity first, as before.
        let got: Vec<String> = order(&nodes, need).into_iter().map(|p| p.0).collect();
        assert_eq!(got, ["emptier", "own", "fuller"]);
    }

    #[test]
    fn a_lane_goes_where_it_starts_warm_with_room_to_spare() {
        let need = Need {
            cpus: 1.0,
            memory: 4 * GIB,
            ..Need::default()
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
        let need = Need {
            cpus: 2.0,
            memory: 2 * GIB,
            ..Need::default()
        };
        // Much memory, little CPU: 1.25× the need in CPU.
        let ram = node(2.5, 64, &[]);
        let both = node(8.0, 8, &[]);
        let o = order(&[("a", "ram", &ram), ("b", "both", &both)], need);
        assert_eq!(o[0].0, "both", "{o:?}");
    }

    #[test]
    fn what_cannot_take_the_lane_says_why_and_comes_last() {
        let need = Need {
            cpus: 1.0,
            memory: 4 * GIB,
            ..Need::default()
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
        assert!(why("full").contains("on disk"), "{o:?}");
        assert!(why("off").contains("does not share"), "{o:?}");
    }

    #[test]
    fn a_lane_needs_the_disk_it_does_not_have_there_yet() {
        let need = Need {
            cpus: 1.0,
            memory: GIB,
            disk_repo: 20 * GIB,
            disk_lane: 10 * GIB,
            gpus: None,
        };
        // 30 GiB room: enough for a node with the repository's images
        // (10 + 5 free), not for a cold one (30 + 5).
        let room = |slots: &[(&str, &str, &str)]| {
            let mut n = node(8.0, 32, slots);
            n.disk_free = Some(30 * GIB);
            n
        };
        let cold = room(&[]);
        let warm = room(&[("someone", "shop", "other")]);
        let kept = room(&[("me", "shop", "fix")]);
        let o = rank(
            &[
                ("a", "cold", &cold),
                ("b", "warm", &warm),
                ("c", "kept", &kept),
            ],
            "me",
            "shop",
            "fix",
            need,
            Some(&Release::this()),
        );
        let by = |n: &str| o.iter().find(|p| p.name == n).unwrap().clone();
        assert!(!by("cold").eligible, "{o:?}");
        assert!(
            by("cold").why.contains("the lane needs 30.0 GiB there"),
            "{o:?}"
        );
        assert_eq!((by("warm").eligible, by("warm").disk), (true, 10 * GIB));
        assert_eq!((by("kept").eligible, by("kept").disk), (true, 0));
        // What a node could free counts: its room says so.
        let mut roomy = room(&[]);
        roomy.disk_room = Some(60 * GIB);
        assert!(order(&[("d", "roomy", &roomy)], need)[0].1);
    }

    #[test]
    fn gpus_asked_for_are_gpus_offered() {
        let need = Need {
            cpus: 1.0,
            memory: GIB,
            gpus: Some(GpuNeed {
                count: 1,
                memory: 16 * GIB,
            }),
            ..Need::default()
        };
        let gpu = |memory: u64, offered: bool| Gpu {
            name: "gpu".into(),
            memory,
            offered,
            ..Gpu::default()
        };
        let mut small = node(8.0, 32, &[]);
        small.gpus = vec![gpu(8 * GIB, true)];
        let mut held = node(8.0, 32, &[]);
        held.gpus = vec![gpu(24 * GIB, false)];
        let mut big = node(8.0, 32, &[]);
        big.gpus = vec![gpu(24 * GIB, true)];
        let o = order(
            &[
                ("a", "small", &small),
                ("b", "held", &held),
                ("c", "big", &big),
            ],
            need,
        );
        assert_eq!((o[0].0.as_str(), o[0].1), ("big", true), "{o:?}");
        assert!(o[1..].iter().all(|x| !x.1 && x.2.contains("GPU")), "{o:?}");
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
                        need: Need {
                            cpus: 1.0,
                            memory: gib * GIB,
                            ..Need::default()
                        },
                        disk: gib * GIB,
                        state: state.into(),
                        created: 0,
                        expires_in: None,
                        shared: None,
                        warm: false,
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
            Need {
                cpus: 1.0,
                memory: 4 * GIB,
                ..Need::default()
            },
        );
        assert!(!o[0].1, "{o:?}");
        // The refusal says what they hold; with nothing held, it is as it was.
        assert_eq!(
            with_held("offers 2.0 CPU", held),
            "offers 2.0 CPU; 2.0 CPU and 6144 MiB more are held for a lane starting there"
        );
        assert_eq!(
            with_held("offers 2.0 CPU", Held::default()),
            "offers 2.0 CPU"
        );
        // Their disk is held too.
        assert_eq!(held.disk, 6 * GIB);
        assert_eq!(rep.disk_room, Some(94 * GIB));
        // More held than offered: nothing left, never less.
        let rep = less_offered(&node(1.0, 1, &[]), held);
        assert_eq!((rep.cpus, rep.memory), (0.0, 0));
        // Lane `a` sees its own offer as its room, the others' held.
        let a = with_own(&node(4.0, 8, &[]), &leases, "n1", |l| l.lane == "a");
        assert_eq!((a.cpus, a.memory), (3.0, 6 * GIB));
        // Lane `c` runs there: the room its need takes in the report is its.
        let c = with_own(&node(4.0, 8, &[]), &leases, "n1", |l| l.lane == "c");
        assert_eq!((c.cpus, c.memory), (3.0, 10 * GIB));
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

/// The skill sets to fetch again (all of them, or these), and whether a
/// fetch runs: one at a time, the ones asked for meanwhile after it.
#[derive(Default)]
struct SkillFetch {
    running: bool,
    all: bool,
    sets: std::collections::BTreeSet<String>,
}

/// Fetch the set `set` (`None`: every set) again, after a fetch that runs.
fn fetch_skills(s: &Shared, set: Option<String>) {
    {
        let mut f = s.skill_fetch.lock().unwrap();
        match set {
            Some(n) => {
                f.sets.insert(n);
            }
            None => f.all = true,
        }
        if f.running {
            return;
        }
        f.running = true;
    }
    let s = s.clone();
    tokio::spawn(async move {
        loop {
            let (all, sets) = {
                let mut f = s.skill_fetch.lock().unwrap();
                if !f.all && f.sets.is_empty() {
                    f.running = false;
                    return;
                }
                (std::mem::take(&mut f.all), std::mem::take(&mut f.sets))
            };
            let dir = s.dir.clone();
            let _ = tokio::task::spawn_blocking(move || {
                for set in crate::skills::load(&dir) {
                    if all || sets.contains(&set.name) {
                        crate::skills::refresh(&dir, &set);
                    }
                }
            })
            .await;
            s.recount.notify_one();
        }
    });
}

/// `GET /v1/skills` (members, ADR-0074): the sets given to the caller, as
/// last fetched.
async fn skills_list(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    headers: axum::http::HeaderMap,
) -> ApiResult<Response> {
    let m = member(&s, &peer)?;
    Ok(etagged(
        &headers,
        &crate::skills::for_member(&s.dir, &m.id, &m.tags),
    ))
}

/// `GET /v1/skills/{set}/{skill}.tar.gz` (members given the set): a
/// skill's archive; each download in the audit log.
async fn skill_archive(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    UrlPath((set, file)): UrlPath<(String, String)>,
) -> ApiResult<Response> {
    let m = member(&s, &peer)?;
    let not_found = || {
        (
            StatusCode::NOT_FOUND,
            format!("no skill `{file}` in a set `{set}` given to you"),
        )
    };
    let skill = file.strip_suffix(".tar.gz").ok_or_else(not_found)?;
    // The set, and the folder the skill is in (ADR-0079), given to it.
    if !crate::skills::skill_given_to(&s.dir, &set, skill, &m.id, &m.tags) {
        return Err(not_found());
    }
    let bytes = crate::skills::archive(&s.dir, &set, skill).ok_or_else(not_found)?;
    use sha2::Digest;
    let sha = crate::skills::hex(&sha2::Sha256::digest(&bytes));
    let by = format!("{} ({})", m.id, m.name);
    s.store
        .record(
            &by,
            "skill",
            &m.id,
            Some(format!("{set}/{skill} {}", &sha[..12])),
        )
        .map_err(store_err)?;
    Ok((
        [
            (
                axum::http::header::CONTENT_TYPE,
                "application/gzip".to_string(),
            ),
            (axum::http::header::ETAG, format!("\"{sha}\"")),
        ],
        bytes,
    )
        .into_response())
}

/// `POST /v1/skills/refresh`: a source says it changed (a push, through a
/// webhook relay). No client certificate: signed as Hookaido signs a
/// delivery (`X-Hookaido-Signature`, `X-Hookaido-Timestamp`) with
/// `DEVSTACK_SKILLS_REFRESH_SECRET`. It only has every set fetched again;
/// its body is read for nothing else. Without the secret: `404`.
async fn skills_refresh_signed(
    AxState(s): AxState<Shared>,
    req: axum::extract::Request,
) -> ApiResult<StatusCode> {
    let Some(secret) = crate::skills::refresh_secret() else {
        return Err((StatusCode::NOT_FOUND, "no signed refresh here".into()));
    };
    let (parts, body) = req.into_parts();
    let header = |n: &str| {
        parts
            .headers
            .get(n)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string()
    };
    let body = axum::body::to_bytes(body, 1 << 20)
        .await
        .map_err(|_| (StatusCode::PAYLOAD_TOO_LARGE, "body over 1 MiB".into()))?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    if !crate::skills::signed(
        &secret,
        parts.method.as_str(),
        parts.uri.path(),
        &header("x-hookaido-timestamp"),
        &header("x-hookaido-signature"),
        &body,
        now,
    ) {
        return Err((StatusCode::UNAUTHORIZED, "not signed for this pool".into()));
    }
    fetch_skills(&s, None);
    Ok(StatusCode::ACCEPTED)
}

/// `GET /v1/admin/skills`: every set with its state (admins).
async fn admin_skills(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
) -> ApiResult<Json<crate::skills::AdminView>> {
    admin(&s, &peer)?;
    Ok(Json(crate::skills::admin_view(&s.dir)))
}

/// `POST /v1/admin/skills`: add a set, or change the one of its name
/// (admins; audited); it is fetched at once.
async fn admin_skill_put(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    Json(set): Json<crate::skills::SkillSet>,
) -> ApiResult<Json<crate::skills::AdminView>> {
    let (me, by) = admin(&s, &peer)?;
    let detail = if set.folders.is_empty() {
        format!(
            "{} {} to {}",
            set.name,
            set.source.label(),
            set.to.join(",")
        )
    } else {
        let folders: Vec<String> = set
            .folders
            .iter()
            .map(|f| format!("{} to {}", f.path, f.to.join(",")))
            .collect();
        format!(
            "{} {} ({})",
            set.name,
            set.source.label(),
            folders.join("; ")
        )
    };
    let name = set.name.clone();
    {
        let _one = s.upstreams.lock().unwrap();
        crate::skills::put(&s.dir, set)
            .map_err(|e| (StatusCode::BAD_REQUEST, e))?
            .map_err(internal)?;
    }
    s.store
        .record(&by, "skill-set", &me.id, Some(detail))
        .map_err(store_err)?;
    fetch_skills(&s, Some(name));
    Ok(Json(crate::skills::admin_view(&s.dir)))
}

/// `DELETE /v1/admin/skills/{name}`: stop handing a set out (admins;
/// audited). Members' hubs remove what they installed of it.
async fn admin_skill_remove(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    UrlPath(name): UrlPath<String>,
) -> ApiResult<Json<crate::skills::AdminView>> {
    let (me, by) = admin(&s, &peer)?;
    let removed = {
        let _one = s.upstreams.lock().unwrap();
        crate::skills::remove(&s.dir, &name).map_err(internal)?
    };
    if !removed {
        return Err((
            StatusCode::NOT_FOUND,
            format!("the pool hands out no set `{name}`"),
        ));
    }
    s.store
        .record(&by, "skill-set-remove", &me.id, Some(name))
        .map_err(store_err)?;
    Ok(Json(crate::skills::admin_view(&s.dir)))
}

#[derive(Deserialize, Default)]
pub struct RefreshQuery {
    #[serde(default)]
    pub set: Option<String>,
}

/// `POST /v1/admin/skills/refresh[?set=]`: fetch the sets again now
/// (admins).
async fn admin_skills_refresh(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    axum::extract::Query(q): axum::extract::Query<RefreshQuery>,
) -> ApiResult<StatusCode> {
    admin(&s, &peer)?;
    fetch_skills(&s, q.set.filter(|x| !x.is_empty()));
    Ok(StatusCode::ACCEPTED)
}
