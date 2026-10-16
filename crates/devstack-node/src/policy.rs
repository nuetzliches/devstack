//! What a node lets run on it (ADR-0033): who may start lanes there (by tag
//! or member), which repositories (a guard against mistakes: the requester
//! names the repository), how much a lane may take, and what its network
//! reaches. Set by an admin, kept with the node's member record, enforced
//! by the node when it admits a lease and by the coordinator when it places.

use serde::{Deserialize, Serialize};

/// What a lane container's network reaches beyond its own infrastructure.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Network {
    /// The internet and the node's local network (as a container has it).
    #[default]
    Lan,
    /// Nothing outside the lane: its containers reach each other only. A
    /// lane that installs packages at start (`prepare`) cannot here.
    None,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NodePolicy {
    /// Who may start lanes here: member ids or tags (`team:backend`); empty:
    /// every approved member.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<String>,
    /// The repositories (`[stack] name`) that run here; empty: any.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repos: Vec<String>,
    /// A lane's memory limit: its expected need times this (at least 2 GiB);
    /// `None`: no limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_headroom: Option<f64>,
    #[serde(default)]
    pub network: Network,
}

impl NodePolicy {
    /// May the member `id` with `tags` run a lane of `repo` here?
    pub fn admits(&self, id: &str, tags: &[String], repo: &str) -> Result<(), String> {
        if !self.allow.is_empty()
            && !self
                .allow
                .iter()
                .any(|a| a == id || tags.iter().any(|t| t == a))
        {
            return Err(format!(
                "this node runs lanes of {} only",
                self.allow.join(", ")
            ));
        }
        if !self.repos.is_empty() && !self.repos.iter().any(|r| r.eq_ignore_ascii_case(repo)) {
            return Err(format!(
                "this node runs {} only, not `{repo}`",
                self.repos.join(", ")
            ));
        }
        Ok(())
    }

    /// The memory limit of a lane expected to need `need` bytes.
    pub fn memory_limit(&self, need: u64) -> Option<u64> {
        let h = self.memory_headroom?;
        Some(((need as f64 * h) as u64).max(2 << 30))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_node_admits_by_tag_member_and_repository() {
        let open = NodePolicy::default();
        assert!(open.admits("a1", &[], "shop").is_ok());
        let p = NodePolicy {
            allow: vec!["team:backend".into(), "b2".into()],
            repos: vec!["shop".into()],
            memory_headroom: Some(2.0),
            network: Network::None,
        };
        assert!(p.admits("a1", &["team:backend".into()], "shop").is_ok());
        assert!(p.admits("b2", &[], "Shop").is_ok());
        assert!(p.admits("c3", &["team:web".into()], "shop").is_err());
        assert!(p.admits("b2", &[], "crm").is_err());
        assert_eq!(p.memory_limit(512 << 20), Some(2 << 30));
        assert_eq!(p.memory_limit(4 << 30), Some(8 << 30));
    }
}
