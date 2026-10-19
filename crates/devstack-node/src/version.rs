//! Which devstack a peer of the pool runs (ADR-0031, amended 2026-10-19).
//!
//! Members, nodes and the coordinator are updated one by one, so a pool runs
//! several releases at once. Each party says what it runs ([`Release`]): a
//! node in its report and in `GET /v1/node`, the coordinator in its answers
//! (a lease offer, a report's answer) and passed on for the nodes it names (a
//! lease offer's and a shared lane's `node`). A peer older than that says
//! nothing: its release is unknown.
//!
//! [`PROTOCOL`] is bumped by a change that makes two releases misread each
//! other (a field that means something else, a route that behaves
//! differently); releases with the same number work together. A requester
//! refuses a node with another protocol, and warns about a node that runs
//! another release or does not say which.

use serde::{Deserialize, Serialize};

/// This build's devstack version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The pool's protocol: bumped on a change between members, nodes, lane
/// daemons and the coordinator that an older peer would misread.
///
/// - `1` (0.10.0): a shared lane's service port is the stack's (main) port,
///   a lane on a dedicated node serves the main ports (ADR-0025 point 4).
pub const PROTOCOL: u32 = 1;

/// What a peer of the pool runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    /// Its devstack version (`0.10.2`).
    pub version: String,
    /// Its [`PROTOCOL`].
    pub protocol: u32,
}

impl Release {
    /// What this build runs.
    pub fn this() -> Self {
        Self {
            version: VERSION.into(),
            protocol: PROTOCOL,
        }
    }
}

impl std::fmt::Display for Release {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "devstack {} (pool protocol {})",
            self.version, self.protocol
        )
    }
}

/// How a peer's release compares with this one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compat {
    /// The same version.
    Same,
    /// Another version, the same protocol: they work together.
    Other,
    /// Another protocol: they misread each other.
    Incompatible,
    /// It does not say (older than version reporting).
    Unknown,
}

/// How `peer` compares with this build.
pub fn compat(peer: Option<&Release>) -> Compat {
    compat_with(peer, &Release::this())
}

/// How `peer` compares with `me`.
pub fn compat_with(peer: Option<&Release>, me: &Release) -> Compat {
    match peer {
        None => Compat::Unknown,
        Some(p) if p.protocol != me.protocol => Compat::Incompatible,
        Some(p) if p.version != me.version => Compat::Other,
        Some(_) => Compat::Same,
    }
}

/// What `peer` runs, for a message: `devstack 0.9.8` or that it does not say.
pub fn describe(peer: Option<&Release>) -> String {
    match peer {
        Some(p) => format!("devstack {}", p.version),
        None => "an older devstack that does not say its version".into(),
    }
}

/// A sentence on `who` (`the node build-server`) running another release
/// than this machine; `None` for the same version.
pub fn note(who: &str, peer: Option<&Release>) -> Option<String> {
    note_with(who, peer, &Release::this())
}

/// [`note`] against `me`.
pub fn note_with(who: &str, peer: Option<&Release>, me: &Release) -> Option<String> {
    match compat_with(peer, me) {
        Compat::Same => None,
        Compat::Other => Some(format!(
            "{who} runs {}, this machine devstack {}",
            describe(peer),
            me.version
        )),
        Compat::Incompatible => {
            let p = peer.expect("a known release");
            Some(format!(
                "{who} runs {p}, this machine {me}: they do not work together; update the older one"
            ))
        }
        Compat::Unknown => Some(format!(
            "{who} runs {}, this machine devstack {}: if something fails there, update it",
            describe(peer),
            me.version
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(v: &str, p: u32) -> Release {
        Release {
            version: v.into(),
            protocol: p,
        }
    }

    #[test]
    fn compares_version_and_protocol() {
        let me = r("0.10.2", 1);
        assert_eq!(compat_with(Some(&r("0.10.2", 1)), &me), Compat::Same);
        assert_eq!(compat_with(Some(&r("0.10.1", 1)), &me), Compat::Other);
        assert_eq!(
            compat_with(Some(&r("0.11.0", 2)), &me),
            Compat::Incompatible
        );
        assert_eq!(compat_with(None, &me), Compat::Unknown);
        assert_eq!(compat(Some(&Release::this())), Compat::Same);
    }

    #[test]
    fn a_note_names_both_releases() {
        let me = r("0.10.2", 1);
        assert_eq!(note_with("the node a", Some(&r("0.10.2", 1)), &me), None);
        let other = note_with("the node a", Some(&r("0.10.1", 1)), &me).unwrap();
        assert!(
            other.contains("0.10.1") && other.contains("0.10.2"),
            "{other}"
        );
        let bad = note_with("the node a", Some(&r("0.11.0", 2)), &me).unwrap();
        assert!(
            bad.contains("pool protocol 2") && bad.contains("pool protocol 1"),
            "{bad}"
        );
        assert!(bad.contains("do not work together"), "{bad}");
        let unknown = note_with("the node a", None, &me).unwrap();
        assert!(
            unknown.contains("does not say") && unknown.contains("0.10.2"),
            "{unknown}"
        );
    }

    #[test]
    fn an_older_peer_without_the_field_reads() {
        // A 0.9.8 node's report and a coordinator's node reference carry
        // no release: they still read, as unknown.
        let n: crate::coordinator::NodeRef =
            serde_json::from_str(r#"{"id":"a","name":"b","api":"https://x:1"}"#).unwrap();
        assert_eq!(n.devstack, None);
        let rep: crate::coordinator::Report =
            serde_json::from_str(r#"{"sharing":true,"cpus":1,"memory":2}"#).unwrap();
        assert_eq!(rep.devstack, None);
    }
}
