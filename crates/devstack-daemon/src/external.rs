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
    /// The lane's requester relays its UDP ports (ADR-0056): it asked for
    /// it at the start, and this node's agent passes them on. An older
    /// requester or agent leaves it out, and the ports stay not forwarded.
    #[serde(default)]
    pub udp: bool,
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

/// A container port and its protocol, as the agent's file keys them: `5432`
/// for TCP (as every agent wrote a port), `53/udp` for another protocol, so
/// a service's `53/tcp` and `53/udp` are two entries (#192).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct PortKey {
    pub port: u16,
    /// Lower case; `tcp` sorts before `udp`, so a port's TCP entry comes
    /// first.
    pub protocol: String,
}

impl PortKey {
    pub fn is_tcp(&self) -> bool {
        self.protocol == "tcp"
    }

    pub fn is_udp(&self) -> bool {
        self.protocol == "udp"
    }
}

impl std::str::FromStr for PortKey {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let (port, protocol) = s.split_once('/').unwrap_or((s, "tcp"));
        let port = port
            .trim()
            .parse()
            .map_err(|_| format!("`{s}` is no port"))?;
        let protocol = match protocol.trim().to_ascii_lowercase() {
            p if p.is_empty() => "tcp".to_string(),
            p => p,
        };
        Ok(Self { port, protocol })
    }
}

impl<'de> Deserialize<'de> for PortKey {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ExternalService {
    #[serde(default)]
    pub environment: BTreeMap<String, Option<String>>,
    /// Container port -> address the lane container reaches it at.
    #[serde(default)]
    pub ports: BTreeMap<PortKey, String>,
    /// Container port -> the port it is on at the lane's developer's machine
    /// (what a local lane would publish it on): its links point there, and
    /// the developer's tunnels to it come in on that port (ADR-0029 point 4).
    /// A UDP port is relayed when the requester asked for it (ADR-0056); a
    /// port of another protocol is listed, but not forwarded.
    #[serde(default)]
    pub published: BTreeMap<PortKey, u16>,
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

impl ExternalService {
    /// Its TCP ports where the developer reaches them, `(container port,
    /// port there)`: what is forwarded, and linked.
    pub fn forwarded(&self) -> impl Iterator<Item = (u16, u16)> + '_ {
        self.published
            .iter()
            .filter(|(k, _)| k.is_tcp())
            .map(|(k, h)| (k.port, *h))
    }

    /// Its published ports the developer does not reach, as they would
    /// (`5353/udp`): those of another protocol than TCP (#192), UDP ones
    /// too unless the requester relays them (`udp`, ADR-0056).
    pub fn not_forwarded(&self, udp: bool) -> Vec<String> {
        self.published
            .iter()
            .filter(|(k, _)| !k.is_tcp() && !(udp && k.is_udp()))
            .map(|(k, h)| format!("{h}/{}", k.protocol))
            .collect()
    }

    /// Its ports as the view shows them, as the developer reaches them
    /// (`15432->db:5432`, `5353/udp->dns:53`).
    pub fn shown_ports(&self) -> Vec<String> {
        self.ports
            .iter()
            .map(|(k, a)| {
                let at = self.published.get(k).copied().unwrap_or(k.port);
                match k.is_tcp() {
                    true => format!("{at}->{a}"),
                    false => format!("{at}/{}->{a}", k.protocol),
                }
            })
            .collect()
    }
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
        assert_eq!(g.services["db"].ports[&"5432".parse().unwrap()], "db:5432");
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

    #[test]
    fn a_port_of_another_protocol_is_its_own_and_not_forwarded() {
        let x: super::ExternalInfra = serde_json::from_str(
            r#"{"groups":{"app":{"services":{"dns":{
                "ports":{"53":"dns:53","53/udp":"dns:53","8080/tcp":"dns:8080"},
                "published":{"53":5453,"53/udp":5453,"8080/tcp":8180},
                "state":"running"}}}}}"#,
        )
        .unwrap();
        let dns = &x.groups["app"].services["dns"];
        assert_eq!(dns.ports.len(), 3, "53/tcp and 53/udp are two");
        assert_eq!(
            dns.forwarded().collect::<Vec<_>>(),
            [(53, 5453), (8080, 8180)]
        );
        assert_eq!(dns.not_forwarded(false), ["5453/udp"]);
        assert!(dns.not_forwarded(true).is_empty(), "relayed (ADR-0056)");
        assert_eq!(
            dns.shown_ports(),
            ["5453->dns:53", "5453/udp->dns:53", "8180->dns:8080"]
        );
        assert!("x/udp".parse::<super::PortKey>().is_err());
        assert_eq!("53/UDP".parse::<super::PortKey>().unwrap().protocol, "udp");
        assert!(
            !x.udp,
            "an older agent's file, or an older requester's lane"
        );
    }

    #[test]
    fn a_relayed_lane_still_names_a_port_of_a_third_protocol() {
        let x: super::ExternalInfra = serde_json::from_str(
            r#"{"udp":true,"groups":{"app":{"services":{"voice":{
                "ports":{"5060/udp":"voice:5060","9000/sctp":"voice:9000"},
                "published":{"5060/udp":5160,"9000/sctp":9100},
                "state":"running"}}}}}"#,
        )
        .unwrap();
        assert!(x.udp);
        let voice = &x.groups["app"].services["voice"];
        assert_eq!(voice.not_forwarded(x.udp), ["9100/sctp"]);
    }
}
