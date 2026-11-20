//! Machines that ask to run the pool's lanes (#599): a member switched
//! sharing on (ADR-0113), its node agent reports, and no admin granted it
//! yet. The coordinator keeps such a report and places nothing there; an
//! admin of the pool is told once per machine that starts asking (ADR-0045,
//! `machine_asks`). The dashboard finds them in the pool overview itself.
//! Read when the pool's stream says its members or nodes changed
//! (`poolfeed.rs`, ADR-0073), like the guest lanes.

use devstack_node::coordinator::{MemberView, NodeView};
use devstack_node::store::{Role, State};
use std::collections::{BTreeMap, HashSet};
use std::sync::{Mutex, OnceLock};

#[derive(Debug, Clone, PartialEq)]
pub struct Ask {
    pub pool: String,
    /// The member, and how the pool names it.
    pub id: String,
    pub name: String,
}

type Started = Box<dyn Fn(&Ask) + Send + Sync>;

struct Asks {
    /// Per pool, the machines already told (or asking when the hub first
    /// read the pool).
    told: Mutex<BTreeMap<String, HashSet<String>>>,
    started: OnceLock<Started>,
}

fn asks() -> &'static Asks {
    static ASKS: OnceLock<Asks> = OnceLock::new();
    ASKS.get_or_init(|| Asks {
        told: Mutex::new(BTreeMap::new()),
        started: OnceLock::new(),
    })
}

/// What to do when a machine starts asking (the hub's notification).
pub fn on_started(f: impl Fn(&Ask) + Send + Sync + 'static) {
    let _ = asks().started.set(Box::new(f));
}

/// The machines of `pool` that ask, as the member `me` sees them: none
/// unless `me` is an admin there. A machine asks while its agent reports
/// sharing, online, and the member has no grant to run lanes.
pub fn of(pool: &str, me: &str, members: &[MemberView], nodes: &[NodeView]) -> Vec<Ask> {
    let admin = members
        .iter()
        .any(|m| m.id == me && m.role == Role::Admin && m.state == State::Approved);
    if !admin {
        return Vec::new();
    }
    members
        .iter()
        .filter(|m| m.state == State::Approved && !m.node)
        .filter(|m| {
            nodes
                .iter()
                .any(|n| n.id == m.id && n.online && n.report.as_ref().is_some_and(|r| r.sharing))
        })
        .map(|m| Ask {
            pool: pool.to_string(),
            id: m.id.clone(),
            name: m.name.clone(),
        })
        .collect()
}

/// The machines of `pool` that ask, as just read (`None`: it could not be
/// asked, nothing changes). A pool's first reading is its baseline: what
/// asked then is not told; a machine that stops asking and asks again is.
pub fn seen(pool: &str, now: Option<Vec<Ask>>) {
    let a = asks();
    let Some(list) = now else { return };
    let fresh: Vec<Ask> = {
        let mut told = a.told.lock().unwrap();
        match told.get_mut(pool) {
            None => {
                told.insert(
                    pool.to_string(),
                    list.iter().map(|x| x.id.clone()).collect(),
                );
                Vec::new()
            }
            Some(set) => {
                set.retain(|id| list.iter().any(|x| &x.id == id));
                list.iter()
                    .filter(|x| set.insert(x.id.clone()))
                    .cloned()
                    .collect()
            }
        }
    };
    if let Some(f) = a.started.get() {
        for x in &fresh {
            f(x);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(id: &str, role: &str, node: bool) -> MemberView {
        serde_json::from_value(serde_json::json!({
            "id": id, "name": id.to_uppercase(), "state": "approved", "role": role,
            "requested_at": 1_700_000_000u64, "node": node,
        }))
        .unwrap()
    }

    fn node(id: &str, online: bool, sharing: bool) -> NodeView {
        serde_json::from_value(serde_json::json!({
            "id": id, "name": id, "online": online,
            "report": { "sharing": sharing, "cpus": 4.0, "memory": 8_000_000_000u64 },
        }))
        .unwrap()
    }

    #[test]
    fn a_machine_asks_while_it_shares_without_a_grant_and_only_admins_see_it() {
        let members = [
            member("me", "admin", false),
            member("anna", "member", false),
            member("ben", "member", true),
            member("carl", "member", false),
            member("dora", "member", false),
        ];
        let nodes = [
            node("anna", true, true),  // asks
            node("ben", true, true),   // granted
            node("carl", true, false), // not sharing
            node("dora", false, true), // offline
        ];
        let a = of("default", "me", &members, &nodes);
        assert_eq!(
            a.iter().map(|x| x.id.as_str()).collect::<Vec<_>>(),
            ["anna"]
        );
        assert_eq!(a[0].name, "ANNA");
        // A member who is no admin is told nothing.
        assert!(of("default", "anna", &members, &nodes).is_empty());
    }

    #[test]
    fn a_machine_that_starts_asking_is_told_once_and_a_pools_first_reading_is_none() {
        static TOLD: Mutex<Vec<String>> = Mutex::new(Vec::new());
        on_started(|a| TOLD.lock().unwrap().push(a.id.clone()));
        let ask = |id: &str| Ask {
            pool: "t".into(),
            id: id.into(),
            name: id.into(),
        };
        seen("t", Some(vec![ask("anna")]));
        assert!(TOLD.lock().unwrap().is_empty(), "the baseline is not told");
        seen("t", Some(vec![ask("anna"), ask("ben")]));
        seen("t", Some(vec![ask("anna"), ask("ben")]));
        seen("t", None);
        // Granted (or switched off), then asking again: told again.
        seen("t", Some(vec![ask("ben")]));
        seen("t", Some(vec![ask("anna"), ask("ben")]));
        assert_eq!(*TOLD.lock().unwrap(), ["ben", "anna"]);
    }
}
