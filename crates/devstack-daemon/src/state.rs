//! What the daemon knows and shows (API, events) and what it persists
//! (ADR-0016: which artifact is in the main slot, what to clean up).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum Status {
    Stopped,
    /// Syncing the snapshot or building.
    Building {
        build_id: Option<String>,
    },
    /// Process started, waiting for health (or for a job to finish).
    Starting,
    /// Running without a health check (tools, services without `health`).
    Running,
    Healthy,
    Unhealthy {
        reason: String,
    },
    /// A job that finished.
    Completed {
        exit_code: i32,
    },
    /// A long-running process that ended by itself.
    Exited {
        exit_code: i32,
    },
    Failed {
        reason: String,
    },
}

impl Status {
    /// Up and usable by dependants.
    pub fn is_ready(&self) -> bool {
        matches!(
            self,
            Status::Running | Status::Healthy | Status::Completed { exit_code: 0 }
        )
    }
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            Status::Building { .. }
                | Status::Starting
                | Status::Running
                | Status::Healthy
                | Status::Unhealthy { .. }
        )
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ServiceView {
    pub name: String,
    pub kind: String,
    pub groups: Vec<String>,
    pub port: Option<u16>,
    /// Where a browser opens the service (`https://localhost:4200`), if it has a port.
    pub url: Option<String>,
    pub autostart: bool,
    pub status: Status,
    pub build_id: Option<String>,
    pub pid: Option<u32>,
    /// Unix ms.
    pub since: u128,
    pub log: Option<PathBuf>,
    /// Port the running instance listens on; behind the proxy an internal one.
    pub instance_port: Option<u16>,
    /// The proxy on the fixed port (ADR-0010).
    pub proxy: Option<crate::proxy::ProxyView>,
    /// A newer build on its way into the main slot, or why it did not arrive.
    pub candidate: Option<Candidate>,
    /// A dev server's last rebuild, read from its output (ADR-0042): under
    /// way, done (for a minute) or failed (until the next).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rebuild: Option<crate::rebuild::RebuildView>,
    /// Runs in hot-reload mode: the toolchain's watcher from the source tree
    /// instead of a build (plan §3.4a).
    pub hot_reload: bool,
    /// The stack file gives it a hot-reload mode.
    pub hot_reload_available: bool,
}

/// A replacement for the main instance in progress (ADR-0003).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Candidate {
    /// `building`, `starting`, `switching`, `held`, `failed`.
    pub state: String,
    pub build_id: Option<String>,
    /// Unix ms.
    pub since: u128,
    pub reason: Option<String>,
}

impl Candidate {
    pub fn new(state: &str, build_id: Option<String>, reason: Option<String>) -> Self {
        Self {
            state: state.into(),
            build_id,
            since: now_ms(),
            reason,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ContainerView {
    pub service: String,
    /// Container name, as `docker stats` knows it.
    pub name: String,
    pub state: String,
    pub health: String,
    /// Exit code of a container that is not running: 0, 130, 137 or 143 is
    /// a stop (by devstack, Docker or a signal), anything else a failure.
    pub exit_code: i64,
    pub ports: Vec<String>,
    /// Links for the dashboard: the stack file's `urls` entry, or
    /// `http://localhost:<port>` per published port.
    pub urls: Vec<String>,
    /// Published ports that accept a connection from this machine and close
    /// it at once (`5432->127.0.0.1:5432`): a lost port forward, the
    /// container may look healthy. A restart of the container fixes it.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub unreachable: Vec<String>,
    /// Its fixed ports devstack holds (`main_ports`, ADR-0048): `port` the
    /// fixed one, `upstream` where it forwards now (the container's
    /// instance port, or a lane's port while that serves the main ports).
    /// `ports` then names the fixed port.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub held: Vec<crate::proxy::ProxyView>,
    /// The Compose group starts it (its service is in the group's
    /// `services`). `false` for any other container of the project that
    /// `compose ps` reports (started by hand, a service of a profile, an
    /// orphan of an earlier stack file): shown, and it can be stopped, but
    /// devstack neither starts nor restarts it.
    pub managed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct InfraView {
    pub group: String,
    pub project: String,
    pub containers: Vec<ContainerView>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GroupView {
    pub name: String,
    pub label: String,
    pub order: Option<i32>,
    pub members: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CommandView {
    pub name: String,
    pub description: String,
    /// Text to confirm before the command runs; `None` runs without asking.
    pub confirm: Option<String>,
    /// Services that must be healthy for the command to be enabled.
    pub requires: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StackView {
    pub name: String,
    pub repo: PathBuf,
    /// Version of the daemon.
    pub version: String,
    pub services: Vec<ServiceView>,
    pub infra: Vec<InfraView>,
    pub groups: Vec<GroupView>,
    pub commands: Vec<CommandView>,
    /// The stack file differs from what this daemon read at start; it takes
    /// effect with the next start (`down`, `up`).
    pub config_changed: bool,
    /// The lane: absent for the main working tree.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lane: Option<String>,
    /// The branch checked out in this working tree (`None`: detached, or
    /// not known yet).
    pub branch: Option<String>,
    /// The lane whose services answer on the main ports (`promote --lane`);
    /// absent while the main working tree serves them itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub main_served_by: Option<String>,
    /// Nothing of this stack moves with the main slot: no service on a port,
    /// none with workers (Compose containers only, #184). Serving the main
    /// ports is refused; the dashboard does not offer it.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub no_main_ports: bool,
    /// Why the file watcher does not work (it could not be set up, or the
    /// OS dropped its events): edits then make no candidates until it does.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub watcher_error: Option<String>,
    /// The profile this tree runs with (ADR-0041).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// The stack file's profiles, for a start with one.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub profiles: Vec<ProfileView>,
}

/// A profile of the stack file (ADR-0041).
#[derive(Debug, Clone, Serialize)]
pub struct ProfileView {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// It throws the tree's data away at start.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub fresh: bool,
    /// A command it is for (`up --profile <name> --run`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Event {
    pub seq: u64,
    /// Unix ms.
    pub at: u128,
    #[serde(flatten)]
    pub body: EventBody,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum EventBody {
    /// The whole state; the first event of every stream (ADR-0021: no polling).
    Snapshot {
        stack: Box<StackView>,
    },
    Service {
        service: String,
        status: Status,
        build_id: Option<String>,
    },
    Infra {
        group: String,
        message: String,
    },
    Command {
        command: String,
        run: u64,
        status: String,
    },
    /// A candidate appeared, changed state or is gone (`None`: promoted or dropped).
    Candidate {
        service: String,
        candidate: Option<Candidate>,
    },
    /// A dev server's rebuild started, ended or is gone from its view
    /// (ADR-0042); its diagnostic line, once over, has `ms` and `result`.
    DevserverRebuild {
        service: String,
        rebuild: Option<crate::rebuild::RebuildView>,
    },
    /// The stack file changed since the daemon started (`true`), or was changed back.
    Config {
        changed: bool,
    },
    /// Current load, every native sample (plan §12); history via `/v1/usage`.
    Usage(Box<crate::usage::UsageReport>),
    /// The lanes changed: a worktree added or removed, a branch checked out, a
    /// lane started or stopped, the main slot moved (ADR-0024).
    Lanes {
        lanes: Vec<crate::lanes::LaneView>,
    },
}

/// Persisted in `<state>/state.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Persisted {
    /// Service -> artifact id in the main slot (ADR-0016: restored without building).
    #[serde(default)]
    pub main: BTreeMap<String, String>,
    /// Process trees to clean up after a crash (Linux; ADR-0016).
    #[serde(default)]
    pub records: Vec<devstack_proc::Record>,
    /// Service -> the candidate a promotion gate holds (ADR-0013), so the
    /// gate outlives a daemon restart.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub held: BTreeMap<String, HeldRecord>,
}

/// A held candidate as `state.json` keeps it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct HeldRecord {
    /// Empty while the newest build failed (the gate stays).
    pub build_id: String,
    pub reason: String,
}

impl Persisted {
    pub fn load(state: &Path) -> Self {
        std::fs::read(state.join("state.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }
    pub fn save(&self, state: &Path) {
        let tmp = state.join("state.json.tmp");
        if std::fs::write(
            &tmp,
            serde_json::to_vec_pretty(self).expect("state serialises"),
        )
        .is_ok()
        {
            let _ = std::fs::rename(&tmp, state.join("state.json"));
        }
    }
}

pub fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis())
}
