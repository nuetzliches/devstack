//! What a node lets run on it (ADR-0033): who may start lanes there (by tag
//! or member), which repositories (a guard against mistakes: the requester
//! names the repository), how much a lane may take, and what its network
//! reaches. Set by an admin, kept with the node's member record, enforced
//! by the node when it admits a lease and by the coordinator when it places.

use crate::share::Need;
use serde::{Deserialize, Serialize};

/// The least a lane is taken to need, whatever its requester says: a lease
/// that claims nothing would otherwise fit every node (and its limits,
/// derived from it, would be the smallest).
pub const MIN_NEED: Need = Need {
    cpus: 0.25,
    memory: 256 << 20,
    disk_repo: 0,
    disk_lane: 0,
    gpus: None,
};

/// A lease's repository (`[stack] name`) and lane as a requester names
/// them, checked before they go into a lease, the node's files, the lane
/// container's environment and every report: a repository of at most 128
/// characters without control characters, a lane of at most 64 letters,
/// digits, `-`, `_` and `.` (a lane's name is `[a-z0-9-]` already).
pub fn valid_lease_names(repo: &str, lane: &str) -> Result<(), String> {
    if repo.trim().is_empty() || repo.chars().count() > 128 || repo.chars().any(char::is_control) {
        return Err(
            "the repository name must be 1 to 128 characters without control characters".into(),
        );
    }
    if lane.is_empty()
        || lane.len() > 64
        || !lane
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return Err("the lane name must be 1 to 64 letters, digits, `-`, `_` or `.`".into());
    }
    Ok(())
}

/// `need`, at least [`MIN_NEED`].
pub fn floor_need(need: Need) -> Need {
    Need {
        cpus: if need.cpus.is_finite() {
            need.cpus.max(MIN_NEED.cpus)
        } else {
            MIN_NEED.cpus
        },
        memory: need.memory.max(MIN_NEED.memory),
        ..need
    }
}

/// What a lane container's network reaches beyond its own infrastructure,
/// from the most to the least (`Ord`: the stricter is the greater).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Network {
    /// The internet and the node's local network (as a container has it).
    #[default]
    Lan,
    /// The internet, but no private address range: not the node's local
    /// network, not the host (iptables rules per lane, `egress`).
    Internet,
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
    /// The most CPU cores one lane's container may use; `None`: no limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cpus: Option<f64>,
    /// What of its free CPU and memory the node offers the pool, percent
    /// (1 to 100, ADR-0031 point 3); `None`: half.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub share_percent: Option<u8>,
    #[serde(default)]
    pub network: Network,
    /// How long a stopped lane's mirror and data stay here for its next
    /// start (ADR-0038 point 3), hours; `0`: not at all. Default: a week on
    /// a dedicated node, a day on another.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_lanes_hours: Option<u64>,
    /// What the kept lanes of one member may take here, GiB (their mirrors;
    /// the least recently used goes first). Default 20.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_lanes_gib: Option<u64>,
    /// Whether an admin of the pool may have this node update its devstack
    /// (ADR-0039); only this node's own `policy.json` says `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_update: Option<bool>,
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

    /// One policy that holds what each of `all` holds (the node's own and
    /// the coordinator's, ADR-0033 point 6): admitted only if every one
    /// admits; the smaller limits, the stricter network.
    pub fn admits_all(
        all: &[&NodePolicy],
        id: &str,
        tags: &[String],
        repo: &str,
    ) -> Result<(), String> {
        all.iter().try_for_each(|p| p.admits(id, tags, repo))
    }

    /// The limits of `all` together: the smallest of each, the stricter
    /// network; `None` without any policy.
    pub fn strictest(all: &[&NodePolicy]) -> Option<NodePolicy> {
        let min = |a: Option<f64>, b: Option<f64>| match (a, b) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        fn lower<T: Ord>(a: Option<T>, b: Option<T>) -> Option<T> {
            match (a, b) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            }
        }
        all.iter().fold(None, |acc: Option<NodePolicy>, p| {
            Some(match acc {
                None => (*p).clone(),
                Some(a) => NodePolicy {
                    allow: Vec::new(),
                    repos: Vec::new(),
                    memory_headroom: min(a.memory_headroom, p.memory_headroom),
                    max_cpus: min(a.max_cpus, p.max_cpus),
                    share_percent: lower(a.share_percent, p.share_percent),
                    network: a.network.max(p.network),
                    keep_lanes_hours: lower(a.keep_lanes_hours, p.keep_lanes_hours),
                    keep_lanes_gib: lower(a.keep_lanes_gib, p.keep_lanes_gib),
                    remote_update: a.remote_update.or(p.remote_update),
                },
            })
        })
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
            max_cpus: None,
            memory_headroom: Some(2.0),
            network: Network::None,
            ..NodePolicy::default()
        };
        assert!(p.admits("a1", &["team:backend".into()], "shop").is_ok());
        assert!(p.admits("b2", &[], "Shop").is_ok());
        assert!(p.admits("c3", &["team:web".into()], "shop").is_err());
        assert!(p.admits("b2", &[], "crm").is_err());
        assert_eq!(p.memory_limit(512 << 20), Some(2 << 30));
        assert_eq!(p.memory_limit(4 << 30), Some(8 << 30));
    }

    #[test]
    fn the_node_and_the_coordinator_both_hold() {
        let node = NodePolicy {
            allow: vec!["team:a".into()],
            max_cpus: Some(4.0),
            network: Network::Internet,
            ..Default::default()
        };
        let coord = NodePolicy {
            memory_headroom: Some(2.0),
            max_cpus: Some(8.0),
            share_percent: Some(80),
            ..Default::default()
        };
        assert!(NodePolicy::admits_all(&[&node, &coord], "x", &[], "shop").is_err());
        assert!(NodePolicy::admits_all(&[&node, &coord], "x", &["team:a".into()], "shop").is_ok());
        let s = NodePolicy::strictest(&[&node, &coord]).unwrap();
        assert_eq!(
            (s.max_cpus, s.memory_headroom, s.network, s.share_percent),
            (Some(4.0), Some(2.0), Network::Internet, Some(80))
        );
        assert!(NodePolicy::strictest(&[]).is_none());
        let none = NodePolicy {
            network: Network::None,
            ..Default::default()
        };
        assert_eq!(
            NodePolicy::strictest(&[&node, &none]).unwrap().network,
            Network::None
        );
        let n = floor_need(Need::default());
        assert_eq!((n.cpus, n.memory), (0.25, 256 << 20));
        assert_eq!(
            floor_need(Need {
                cpus: 4.0,
                memory: 8 << 30,
                ..Need::default()
            })
            .memory,
            8 << 30
        );
    }
}
