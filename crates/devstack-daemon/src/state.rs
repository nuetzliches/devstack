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
    pub autostart: bool,
    pub status: Status,
    pub build_id: Option<String>,
    pub pid: Option<u32>,
    /// Unix ms.
    pub since: u128,
    pub log: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ContainerView {
    pub service: String,
    /// Container name, as `docker stats` knows it.
    pub name: String,
    pub state: String,
    pub health: String,
    pub ports: Vec<String>,
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
    pub services: Vec<ServiceView>,
    pub infra: Vec<InfraView>,
    pub groups: Vec<GroupView>,
    pub commands: Vec<CommandView>,
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
    /// Current load, every native sample (plan §12); history via `/v1/usage`.
    Usage(Box<crate::usage::UsageReport>),
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
