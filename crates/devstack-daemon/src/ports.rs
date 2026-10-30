//! The ports a stack holds or uses on this machine (ADR-0082 point 1):
//! what `GET /v1/ports` answers, one [`PortEntry`] per port, and what the
//! hub's `GET /hub/ports` joins over every stack and marks overlaps in.
//!
//! What a tree holds is gathered from its supervisor
//! ([`crate::supervisor::Supervisor::port_facts`]) and turned into entries
//! here ([`tree_entries`]), so the rules are tested without a daemon. A
//! stack whose daemon does not run is known by what its stack file declares
//! ([`declared`]).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The fixed port of a service: the one the stack file gives it, in a lane
/// moved by the lane's offset. In the main tree, the main slot's.
pub const FIXED: &str = "fixed";
/// The port a running instance listens on behind devstack's proxy or a
/// dev server's front.
pub const INSTANCE: &str = "instance";
/// A host port Docker publishes for a container.
pub const PUBLISHED: &str = "published";
/// A `main_ports` container's fixed port, which devstack holds and
/// forwards (ADR-0048).
pub const HELD: &str = "held";
/// A port of a lane on a node, forwarded (TCP) or relayed (UDP) here.
pub const NODE: &str = "node";
/// A port the stack file of a stack whose daemon does not run names.
pub const DECLARED: &str = "declared";

/// devstack's HTTP proxy listens on it (ADR-0010).
pub const VIA_PROXY: &str = "proxy";
/// devstack listens on it and passes the bytes on: a dev server's front
/// (ADR-0043), a held port (ADR-0048), the main slot's forward to a lane,
/// a lane on a node.
pub const VIA_FORWARD: &str = "forward";
/// The process itself listens on it.
pub const VIA_DIRECT: &str = "direct";
/// Docker publishes it.
pub const VIA_DOCKER: &str = "docker";

/// One port a stack holds or uses on this machine.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PortEntry {
    pub port: u16,
    /// `tcp` or `udp`.
    pub proto: String,
    /// `main` for the main working tree, else the lane's name.
    pub lane: String,
    /// [`FIXED`], [`INSTANCE`], [`PUBLISHED`], [`HELD`], [`NODE`] or
    /// [`DECLARED`].
    pub kind: String,
    /// A service, or a container as `<group>.<service>`; absent when it is
    /// not known (a forward of a lane on a node no service accounts for).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holder: Option<String>,
    /// [`VIA_PROXY`], [`VIA_FORWARD`], [`VIA_DIRECT`] or [`VIA_DOCKER`].
    pub via: String,
    /// Where it leads: the instance's port behind a proxy or front, the
    /// lane's port the main slot forwards to, the container's own port,
    /// the port on the node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<u16>,
    /// The lane `target` belongs to when it is not this entry's own: the
    /// lane that serves the main slot's port.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_lane: Option<String>,
    /// The node of a lane on a node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// Something listens on it now, as devstack knows it (not probed).
    pub listening: bool,
}

impl PortEntry {
    fn new(port: u16, proto: &str, lane: &str, kind: &str, via: &str) -> Self {
        Self {
            port,
            proto: proto.into(),
            lane: lane.into(),
            kind: kind.into(),
            holder: None,
            via: via.into(),
            target: None,
            target_lane: None,
            node: None,
            listening: false,
        }
    }

    fn held_by(mut self, holder: impl Into<String>) -> Self {
        self.holder = Some(holder.into());
        self
    }

    fn to(mut self, target: Option<u16>) -> Self {
        self.target = target;
        self
    }

    fn listening(mut self, on: bool) -> Self {
        self.listening = on;
        self
    }
}

/// Port pairs `(on the node, here)` of a lane on a node.
pub type PortPairs = Vec<(u16, u16)>;
/// A container's held ports: the fixed port, where it forwards now.
pub type HeldPorts = Vec<(u16, Option<u16>)>;

/// A host port a container publishes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Published {
    /// On this machine.
    pub port: u16,
    /// In the container.
    pub target: u16,
    /// `tcp` or `udp`.
    pub proto: String,
}

/// The host ports `publishers` (`compose ps`) name, each port and protocol
/// once (Docker lists one per address family).
pub fn bound(publishers: &[devstack_compose::Publisher]) -> Vec<Published> {
    let mut out: Vec<Published> = Vec::new();
    for p in publishers.iter().filter(|p| p.published_port != 0) {
        let proto = match p.protocol.as_str() {
            "" => "tcp",
            other => other,
        };
        if !out
            .iter()
            .any(|o| o.port == p.published_port && o.proto == proto)
        {
            out.push(Published {
                port: p.published_port,
                target: p.target_port,
                proto: proto.into(),
            });
        }
    }
    out
}

/// What a tree's supervisor knows of a service's ports.
#[derive(Debug, Clone, Default)]
pub struct ServiceFacts {
    pub name: String,
    /// Its fixed port in this tree (moved by a lane's offset).
    pub port: Option<u16>,
    /// devstack's HTTP proxy on the fixed port: where it forwards now.
    pub proxy: Option<Option<u16>>,
    /// A dev server's front on the fixed port (ADR-0043): where it
    /// forwards now.
    pub front: Option<Option<u16>>,
    /// The main tree's TCP forward of the fixed port to a lane's.
    pub forward: Option<Option<u16>>,
    /// The fixed port leads to the lane that serves the main slot, not to
    /// this tree's instance.
    pub to_lane: bool,
    /// The port its running instance listens on.
    pub instance: Option<u16>,
    /// It runs (builds, starts, runs).
    pub running: bool,
}

/// What a tree's supervisor knows of a container's ports.
#[derive(Debug, Clone, Default)]
pub struct ContainerFacts {
    pub group: String,
    pub service: String,
    pub running: bool,
    pub published: Vec<Published>,
    /// Its fixed ports devstack holds (ADR-0048): the port, where it
    /// forwards now.
    pub held: HeldPorts,
}

/// What a tree's supervisor knows of the ports it holds.
#[derive(Debug, Clone, Default)]
pub struct TreeFacts {
    /// `main`, or the lane's name.
    pub lane: String,
    /// The lane that serves the main slot's ports, in the main tree.
    pub served_by: Option<String>,
    /// The held ports forward to that lane.
    pub held_to_lane: bool,
    pub services: Vec<ServiceFacts>,
    pub containers: Vec<ContainerFacts>,
}

/// The entries of one tree: its services' fixed ports and instance ports,
/// its containers' published and held ports.
pub fn tree_entries(f: &TreeFacts) -> Vec<PortEntry> {
    let lane = f.lane.as_str();
    let mut out = Vec::new();
    for s in &f.services {
        let fixed = |via: &str, target: Option<u16>| {
            let mut e = PortEntry::new(s.port.unwrap_or_default(), "tcp", lane, FIXED, via)
                .held_by(&s.name)
                .to(target)
                .listening(true);
            if s.to_lane {
                e.target_lane = f.served_by.clone();
            }
            e
        };
        match (s.port, s.proxy, s.front, s.forward) {
            (Some(_), Some(up), _, _) => out.push(fixed(VIA_PROXY, up)),
            (Some(_), None, Some(up), _) => out.push(fixed(VIA_FORWARD, up)),
            (Some(_), None, None, Some(to)) => out.push(fixed(VIA_FORWARD, to)),
            (Some(p), None, None, None) => out.push(
                PortEntry::new(p, "tcp", lane, FIXED, VIA_DIRECT)
                    .held_by(&s.name)
                    .listening(s.running && s.instance == Some(p)),
            ),
            (None, ..) => {}
        }
        if let Some(i) = s.instance.filter(|i| Some(*i) != s.port) {
            out.push(
                PortEntry::new(i, "tcp", lane, INSTANCE, VIA_DIRECT)
                    .held_by(&s.name)
                    .listening(s.running),
            );
        }
    }
    for c in &f.containers {
        let holder = format!("{}.{}", c.group, c.service);
        for (port, up) in &c.held {
            let mut e = PortEntry::new(*port, "tcp", lane, HELD, VIA_FORWARD)
                .held_by(&holder)
                .to(*up)
                .listening(true);
            if f.held_to_lane {
                e.target_lane = f.served_by.clone();
            }
            out.push(e);
        }
        for p in &c.published {
            out.push(
                PortEntry::new(p.port, &p.proto, lane, PUBLISHED, VIA_DOCKER)
                    .held_by(&holder)
                    .to(Some(p.target))
                    .listening(c.running),
            );
        }
    }
    out
}

/// What the main tree's daemon knows of a lane on a node: the ports it
/// forwards or relays here.
#[derive(Debug, Clone, Default)]
pub struct NodeFacts {
    pub lane: String,
    pub node: String,
    /// `(on the node, here)`.
    pub tcp: PortPairs,
    pub udp: PortPairs,
    /// This machine listens on them for the lane now.
    pub listening: bool,
    /// What each port here is, by port: a service or `<group>.<service>`.
    pub holders: BTreeMap<u16, String>,
}

/// The entries of a lane on a node, as this machine holds them.
pub fn node_entries(f: &NodeFacts) -> Vec<PortEntry> {
    let one = |(there, here): &(u16, u16), proto: &str| {
        let mut e = PortEntry::new(*here, proto, &f.lane, NODE, VIA_FORWARD)
            .to(Some(*there))
            .listening(f.listening);
        e.holder = f.holders.get(here).cloned();
        e.node = Some(f.node.clone());
        e
    };
    f.tcp
        .iter()
        .map(|p| one(p, "tcp"))
        .chain(f.udp.iter().map(|p| one(p, "udp")))
        .collect()
}

/// What each port of a lane moved by `offset` is: the main tree's
/// services' fixed ports and its containers' host ports (`published`:
/// `(group, service, container port) -> host:port`, held ports as their
/// fixed one), each moved.
pub fn lane_holders(
    stack: &devstack_config::Stack,
    published: &BTreeMap<(String, String, u16), String>,
    offset: u16,
) -> BTreeMap<u16, String> {
    let mut out = BTreeMap::new();
    for (name, s) in &stack.services {
        if let Some(p) = s.port.and_then(|p| p.checked_add(offset)) {
            out.entry(p).or_insert_with(|| name.clone());
        }
    }
    for ((group, service, _), addr) in published {
        let Some(p) = addr
            .rsplit(':')
            .next()
            .and_then(|p| p.parse::<u16>().ok())
            .and_then(|p| p.checked_add(offset))
        else {
            continue;
        };
        out.entry(p).or_insert_with(|| format!("{group}.{service}"));
    }
    out
}

/// The ports the main tree of `stack` declares, for a stack whose daemon
/// does not run: its services' fixed ports, its Compose groups' announced
/// ports (ADR-0057) and the ports on this machine their `urls` name. Other
/// published ports of the Compose files are not read (that takes `docker
/// compose config`).
pub fn declared(stack: &devstack_config::Stack) -> Vec<PortEntry> {
    let mut out: Vec<PortEntry> = Vec::new();
    let mut add = |port: u16, proto: &str, holder: String| {
        if !out.iter().any(|e| e.port == port && e.proto == proto) {
            out.push(PortEntry::new(port, proto, "main", DECLARED, VIA_DIRECT).held_by(holder));
        }
    };
    for (name, s) in &stack.services {
        if let Some(p) = s.port {
            add(p, "tcp", name.clone());
        }
    }
    for (group, g) in &stack.compose {
        for a in g.announced() {
            add(
                a.port,
                if a.udp { "udp" } else { "tcp" },
                format!("{group}.{}", a.service),
            );
        }
        for (service, url) in &g.urls {
            if let Some(p) = local_port(url) {
                add(p, "tcp", format!("{group}.{service}"));
            }
        }
    }
    for e in &mut out {
        e.via = if e.holder.as_deref().is_some_and(|h| h.contains('.')) {
            VIA_DOCKER.into()
        } else {
            VIA_DIRECT.into()
        };
    }
    out
}

/// The port of a link on this machine (`http://localhost:8025/ui`).
fn local_port(url: &str) -> Option<u16> {
    let (_, rest) = url.split_once("://")?;
    let authority = rest.find(['/', '?', '#']).map_or(rest, |i| &rest[..i]);
    let (host, port) = authority.rsplit_once(':')?;
    matches!(host, "localhost" | "127.0.0.1" | "[::1]")
        .then(|| port.parse().ok())
        .flatten()
}

/// One stack's ports, as `GET /hub/ports` lists it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StackPorts {
    pub id: String,
    pub name: String,
    /// Its daemon runs; else `ports` is what its stack file declares.
    pub running: bool,
    pub ports: Vec<PortEntry>,
}

/// One stack's use of an overlapping port.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OverlapUse {
    pub id: String,
    pub name: String,
    pub lane: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holder: Option<String>,
    pub kind: String,
}

/// A port and protocol more than one stack holds or declares.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Overlap {
    pub port: u16,
    pub proto: String,
    /// Who uses it, by stack, each use once.
    pub stacks: Vec<OverlapUse>,
}

/// The ports and protocols more than one of `stacks` uses, by port. An
/// instance port does not count: the OS hands it out, so two running
/// processes never share one, and a stopped service's last one says
/// nothing about tomorrow.
pub fn overlaps(stacks: &[StackPorts]) -> Vec<Overlap> {
    let mut by: BTreeMap<(u16, String), Vec<OverlapUse>> = BTreeMap::new();
    for s in stacks {
        for e in s.ports.iter().filter(|e| e.kind != INSTANCE) {
            let uses = by.entry((e.port, e.proto.clone())).or_default();
            let u = OverlapUse {
                id: s.id.clone(),
                name: s.name.clone(),
                lane: e.lane.clone(),
                holder: e.holder.clone(),
                kind: e.kind.clone(),
            };
            if !uses.contains(&u) {
                uses.push(u);
            }
        }
    }
    by.into_iter()
        .filter(|(_, uses)| {
            let first = &uses[0].id;
            uses.iter().any(|u| &u.id != first)
        })
        .map(|((port, proto), stacks)| Overlap {
            port,
            proto,
            stacks,
        })
        .collect()
}

/// An overlap in one line: `4200/tcp: shop (main, web), docs (declared, site)`.
pub fn overlap_text(o: &Overlap) -> String {
    let uses: Vec<String> = o
        .stacks
        .iter()
        .map(|u| {
            let place = if u.kind == DECLARED {
                "declared".to_string()
            } else {
                u.lane.clone()
            };
            match &u.holder {
                Some(h) => format!("{} ({place}, {h})", u.name),
                None => format!("{} ({place})", u.name),
            }
        })
        .collect();
    format!("{}/{}: {}", o.port, o.proto, uses.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn by_port(entries: &[PortEntry], port: u16) -> &PortEntry {
        entries
            .iter()
            .find(|e| e.port == port)
            .unwrap_or_else(|| panic!("no entry for {port}: {entries:?}"))
    }

    #[test]
    fn a_proxied_service_holds_its_fixed_port_and_its_instance_one() {
        let f = TreeFacts {
            lane: "main".into(),
            services: vec![ServiceFacts {
                name: "api".into(),
                port: Some(5000),
                proxy: Some(Some(49152)),
                instance: Some(49152),
                running: true,
                ..Default::default()
            }],
            ..Default::default()
        };
        let e = tree_entries(&f);
        assert_eq!(e.len(), 2);
        let fixed = by_port(&e, 5000);
        assert_eq!(
            (fixed.kind.as_str(), fixed.via.as_str(), fixed.target),
            (FIXED, VIA_PROXY, Some(49152))
        );
        assert_eq!(fixed.holder.as_deref(), Some("api"));
        assert!(fixed.listening);
        assert_eq!(fixed.lane, "main");
        let inst = by_port(&e, 49152);
        assert_eq!(
            (inst.kind.as_str(), inst.via.as_str()),
            (INSTANCE, VIA_DIRECT)
        );
        assert!(inst.listening);
    }

    #[test]
    fn a_dev_servers_front_forwards_to_its_instance() {
        let f = TreeFacts {
            lane: "main".into(),
            services: vec![ServiceFacts {
                name: "web".into(),
                port: Some(4200),
                front: Some(Some(51000)),
                instance: Some(51000),
                running: true,
                ..Default::default()
            }],
            ..Default::default()
        };
        let e = tree_entries(&f);
        let fixed = by_port(&e, 4200);
        assert_eq!(
            (fixed.kind.as_str(), fixed.via.as_str(), fixed.target),
            (FIXED, VIA_FORWARD, Some(51000))
        );
        assert_eq!(by_port(&e, 51000).kind, INSTANCE);
    }

    #[test]
    fn a_dev_server_without_a_front_listens_on_its_port_itself() {
        let f = TreeFacts {
            lane: "main".into(),
            services: vec![
                ServiceFacts {
                    name: "web".into(),
                    port: Some(4200),
                    instance: Some(4200),
                    running: true,
                    ..Default::default()
                },
                ServiceFacts {
                    name: "docs".into(),
                    port: Some(8080),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let e = tree_entries(&f);
        assert_eq!(e.len(), 2, "no instance entry for the fixed port: {e:?}");
        let web = by_port(&e, 4200);
        assert_eq!((web.via.as_str(), web.listening), (VIA_DIRECT, true));
        // Not running: its port is the stack's all the same.
        assert!(!by_port(&e, 8080).listening);
    }

    #[test]
    fn the_main_slot_served_by_a_lane_leads_there() {
        let f = TreeFacts {
            lane: "main".into(),
            served_by: Some("feat".into()),
            held_to_lane: true,
            services: vec![
                ServiceFacts {
                    name: "api".into(),
                    port: Some(5000),
                    proxy: Some(Some(49300)),
                    to_lane: true,
                    ..Default::default()
                },
                ServiceFacts {
                    name: "web".into(),
                    port: Some(4200),
                    forward: Some(Some(4300)),
                    to_lane: true,
                    ..Default::default()
                },
            ],
            containers: vec![ContainerFacts {
                group: "app".into(),
                service: "ui".into(),
                running: true,
                published: vec![Published {
                    port: 51234,
                    target: 80,
                    proto: "tcp".into(),
                }],
                held: vec![(8081, Some(8181))],
            }],
        };
        let e = tree_entries(&f);
        let api = by_port(&e, 5000);
        assert_eq!(api.target_lane.as_deref(), Some("feat"));
        let web = by_port(&e, 4200);
        assert_eq!(
            (web.via.as_str(), web.target, web.target_lane.as_deref()),
            (VIA_FORWARD, Some(4300), Some("feat"))
        );
        assert!(
            !e.iter().any(|x| x.kind == INSTANCE),
            "the main tree's instances are stopped: {e:?}"
        );
        // A `main_ports` container: its fixed port held, its instance
        // published by Docker on another.
        let held = by_port(&e, 8081);
        assert_eq!(
            (
                held.kind.as_str(),
                held.via.as_str(),
                held.holder.as_deref(),
                held.target,
                held.target_lane.as_deref()
            ),
            (HELD, VIA_FORWARD, Some("app.ui"), Some(8181), Some("feat"))
        );
        let published = by_port(&e, 51234);
        assert_eq!(
            (
                published.kind.as_str(),
                published.via.as_str(),
                published.target
            ),
            (PUBLISHED, VIA_DOCKER, Some(80))
        );
        assert!(published.listening);
    }

    #[test]
    fn a_lanes_entries_carry_its_name_and_moved_ports() {
        let f = TreeFacts {
            lane: "feat".into(),
            services: vec![ServiceFacts {
                name: "api".into(),
                port: Some(5100),
                proxy: Some(Some(49400)),
                instance: Some(49400),
                running: true,
                ..Default::default()
            }],
            containers: vec![ContainerFacts {
                group: "data".into(),
                service: "db".into(),
                running: false,
                published: vec![
                    Published {
                        port: 5532,
                        target: 5432,
                        proto: "tcp".into(),
                    },
                    Published {
                        port: 5453,
                        target: 53,
                        proto: "udp".into(),
                    },
                ],
                held: Vec::new(),
            }],
            ..Default::default()
        };
        let e = tree_entries(&f);
        assert!(e.iter().all(|x| x.lane == "feat"));
        assert_eq!(by_port(&e, 5100).kind, FIXED);
        let db = by_port(&e, 5532);
        assert_eq!(db.holder.as_deref(), Some("data.db"));
        assert!(!db.listening, "its container does not run");
        assert_eq!(by_port(&e, 5453).proto, "udp");
    }

    #[test]
    fn a_lane_on_a_node_is_its_forwards_here() {
        let mut holders = BTreeMap::new();
        holders.insert(5100, "api".to_string());
        let e = node_entries(&NodeFacts {
            lane: "feat".into(),
            node: "build-server".into(),
            tcp: vec![(5100, 5100), (5532, 5532)],
            udp: vec![(5453, 5453)],
            listening: true,
            holders,
        });
        assert_eq!(e.len(), 3);
        let api = by_port(&e, 5100);
        assert_eq!(
            (
                api.kind.as_str(),
                api.via.as_str(),
                api.node.as_deref(),
                api.holder.as_deref()
            ),
            (NODE, VIA_FORWARD, Some("build-server"), Some("api"))
        );
        assert_eq!(by_port(&e, 5532).holder, None);
        assert_eq!(by_port(&e, 5453).proto, "udp");
    }

    #[test]
    fn a_lanes_holders_are_the_main_trees_ports_moved() {
        let stack = devstack_config::from_str(
            r#"
version = 1
[stack]
name = "shop"
[services.api]
kind = "service"
run = "x"
port = 5000
"#,
            std::path::Path::new("t"),
        )
        .unwrap();
        let mut published = BTreeMap::new();
        published.insert(
            ("data".to_string(), "db".to_string(), 5432),
            "127.0.0.1:15432".to_string(),
        );
        let h = lane_holders(&stack, &published, 100);
        assert_eq!(h.get(&5100).map(String::as_str), Some("api"));
        assert_eq!(h.get(&15532).map(String::as_str), Some("data.db"));
    }

    #[test]
    fn publishers_are_each_port_and_protocol_once() {
        let c = devstack_compose::parse_ps(
            r#"{"Service":"db","Name":"x-db-1","State":"running","Health":"","ExitCode":0,"Publishers":[{"URL":"0.0.0.0","TargetPort":5432,"PublishedPort":15432,"Protocol":"tcp"},{"URL":"::","TargetPort":5432,"PublishedPort":15432,"Protocol":"tcp"},{"URL":"0.0.0.0","TargetPort":53,"PublishedPort":5353,"Protocol":"udp"},{"URL":"","TargetPort":9000,"PublishedPort":0}]}"#,
        )
        .unwrap()
        .remove(0);
        let b = bound(&c.publishers);
        assert_eq!(
            b,
            [
                Published {
                    port: 15432,
                    target: 5432,
                    proto: "tcp".into()
                },
                Published {
                    port: 5353,
                    target: 53,
                    proto: "udp".into()
                }
            ]
        );
    }

    #[test]
    fn a_stack_file_declares_its_services_announced_and_linked_ports() {
        let stack = devstack_config::from_str(
            r#"
version = 1
[stack]
name = "shop"
[services.api]
kind = "service"
run = "x"
port = 5000
[services.worker]
kind = "service"
run = "x"
[compose.data]
project = "shop"
files = ["compose.yml"]
services = ["db", "dns", "mail"]
announced_ports = ["db:5432", "dns:53/udp"]
[compose.data.urls]
mail = "http://localhost:8025"
db = ""
"#,
            std::path::Path::new("t"),
        )
        .unwrap();
        let mut d = declared(&stack);
        d.sort_by_key(|e| (e.port, e.proto.clone()));
        let got: Vec<(u16, &str, &str, &str)> = d
            .iter()
            .map(|e| {
                (
                    e.port,
                    e.proto.as_str(),
                    e.holder.as_deref().unwrap(),
                    e.via.as_str(),
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                (53, "udp", "data.dns", VIA_DOCKER),
                (5000, "tcp", "api", VIA_DIRECT),
                (5432, "tcp", "data.db", VIA_DOCKER),
                (8025, "tcp", "data.mail", VIA_DOCKER),
            ]
        );
        assert!(
            d.iter()
                .all(|e| e.kind == DECLARED && e.lane == "main" && !e.listening)
        );
    }

    fn entry(port: u16, proto: &str, lane: &str, holder: &str) -> PortEntry {
        PortEntry::new(port, proto, lane, FIXED, VIA_PROXY).held_by(holder)
    }

    #[test]
    fn an_overlap_is_one_port_and_protocol_in_more_than_one_stack() {
        let stacks = vec![
            StackPorts {
                id: "a".into(),
                name: "shop".into(),
                running: true,
                ports: vec![
                    entry(4200, "tcp", "main", "web"),
                    entry(5000, "tcp", "main", "api"),
                    entry(5353, "udp", "main", "data.dns"),
                ],
            },
            StackPorts {
                id: "b".into(),
                name: "crm".into(),
                running: true,
                ports: vec![
                    entry(4200, "tcp", "feat", "portal"),
                    entry(5353, "tcp", "main", "gateway"),
                    // The OS hands instance ports out: never an overlap.
                    PortEntry::new(5000, "tcp", "main", INSTANCE, VIA_DIRECT).held_by("api"),
                ],
            },
            StackPorts {
                id: "c".into(),
                name: "docs".into(),
                running: false,
                ports: vec![
                    PortEntry::new(4200, "tcp", "main", DECLARED, VIA_DIRECT).held_by("site"),
                    // Twice in one stack is no overlap.
                    entry(9000, "tcp", "main", "one"),
                    entry(9000, "tcp", "feat", "one"),
                ],
            },
        ];
        let o = overlaps(&stacks);
        assert_eq!(o.len(), 1, "{o:?}");
        assert_eq!((o[0].port, o[0].proto.as_str()), (4200, "tcp"));
        let names: Vec<&str> = o[0].stacks.iter().map(|u| u.name.as_str()).collect();
        assert_eq!(names, ["shop", "crm", "docs"]);
        assert_eq!(
            overlap_text(&o[0]),
            "4200/tcp: shop (main, web), crm (feat, portal), docs (declared, site)"
        );
    }
}
