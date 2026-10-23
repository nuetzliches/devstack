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
//!   (`409` while the lane starts: the agent checks its files then)
//! - `POST /v1/leases/{id}/start` `{stack_file}`, `POST /v1/leases/{id}/stop`,
//!   `GET /v1/leases/{id}/lane`: the lane itself (`lanerun`)
//! - `ANY /v1/leases/{id}/api/{*rest}`: the lane daemon's API, streamed

use crate::coordinator::{Lease, LeaseReport, Report};
use crate::lanerun::{DaemonAddr, LaneRun};
use crate::member::Local;
use crate::share::{Amount, Need, Settings, idle_cpus};
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
/// A blob this young is never collected: a lease uploading has not applied it.
const BLOB_YOUNG: Duration = Duration::from_secs(600);
/// How often the node looks for members the coordinator no longer lists,
/// and for kept lanes past their time.
const PURGE_EVERY: Duration = Duration::from_secs(600);
/// How often the node measures what its running lanes take of its disk
/// (ADR-0049 point 2): their slots, volumes (`docker system df -v`, slow)
/// and images.
const DISK_EVERY: Duration = Duration::from_secs(300);
/// How long a stopped lane's slot stays, unless the node's policy says
/// (ADR-0038 point 3).
const KEEP_DEDICATED: Duration = Duration::from_secs(7 * 24 * 3600);
const KEEP_OTHER: Duration = Duration::from_secs(24 * 3600);
/// What one member's kept lanes may take here, unless the policy says.
const KEEP_GIB: u64 = 20;
/// The largest list of files (or hashes) one sync sends: about 250 000.
const MAX_LIST: usize = 32 << 20;
/// How long the node trusts its last list of members while the coordinator
/// does not answer (ADR-0031 point 6): renewals go on, a member removed in
/// the meantime is refused after this at the latest.
const MEMBERS_VALID_FOR: Duration = Duration::from_secs(3600);
/// How long a node waits for its lanes to end before an update an admin
/// ordered (ADR-0039): then it gives up and takes lanes again.
const DRAIN_FOR: Duration = Duration::from_secs(8 * 3600);
/// The coordinator's list of members is not asked for again within this,
/// for a caller it does not name either: a member approved just now waits
/// this long at most.
const MEMBERS_ASKED_AGAIN: Duration = Duration::from_secs(5);
/// How many container output streams one member follows here at once
/// (each holds a `docker logs --follow` and its readers).
const MAX_FOLLOWS: usize = 16;

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
    /// What is free on the machine, taken as it is instead of measured:
    /// a test fixes it so that what the node offers does not depend on how
    /// busy the machine running the test is. `None` (the agent): measured.
    pub free: Option<Amount>,
    /// How it installs a release an admin ordered (ADR-0039): the binary it
    /// is to run then, `None` when it runs that release already. Without
    /// one, an order is refused.
    pub on_update: Option<UpdateFn>,
}

/// See [`NodeConfig::on_update`].
pub type UpdateFn =
    Arc<dyn Fn(&crate::coordinator::UpdateOrder) -> Result<Option<PathBuf>, String> + Send + Sync>;

/// A lease the node admitted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeLease {
    pub id: String,
    /// The requester.
    pub member: String,
    pub repo: String,
    pub lane: String,
    pub need: Need,
    /// The disk it is expected to take here in all (ADR-0049 point 7):
    /// its slot's part of the need, and the repository's when its images
    /// were not here (`images`). What it has not written yet is held.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub disk: u64,
    /// Its images were not here when it was admitted: what they take
    /// counts as written by it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub images: bool,
    /// The node is short of disk (ADR-0049 point 8): said with each renewal
    /// while it is, never stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
    /// Unix seconds.
    pub expires: u64,
    /// Unix seconds.
    pub admitted: u64,
    /// The stack file the lane runs, relative to its root, once started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack_file: Option<String>,
    /// Its slot (ADR-0038): where its mirror and data are, kept for the next
    /// lease of the same lane. `None`: admitted by an older agent, under
    /// `lanes/<id>`, removed with the lease.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<String>,
    /// The lane starts from what an earlier lease kept there, last used then
    /// (Unix seconds); `None`: a fresh start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kept: Option<u64>,
}

/// What a slot keeps about itself (`slots/<slot>/slot.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlotInfo {
    pub member: String,
    pub repo: String,
    pub lane: String,
    /// When a lease last ended there, or began (Unix seconds).
    pub used: u64,
}

/// `GET /v1/slots`: one of the caller's kept lanes here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlotView {
    pub repo: String,
    pub lane: String,
    pub used: u64,
    /// What its directory (the mirror, builds, caches) and its volumes
    /// take, bytes; the volumes as the node last counted them.
    pub bytes: u64,
    /// A lease runs there now.
    pub active: bool,
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
    /// The devstack the agent runs, and so its lanes' daemons; absent from
    /// an older agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub devstack: Option<crate::version::Release>,
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
    /// The files that could not be written, `path: error`; the others were
    /// (#128). An older node fails the whole sync at the first one instead.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed: Vec<String>,
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
    /// See [`NodeConfig::free`].
    free: Option<Amount>,
    /// [`NodeConfig::on_update`].
    on_update: Option<UpdateFn>,
    /// The update an admin ordered, as it goes (ADR-0039).
    update: Mutex<Option<crate::coordinator::UpdateReport>>,
    /// Updating: no new lane is admitted.
    draining: std::sync::atomic::AtomicBool,
    /// The order (its `at`) the coordinator no longer sends while the node
    /// waits for its lanes: an admin cancelled it.
    update_withdrawn: Mutex<Option<u64>>,
    leases: Mutex<HashMap<String, NodeLease>>,
    /// The approved members, as the coordinator last listed them.
    /// With their tags, which this node's policy may ask for (ADR-0033).
    members: Mutex<(Option<Instant>, MemberTags)>,
    /// When the coordinator was last asked for that list, answered or not.
    members_asked: Mutex<Option<Instant>>,
    /// How many container output streams each member follows now.
    follows: Mutex<HashMap<String, usize>>,
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
    /// Slots a lease's end is taking down, or a purge is removing: a new
    /// lease of the lane waits for it (ADR-0038 point 1). Counted: the end
    /// of one lease can still run when the next lease of the lane ends.
    slot_busy: Mutex<HashMap<String, usize>>,
    /// Slots being removed: no lease is admitted to one until it is gone.
    slot_purging: Mutex<HashSet<String>>,
    /// Whether a member may view a lease (ADR-0040), as the coordinator said
    /// lately: asked again after [`VIEW_FRESH`].
    views_ok: Mutex<HashMap<(String, String), (Instant, bool)>>,
    /// What the stopped lanes kept here take, as the last sweep counted.
    kept_use: Mutex<Option<crate::coordinator::Kept>>,
    /// What each slot's volumes take, as the last sweep counted (counting
    /// is slow).
    volume_use: Mutex<HashMap<String, u64>>,
    /// What each slot takes (its directory and volumes), as last counted.
    slot_use: Mutex<HashMap<String, u64>>,
    /// What each lease's lane took so far (ADR-0049 point 2). Locked after
    /// `leases` where both are.
    runs: Mutex<crate::runs::Runs>,
    /// Whether the coordinator refused this node's API (not granted to run
    /// lanes), said once.
    refused: Mutex<Option<String>>,
    /// What the coordinator said it runs at its last answer (`None`: not
    /// yet, `Some(None)`: an older one that does not say), told once.
    coordinator: Mutex<Option<Option<crate::version::Release>>>,
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
    /// Leases whose mirror a sync writes now. A sync waits for no start
    /// and a start for no sync: one taken under the `starting` lock while
    /// the lane starts is refused (`409`), and a start waits until those
    /// begun before it are done (the agent checks the lane's files, and
    /// nothing may change them meanwhile).
    applying: Mutex<HashMap<String, usize>>,
    /// Leases whose start is under way or whose containers are still being
    /// brought up after their daemon answered (#164): a stop, a lease's end
    /// or a new start ends that first ([`Inner::end_bring_up`]).
    bringing: Mutex<HashMap<String, Arc<Bringing>>>,
    /// The phases of each lane's last start that are over, with how long
    /// each took (`LaneState::phases`), and when the current one began.
    phase_times: Mutex<HashMap<String, (Vec<PhaseTime>, std::time::Instant)>>,
    /// A lease came or went: the next report is due now, not in `EVERY`.
    /// The coordinator admits a lease against the node's last report; one
    /// that still counts a released lease refuses a lane the node has room
    /// for.
    changed: tokio::sync::Notify,
}

/// A lane's containers being brought up in the background (#164).
#[derive(Default)]
struct Bringing {
    /// The lane stops or starts afresh: Compose ends, nothing more starts.
    ended: std::sync::atomic::AtomicBool,
    /// Held while the containers are brought up: whoever ends it waits on
    /// it, so nothing of the lane starts after its containers were taken
    /// down.
    running: Mutex<()>,
}

impl Bringing {
    fn ended(&self) -> bool {
        self.ended.load(std::sync::atomic::Ordering::SeqCst)
    }
}

fn is_zero(v: &u64) -> bool {
    *v == 0
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

    fn slots(&self) -> PathBuf {
        self.root.join("slots")
    }

    /// The slot of `member`'s lane `lane` of `repo` (ADR-0038 point 1): a
    /// name only this node can tell, so the same lane finds its data again
    /// and nobody else's.
    fn slot_name(&self, member: &str, repo: &str, lane: &str) -> String {
        crate::lanerun::secret_name(&self.secret, &["slot", member, repo, lane])
    }

    /// Where lease `l` keeps its mirror: its slot, or `lanes/<id>` for one
    /// an older agent admitted.
    fn dir_of(&self, l: &NodeLease) -> PathBuf {
        match &l.slot {
            Some(slot) => self.slots().join(slot),
            None => self.lanes().join(&l.id),
        }
    }

    /// The slots kept here, with what they say about themselves.
    fn kept(&self) -> Vec<(String, SlotInfo)> {
        let Ok(rd) = std::fs::read_dir(self.slots()) else {
            return Vec::new();
        };
        rd.flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                let text = std::fs::read(e.path().join("slot.json")).ok()?;
                Some((name, serde_json::from_slice(&text).ok()?))
            })
            .collect()
    }

    /// Written next to it and renamed into place: the end of a lease writes
    /// it while the lane's next lease may already read it (`admit`), and a
    /// file read half-written made that lease start without its kept data.
    fn note_slot(&self, slot: &str, info: &SlotInfo) {
        let dir = self.slots().join(slot);
        let _ = std::fs::create_dir_all(&dir);
        let tmp = dir.join(format!(
            "slot.json.{}.tmp",
            crate::coordinator::random_hex(4)
        ));
        let written = std::fs::write(&tmp, serde_json::to_vec_pretty(info).unwrap_or_default())
            .and_then(|_| std::fs::rename(&tmp, dir.join("slot.json")));
        if written.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }

    /// A lane kept in `slot`, run as nothing: to remove it.
    fn slot_run(&self, slot: &str, info: &SlotInfo) -> LaneRun {
        let mut run = self.run(&NodeLease {
            id: slot.to_string(),
            member: info.member.clone(),
            repo: info.repo.clone(),
            lane: info.lane.clone(),
            need: Need::default(),
            disk: 0,
            images: false,
            warning: None,
            expires: 0,
            admitted: 0,
            stack_file: None,
            slot: Some(slot.to_string()),
            kept: None,
        });
        // Its stack file, wherever it was: the default unless one is there.
        if let Some(f) = ["devstack.toml"]
            .iter()
            .find(|f| run.src().join(f).is_file())
        {
            run.stack_file = (*f).to_string();
        }
        run
    }

    /// One more keeps `slot` busy, until its [`Self::slot_done`].
    fn slot_begin(&self, slot: &str) {
        *self
            .slot_busy
            .lock()
            .unwrap()
            .entry(slot.to_string())
            .or_default() += 1;
    }

    /// One of what keeps `slot` busy is done.
    fn slot_done(&self, slot: &str) {
        let mut busy = self.slot_busy.lock().unwrap();
        if let Some(n) = busy.get_mut(slot) {
            *n -= 1;
            if *n == 0 {
                busy.remove(slot);
            }
        }
    }

    /// Remove slot `slot` and what it holds, unless a lease uses it.
    fn purge_slot(&self, slot: &str, info: &SlotInfo, why: &str) -> bool {
        {
            // Under the leases' lock: a lease of the lane admitted meanwhile
            // would get a directory that is being removed (`admit` checks).
            let leases = self.leases.lock().unwrap();
            if leases.values().any(|l| l.slot.as_deref() == Some(slot)) {
                return false;
            }
            let mut busy = self.slot_busy.lock().unwrap();
            if busy.contains_key(slot) {
                return false;
            }
            busy.insert(slot.to_string(), 1);
            self.slot_purging.lock().unwrap().insert(slot.to_string());
        }
        eprintln!(
            "lane `{}` of {} ({}): its kept data goes: {why}",
            info.lane, info.member, info.repo
        );
        self.slot_run(slot, info).purge(self.container.as_deref());
        remove_built_images(slot);
        self.volume_use.lock().unwrap().remove(slot);
        self.slot_purging.lock().unwrap().remove(slot);
        self.slot_done(slot);
        true
    }

    /// What the running leases are still expected to write to disk
    /// (ADR-0049 point 7): each its `disk`, less what its slot (and the
    /// images it brought) take as last measured.
    fn disk_held_in(&self, leases: &HashMap<String, NodeLease>) -> u64 {
        let runs = self.runs.lock().unwrap();
        leases
            .values()
            .map(|l| {
                let (lane, images) = runs.written(&l.id);
                l.disk
                    .saturating_sub(lane.saturating_add(if l.images { images } else { 0 }))
            })
            .sum()
    }

    /// What the node could free: kept lanes no lease uses (ADR-0038), as
    /// last counted, all but `except`.
    fn could_free_in(&self, leases: &HashMap<String, NodeLease>, except: Option<&str>) -> u64 {
        let sizes = self.slot_use.lock().unwrap();
        self.kept()
            .into_iter()
            .filter(|(slot, _)| Some(slot.as_str()) != except)
            .filter(|(slot, _)| !leases.values().any(|l| l.slot.as_deref() == Some(slot)))
            .filter_map(|(slot, _)| sizes.get(&slot).copied())
            .sum()
    }

    /// What a new lane can take of the disk (ADR-0049 point 6): free, and
    /// what the node could free, less what its lanes are still expected
    /// to write.
    fn disk_room(&self) -> Option<u64> {
        let free = devstack_proc::disk_free(&self.root)?;
        let leases = self.leases.lock().unwrap();
        let held = self.disk_held_in(&leases);
        Some(
            free.saturating_add(self.could_free_in(&leases, None))
                .saturating_sub(held),
        )
    }

    /// Kept lanes go, the least recently used first, until `want` bytes
    /// are free or none is left; never `except`, nor one a lease uses.
    fn make_room(&self, want: u64, except: &str) {
        let mut kept: Vec<(String, SlotInfo)> = self
            .kept()
            .into_iter()
            .filter(|(slot, _)| slot != except)
            .collect();
        kept.sort_by_key(|(_, i)| i.used);
        for (slot, info) in kept {
            if devstack_proc::disk_free(&self.root).is_none_or(|f| f >= want) {
                return;
            }
            self.purge_slot(&slot, &info, "a lane starting here needs its room on disk");
        }
    }

    /// Measure what leases `ls` take of the disk now: their slots, with
    /// their volumes, and the images of their containers (ADR-0049 point
    /// 2). `images` taken before, when their containers are gone by now.
    fn measure_disk(&self, ls: &[NodeLease], images: Option<&HashMap<String, u64>>) {
        if ls.is_empty() {
            return;
        }
        let volumes = slot_volumes();
        let measured;
        let images = match images {
            Some(i) => i,
            None => {
                measured = crate::runs::images_by(&lease_keys(ls));
                &measured
            }
        };
        let mut sizes = Vec::new();
        for l in ls {
            let lane = tree_size(&self.dir_of(l))
                + l.slot
                    .as_ref()
                    .and_then(|sl| volumes.get(sl))
                    .copied()
                    .unwrap_or(0);
            self.runs
                .lock()
                .unwrap()
                .disk(&l.id, lane, images.get(&l.id).copied().unwrap_or(0));
            if let Some(sl) = &l.slot {
                sizes.push((sl.clone(), lane));
            }
        }
        self.slot_use.lock().unwrap().extend(sizes);
        let mut vu = self.volume_use.lock().unwrap();
        for (slot, bytes) in volumes {
            vu.insert(slot, bytes);
        }
    }

    fn in_use(&self, slot: &str) -> bool {
        self.leases
            .lock()
            .unwrap()
            .values()
            .any(|l| l.slot.as_deref() == Some(slot))
    }

    /// How long a stopped lane stays here, and what a member's kept lanes
    /// may take (ADR-0038 point 3).
    fn keep(&self) -> (Duration, u64) {
        let policies = self.policies();
        let all: Vec<&crate::policy::NodePolicy> = policies.iter().collect();
        let p = crate::policy::NodePolicy::strictest(&all);
        let hours = p.as_ref().and_then(|p| p.keep_lanes_hours);
        let gib = p.as_ref().and_then(|p| p.keep_lanes_gib);
        (
            hours.map_or(
                if self.dedicated {
                    KEEP_DEDICATED
                } else {
                    KEEP_OTHER
                },
                |h| Duration::from_secs(h * 3600),
            ),
            gib.unwrap_or(KEEP_GIB) << 30,
        )
    }

    /// Kept lanes past their time go, and the least recently used of a
    /// member over its room (ADR-0038 point 3).
    /// What slot `slot` takes: its directory, and its volumes as the last
    /// sweep counted them.
    fn slot_bytes(&self, slot: &str) -> u64 {
        tree_size(&self.slots().join(slot))
            + self
                .volume_use
                .lock()
                .unwrap()
                .get(slot)
                .copied()
                .unwrap_or(0)
    }

    fn sweep_slots(&self) {
        let (keep_for, room) = self.keep();
        let now = crate::store::now();
        let kept = self.kept();
        // Volumes count against the room too (ADR-0038 point 3).
        *self.volume_use.lock().unwrap() = if kept.is_empty() {
            HashMap::new()
        } else {
            slot_volumes()
        };
        // Short of disk: the lane images no lane uses go (built again from
        // their Dockerfile when one starts).
        if devstack_proc::disk_free(&self.root)
            .is_some_and(|f| f < 2 * crate::coordinator::MIN_DISK_FREE)
        {
            remove_lane_images(&self.owner(), |_| true);
        }
        let mut by_member: HashMap<String, Vec<(String, SlotInfo, u64)>> = HashMap::new();
        for (slot, info) in kept {
            if self.in_use(&slot) {
                continue;
            }
            if now.saturating_sub(info.used) >= keep_for.as_secs() {
                self.purge_slot(&slot, &info, "kept for as long as this node keeps lanes");
                continue;
            }
            let bytes = self.slot_bytes(&slot);
            self.slot_use.lock().unwrap().insert(slot.clone(), bytes);
            by_member
                .entry(info.member.clone())
                .or_default()
                .push((slot, info, bytes));
        }
        *self.kept_use.lock().unwrap() = Some(crate::coordinator::Kept {
            lanes: by_member.values().map(Vec::len).sum(),
            bytes: by_member.values().flatten().map(|s| s.2).sum(),
        });
        for (_, mut slots) in by_member {
            let mut total: u64 = slots.iter().map(|s| s.2).sum();
            // The least recently used first.
            slots.sort_by_key(|s| s.1.used);
            for (slot, info, bytes) in slots {
                if total <= room {
                    break;
                }
                if self.purge_slot(
                    &slot,
                    &info,
                    "the member's kept lanes take more than this node keeps",
                ) {
                    total = total.saturating_sub(bytes);
                }
            }
        }
    }

    /// What tells this agent's lane containers, images and network rules
    /// from another pool's agent's on the same engine (ADR-0034).
    fn owner(&self) -> String {
        crate::lanerun::owner_tag(&self.secret)
    }

    /// Left over by an agent that stopped between a lease's end and its
    /// cleanup: lane directories, slots without what they say about
    /// themselves, lane containers no lease has, and their network rules.
    /// Only this agent's: another pool's agent on the same engine keeps its
    /// own (ADR-0034).
    fn sweep_orphans(&self) {
        let leases: HashSet<String> = self.leases.lock().unwrap().keys().cloned().collect();
        if let Ok(rd) = std::fs::read_dir(self.lanes()) {
            for e in rd.flatten() {
                let id = e.file_name().to_string_lossy().into_owned();
                if !leases.contains(&id) {
                    let _ = std::fs::remove_dir_all(e.path());
                }
            }
        }
        if let Ok(rd) = std::fs::read_dir(self.slots()) {
            for e in rd.flatten() {
                if !e.path().join("slot.json").is_file() {
                    let _ = std::fs::remove_dir_all(e.path());
                }
            }
        }
        let owner = self.owner();
        let mut live = Vec::new();
        for c in lane_containers() {
            let ours = match c.owner.as_str() {
                o if o == owner => true,
                // An older agent's: labelled without its owner. Its mirror
                // is under this agent's state if it is this agent's.
                "" => mounts_under(&c.name, &self.root),
                _ => false,
            };
            if ours && !self.leases.lock().unwrap().contains_key(&c.lease) {
                let _ = std::process::Command::new("docker")
                    .args(["rm", "-f", "--", &c.name])
                    .output();
            } else {
                live.push(c.lease);
            }
        }
        // The rules of lanes it no longer holds (ADR-0033 point 5), read
        // which they are when nothing sets rules: a lane starting meanwhile
        // is held already.
        crate::egress::cleanup(&self.root, &owner, || {
            let held = self.leases.lock().unwrap().keys().cloned().collect();
            (held, live)
        });
    }

    fn update_state(&self, at: u64, state: &str, target: Option<String>, error: Option<String>) {
        *self.update.lock().unwrap() = Some(crate::coordinator::UpdateReport {
            at,
            state: state.into(),
            target,
            error,
        });
        self.changed.notify_one();
    }

    /// Why this agent refuses an update an admin orders, if it does: its
    /// own policy says so, it runs in a container (its image is updated
    /// instead), or it was started without a way to install one.
    fn no_update(&self) -> Option<String> {
        if self
            .local_policy
            .as_ref()
            .is_some_and(|p| p.remote_update == Some(false))
        {
            return Some(
                "this node's own policy refuses remote updates (`remote_update = false`)".into(),
            );
        }
        if self.container.is_some() {
            return Some("the agent runs in a container: update its image".into());
        }
        if self.on_update.is_none() {
            return Some("this agent cannot update itself".into());
        }
        None
    }

    /// Carry out an update an admin ordered (ADR-0039): refused by this
    /// node's own policy or in a container; else, by default, once its lanes
    /// have ended (no new one admitted meanwhile), the release installed and
    /// the agent started again on it.
    fn carry_out(self: Arc<Self>, order: crate::coordinator::UpdateOrder) {
        let at = order.at;
        let target = order.version.clone();
        if let Some(why) = self.no_update() {
            eprintln!("node agent: {} orders an update, refused: {why}", order.by);
            crate::telemetry::node_log(
                "warn",
                "node_update_refused",
                &[("by", order.by.clone()), ("error", why.clone())],
            );
            return self.update_state(at, "refused", target, Some(why));
        }
        let Some(install) = self.on_update.clone() else {
            return;
        };
        eprintln!(
            "node agent: {} orders an update to {}",
            order.by,
            target.as_deref().unwrap_or("the latest release")
        );
        if order.drain {
            // The state first: a drain an earlier order ends meanwhile sees
            // it is no longer its own and leaves admission to this one.
            self.update_state(at, "draining", target.clone(), None);
            self.draining
                .store(true, std::sync::atomic::Ordering::SeqCst);
            let until = Instant::now() + DRAIN_FOR;
            while !self.leases.lock().unwrap().is_empty() {
                // The admin withdrew the order (or the coordinator forgot
                // it): the node takes lanes again.
                if self.update_withdrawn(at) {
                    if self.end_drain(at, "cancelled", target, None) {
                        eprintln!("node agent: the update was cancelled; lanes are admitted again");
                    }
                    return;
                }
                if Instant::now() >= until {
                    let why = format!(
                        "its lanes still ran after {} hours: order it again, or without waiting for them",
                        DRAIN_FOR.as_secs() / 3600
                    );
                    if self.end_drain(at, "failed", target, Some(why.clone())) {
                        eprintln!("node agent: the update gave up: {why}");
                        crate::telemetry::node_log("warn", "node_update_failed", &[("error", why)]);
                    }
                    return;
                }
                std::thread::sleep(Duration::from_secs(5));
            }
        }
        self.update_state(at, "installing", target.clone(), None);
        match install(&order) {
            Ok(None) => {
                self.draining
                    .store(false, std::sync::atomic::Ordering::SeqCst);
                self.update_state(at, "done", target, None);
            }
            Err(e) => {
                self.draining
                    .store(false, std::sync::atomic::Ordering::SeqCst);
                eprintln!("node agent: the update failed: {e}");
                crate::telemetry::node_log("warn", "node_update_failed", &[("error", e.clone())]);
                self.update_state(at, "failed", target, Some(e));
            }
            Ok(Some(binary)) => {
                self.update_state(at, "restarting", target.clone(), None);
                eprintln!("node agent: starting again on {}", binary.display());
                // On success this process ends (or is replaced); on failure
                // it goes on with the release it runs.
                let e = restart_on(&binary);
                self.draining
                    .store(false, std::sync::atomic::Ordering::SeqCst);
                eprintln!(
                    "node agent: {e}; it goes on running {}",
                    env!("CARGO_PKG_VERSION")
                );
                crate::telemetry::node_log("warn", "node_update_failed", &[("error", e.clone())]);
                self.update_state(at, "failed", target, Some(e));
            }
        }
    }

    /// End the wait for lanes of the update ordered at `at`, unless a later
    /// order is under way already: admission opens again and the report
    /// says `state`. Whether it was still this order's.
    fn end_drain(
        &self,
        at: u64,
        state: &str,
        target: Option<String>,
        error: Option<String>,
    ) -> bool {
        let mut u = self.update.lock().unwrap();
        if u.as_ref().is_none_or(|u| u.at != at) {
            return false;
        }
        self.draining
            .store(false, std::sync::atomic::Ordering::SeqCst);
        *u = Some(crate::coordinator::UpdateReport {
            at,
            state: state.into(),
            target,
            error,
        });
        drop(u);
        self.changed.notify_one();
        true
    }

    /// Whether the coordinator's last answer no longer carries the update
    /// ordered at `at`.
    fn update_withdrawn(&self, at: u64) -> bool {
        self.update_withdrawn
            .lock()
            .unwrap()
            .is_some_and(|w| w == at)
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

    /// Under pressure only ([`MAX_MEMBER_BLOBS`]): remove the blobs of
    /// `member` no mirror of its leases here was written from. A lease that
    /// ended keeps its blobs, so the same lane started again sends only what
    /// changed (ADR-0029 point 2, ADR-0038). Blobs younger than
    /// [`BLOB_YOUNG`] stay too: a new lease's, uploaded and not yet applied,
    /// are in no manifest yet.
    fn gc_blobs(&self, member: &str) {
        let mut dirs: Vec<PathBuf> = self
            .leases
            .lock()
            .unwrap()
            .values()
            .filter(|l| l.member == member)
            .map(|l| self.dir_of(l))
            .collect();
        dirs.extend(
            self.kept()
                .into_iter()
                .filter(|(_, i)| i.member == member)
                .map(|(slot, _)| self.slots().join(slot)),
        );
        let keep: HashSet<String> = dirs
            .iter()
            .flat_map(|d| manifest(d).into_values())
            .collect();
        let dir = self.blobs(member);
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let young = e
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .is_none_or(|age| age < BLOB_YOUNG);
                if !young && !keep.contains(e.file_name().to_string_lossy().as_ref()) {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        self.blob_use.lock().unwrap().remove(member);
    }

    /// What members the coordinator no longer lists left here goes: their
    /// blobs, the content of their files (ADR-0029 Consequences, ADR-0038
    /// point 3). Only on an answer: a coordinator that does not answer
    /// purges nothing.
    fn purge_gone_members(&self) {
        let Ok(list) = self.local.members() else {
            return;
        };
        let approved: HashSet<String> = list
            .into_iter()
            .filter(|m| m.state == crate::store::State::Approved)
            .map(|m| m.id)
            .collect();
        let holding: HashSet<String> = self
            .leases
            .lock()
            .unwrap()
            .values()
            .map(|l| l.member.clone())
            .collect();
        for (slot, info) in self.kept() {
            if !approved.contains(&info.member) {
                self.purge_slot(&slot, &info, "the pool no longer lists its member");
            }
        }
        // The lane images built for them (ADR-0028).
        remove_lane_images(&self.owner(), |m| {
            !approved.contains(m) && !holding.contains(m)
        });
        let Ok(rd) = std::fs::read_dir(self.root.join("blobs")) else {
            return;
        };
        for e in rd.flatten() {
            let member = e.file_name().to_string_lossy().into_owned();
            if !approved.contains(&member) && !holding.contains(&member) {
                let _ = std::fs::remove_dir_all(e.path());
                self.blob_use.lock().unwrap().remove(&member);
            }
        }
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
        self.available_in(&self.leases.lock().unwrap())
    }

    /// [`Inner::available`] with the leases' lock held: admission checks and
    /// records a lease in one step, so two at once cannot both take the
    /// same room (ADR-0026 point 4).
    fn available_in(&self, leases: &HashMap<String, NodeLease>) -> Amount {
        let offer = *self.offer.lock().unwrap();
        let now = crate::store::now();
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
            slot: l.slot.clone().unwrap_or_else(|| l.id.clone()),
            member: l.member.clone(),
            secret: self.secret.clone(),
            lane: l.lane.clone(),
            dir: self.dir_of(l),
            stack_file: l
                .stack_file
                .clone()
                .unwrap_or_else(|| "devstack.toml".into()),
            display_root: None,
            port_offset: 0,
            profile: None,
            relay_udp: false,
            memory_limit: None,
            cpu_limit: None,
            network: crate::policy::Network::Lan,
        }
    }

    /// End the bringing up of lease `id`'s containers, if it goes on, and
    /// return once it stopped: the caller takes them down, or starts the
    /// lane afresh, after it (#164). Blocking (Compose ends within a moment).
    fn end_bring_up(&self, id: &str) {
        let b = self.bringing.lock().unwrap().remove(id);
        if let Some(b) = b {
            b.ended.store(true, std::sync::atomic::Ordering::SeqCst);
            drop(b.running.lock().unwrap_or_else(|e| e.into_inner()));
        }
    }

    /// The phase `previous` of lease `id`'s start is over (another began,
    /// or the start ended): how long it took goes into its `phases`.
    fn phase_over(&self, id: &str, previous: Option<Result<String, String>>) {
        let Some(Ok(phase)) = previous else { return };
        let mut times = self.phase_times.lock().unwrap();
        if let Some((done, since)) = times.get_mut(id) {
            // "starting" is only the moment between the request and the
            // first phase the lane reports.
            if phase != "starting" {
                done.push(PhaseTime {
                    phase,
                    ms: since.elapsed().as_millis() as u64,
                });
            }
            *since = std::time::Instant::now();
        }
    }

    /// A lease is over: its lane stops. Its slot stays, with the mirror,
    /// the builds and the volumes, for the next lease of the same lane
    /// (ADR-0038), unless its stack file says `keep_data = false`; a lease
    /// of an older agent takes its directory along. Off the caller's
    /// thread, Docker takes its time; the slot is busy until it is done.
    fn drop_lease(self: &Arc<Self>, l: NodeLease) {
        crate::telemetry::node_log(
            "info",
            "node_lease_ended",
            &[
                ("lease", l.id.clone()),
                ("repository", l.repo.clone()),
                ("lane", l.lane.clone()),
                (
                    "held_s",
                    crate::store::now().saturating_sub(l.admitted).to_string(),
                ),
            ],
        );
        self.daemons.lock().unwrap().remove(&l.id);
        self.phase_times.lock().unwrap().remove(&l.id);
        // What it needed is on offer again: the coordinator hears it now.
        self.changed.notify_one();
        if let Some(slot) = &l.slot {
            self.slot_begin(slot);
        }
        let s = self.clone();
        std::thread::spawn(move || {
            // Nothing of it starts once it is taken down.
            s.end_bring_up(&l.id);
            // What its run took (ADR-0049 point 2): the images while its
            // containers are there, its slot once they stopped.
            let images = crate::runs::images_by(&lease_keys(std::slice::from_ref(&l)));
            let run = s.run(&l);
            match &l.slot {
                Some(slot) => {
                    let keep = l.stack_file.is_none() || run.keeps_data();
                    if keep {
                        if l.stack_file.is_some() {
                            run.stop(s.container.as_deref());
                        }
                        s.measure_disk(std::slice::from_ref(&l), Some(&images));
                        s.note_slot(
                            slot,
                            &SlotInfo {
                                member: l.member.clone(),
                                repo: l.repo.clone(),
                                lane: l.lane.clone(),
                                used: crate::store::now(),
                            },
                        );
                    } else {
                        s.measure_disk(std::slice::from_ref(&l), Some(&images));
                        run.purge(s.container.as_deref());
                        remove_built_images(slot);
                        s.slot_use.lock().unwrap().remove(slot);
                    }
                    s.slot_done(slot);
                }
                None => {
                    if l.stack_file.is_some() {
                        run.purge(s.container.as_deref());
                        remove_built_images(&l.id);
                    }
                    let _ = std::fs::remove_dir_all(&run.dir);
                }
            }
            s.runs.lock().unwrap().end(&l.id);
            // Its blobs stay: the same lane started again sends only what
            // changed (#99); they go under pressure, or with the member.
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
    // A certificate the list does not name is asked about at most every
    // few seconds: unknown callers cannot make every request a round trip
    // to the coordinator.
    let fresh = {
        let m = s.members.lock().unwrap();
        m.0.is_some_and(|at| at.elapsed() < Duration::from_secs(30)) && m.1.contains_key(&id)
    };
    let ask = !fresh && {
        let mut last = s.members_asked.lock().unwrap();
        let due = last.is_none_or(|t| t.elapsed() >= MEMBERS_ASKED_AGAIN);
        if due {
            *last = Some(Instant::now());
        }
        due
    };
    let mut asked = false;
    if ask {
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
        .route("/v1/leases", post(admit_logged))
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
        .route("/v1/leases/{id}/view/{*rest}", get(view))
        .route("/v1/slots", get(slots_list).delete(slots_purge))
        .route(
            "/v1/blobs/{hash}",
            put(blob).layer(DefaultBodyLimit::max(MAX_BLOB)),
        )
        .with_state(s)
}

/// `GET /v1/slots`: the caller's lanes kept here (ADR-0038 point 5).
async fn slots_list(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
) -> ApiResult<Json<Vec<SlotView>>> {
    let who = caller(&s, &peer).await?;
    let s2 = s.clone();
    let out = tokio::task::spawn_blocking(move || {
        s2.kept()
            .into_iter()
            .filter(|(_, i)| i.member == who)
            .map(|(slot, i)| SlotView {
                active: s2.in_use(&slot),
                bytes: s2.slot_bytes(&slot),
                repo: i.repo,
                lane: i.lane,
                used: i.used,
            })
            .collect()
    })
    .await
    .map_err(internal)?;
    Ok(Json(out))
}

#[derive(Deserialize)]
struct SlotQuery {
    repo: String,
    lane: String,
}

/// `DELETE /v1/slots?repo=&lane=`: throw the caller's kept lane away (its
/// worktree is gone, or it is to start fresh). `204`, also when there is
/// none; `409` while a lease runs there.
async fn slots_purge(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    axum::extract::Query(q): axum::extract::Query<SlotQuery>,
) -> ApiResult<StatusCode> {
    let who = caller(&s, &peer).await?;
    let slot = s.slot_name(&who, &q.repo, &q.lane);
    if s.in_use(&slot) {
        return Err(err(
            StatusCode::CONFLICT,
            format!("lane `{}` runs here: stop it first", q.lane),
        ));
    }
    let s2 = s.clone();
    tokio::task::spawn_blocking(move || {
        // The end of its last lease may still be taking it down.
        let until = Instant::now() + Duration::from_secs(120);
        while s2.slot_busy.lock().unwrap().contains_key(&slot) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(250));
        }
        if let Some((_, info)) = s2.kept().into_iter().find(|(name, _)| *name == slot) {
            s2.purge_slot(&slot, &info, "its owner asked");
        }
    })
    .await
    .map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
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
        devstack: Some(crate::version::Release::this()),
    }))
}

/// [`admit`], with a refusal in the node's log: the requester sees it, the
/// pool's operator did not (ADR-0037).
async fn admit_logged(
    st: AxState<Shared>,
    peer: Extension<Peer>,
    a: Json<Admit>,
) -> ApiResult<(StatusCode, Json<NodeLease>)> {
    let id = a.id.clone();
    let r = admit(st, peer, a).await;
    match &r {
        Ok((StatusCode::CREATED, Json(l))) => crate::telemetry::node_log(
            "info",
            "node_lease_admitted",
            &[
                ("lease", l.id.clone()),
                ("repository", l.repo.clone()),
                ("lane", l.lane.clone()),
                ("cpus", format!("{:.2}", l.need.cpus)),
                ("memory_mib", (l.need.memory >> 20).to_string()),
                ("kept", l.kept.is_some().to_string()),
            ],
        ),
        Err((status, e)) => crate::telemetry::node_log(
            "warn",
            "node_lease_refused",
            &[
                ("lease", id),
                ("status", status.as_u16().to_string()),
                ("error", e.clone()),
            ],
        ),
        Ok(_) => {}
    }
    r
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
    if s.draining.load(std::sync::atomic::Ordering::SeqCst) {
        return Err(err(
            StatusCode::SERVICE_UNAVAILABLE,
            format!(
                "{} is updating its devstack: no new lanes until it is back",
                s.name
            ),
        ));
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
    // An older coordinator did not check them.
    crate::policy::valid_lease_names(&lease.repo, &lease.lane)
        .map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
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
    // a lane to need at least the least; checked below, with the lease
    // recorded in the same step.
    let mut lease = lease;
    lease.need = crate::policy::floor_need(lease.need);
    let now = crate::store::now();
    let slot = s.slot_name(&who, &lease.repo, &lease.lane);
    // What an earlier lease of the lane kept here, and since when.
    let kept = std::fs::read(s.slots().join(&slot).join("slot.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<SlotInfo>(&b).ok())
        .filter(|_| s.slots().join(&slot).join("manifest.json").is_file())
        .map(|i| i.used);
    // Its disk (ADR-0049 point 6): the repository's images unless a lane of
    // it ran here, and its slot's part, less what a kept slot takes already.
    let images = !s.kept().iter().any(|(_, i)| i.repo == lease.repo);
    let disk = if images { lease.need.disk_repo } else { 0 }.saturating_add(lease.need.disk_lane);
    let there = if kept.is_some() {
        s.slot_use.lock().unwrap().get(&slot).copied().unwrap_or(0)
    } else {
        0
    };
    // GPUs it asks for are GPUs this machine has.
    if let Some(g) = lease.need.gpus.filter(|g| g.count > 0) {
        let fit = tokio::task::spawn_blocking(crate::share::gpus)
            .await
            .unwrap_or_default()
            .iter()
            .filter(|x| x.memory >= g.memory)
            .count();
        if (fit as u32) < g.count {
            return Err(err(
                StatusCode::CONFLICT,
                format!(
                    "{} has {fit} GPU(s) with {} or more, the lane asks for {}",
                    s.name,
                    crate::coordinator::gib(g.memory),
                    g.count
                ),
            ));
        }
    }
    let l = NodeLease {
        id: lease.id,
        member: who,
        repo: lease.repo,
        lane: lease.lane,
        need: lease.need,
        disk,
        images,
        warning: None,
        expires: now + TTL.as_secs(),
        admitted: now,
        stack_file: None,
        slot: Some(slot.clone()),
        kept,
    };
    let mut leases = s.leases.lock().unwrap();
    let free = s.available_in(&leases);
    if l.need.cpus > free.cpus || l.need.memory > free.memory {
        return Err(err(
            StatusCode::CONFLICT,
            format!(
                "{} offers {:.1} CPU and {} MiB now; the lane needs {:.1} CPU and {} MiB",
                s.name,
                free.cpus,
                free.memory >> 20,
                l.need.cpus,
                l.need.memory >> 20
            ),
        ));
    }
    // Room on disk, with what its lanes are still to write held, and what
    // kept lanes take counted as room: they go first (ADR-0049 points 6, 7).
    let mut prune = None;
    if let Some(free) = devstack_proc::disk_free(&s.root) {
        let held = s.disk_held_in(&leases);
        let beyond = disk.saturating_sub(there);
        let want = beyond
            .saturating_add(held)
            .saturating_add(crate::coordinator::MIN_DISK_FREE);
        let room = free.saturating_add(s.could_free_in(&leases, Some(&slot)));
        if room < want {
            let gib = crate::coordinator::gib;
            return Err(err(
                StatusCode::CONFLICT,
                format!(
                    "{} has {} for lanes on disk (free, and kept lanes it could remove); the lane needs {} there, its lanes are to write {} yet, and {} stay free",
                    s.name,
                    gib(room),
                    gib(beyond),
                    gib(held),
                    gib(crate::coordinator::MIN_DISK_FREE)
                ),
            ));
        }
        if free < want {
            prune = Some(want);
        }
    }
    if s.slot_purging.lock().unwrap().contains(&slot) {
        return Err(err(
            StatusCode::CONFLICT,
            format!(
                "lane `{}`'s kept data is being removed here: try again in a moment",
                l.lane
            ),
        ));
    }
    // One lease at a time per lane: its data is one.
    if let Some(other) = leases.values().find(|x| x.slot.as_deref() == Some(&slot)) {
        return Err(err(
            StatusCode::CONFLICT,
            format!(
                "lane `{}` of `{}` runs here already (lease {}): stop it first",
                l.lane, l.repo, other.id
            ),
        ));
    }
    std::fs::create_dir_all(s.dir_of(&l).join("src")).map_err(internal)?;
    s.note_slot(
        &slot,
        &SlotInfo {
            member: l.member.clone(),
            repo: l.repo.clone(),
            lane: l.lane.clone(),
            used: now,
        },
    );
    leases.insert(l.id.clone(), l.clone());
    s.save(&leases);
    s.runs.lock().unwrap().begin(&l.id, &l.repo);
    drop(leases);
    if let Some(want) = prune {
        let s2 = s.clone();
        let keep = slot.clone();
        std::thread::spawn(move || s2.make_room(want, &keep));
    }
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
    let mut out = l.clone();
    s.save(&leases);
    drop(leases);
    // Short of disk: its requester hears it with each renewal (ADR-0049
    // point 8); the lane goes on.
    if let Some(free) =
        devstack_proc::disk_free(&s.root).filter(|f| *f < crate::coordinator::MIN_DISK_FREE)
    {
        out.warning = Some(format!(
            "{} has only {} of disk free: the lane may fail to write",
            s.name,
            crate::coordinator::gib(free)
        ));
    }
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

/// The bytes the files under `dir` take, all the way down (no link followed).
fn tree_size(dir: &Path) -> u64 {
    let mut total = 0;
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            match e.file_type() {
                Ok(t) if t.is_dir() => todo.push(e.path()),
                Ok(t) if t.is_file() => total += e.metadata().map_or(0, |m| m.len()),
                _ => {}
            }
        }
    }
    total
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
    // Up to about 250 000 lookups: not on the async workers.
    let hashes = tokio::task::spawn_blocking(move || {
        let mut seen = HashSet::new();
        h.hashes
            .into_iter()
            .filter(|x| valid_hash(x) && seen.insert(x.clone()) && !blobs.join(x).exists())
            .collect()
    })
    .await
    .map_err(internal)?;
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
/// root, `/`-separated. A `:` is a file name's on Linux, where lanes run; on
/// Windows it would name a drive or a stream.
fn safe_path(p: &str) -> Option<PathBuf> {
    let p = p.replace('\\', "/");
    if cfg!(windows) && p.contains(':') {
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
        let written = crate::safefs::write_beneath(&src, &rel, mode, |to| {
            let h = crate::safefs::copy_hashing(&mut from, to)?;
            if h == f.hash {
                Ok(())
            } else {
                Err(std::io::Error::other(format!(
                    "blob {} does not have its hash",
                    f.hash
                )))
            }
        });
        match written {
            Ok(()) => {
                have.insert(f.path.clone(), f.hash.clone());
                out.written += 1;
            }
            // One path that cannot be written (a directory the lane made
            // root's) holds up no other (#128): it is named, and tried again
            // with the next sync.
            Err(e) => {
                have.remove(&f.path);
                out.failed.push(format!("{}: {e}", f.path));
            }
        }
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
    /// The profile it runs with (ADR-0041); absent: none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// The requester relays the lane's UDP ports (ADR-0056): the lane's
    /// daemon is told, and the lane's state names them. An older requester
    /// does not send it, an older agent does not read it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub relay_udp: bool,
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
    /// Those published as UDP, when the start asked for them (`relay_udp`,
    /// ADR-0056). An older agent does not send it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub infra_udp_ports: Vec<u16>,
    /// The phases of its last start that are over, in order, each with how
    /// long it took, measured here (the requester polls, so it would see a
    /// phase only to within its polling interval, or not at all). A failed
    /// start's last entry is the phase it failed in. Older agents do not
    /// send it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub phases: Vec<PhaseTime>,
}

/// A phase of a lane's start that is over, and how long it took.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhaseTime {
    pub phase: String,
    pub ms: u64,
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
    let (stack_file, display_root, port_offset, profile, relay_udp) = body.map_or_else(
        || (default_stack_file(), None, 0, None, false),
        |Json(b)| {
            (
                b.stack_file,
                b.display_root,
                b.port_offset,
                b.profile,
                b.relay_udp,
            )
        },
    );
    if let Some(p) = &profile
        && !p
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(err(StatusCode::BAD_REQUEST, "not a profile's name"));
    }
    // Its run is the profile's (ADR-0049 point 3).
    s.runs.lock().unwrap().profile(&id, profile.clone());
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
    // The lane's last lease may still be taking its containers down.
    if let Some(slot) = l.slot.clone() {
        let s2 = s.clone();
        let free = tokio::task::spawn_blocking(move || {
            let until = Instant::now() + Duration::from_secs(120);
            while s2.slot_busy.lock().unwrap().contains_key(&slot) {
                if Instant::now() > until {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(250));
            }
            true
        })
        .await
        .map_err(internal)?;
        if !free {
            return Err(err(
                StatusCode::CONFLICT,
                "the lane's last run is still being taken down here",
            ));
        }
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
    s.phase_times
        .lock()
        .unwrap()
        .insert(id.clone(), (Vec::new(), std::time::Instant::now()));
    let s2 = s.clone();
    let lid = id.clone();
    // The whole start, every phase: `phase_times` restarts its clock at each.
    let t0 = std::time::Instant::now();
    tokio::task::spawn_blocking(move || {
        // A sync that began before the start ends first; none begins now.
        while s2.applying.lock().unwrap().contains_key(&lid) {
            std::thread::sleep(Duration::from_millis(50));
        }
        // The containers of its last start, if they still come up, stop
        // coming up before this one reads the lane's files.
        s2.end_bring_up(&lid);
        let bringing = Arc::new(Bringing::default());
        s2.bringing
            .lock()
            .unwrap()
            .insert(lid.clone(), bringing.clone());
        let mut run = s2.run(&l);
        run.display_root = display_root;
        run.port_offset = port_offset;
        run.profile = profile;
        run.relay_udp = relay_udp;
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
            let previous = s3
                .starting
                .lock()
                .unwrap()
                .insert(pid.clone(), Ok(p.to_string()));
            s3.phase_over(&pid, previous);
        };
        let result = run.start(&bin, s2.container.as_deref(), &phase);
        // The last phase's time is in before the requester can see the
        // start over.
        let last = s2.starting.lock().unwrap().get(&lid).cloned();
        s2.phase_over(&lid, last.clone());
        let ms = t0.elapsed().as_millis().to_string();
        let what = |extra: Vec<(&'static str, String)>| {
            let mut f = vec![
                ("lease", lid.clone()),
                ("repository", l.repo.clone()),
                ("lane", l.lane.clone()),
                ("ms", ms.clone()),
            ];
            f.extend(extra);
            f
        };
        let forget = || {
            let mut b = s2.bringing.lock().unwrap();
            if b.get(&lid).is_some_and(|x| Arc::ptr_eq(x, &bringing)) {
                b.remove(&lid);
            }
        };
        match result {
            Ok(started) => {
                crate::telemetry::node_log("info", "node_lane_started", &what(Vec::new()));
                let running = bringing.running.lock().unwrap();
                if bringing.ended() {
                    // Stopped, or its lease ended, while it started: what
                    // the start made after that goes too.
                    drop(running);
                    s2.starting.lock().unwrap().remove(&lid);
                    run.stop(s2.container.as_deref());
                    return;
                }
                // Its daemon answers: the requester sees the lane run, and
                // its containers come up under the daemon (#164).
                s2.daemons.lock().unwrap().insert(lid.clone(), started.addr);
                s2.starting.lock().unwrap().remove(&lid);
                let up = started
                    .infra
                    .run(&run, &|| bringing.ended(), &|group, took, e| {
                        let mut f = vec![
                            ("lease", lid.clone()),
                            ("repository", l.repo.clone()),
                            ("lane", l.lane.clone()),
                            ("group", group.to_string()),
                            ("ms", took.as_millis().to_string()),
                        ];
                        match e {
                            Some(e) => {
                                // Compose's reason, its last line: the
                                // first names the command and the node's
                                // paths.
                                let why = e.lines().rev().find(|l| !l.trim().is_empty());
                                f.push(("error", why.unwrap_or(e).trim().to_string()));
                                crate::telemetry::node_log("warn", "node_lane_infra_failed", &f);
                            }
                            None => crate::telemetry::node_log("info", "node_lane_infra_up", &f),
                        }
                    });
                if let Err(e) = up {
                    eprintln!("lane {} of {}: {e}", l.lane, l.member);
                }
                drop(running);
                forget();
            }
            Err(e) => {
                forget();
                let phase = match &last {
                    Some(Ok(p)) => p.clone(),
                    _ => String::new(),
                };
                crate::telemetry::node_log(
                    "warn",
                    "node_lane_not_started",
                    &what(vec![("phase", phase), ("error", e.to_string())]),
                );
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
    let t0 = std::time::Instant::now();
    let (repo, lane) = (l.repo.clone(), l.lane.clone());
    tokio::task::spawn_blocking(move || {
        s2.end_bring_up(&l.id);
        s2.run(&l).stop(s2.container.as_deref())
    })
    .await
    .map_err(internal)?;
    crate::telemetry::node_log(
        "info",
        "node_lane_stopped",
        &[
            ("lease", id.clone()),
            ("repository", repo),
            ("lane", lane),
            ("ms", t0.elapsed().as_millis().to_string()),
        ],
    );
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
    let phases = s
        .phase_times
        .lock()
        .unwrap()
        .get(&id)
        .map(|(p, _)| p.clone())
        .unwrap_or_default();
    match s.starting.lock().unwrap().get(&id).cloned() {
        Some(Ok(phase)) => {
            return Ok(Json(LaneState {
                started: true,
                phase: Some(phase),
                phases,
                ..LaneState::default()
            }));
        }
        Some(Err(e)) => {
            return Ok(Json(LaneState {
                started: true,
                error: Some(e),
                phases,
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
        .map(|d| (d.infra_ports.clone(), d.infra_udp_ports.clone()));
    let s2 = s.clone();
    let running = tokio::task::spawn_blocking(move || s2.run(&l).running())
        .await
        .map_err(internal)?;
    Ok(Json(LaneState {
        started: true,
        running,
        ready: running && infra_ports.is_some(),
        infra_udp_ports: infra_ports
            .as_ref()
            .map(|p| p.1.clone())
            .unwrap_or_default(),
        infra_ports: infra_ports.map(|p| p.0).unwrap_or_default(),
        phases,
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
    let d = tokio::task::spawn_blocking(move || {
        let run = s2.run(&l);
        let d = run.attach(s2.container.as_deref())?;
        // Found running, not started by this agent (it restarted): what its
        // predecessor was still bringing up never comes up (#164).
        if !s2.bringing.lock().unwrap().contains_key(&l.id) {
            crate::laneinfra::interrupted(&run.dir, &run.src());
        }
        Ok::<_, crate::lanerun::RunError>(d)
    })
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
    let lease = async {
        let who = caller(&s, &peer).await?;
        lease_of(&s, &id, &who)
    };
    let l = match lease.await {
        Ok(l) => l,
        Err(e) => return e.into_response(),
    };
    // The lane's containers are the node's to reach: its daemon has no
    // Docker (#101).
    if let Some(route) = InfraRoute::parse(req.method(), &rest) {
        return infra(&s, &l.member.clone(), &l, route, req.uri()).await;
    }
    // A service route that names containers (a group's restart, a Compose
    // service by its name): the agent acts on them, the daemon on the rest
    // and answers (#128). Only the agent says it did.
    req.headers_mut().remove(crate::laneinfra::ACTED_HEADER);
    let mut failed = Vec::new();
    if let Some((route, name, action)) = named_route(req.method(), &rest) {
        let run = s.run(&l);
        let acted = tokio::task::spawn_blocking(move || act_named(&run, route, &name, &action))
            .await
            .unwrap_or(None);
        if let Some(mut a) = acted {
            if a.single && !a.failed.is_empty() {
                let (status, e) = a.failed.remove(0);
                return infra_err(status, e);
            }
            failed = a.failed;
            req.headers_mut().insert(
                crate::laneinfra::ACTED_HEADER,
                hyper::header::HeaderValue::from_static("1"),
            );
        }
    }
    let d = match daemon_of(&s, l).await {
        Ok(d) => d,
        Err(e) => return e.into_response(),
    };
    let query = req
        .uri()
        .query()
        .map(|q| format!("?{q}"))
        .unwrap_or_default();
    let res = pass(d, format!("/{rest}{query}"), upgrade_in, req).await;
    if failed.is_empty() {
        res
    } else {
        with_failures(res, failed).await
    }
}

/// `POST v1/services/{name}/{action}` or `POST v1/groups/{name}/{action}`
/// of the lane daemon's API, for `start`, `stop` and `restart`.
fn named_route(
    method: &hyper::Method,
    rest: &str,
) -> Option<(crate::laneinfra::Named, String, String)> {
    use crate::laneinfra::Named;
    if method != hyper::Method::POST {
        return None;
    }
    let rest = rest.strip_prefix("v1/")?;
    let (route, rest) = if let Some(r) = rest.strip_prefix("services/") {
        (Named::Services, r)
    } else {
        (Named::Groups, rest.strip_prefix("groups/")?)
    };
    let (name, action) = rest.split_once('/')?;
    (!name.is_empty() && matches!(action, "start" | "stop" | "restart"))
        .then(|| (route, name.to_string(), action.to_string()))
}

/// What the agent did to the containers a named route names.
struct Acted {
    /// The route names one Compose service (no group): its error is the
    /// answer, the daemon is asked only for its view.
    single: bool,
    /// The containers it could not act on: the status and the error.
    failed: Vec<(StatusCode, String)>,
}

/// Act on the containers that `action` of `name` on `route` names, as
/// [`crate::laneinfra::named`] reads the lane's stack file; `None` when it
/// names none, or the stack file does not read (the daemon answers then,
/// and acts on no container: it has no Docker).
fn act_named(
    run: &LaneRun,
    route: crate::laneinfra::Named,
    name: &str,
    action: &str,
) -> Option<Acted> {
    use crate::laneinfra;
    let stack = run.stack().ok()?;
    let targets = laneinfra::named(&stack, route, name, action);
    if targets.is_empty() {
        return None;
    }
    let single = route == laneinfra::Named::Services && !stack.groups.contains_key(name);
    let mut failed = Vec::new();
    for (group, service) in &targets {
        let r = laneinfra::may_act(&stack, group, service, action)
            .map_err(|e| (StatusCode::CONFLICT, e))
            .and_then(|()| {
                laneinfra::find(&run.dir, group, service).map_err(|e| (StatusCode::NOT_FOUND, e))
            })
            .and_then(|c| laneinfra::act(&c.name, action).map_err(|e| (StatusCode::CONFLICT, e)));
        if let Err((status, e)) = r {
            failed.push((status, if single { e } else { format!("{service}: {e}") }));
        }
    }
    // The new states, for the lane's dashboard at once.
    let _ = laneinfra::refresh(&run.dir, &run.src());
    Some(Acted { single, failed })
}

/// The daemon's answer to a group's restart, with the containers the agent
/// could not restart: `409` with every error, as the daemon names the
/// members that failed.
async fn with_failures(
    res: axum::response::Response,
    failed: Vec<(StatusCode, String)>,
) -> axum::response::Response {
    use http_body_util::BodyExt;
    let status = res.status();
    let body = res
        .into_body()
        .collect()
        .await
        .map(|b| b.to_bytes())
        .unwrap_or_default();
    let mut errors: Vec<String> = Vec::new();
    if !status.is_success() {
        let e = serde_json::from_slice::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v["error"].as_str().map(str::to_string))
            .unwrap_or_else(|| String::from_utf8_lossy(&body).trim().to_string());
        if !e.is_empty() {
            errors.push(e);
        }
    }
    errors.extend(failed.into_iter().map(|(_, e)| e));
    infra_err(StatusCode::CONFLICT, errors.join("\n"))
}

/// What of the lane daemon's `/v1/infra/{group}/{service}/…` the node answers
/// itself: a container's output, once or followed, and its start, stop and
/// restart.
#[derive(Debug, PartialEq, Eq)]
enum InfraRoute {
    Logs {
        group: String,
        service: String,
    },
    Stream {
        group: String,
        service: String,
    },
    Act {
        group: String,
        service: String,
        action: String,
    },
}

impl InfraRoute {
    fn parse(method: &hyper::Method, rest: &str) -> Option<Self> {
        let parts: Vec<&str> = rest.strip_prefix("v1/infra/")?.split('/').collect();
        let plain = |p: &str| {
            p.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric())
                && p.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        };
        if parts.len() < 3 || !plain(parts[0]) || !plain(parts[1]) {
            return None;
        }
        let (group, service) = (parts[0].to_string(), parts[1].to_string());
        match (method, &parts[2..]) {
            (&hyper::Method::GET, ["logs"]) => Some(Self::Logs { group, service }),
            (&hyper::Method::GET, ["logs", "stream"]) => Some(Self::Stream { group, service }),
            (&hyper::Method::POST, [a]) if matches!(*a, "start" | "stop" | "restart") => {
                Some(Self::Act {
                    group,
                    service,
                    action: a.to_string(),
                })
            }
            _ => None,
        }
    }
}

#[derive(Deserialize, Default)]
struct InfraQuery {
    tail: Option<usize>,
    grep: Option<String>,
    since: Option<String>,
}

/// An error as the lane's daemon would answer it.
fn infra_err(status: StatusCode, msg: impl Into<String>) -> axum::response::Response {
    use axum::response::IntoResponse;
    (status, Json(serde_json::json!({ "error": msg.into() }))).into_response()
}

/// Answer an [`InfraRoute`] of lease `l`, for its owner (checked before),
/// on the containers of the projects the agent started for it.
async fn infra(
    s: &Shared,
    who: &str,
    l: &NodeLease,
    route: InfraRoute,
    uri: &hyper::Uri,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let q: InfraQuery = axum::extract::Query::try_from_uri(uri)
        .map(|q| q.0)
        .unwrap_or_default();
    let dir = s.dir_of(l);
    let (group, service) = match &route {
        InfraRoute::Logs { group, service }
        | InfraRoute::Stream { group, service }
        | InfraRoute::Act { group, service, .. } => (group.clone(), service.clone()),
    };
    let found = {
        let dir = dir.clone();
        tokio::task::spawn_blocking(move || crate::laneinfra::find(&dir, &group, &service)).await
    };
    let c = match found {
        Ok(Ok(c)) => c,
        Ok(Err(e)) => return infra_err(StatusCode::NOT_FOUND, e),
        Err(e) => return infra_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    match route {
        InfraRoute::Logs { .. } => {
            let tail = q.tail.unwrap_or(200).min(20_000);
            let needle = q.grep.filter(|g| !g.is_empty()).map(|g| g.to_lowercase());
            // With a filter, Docker hands over more and the filter keeps the last `tail`.
            let fetch = if needle.is_some() { 20_000 } else { tail };
            let since = q.since.filter(|v| !v.is_empty());
            let name = c.name.clone();
            let out = tokio::task::spawn_blocking(move || {
                crate::laneinfra::logs(&name, fetch, since.as_deref())
            })
            .await;
            match out {
                Ok(Ok(text)) => {
                    let mut lines: Vec<&str> = text
                        .lines()
                        .rev()
                        .filter(|l| needle.as_ref().is_none_or(|g| l.to_lowercase().contains(g)))
                        .take(tail)
                        .collect();
                    lines.reverse();
                    (
                        [(hyper::header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                        lines.join("\n"),
                    )
                        .into_response()
                }
                Ok(Err(e)) => infra_err(StatusCode::BAD_GATEWAY, e),
                Err(e) => infra_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
            }
        }
        InfraRoute::Stream { .. } => {
            let Some(held) = Follow::take(s, who) else {
                return infra_err(
                    StatusCode::TOO_MANY_REQUESTS,
                    format!("you follow {MAX_FOLLOWS} container outputs here already"),
                );
            };
            let tail = q.tail.unwrap_or(500).min(5000);
            match crate::laneinfra::follow(&c.name, tail) {
                Ok(child) => sse_lines(child, held),
                Err(e) => infra_err(StatusCode::BAD_GATEWAY, e),
            }
        }
        InfraRoute::Act {
            action,
            group,
            service,
        } => {
            let name = c.name.clone();
            let run = s.run(l);
            let done = tokio::task::spawn_blocking(move || {
                // Only what the lane's group starts (ADR-0033, #128).
                let stack = run
                    .stack()
                    .map_err(|e| format!("the lane's stack file, read on its node: {e}"))?;
                crate::laneinfra::may_act(&stack, &group, &service, &action)?;
                let r = crate::laneinfra::act(&name, &action);
                // The new state, for the lane's dashboard at once.
                let _ = crate::laneinfra::refresh(&dir, &dir.join("src"));
                r
            })
            .await;
            match done {
                Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
                Ok(Err(e)) => infra_err(StatusCode::CONFLICT, e),
                Err(e) => infra_err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
            }
        }
    }
}

/// One of a member's container output streams ([`MAX_FOLLOWS`]): given
/// back when it is dropped, with the stream.
struct Follow {
    s: Shared,
    who: String,
}

impl Follow {
    fn take(s: &Shared, who: &str) -> Option<Self> {
        let mut follows = s.follows.lock().unwrap();
        let n = follows.entry(who.to_string()).or_insert(0);
        if *n >= MAX_FOLLOWS {
            return None;
        }
        *n += 1;
        Some(Self {
            s: s.clone(),
            who: who.to_string(),
        })
    }
}

impl Drop for Follow {
    fn drop(&mut self) {
        let mut follows = self.s.follows.lock().unwrap();
        if let Some(n) = follows.get_mut(&self.who) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                follows.remove(&self.who);
            }
        }
    }
}

/// A followed container's output as the lane's daemon streams it: one SSE
/// event `{"type":"line","text"}` per line, until the reader goes away or
/// the output ends. `held` is given back then.
fn sse_lines(mut child: std::process::Child, held: Follow) -> axum::response::Response {
    use axum::response::IntoResponse;
    let (tx, body) = http_body_util::channel::Channel::<bytes::Bytes, std::io::Error>::new(64);
    let tx = Arc::new(tokio::sync::Mutex::new(tx));
    let rt = tokio::runtime::Handle::current();
    let mut readers = Vec::new();
    for pipe in [
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn std::io::Read + Send>),
    ]
    .into_iter()
    .flatten()
    {
        let (tx, rt) = (tx.clone(), rt.clone());
        readers.push(std::thread::spawn(move || {
            use std::io::BufRead;
            for l in std::io::BufReader::new(pipe).lines() {
                let Ok(l) = l else { return };
                let event = format!(
                    "data: {}\n\n",
                    serde_json::json!({ "type": "line", "text": l })
                );
                let sent = rt
                    .block_on(async { tx.lock().await.send_data(bytes::Bytes::from(event)).await });
                if sent.is_err() {
                    return; // the reader is gone
                }
            }
        }));
    }
    // A reader gone or the output over: the follower ends. A container
    // that says nothing would keep a gone reader's follower running: a
    // comment every few seconds finds out, and keeps proxies from closing
    // the stream.
    let child = Arc::new(Mutex::new(child));
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let (child, done) = (child.clone(), done.clone());
        std::thread::spawn(move || {
            while !done.load(std::sync::atomic::Ordering::SeqCst) {
                std::thread::sleep(Duration::from_secs(5));
                let alive = rt.block_on(async {
                    tx.lock()
                        .await
                        .send_data(bytes::Bytes::from_static(b": ping\n\n"))
                        .await
                });
                if alive.is_err() {
                    let _ = child.lock().unwrap().kill();
                    return;
                }
            }
        });
    }
    std::thread::spawn(move || {
        for r in readers {
            let _ = r.join();
        }
        done.store(true, std::sync::atomic::Ordering::SeqCst);
        let mut c = child.lock().unwrap();
        let _ = c.kill();
        let _ = c.wait();
        drop(held);
    });
    (
        [
            (hyper::header::CONTENT_TYPE, "text/event-stream"),
            (hyper::header::CACHE_CONTROL, "no-cache"),
        ],
        axum::body::Body::new(body),
    )
        .into_response()
}

/// How long the node trusts the coordinator's word that someone may view a
/// lane (ADR-0040): unsharing holds within this.
const VIEW_FRESH: Duration = Duration::from_secs(5);

/// What of the lane daemon's API a viewer reads (ADR-0040 point 2): its
/// state and events, its services' logs and builds, its commands' runs,
/// its usage. Nothing that changes it, nor its environment.
fn viewable(rest: &str) -> bool {
    let p: Vec<&str> = rest.split('/').collect();
    let name = |s: &str| {
        s.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric())
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    };
    match p.as_slice() {
        ["v1", "stack" | "events" | "usage" | "logs"] => true,
        ["v1", "services", s, "logs" | "builds" | "build-log"] => name(s),
        ["v1", "services", s, "logs", "stream" | "files"] => name(s),
        ["v1", "commands", c, "runs"] => name(c),
        ["v1", "runs", id] | ["v1", "runs", id, "log"] => id.bytes().all(|b| b.is_ascii_digit()),
        ["v1", "runs", id, "log", "stream"] => id.bytes().all(|b| b.is_ascii_digit()),
        _ => false,
    }
}

/// May `who` view lease `l` (ADR-0040)? Its owner may; anyone else as the
/// coordinator says, asked at most every [`VIEW_FRESH`].
async fn may_view(s: &Shared, l: &NodeLease, who: &str) -> ApiResult<()> {
    if l.member == who {
        return Ok(());
    }
    let key = (l.id.clone(), who.to_string());
    let known = s
        .views_ok
        .lock()
        .unwrap()
        .get(&key)
        .filter(|(at, _)| at.elapsed() < VIEW_FRESH)
        .map(|(_, ok)| *ok);
    let ok = match known {
        Some(ok) => ok,
        None => {
            let s2 = s.clone();
            let lease = l.id.clone();
            let sharing = tokio::task::spawn_blocking(move || s2.local.lease_shared(&lease))
                .await
                .map_err(internal)?
                .map_err(|e| err(StatusCode::BAD_GATEWAY, format!("the coordinator: {e}")))?;
            let tags = s
                .members
                .lock()
                .unwrap()
                .1
                .get(who)
                .cloned()
                .unwrap_or_default();
            let ok = sharing.is_some_and(|sh| sh.admits(who, &tags));
            s.views_ok.lock().unwrap().insert(key, (Instant::now(), ok));
            ok
        }
    };
    if ok {
        Ok(())
    } else {
        Err(err(
            StatusCode::FORBIDDEN,
            "that lane is not shared with you",
        ))
    }
}

/// `GET /v1/leases/{id}/view/{*rest}`: a lane's daemon API, read only
/// (ADR-0040), for its owner and those it lets view it: [`viewable`] paths,
/// streams included, and the container logs the node answers itself.
async fn view(
    AxState(s): AxState<Shared>,
    Extension(peer): Extension<Peer>,
    UrlPath((id, rest)): UrlPath<(String, String)>,
    req: axum::extract::Request,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    if req.headers().contains_key(hyper::header::UPGRADE) {
        return err(StatusCode::FORBIDDEN, "no tunnel for viewers").into_response();
    }
    let lease = async {
        let who = caller(&s, &peer).await?;
        let l = s
            .leases
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .ok_or_else(|| err(StatusCode::NOT_FOUND, format!("no lane `{id}` here")))?;
        may_view(&s, &l, &who).await?;
        Ok::<_, (StatusCode, String)>((who, l))
    };
    let (who, l) = match lease.await {
        Ok(l) => l,
        Err(e) => return e.into_response(),
    };
    match InfraRoute::parse(&hyper::Method::GET, &rest) {
        Some(InfraRoute::Act { .. }) => {
            return err(StatusCode::FORBIDDEN, "viewers do not change a lane").into_response();
        }
        Some(route) => return infra(&s, &who, &l, route, req.uri()).await,
        None => {}
    }
    if !viewable(&rest) {
        return err(
            StatusCode::FORBIDDEN,
            "not for viewers: a lane's state, logs, events and usage only",
        )
        .into_response();
    }
    let d = match daemon_of(&s, l).await {
        Ok(d) => d,
        Err(e) => return e.into_response(),
    };
    let query = req
        .uri()
        .query()
        .map(|q| format!("?{q}"))
        .unwrap_or_default();
    pass(d, format!("/{rest}{query}"), None, req).await
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
    let l = lease_of(&s, &id, &who)?;
    let (blobs, lane) = (s.blobs(&who), s.dir_of(&l));
    // Not while the lane starts: the agent checks its files then, and
    // Compose must run what was checked. The requester syncs again.
    let _writing = {
        let starting = s.starting.lock().unwrap();
        if matches!(starting.get(&id), Some(Ok(_))) {
            return Err(err(
                StatusCode::CONFLICT,
                "the lane is starting: its files change once it runs",
            ));
        }
        Applying::new(s.clone(), id.clone())
    };
    let r = tokio::task::spawn_blocking(move || apply_files(&blobs, &lane, &a.files))
        .await
        .map_err(internal)?;
    r.map(Json)
        .map_err(|e| err(StatusCode::BAD_REQUEST, e.to_string()))
}

/// A sync writing a lease's mirror ([`Inner::applying`]), until dropped.
struct Applying(Shared, String);

impl Applying {
    fn new(s: Shared, id: String) -> Self {
        *s.applying.lock().unwrap().entry(id.clone()).or_default() += 1;
        Self(s, id)
    }
}

impl Drop for Applying {
    fn drop(&mut self) {
        let mut a = self.0.applying.lock().unwrap();
        if let Some(n) = a.get_mut(&self.1) {
            *n -= 1;
            if *n == 0 {
                a.remove(&self.1);
            }
        }
    }
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

/// The bytes the Compose volumes of each slot take: projects are
/// `<project>-<slot>` (ADR-0038 point 1). `docker system df -v`: slow.
fn slot_volumes() -> HashMap<String, u64> {
    let mut out = HashMap::new();
    for v in devstack_compose::Docker::default()
        .volumes()
        .unwrap_or_default()
    {
        if let Some(slot) = v.project.as_deref().and_then(slot_of_project) {
            *out.entry(slot.to_string()).or_insert(0) += v.size;
        }
    }
    out
}

/// The slot a Compose project of a lane belongs to: its last `-` part, 24
/// hex digits (`lanerun::secret_name`).
fn slot_of_project(project: &str) -> Option<&str> {
    let (_, slot) = project.rsplit_once('-')?;
    (slot.len() == 24 && slot.bytes().all(|b| b.is_ascii_hexdigit())).then_some(slot)
}

/// The ids of images with `filter`, each with the label `label`.
fn images_labelled(filter: &str, label: &str) -> Vec<(String, String)> {
    let docker = |args: &[&str]| {
        std::process::Command::new("docker")
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    };
    let ids = docker(&["image", "ls", "-q", "--no-trunc", "--filter", filter]);
    let mut ids: Vec<&str> = ids.split_whitespace().collect();
    ids.sort_unstable();
    ids.dedup();
    if ids.is_empty() {
        return Vec::new();
    }
    let format = format!("{{{{.Id}}}}\t{{{{index .Config.Labels \"{label}\"}}}}");
    let mut args = vec!["image", "inspect", "--format", &format, "--"];
    args.extend(ids);
    docker(&args)
        .lines()
        .filter_map(|l| {
            let (id, v) = l.trim_end_matches('\r').split_once('\t')?;
            Some((id.to_string(), v.to_string()))
        })
        .collect()
}

/// Remove images `ids` that nothing uses (an image a container uses stays).
fn remove_images(ids: &[String]) {
    for id in ids {
        let _ = std::process::Command::new("docker")
            .args(["image", "rm", "--", id])
            .output();
    }
}

/// The images Compose built for slot `slot`'s projects go with it: a
/// Dockerfile changed is another image each time, kept until the slot goes.
fn remove_built_images(slot: &str) {
    let ids: Vec<String> = images_labelled(
        "label=com.docker.compose.project",
        "com.docker.compose.project",
    )
    .into_iter()
    .filter(|(_, project)| project.rsplit_once('-').is_some_and(|(_, s)| s == slot))
    .map(|(id, _)| id)
    .collect();
    remove_images(&ids);
}

/// The lane images this agent built (`[lanes] dockerfile`, ADR-0028) for
/// the members `gone` says are, or all of them that no container uses.
fn remove_lane_images(owner: &str, gone: impl Fn(&str) -> bool) {
    let ids: Vec<String> =
        images_labelled(&format!("label=devstack.agent={owner}"), "devstack.member")
            .into_iter()
            .filter(|(_, member)| gone(member))
            .map(|(id, _)| id)
            .collect();
    remove_images(&ids);
}

/// A lane container on the engine, any agent's.
struct LaneContainer {
    lease: String,
    /// Its agent's owner tag; empty from an agent before there was one.
    owner: String,
    name: String,
}

/// Every lane container on the engine (`label=devstack.lease`), whichever
/// agent's.
fn lane_containers() -> Vec<LaneContainer> {
    let Ok(out) = std::process::Command::new("docker")
        .args([
            "ps",
            "-a",
            "--filter",
            "label=devstack.lease",
            "--format",
            "{{.Label \"devstack.lease\"}}\t{{.Label \"devstack.agent\"}}\t{{.Names}}",
        ])
        .output()
    else {
        return Vec::new();
    };
    parse_lane_containers(&String::from_utf8_lossy(&out.stdout))
}

fn parse_lane_containers(text: &str) -> Vec<LaneContainer> {
    text.lines()
        .filter_map(|l| {
            let mut f = l.trim_end_matches('\r').splitn(3, '\t');
            let (lease, owner, name) = (f.next()?, f.next()?, f.next()?);
            (!lease.is_empty() && !name.is_empty()).then(|| LaneContainer {
                lease: lease.to_string(),
                // Docker prints a label a container lacks as `<no value>`.
                owner: owner.trim_start_matches("<no value>").to_string(),
                name: name.to_string(),
            })
        })
        .collect()
}

/// Whether container `name` mounts something under `root` (where an agent
/// keeps its lanes' mirrors): an older agent's lane container, by where its
/// mirror is.
fn mounts_under(name: &str, root: &Path) -> bool {
    let Ok(out) = std::process::Command::new("docker")
        .args([
            "inspect",
            "--format",
            "{{range .Mounts}}{{.Source}}\n{{end}}",
            "--",
            name,
        ])
        .output()
    else {
        return false;
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .any(|m| !m.trim().is_empty() && Path::new(m.trim()).starts_with(root))
}

/// Each lease's id with the names its containers carry: the lane container
/// its id, its infrastructure its slot's (ADR-0038).
fn lease_keys(ls: &[NodeLease]) -> Vec<(String, Vec<String>)> {
    ls.iter()
        .map(|l| {
            (
                l.id.clone(),
                std::iter::once(l.id.clone())
                    .chain(l.slot.clone())
                    .collect(),
            )
        })
        .collect()
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
    let mut purged: Option<Instant> = None;
    let mut disk_at: Option<Instant> = None;
    loop {
        next_round(wait, &s.changed).await;
        wait = EVERY;
        if purged.is_none_or(|t| t.elapsed() >= PURGE_EVERY) {
            let first = purged.is_none();
            purged = Some(Instant::now());
            let s2 = s.clone();
            let _ = tokio::task::spawn_blocking(move || {
                if first {
                    s2.sweep_orphans();
                }
                s2.purge_gone_members();
                s2.sweep_slots();
            })
            .await;
        }
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
        let free = s.free.unwrap_or_else(|| Amount {
            cpus: free_cpus,
            memory: devstack_proc::memory_available().unwrap_or(0),
        });
        // What the pool's lanes use here now, their infrastructure included,
        // each lease's into its run (ADR-0049 point 2).
        let keys = lease_keys(
            &s.leases
                .lock()
                .unwrap()
                .values()
                .cloned()
                .collect::<Vec<_>>(),
        );
        let by = tokio::task::spawn_blocking(move || crate::runs::use_by(&keys))
            .await
            .unwrap_or_default();
        {
            let mut runs = s.runs.lock().unwrap();
            for (id, a) in &by {
                runs.load(id, *a);
            }
        }
        let used = by.values().fold(Amount::default(), |a, b| Amount {
            cpus: a.cpus + b.cpus,
            memory: a.memory + b.memory,
        });
        if disk_at.is_none_or(|t| t.elapsed() >= DISK_EVERY) {
            disk_at = Some(Instant::now());
            let s2 = s.clone();
            let _ = tokio::task::spawn_blocking(move || {
                let ls: Vec<NodeLease> = s2.leases.lock().unwrap().values().cloned().collect();
                s2.measure_disk(&ls, None);
            })
            .await;
        }
        // The part of what is free a policy says, else half (ADR-0031 point 3).
        let percent = {
            let policies = s.policies();
            let all: Vec<&crate::policy::NodePolicy> = policies.iter().collect();
            crate::policy::NodePolicy::strictest(&all).and_then(|p| p.share_percent)
        };
        let offer = Settings {
            enabled: true,
            ..s.share.clone()
        }
        .offer(free, percent);
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
            // Every minute on average: a round comes every `EVERY`, so a
            // send waits up to that long; the next is due a minute after
            // this one was, not after it went (one per round at most).
            sent = (sent + Duration::from_secs(60)).max(Instant::now() - EVERY);
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
                let logs = crate::telemetry::node_logs_body("devstack-node");
                let s2 = s.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    if let Some(logs) = logs {
                        let _ = s2.local.telemetry_send("logs", &logs);
                    }
                    s2.local.telemetry_send("metrics", &body)
                })
                .await;
            } else {
                // Nobody takes them: they go.
                let _ = crate::telemetry::node_logs_body("devstack-node");
            }
        }
        // Taken one at a time: a guard in the literal below would live to
        // its end, and `leases` is locked there (`runs` after `leases`).
        let runs = s.runs.lock().unwrap().report();
        let disk_room = s.disk_room();
        let report = Report {
            total: Some(total),
            used: Some(used),
            gpus,
            sharing: true,
            cpus: available.cpus,
            memory: available.memory,
            api: Some(s.api.clone()),
            dedicated: s.dedicated,
            devstack: Some(crate::version::Release::this()),
            kept: *s.kept_use.lock().unwrap(),
            update: s.update.lock().unwrap().clone(),
            no_update: s.no_update(),
            slots: s
                .kept()
                .into_iter()
                .map(|(_, i)| crate::coordinator::SlotRef {
                    member: i.member,
                    repo: i.repo,
                    lane: i.lane,
                })
                .collect(),
            disk_free: devstack_proc::disk_free(&s.root),
            disk_room,
            runs,
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
        let sent = report.runs.clone();
        let s2 = s.clone();
        let answer = tokio::task::spawn_blocking(move || s2.local.report(&report)).await;
        if let Ok(Ok(_)) = &answer {
            s.runs.lock().unwrap().reported(&sent);
        }
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
            // Another release at the coordinator: said once per change.
            let mut known = s.coordinator.lock().unwrap();
            if known.as_ref() != Some(&a.coordinator) {
                if let Some(n) = crate::version::note("the coordinator", a.coordinator.as_ref()) {
                    eprintln!("node agent: {n}");
                }
                *known = Some(a.coordinator.clone());
            }
        }
        // An update the node waits for its lanes for, that the coordinator
        // no longer sends: cancelled by an admin, or forgotten by a
        // coordinator that restarted. The node takes lanes again.
        if let Ok(Ok(a)) = &answer {
            let waiting = s
                .update
                .lock()
                .unwrap()
                .as_ref()
                .filter(|u| u.state == "draining")
                .map(|u| u.at);
            if let Some(at) = waiting
                && a.update.as_ref().is_none_or(|o| o.at != at)
            {
                *s.update_withdrawn.lock().unwrap() = Some(at);
            }
        }
        // An update an admin ordered (ADR-0039), once per order.
        if let Ok(Ok(a)) = &answer
            && let Some(order) = a.update.clone()
        {
            let new = s
                .update
                .lock()
                .unwrap()
                .as_ref()
                .is_none_or(|u| u.at != order.at);
            if new {
                let s2 = s.clone();
                std::thread::spawn(move || s2.carry_out(order));
            }
        }
        // An admin asked for the stopped lanes kept here to go (ADR-0038).
        if let Ok(Ok(a)) = &answer
            && a.prune
        {
            let s2 = s.clone();
            std::thread::spawn(move || {
                for (slot, info) in s2.kept() {
                    s2.purge_slot(&slot, &info, "an admin of the pool asked");
                }
            });
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

/// The agent again, on `binary`, with the arguments it was started with. On
/// Unix in place of this process (a supervisor sees the same process go on);
/// elsewhere a new process, and this one ends. Returns only when that
/// failed, with why: this process then goes on.
fn restart_on(binary: &Path) -> String {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let e = std::process::Command::new(binary).args(&args).exec();
        format!("cannot start {}: {e}", binary.display())
    }
    #[cfg(not(unix))]
    {
        match std::process::Command::new(binary).args(&args).spawn() {
            Ok(_) => std::process::exit(0),
            Err(e) => format!("cannot start {}: {e}", binary.display()),
        }
    }
}

/// Whether a node agent runs for the membership in `dir` (`pool/` or
/// `pools/<name>/` in devstack's per-user state): it holds `node/lock`
/// while it runs. The hub on the same machine then does not report for that
/// pool; the agent reports the machine (#170).
pub fn agent_runs(dir: &Path) -> bool {
    devstack_proc::StateLock::held(&dir.join("node"))
}

/// The agent's lock in `root` (its `node/` directory). Held a moment longer
/// by the agent it replaces (a restart after an update, on Windows a new
/// process) or by a hub that looks whether it is held: tried again for a few
/// seconds.
async fn agent_lock(root: &Path) -> Result<devstack_proc::StateLock, NodeError> {
    let t0 = Instant::now();
    loop {
        match devstack_proc::StateLock::acquire(root) {
            Ok(l) => return Ok(l),
            Err(devstack_proc::LockError::Held(_)) if t0.elapsed() < Duration::from_secs(10) => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(devstack_proc::LockError::Held(p)) => {
                return Err(NodeError::Other(format!(
                    "a node agent of this member runs already ({} is held)",
                    p.display()
                )));
            }
            Err(e) => return Err(NodeError::Other(e.to_string())),
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
    // One reporter per machine and pool (#170): while it is held, the hub
    // does not report for this membership.
    let _lock = agent_lock(&root).await?;
    std::fs::create_dir_all(root.join("lanes")).map_err(|e| NodeError::Io(root.clone(), e))?;
    let leases: HashMap<String, NodeLease> = std::fs::read(root.join("leases.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<Vec<NodeLease>>(&b).ok())
        .unwrap_or_default()
        .into_iter()
        .map(|l| (l.id.clone(), l))
        .collect();
    let bin = linux_binary(&root);
    // Rules of lanes that ended while the agent was down (ADR-0033 point 5)
    // go with the first sweep (`sweep_orphans`).
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
        free: config.free,
        on_update: config.on_update.clone(),
        update: Mutex::new(None),
        draining: std::sync::atomic::AtomicBool::new(false),
        update_withdrawn: Mutex::new(None),
        leases: Mutex::new(leases),
        members: Mutex::new((None, HashMap::new())),
        members_asked: Mutex::new(None),
        follows: Mutex::new(HashMap::new()),
        policy: Mutex::new((false, None)),
        local_policy,
        secret,
        blob_use: Mutex::new(HashMap::new()),
        slot_busy: Mutex::new(HashMap::new()),
        slot_purging: Mutex::new(HashSet::new()),
        kept_use: Mutex::new(None),
        volume_use: Mutex::new(HashMap::new()),
        slot_use: Mutex::new(HashMap::new()),
        runs: Mutex::new(crate::runs::Runs::default()),
        views_ok: Mutex::new(HashMap::new()),
        refused: Mutex::new(None),
        coordinator: Mutex::new(None),
        offer: Mutex::new(Amount::default()),
        bin,
        container: std::env::var("DEVSTACK_NODE_CONTAINER")
            .ok()
            .filter(|c| !c.trim().is_empty()),
        daemons: Mutex::new(HashMap::new()),
        starting: Mutex::new(HashMap::new()),
        applying: Mutex::new(HashMap::new()),
        bringing: Mutex::new(HashMap::new()),
        phase_times: Mutex::new(HashMap::new()),
        changed: tokio::sync::Notify::new(),
    });
    {
        let leases = s.leases.lock().unwrap();
        let mut runs = s.runs.lock().unwrap();
        for l in leases.values() {
            runs.begin(&l.id, &l.repo);
        }
    }
    let bg = tokio::spawn(background(s.clone()));
    // The containers of the running lanes: their states and load into each
    // lane's file, so its dashboard shows them (#101).
    let weak = Arc::downgrade(&s);
    let _ = std::thread::Builder::new()
        .name("lane-infra".into())
        .spawn(move || {
            loop {
                std::thread::sleep(crate::laneinfra::EVERY);
                let Some(s) = weak.upgrade() else { return };
                let ids: Vec<String> = s.daemons.lock().unwrap().keys().cloned().collect();
                for id in ids {
                    let Some(l) = s.leases.lock().unwrap().get(&id).cloned() else {
                        continue;
                    };
                    let dir = s.dir_of(&l);
                    if let Err(e) = crate::laneinfra::refresh(&dir, &dir.join("src")) {
                        eprintln!("lane {id}: its containers: {e}");
                    }
                }
            }
        });
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
    fn lane_containers_tell_their_agent() {
        let c = parse_lane_containers(
            "aa11\t0a1b2c3d\tdevstack-lane-aa11\nbb22\t\tdevstack-lane-bb22\r\ncc33\t<no value>\tdevstack-lane-cc33\n\n",
        );
        let v: Vec<(&str, &str, &str)> = c
            .iter()
            .map(|c| (c.lease.as_str(), c.owner.as_str(), c.name.as_str()))
            .collect();
        assert_eq!(
            v,
            [
                ("aa11", "0a1b2c3d", "devstack-lane-aa11"),
                // An older agent's: no owner.
                ("bb22", "", "devstack-lane-bb22"),
                ("cc33", "", "devstack-lane-cc33"),
            ]
        );
        // Two agents' tags differ, one agent's stays.
        let a = crate::lanerun::owner_tag("secret-of-one-pool");
        assert_eq!(a.len(), 8);
        assert_eq!(a, crate::lanerun::owner_tag("secret-of-one-pool"));
        assert_ne!(a, crate::lanerun::owner_tag("secret-of-another"));
    }

    #[test]
    fn a_slots_volumes_are_known_by_their_project() {
        let slot = "0123456789abcdef01234567";
        assert_eq!(slot_of_project(&format!("shop-{slot}")), Some(slot));
        assert_eq!(slot_of_project(&format!("my-shop-{slot}")), Some(slot));
        assert_eq!(slot_of_project("shop-main"), None);
        assert_eq!(slot_of_project("shop"), None);
    }

    #[test]
    fn a_viewer_reads_a_lane_and_changes_nothing() {
        for p in [
            "v1/stack",
            "v1/events",
            "v1/usage",
            "v1/logs",
            "v1/services/api/logs",
            "v1/services/api/logs/stream",
            "v1/services/api/builds",
            "v1/commands/seed/runs",
            "v1/runs/12/log/stream",
        ] {
            assert!(viewable(p), "{p}");
        }
        for p in [
            "v1/up",
            "v1/down",
            "v1/services/api/restart",
            "v1/services/api/env",
            "v1/commands/seed",
            "v1/lanes",
            "v1/tunnel/5000",
            "v1/services/../logs",
            "v1/runs/x/log",
            "v1/stack/stream",
        ] {
            assert!(!viewable(p), "{p}");
        }
    }

    #[test]
    fn a_viewer_reads_a_lanes_container_logs_but_acts_on_none() {
        use hyper::Method;
        // `view` answers what parses as a container route itself, before
        // `viewable`: the logs and their stream, as for the owner; an action
        // is refused (and never parses for a GET).
        for p in ["v1/infra/app/db/logs", "v1/infra/app/db/logs/stream"] {
            assert!(
                matches!(
                    InfraRoute::parse(&Method::GET, p),
                    Some(InfraRoute::Logs { .. } | InfraRoute::Stream { .. })
                ),
                "{p}"
            );
            assert!(!viewable(p), "{p} is the node's, not the daemon's");
        }
        assert_eq!(
            InfraRoute::parse(&Method::GET, "v1/infra/app/db/restart"),
            None
        );
        assert!(!viewable("v1/infra/app/db/restart"));
    }

    #[test]
    fn the_node_answers_a_lanes_container_routes_only() {
        use hyper::Method;
        let r = |m: &Method, p: &str| InfraRoute::parse(m, p);
        assert_eq!(
            r(&Method::GET, "v1/infra/app/db/logs"),
            Some(InfraRoute::Logs {
                group: "app".into(),
                service: "db".into()
            })
        );
        assert!(matches!(
            r(&Method::GET, "v1/infra/app/db/logs/stream"),
            Some(InfraRoute::Stream { .. })
        ));
        assert!(matches!(
            r(&Method::POST, "v1/infra/app/db/restart"),
            Some(InfraRoute::Act { action, .. }) if action == "restart"
        ));
        // Everything else goes to the lane's daemon, a name that is no plain
        // name included.
        for (m, p) in [
            (Method::GET, "v1/infra/app/db/restart"),
            (Method::POST, "v1/infra/app/db/logs"),
            (Method::POST, "v1/infra/app/db/remove"),
            (Method::GET, "v1/infra/app/../logs"),
            (Method::GET, "v1/infra/app/db"),
            (Method::POST, "v1/infra/check"),
            (Method::GET, "v1/stack"),
        ] {
            assert_eq!(r(&m, p), None, "{m} {p}");
        }
    }

    #[test]
    fn the_node_looks_at_service_routes_that_may_name_containers() {
        use crate::laneinfra::Named;
        use hyper::Method;
        let some = |route, name: &str, action: &str| Some((route, name.into(), action.into()));
        assert_eq!(
            named_route(&Method::POST, "v1/services/app.web/restart"),
            some(Named::Services, "app.web", "restart")
        );
        assert_eq!(
            named_route(&Method::POST, "v1/groups/server/restart"),
            some(Named::Groups, "server", "restart")
        );
        for (m, p) in [
            (Method::GET, "v1/services/web/restart"),
            (Method::POST, "v1/services/web/rebuild"),
            (Method::POST, "v1/services/web/logs/x"),
            (Method::POST, "v1/services//restart"),
            (Method::POST, "v1/commands/seed"),
        ] {
            assert_eq!(named_route(&m, p), None, "{m} {p}");
        }
    }

    #[test]
    fn a_path_from_a_requester_stays_in_the_mirror() {
        assert!(safe_path("src/Api/Program.cs").is_some());
        assert!(safe_path("src\\Api\\Program.cs").is_some());
        for bad in ["", "/etc/passwd", "../x", "a/../../x", "a/./b"] {
            assert!(safe_path(bad).is_none(), "{bad}");
        }
        // A drive on Windows; a file name on Linux, where lanes run.
        assert_eq!(safe_path("C:/x").is_none(), cfg!(windows));
        assert_eq!(safe_path("docs/a:b.md").is_some(), !cfg!(windows));
    }

    #[test]
    fn lease_names_are_bounded() {
        use crate::policy::valid_lease_names;
        assert!(valid_lease_names("Shop API", "login-fix").is_ok());
        assert!(valid_lease_names("shop", "feature_x.2").is_ok());
        for (repo, lane) in [
            ("", "x"),
            ("shop", ""),
            ("sh\nop", "x"),
            ("shop", "a/b"),
            ("shop", "a b"),
            ("shop", "a\u{0}"),
        ] {
            assert!(valid_lease_names(repo, lane).is_err(), "{repo:?} {lane:?}");
        }
        assert!(valid_lease_names(&"r".repeat(129), "x").is_err());
        assert!(valid_lease_names("shop", &"l".repeat(65)).is_err());
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
        // One path that cannot be written holds up no other (#128): it is
        // named, the rest lands, and the next sync tries it again.
        let dh = put("d");
        let r = apply_files(&blobs, &lane, &[f("c.txt", &c), f("d.txt", &dh)]).unwrap();
        assert_eq!(r.written, 1);
        assert_eq!(r.failed.len(), 1, "{:?}", r.failed);
        assert!(
            r.failed[0].starts_with("c.txt: ") && r.failed[0].contains("does not have its hash"),
            "{:?}",
            r.failed
        );
        assert!(!lane.join("src/c.txt").exists());
        assert_eq!(
            std::fs::read_to_string(lane.join("src/d.txt")).unwrap(),
            "d"
        );
        std::fs::write(blobs.join(&c), "c").unwrap();
        let r = apply_files(&blobs, &lane, &[f("c.txt", &c), f("d.txt", &dh)]).unwrap();
        assert_eq!((r.written, r.failed.len()), (1, 0), "only c.txt again");
        assert_eq!(
            std::fs::read_to_string(lane.join("src/c.txt")).unwrap(),
            "c"
        );
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
