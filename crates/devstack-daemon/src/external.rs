//! Infrastructure run by someone else (ADR-0025): in a lane container on a
//! node, the node's agent starts the lane's Compose project and the daemon in
//! the container has no Docker. The agent writes what the daemon needs to a
//! file, `DEVSTACK_EXTERNAL_INFRA` names it, and the daemon reads it instead of
//! asking Docker: every Compose service's environment (ADR-0014), the address
//! at which each of its ports is reached from the lane container (a name on
//! the lane's Compose network, `db:5432`, not a port published on a host), and
//! its state.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ExternalInfra {
    #[serde(default)]
    pub groups: BTreeMap<String, ExternalGroup>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ExternalGroup {
    #[serde(default)]
    pub project: String,
    #[serde(default)]
    pub services: BTreeMap<String, ExternalService>,
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
    }
}
