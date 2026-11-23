//! Machines that ask to run the pool's lanes (#599): a member switched
//! sharing on (ADR-0113), its node agent reports, and no admin granted it
//! yet. The coordinator keeps such a report and places nothing there; an
//! admin of the pool is told once per machine that starts asking (ADR-0045,
//! `machine_asks`). The dashboard finds them in the pool overview itself.
//! Read when the pool's stream says its members or nodes changed
//! (`poolfeed.rs`, ADR-0073), like the guest lanes.

use devstack_node::coordinator::{MemberView, NodeView};
use devstack_node::store::{Role, State};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// A machine that asks again within this time after it was told is not
/// told again: switching sharing on and off, or going on- and offline,
/// would otherwise notify every admin each time (review before 0.52.0).
const TELL_AGAIN_AFTER: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Clone, PartialEq)]
pub struct Ask {
    pub pool: String,
    /// The member, and how the pool names it.
    pub id: String,
    pub name: String,
}

type Started = Box<dyn Fn(&Ask) + Send + Sync>;

struct Asks {
    /// Per pool, the machines asking now, told or there when the hub first
    /// read the pool as its admin.
    told: Mutex<BTreeMap<String, HashSet<String>>>,
    /// When each machine (`pool`, member) was told last.
    last: Mutex<HashMap<(String, String), Instant>>,
    started: OnceLock<Started>,
}

fn asks() -> &'static Asks {
    static ASKS: OnceLock<Asks> = OnceLock::new();
    ASKS.get_or_init(|| Asks {
        told: Mutex::new(BTreeMap::new()),
        last: Mutex::new(HashMap::new()),
        started: OnceLock::new(),
    })
}

/// What to do when a machine starts asking (the hub's notification).
pub fn on_started(f: impl Fn(&Ask) + Send + Sync + 'static) {
    let _ = asks().started.set(Box::new(f));
}

/// The machines of `pool` that ask, as the member `me` sees them; `None`
/// unless `me` is an admin there (no baseline then: what asks when it
/// becomes one is its baseline, not a burst of notices). A machine asks
/// while its agent reports sharing, online, and the member has no grant to
/// run lanes; `me` itself is never told about.
pub fn of(pool: &str, me: &str, members: &[MemberView], nodes: &[NodeView]) -> Option<Vec<Ask>> {
    let admin = members
        .iter()
        .any(|m| m.id == me && m.role == Role::Admin && m.state == State::Approved);
    if !admin {
        return None;
    }
    let asking = members
        .iter()
        .filter(|m| m.id != me && m.state == State::Approved && !m.node)
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
        .collect();
    Some(asking)
}

/// The machines of `pool` that ask, as just read (`None`: it could not be
/// asked, or this machine is no admin there; nothing changes). A pool's
/// first reading is its baseline: what asked then is not told; a machine
/// that stops asking and asks again is, unless it was told within
/// [`TELL_AGAIN_AFTER`].
pub fn seen(pool: &str, now: Option<Vec<Ask>>) {
    seen_at(pool, now, Instant::now());
}

fn seen_at(pool: &str, now: Option<Vec<Ask>>, at: Instant) {
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
                let mut last = a.last.lock().unwrap();
                // An ask too soon after the last told is not counted as
                // told: it is told once the pause is over, if it still
                // asks (code review before 0.53.0, finding 9).
                list.iter()
                    .filter(|x| {
                        if set.contains(&x.id) {
                            return false;
                        }
                        let key = (pool.to_string(), x.id.clone());
                        let due = last
                            .get(&key)
                            .is_none_or(|t| at.saturating_duration_since(*t) >= TELL_AGAIN_AFTER);
                        if due {
                            set.insert(x.id.clone());
                            last.insert(key, at);
                        }
                        due
                    })
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
        let a = of("default", "me", &members, &nodes).unwrap();
        assert_eq!(
            a.iter().map(|x| x.id.as_str()).collect::<Vec<_>>(),
            ["anna"]
        );
        assert_eq!(a[0].name, "ANNA");
        // A member who is no admin has no reading at all (no baseline).
        assert!(of("default", "anna", &members, &nodes).is_none());
        // An admin's own machine that asks is not told about.
        let mut mine = members.to_vec();
        mine.push(member("boss", "admin", false));
        let nodes2 = [node("boss", true, true)];
        assert!(of("default", "boss", &mine, &nodes2).unwrap().is_empty());
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
        let t0 = Instant::now();
        let at = |m: u64| t0 + Duration::from_secs(m * 60);
        seen_at("t", Some(vec![ask("anna")]), at(0));
        assert!(TOLD.lock().unwrap().is_empty(), "the baseline is not told");
        seen_at("t", Some(vec![ask("anna"), ask("ben")]), at(1));
        seen_at("t", Some(vec![ask("anna"), ask("ben")]), at(2));
        seen_at("t", None, at(3));
        // Switched off and on again soon after: not told again.
        seen_at("t", Some(vec![ask("anna")]), at(4));
        seen_at("t", Some(vec![ask("anna"), ask("ben")]), at(5));
        assert_eq!(*TOLD.lock().unwrap(), ["ben"]);
        // Granted (or switched off), then asking again much later: told again.
        seen_at("t", Some(vec![ask("anna")]), at(40));
        seen_at("t", Some(vec![ask("anna"), ask("ben")]), at(41));
        assert_eq!(*TOLD.lock().unwrap(), ["ben", "ben"]);
        // anna was part of the baseline: told once she asks anew.
        seen_at("t", Some(vec![ask("ben")]), at(42));
        seen_at("t", Some(vec![ask("anna"), ask("ben")]), at(43));
        assert_eq!(*TOLD.lock().unwrap(), ["ben", "ben", "anna"]);
        // Code review before 0.53.0, finding 9: asking again too soon is
        // not told then, but once the pause is over while it still asks.
        seen_at("t", Some(vec![ask("anna")]), at(44));
        seen_at("t", Some(vec![ask("anna"), ask("ben")]), at(45));
        assert_eq!(*TOLD.lock().unwrap(), ["ben", "ben", "anna"]);
        seen_at("t", Some(vec![ask("anna"), ask("ben")]), at(72));
        assert_eq!(*TOLD.lock().unwrap(), ["ben", "ben", "anna", "ben"]);
    }
}
