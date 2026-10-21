//! Infrastructure run by someone else (ADR-0025): in a lane container on a
//! node, the node's agent starts the lane's Compose project and the daemon in
//! the container has no Docker. The agent writes what the daemon needs to a
//! file, `DEVSTACK_EXTERNAL_INFRA` names it, and the daemon reads it instead of
//! asking Docker: every Compose service's environment (ADR-0014), the address
//! at which each of its ports is reached from the lane container (a name on
//! the lane's Compose network, `db:5432`, not a port published on a host), and
//! its state. The agent writes it before the daemon starts and brings the
//! containers up after it answered, saying how far each group got (#164);
//! `up` waits for that.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ExternalInfra {
    #[serde(default)]
    pub groups: BTreeMap<String, ExternalGroup>,
    /// When the agent last wrote states and load (ms since the epoch); an
    /// agent from before #101 writes the file once, without it.
    #[serde(default)]
    pub at: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ExternalGroup {
    #[serde(default)]
    pub project: String,
    #[serde(default)]
    pub services: BTreeMap<String, ExternalService>,
    /// How far the agent brought the group up (#164): `created`,
    /// `starting`, `up` or `failed`. An agent from before writes the file
    /// once everything is up, without it.
    #[serde(default)]
    pub state: Option<String>,
    /// Why it is `failed`.
    #[serde(default)]
    pub error: Option<String>,
}

/// What the daemon makes of a group's [`ExternalGroup::state`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupPhase {
    /// Its containers are created; the agent has not started them yet.
    Created,
    /// The agent starts them and waits until they are healthy.
    Starting,
    /// Running and healthy: what comes after it may start.
    Up,
    Failed(String),
}

impl ExternalGroup {
    pub fn phase(&self) -> GroupPhase {
        match self.state.as_deref() {
            Some("created") => GroupPhase::Created,
            Some("starting") => GroupPhase::Starting,
            Some("failed") => GroupPhase::Failed(
                self.error
                    .clone()
                    .filter(|e| !e.trim().is_empty())
                    .unwrap_or_else(|| "its node could not start it".into()),
            ),
            // None: an older agent, which wrote the file once it was up; a
            // state this daemon does not know holds nothing back either.
            _ => GroupPhase::Up,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ExternalService {
    #[serde(default)]
    pub environment: BTreeMap<String, Option<String>>,
    /// Container port -> address the lane container reaches it at.
    #[serde(default)]
    pub ports: BTreeMap<u16, String>,
    /// Container port -> the port it is on at the lane's developer's machine
    /// (what a local lane would publish it on): its links point there, and
    /// the developer's tunnels to it come in on that port (ADR-0029 point 4).
    #[serde(default)]
    pub published: BTreeMap<u16, u16>,
    /// Empty: not started (a parked service that is only an environment source).
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub health: String,
    /// What its container uses, as the agent measured it at `at`: percent
    /// of one core, bytes.
    #[serde(default)]
    pub cpu: Option<f64>,
    #[serde(default)]
    pub memory: Option<u64>,
}

/// The file `DEVSTACK_EXTERNAL_INFRA` names, if it is set.
pub fn path() -> Option<PathBuf> {
    std::env::var_os("DEVSTACK_EXTERNAL_INFRA")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Read it; a missing or broken file is no infrastructure yet.
pub fn read(path: &Path) -> Result<ExternalInfra, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::GroupPhase;

    #[test]
    fn a_groups_state_says_how_far_its_node_brought_it_up() {
        let x: super::ExternalInfra = serde_json::from_str(
            r#"{"groups":{
                "old":{"services":{}},
                "app":{"state":"created"},
                "cache":{"state":"starting"},
                "db":{"state":"up"},
                "mail":{"state":"failed","error":"db is unhealthy"},
                "bare":{"state":"failed"},
                "next":{"state":"paused-for-a-newer-reason"}}}"#,
        )
        .unwrap();
        let p = |g: &str| x.groups[g].phase();
        assert_eq!(p("old"), GroupPhase::Up, "an older node's file: up");
        assert_eq!(p("app"), GroupPhase::Created);
        assert_eq!(p("cache"), GroupPhase::Starting);
        assert_eq!(p("db"), GroupPhase::Up);
        assert_eq!(p("mail"), GroupPhase::Failed("db is unhealthy".into()));
        assert!(matches!(p("bare"), GroupPhase::Failed(e) if !e.is_empty()));
        assert_eq!(
            p("next"),
            GroupPhase::Up,
            "an unknown state holds nothing back"
        );
    }

    #[test]
    fn the_agents_file_reads() {
        let x: super::ExternalInfra = serde_json::from_str(
            r#"{"groups":{"app":{"project":"shop-fix","services":{
                "db":{"environment":{"POSTGRES_DB":"app"},"ports":{"5432":"db:5432"},"state":"running","health":"healthy"},
                "api":{"environment":{"Db":"Server=db:5432","X":null}}}}}}"#,
        )
        .unwrap();
        let g = &x.groups["app"];
        assert_eq!(g.services["db"].ports[&5432], "db:5432");
        assert_eq!(g.services["api"].state, "");
        assert_eq!(g.services["api"].environment["X"], None);
        assert_eq!(
            (x.at, g.services["db"].cpu),
            (None, None),
            "an older agent's file"
        );
        let y: super::ExternalInfra = serde_json::from_str(
            r#"{"at":1760000000000,"groups":{"app":{"services":{"db":{"state":"running","cpu":12.5,"memory":1048576}}}}}"#,
        )
        .unwrap();
        let db = &y.groups["app"].services["db"];
        assert_eq!(
            (y.at, db.cpu, db.memory),
            (Some(1_760_000_000_000), Some(12.5), Some(1 << 20))
        );
    }
}
