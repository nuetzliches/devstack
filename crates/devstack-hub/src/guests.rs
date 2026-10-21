//! Lanes of other members that run on this machine ("guest lanes"): whoever
//! sits at a machine that runs the pool's lanes sees, without asking, that
//! someone else's code runs on it. The coordinator lists every lease of the
//! pool to every member; those whose node is this machine and whose owner is
//! someone else are the guests. The share loop (`share.rs`) asks after each
//! report, every 15 s per pool. The dashboard hears them on `/hub/events`
//! (`guests`), and a guest lane that starts notifies (ADR-0045,
//! `guest_lane`).

use devstack_node::coordinator::{Lease, NodeView};
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};
use std::sync::{Mutex, OnceLock};
use tokio::sync::watch;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Guest {
    pub pool: String,
    pub lease: String,
    /// The member whose lane it is, and how the pool names them.
    pub member: String,
    pub owner: String,
    pub repo: String,
    pub lane: String,
    /// `offered` (admitted by the coordinator, not started yet) or `running`.
    pub state: String,
    /// Unix seconds.
    pub since: u64,
    /// What it asked for.
    pub cpus: f64,
    pub memory: u64,
}

type Started = Box<dyn Fn(&Guest) + Send + Sync>;

struct Guests {
    tx: watch::Sender<Vec<Guest>>,
    /// Per pool, as last read; a pool that could not be asked keeps its last.
    by_pool: Mutex<BTreeMap<String, Vec<Guest>>>,
    /// Per pool, the running leases already told (or there when the hub
    /// first read the pool).
    told: Mutex<BTreeMap<String, HashSet<String>>>,
    started: OnceLock<Started>,
}

static GUESTS: OnceLock<Guests> = OnceLock::new();

fn guests() -> &'static Guests {
    GUESTS.get_or_init(|| Guests {
        tx: watch::channel(Vec::new()).0,
        by_pool: Mutex::new(BTreeMap::new()),
        told: Mutex::new(BTreeMap::new()),
        started: OnceLock::new(),
    })
}

/// The guest lanes now, and each change.
pub fn subscribe() -> watch::Receiver<Vec<Guest>> {
    guests().tx.subscribe()
}

/// What to do when a guest lane starts running here (the hub's notification).
pub fn on_started(f: impl Fn(&Guest) + Send + Sync + 'static) {
    let _ = guests().started.set(Box::new(f));
}

/// The guests in `leases` of `pool`, for the member `me`; owners named from
/// `nodes` (every member reports, so every member is listed there).
pub fn of(pool: &str, me: &str, leases: &[Lease], nodes: &[NodeView]) -> Vec<Guest> {
    leases
        .iter()
        .filter(|l| l.node == me && l.member != me)
        .map(|l| Guest {
            pool: pool.to_string(),
            lease: l.id.clone(),
            member: l.member.clone(),
            owner: nodes
                .iter()
                .find(|n| n.id == l.member)
                .map_or_else(|| l.member.clone(), |n| n.name.clone()),
            repo: l.repo.clone(),
            lane: l.lane.clone(),
            state: l.state.clone(),
            since: l.created,
            cpus: l.need.cpus,
            memory: l.need.memory,
        })
        .collect()
}

/// The guests of `pool` as just read (`None`: it could not be asked, the
/// last list stands). A pool's first reading is its baseline: guest lanes
/// running then are not told.
pub fn seen(pool: &str, now: Option<Vec<Guest>>) {
    let g = guests();
    let Some(list) = now else { return };
    let running: Vec<&Guest> = list.iter().filter(|x| x.state == "running").collect();
    let fresh: Vec<Guest> = {
        let mut told = g.told.lock().unwrap();
        match told.get_mut(pool) {
            None => {
                let set = running.iter().map(|x| x.lease.clone()).collect();
                told.insert(pool.to_string(), set);
                Vec::new()
            }
            Some(set) => {
                // Forget leases that are gone, so the set never only grows.
                set.retain(|l| list.iter().any(|x| &x.lease == l));
                running
                    .iter()
                    .filter(|x| set.insert(x.lease.clone()))
                    .map(|x| (*x).clone())
                    .collect()
            }
        }
    };
    let all: Vec<Guest> = {
        let mut by_pool = g.by_pool.lock().unwrap();
        by_pool.insert(pool.to_string(), list);
        by_pool.values().flatten().cloned().collect()
    };
    g.tx.send_if_modified(|cur| {
        if *cur == all {
            false
        } else {
            *cur = all;
            true
        }
    });
    if let Some(f) = g.started.get() {
        for x in &fresh {
            f(x);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lease(id: &str, node: &str, member: &str, state: &str) -> Lease {
        serde_json::from_value(serde_json::json!({
            "id": id, "node": node, "member": member, "repo": "shop", "lane": "feat-cart",
            "need": { "cpus": 2.0, "memory": 4_000_000_000u64 }, "state": state, "created": 1_700_000_000u64,
        }))
        .unwrap()
    }

    fn node(id: &str, name: &str) -> NodeView {
        serde_json::from_value(serde_json::json!({ "id": id, "name": name, "online": true }))
            .unwrap()
    }

    #[test]
    fn guests_are_others_lanes_on_this_machine() {
        let leases = [
            lease("l1", "me", "anna", "running"),
            lease("l2", "me", "me", "running"), // my own lane here
            lease("l3", "server", "anna", "running"), // anna's lane elsewhere
            lease("l4", "me", "ghost", "offered"),
        ];
        let g = of("default", "me", &leases, &[node("anna", "ANNA-LAPTOP")]);
        assert_eq!(g.len(), 2);
        assert_eq!(
            (g[0].lease.as_str(), g[0].owner.as_str()),
            ("l1", "ANNA-LAPTOP")
        );
        // An owner the node list does not name is shown by id.
        assert_eq!(
            (g[1].owner.as_str(), g[1].state.as_str()),
            ("ghost", "offered")
        );
        assert_eq!((g[0].cpus, g[0].memory), (2.0, 4_000_000_000));
    }

    #[test]
    fn a_guest_lane_that_starts_is_told_once_and_a_pools_first_reading_is_none() {
        static TOLD: Mutex<Vec<String>> = Mutex::new(Vec::new());
        on_started(|g| TOLD.lock().unwrap().push(g.lease.clone()));
        let rx = subscribe();
        let read = |pool: &str, leases: &[Lease]| seen(pool, Some(of(pool, "me", leases, &[])));
        // What runs when the hub first reads a pool is the baseline.
        read("t-a", &[lease("a1", "me", "anna", "running")]);
        read("t-b", &[lease("b1", "me", "ben", "running")]);
        assert!(TOLD.lock().unwrap().is_empty());
        // Offered, then running: told when it runs, once.
        read(
            "t-a",
            &[
                lease("a1", "me", "anna", "running"),
                lease("a2", "me", "anna", "offered"),
            ],
        );
        read(
            "t-a",
            &[
                lease("a1", "me", "anna", "running"),
                lease("a2", "me", "anna", "running"),
            ],
        );
        read("t-a", &[lease("a2", "me", "anna", "running")]);
        // A pool that could not be asked keeps its guests.
        seen("t-b", None);
        assert_eq!(*TOLD.lock().unwrap(), ["a2"]);
        let now: Vec<String> = rx.borrow().iter().map(|g| g.lease.clone()).collect();
        assert!(
            now.contains(&"a2".to_string()) && now.contains(&"b1".to_string()),
            "{now:?}"
        );
        assert!(!now.contains(&"a1".to_string()), "{now:?}");
    }
}
