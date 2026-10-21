//! What notifies (ADR-0045 point 2): the changes in a stack's or a lane's
//! event stream that need someone's attention. Only changes: the state a
//! stream starts with (its `snapshot`, the lanes as first read) is the
//! baseline, and what was already so then says nothing (point 3).
//!
//! Events are read as JSON, not as the daemon's types: a daemon of another
//! version may add fields or states, and what is not understood here is
//! simply not notified.

use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    BuildFailed,
    Held,
    ServiceFailed,
    LaneFailed,
    MainPorts,
    Live,
    GuestLane,
}

impl Kind {
    pub const ALL: [Kind; 7] = [
        Kind::BuildFailed,
        Kind::Held,
        Kind::ServiceFailed,
        Kind::LaneFailed,
        Kind::MainPorts,
        Kind::Live,
        Kind::GuestLane,
    ];

    pub fn key(self) -> &'static str {
        match self {
            Kind::BuildFailed => "build_failed",
            Kind::Held => "held",
            Kind::ServiceFailed => "service_failed",
            Kind::LaneFailed => "lane_failed",
            Kind::MainPorts => "main_ports",
            Kind::Live => "live",
            Kind::GuestLane => "guest_lane",
        }
    }

    pub fn from_key(key: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.key() == key)
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::BuildFailed => "A build failed",
            Kind::Held => "A candidate is held and waits for promote",
            Kind::ServiceFailed => "A service exited by itself or failed to start",
            Kind::LaneFailed => "A lane's start or move on a node failed",
            Kind::MainPorts => "A lane took or gave back the main ports",
            Kind::Live => "A candidate went live",
            Kind::GuestLane => "A lane of another member started on this machine",
        }
    }

    pub fn default_on(self) -> bool {
        !matches!(self, Kind::MainPorts | Kind::Live)
    }
}

/// Something to tell, before it is known whose stack it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub kind: Kind,
    /// The service it is about; `None` for a lane as a whole.
    pub service: Option<String>,
    /// The lane it is about, for a lane event of the main tree's stream
    /// (a lane's own stream knows its lane already).
    pub lane: Option<String>,
    pub title: String,
    /// One line or a few: why, from the event.
    pub body: String,
}

/// A start the user stopped is no failure (the daemon's `STOPPED`).
const STOPPED_PREFIX: &str = "stopped while it started";
/// A service held back because one it comes `after` failed: that one
/// notifies, not each of its dependants.
const CASCADE_PREFIX: &str = "not started: ";

#[derive(Debug, Default, Clone)]
struct Svc {
    /// `status.state`.
    state: String,
    /// The candidate's `state` and `build_id`.
    candidate: Option<(String, Option<String>)>,
}

/// What one event stream (the main tree's, or one lane's) showed last, per
/// service.
#[derive(Debug, Default)]
pub struct Scope {
    services: HashMap<String, Svc>,
}

impl Scope {
    /// The notices `event` gives, after the state it changed.
    pub fn apply(&mut self, event: &Value) -> Vec<Notice> {
        match event["type"].as_str() {
            Some("snapshot") => {
                self.services = event["stack"]["services"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|s| {
                        Some((
                            s["name"].as_str()?.to_string(),
                            Svc {
                                state: state_of(&s["status"]),
                                candidate: candidate_of(&s["candidate"]),
                            },
                        ))
                    })
                    .collect();
                Vec::new()
            }
            Some("service") => {
                let Some(name) = event["service"].as_str() else {
                    return Vec::new();
                };
                let svc = self.services.entry(name.to_string()).or_default();
                let before = std::mem::replace(&mut svc.state, state_of(&event["status"]));
                service_notice(name, &before, &event["status"])
                    .into_iter()
                    .collect()
            }
            Some("candidate") => {
                let Some(name) = event["service"].as_str() else {
                    return Vec::new();
                };
                let svc = self.services.entry(name.to_string()).or_default();
                let now = candidate_of(&event["candidate"]);
                let before = std::mem::replace(&mut svc.candidate, now.clone());
                candidate_notice(name, before, now.as_ref(), &event["candidate"])
                    .into_iter()
                    .collect()
            }
            _ => Vec::new(),
        }
    }
}

fn state_of(status: &Value) -> String {
    status["state"].as_str().unwrap_or("").to_string()
}

fn candidate_of(c: &Value) -> Option<(String, Option<String>)> {
    let state = c["state"].as_str()?;
    Some((
        state.to_string(),
        c["build_id"].as_str().map(str::to_string),
    ))
}

fn service_notice(name: &str, before: &str, status: &Value) -> Option<Notice> {
    let notice = |kind, title: String, body: String| Notice {
        kind,
        service: Some(name.to_string()),
        lane: None,
        title,
        body,
    };
    match status["state"].as_str()? {
        "failed" if before != "failed" => {
            let reason = status["reason"].as_str().unwrap_or("");
            if reason.starts_with(STOPPED_PREFIX) || reason.starts_with(CASCADE_PREFIX) {
                return None;
            }
            // The build at its start: the status said `building` until now.
            if before == "building" {
                Some(notice(
                    Kind::BuildFailed,
                    format!("{name}: build failed"),
                    first_lines(reason, 3),
                ))
            } else {
                Some(notice(
                    Kind::ServiceFailed,
                    format!("{name} failed"),
                    first_lines(reason, 3),
                ))
            }
        }
        "exited" if before != "exited" && before != "stopped" && !before.is_empty() => {
            let code = status["exit_code"].as_i64().unwrap_or(0);
            Some(notice(
                Kind::ServiceFailed,
                format!("{name} exited"),
                format!("It ended by itself with code {code}; devstack starts it again."),
            ))
        }
        _ => None,
    }
}

fn candidate_notice(
    name: &str,
    before: Option<(String, Option<String>)>,
    now: Option<&(String, Option<String>)>,
    raw: &Value,
) -> Option<Notice> {
    let notice = |kind, title: String, body: String| Notice {
        kind,
        service: Some(name.to_string()),
        lane: None,
        title,
        body,
    };
    let reason = raw["reason"].as_str().unwrap_or("");
    match (before, now) {
        // Promoted: the candidate that was switching is gone.
        (Some((was, build)), None) if was == "switching" => Some(notice(
            Kind::Live,
            format!("{name}: new build is live"),
            build
                .map(|b| format!("Build {} serves now.", short(&b)))
                .unwrap_or_default(),
        )),
        (before, Some((state, _))) if state == "failed" => {
            if before.is_some_and(|(s, _)| s == "failed") {
                return None;
            }
            Some(notice(
                Kind::BuildFailed,
                format!("{name}: build failed"),
                first_lines(reason, 3),
            ))
        }
        (before, Some((state, build))) if state == "held" => {
            if before.is_some_and(|(s, b)| s == "held" && b == *build) {
                return None;
            }
            Some(notice(
                Kind::Held,
                format!("{name}: build held for promote"),
                first_lines(reason, 3),
            ))
        }
        _ => None,
    }
}

/// What the lanes showed last, from the main tree's stream: who serves the
/// main ports, and the last failed start or move of each.
#[derive(Debug, Default)]
pub struct Lanes {
    known: Option<HashMap<String, LaneSeen>>,
}

#[derive(Debug, Clone, Default)]
struct LaneSeen {
    owns_main: bool,
    failed_at: Option<u64>,
}

impl Lanes {
    /// The lanes as read when the stream (re)connects: the baseline.
    pub fn baseline(&mut self, lanes: &Value) {
        self.known = Some(seen(lanes));
    }

    /// The notices a `lanes` event gives. Before a baseline it is one.
    pub fn apply(&mut self, lanes: &Value) -> Vec<Notice> {
        let now = seen(lanes);
        let Some(before) = self.known.replace(now.clone()) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for l in lanes.as_array().into_iter().flatten() {
            let Some(name) = l["name"].as_str() else {
                continue;
            };
            let was = before.get(name).cloned().unwrap_or_default();
            let is = &now[name];
            if let Some(at) = is.failed_at
                && was.failed_at != Some(at)
            {
                let m = &l["moving"];
                let what = if m["kind"] == "move" { "move" } else { "start" };
                let to = m["to"].as_str().unwrap_or("a node");
                out.push(Notice {
                    kind: Kind::LaneFailed,
                    service: None,
                    lane: Some(name.to_string()),
                    title: format!("Lane {name}: {what} on {to} failed"),
                    body: first_lines(m["error"].as_str().unwrap_or(""), 3),
                });
            }
            if is.owns_main != was.owns_main && before.contains_key(name) {
                out.push(Notice {
                    kind: Kind::MainPorts,
                    service: None,
                    lane: Some(name.to_string()),
                    title: if is.owns_main {
                        format!("Lane {name} serves the main ports")
                    } else {
                        format!("Lane {name} gave the main ports back")
                    },
                    body: String::new(),
                });
            }
        }
        out
    }

    /// The lanes that have an API of their own now: started, on this
    /// machine or on a node.
    pub fn started(lanes: &Value) -> Vec<String> {
        lanes
            .as_array()
            .into_iter()
            .flatten()
            .filter(|l| l["active"].as_bool() == Some(true))
            .filter_map(|l| l["name"].as_str().map(str::to_string))
            .collect()
    }
}

fn seen(lanes: &Value) -> HashMap<String, LaneSeen> {
    lanes
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|l| {
            Some((
                l["name"].as_str()?.to_string(),
                LaneSeen {
                    owns_main: l["owns_main"].as_bool().unwrap_or(false),
                    failed_at: l["moving"]["failed_at"].as_u64(),
                },
            ))
        })
        .collect()
}

/// At most `n` non-empty lines, each at most 200 characters.
fn first_lines(text: &str, n: usize) -> String {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(n)
        .map(|l| {
            if l.chars().count() > 200 {
                format!("{}…", l.chars().take(199).collect::<String>())
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn short(build: &str) -> &str {
    build.get(..8).unwrap_or(build)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn snapshot(services: Value) -> Value {
        json!({ "type": "snapshot", "seq": 0, "stack": { "services": services } })
    }

    fn service(name: &str, status: Value) -> Value {
        json!({ "type": "service", "service": name, "status": status, "build_id": null })
    }

    fn candidate(name: &str, c: Value) -> Value {
        json!({ "type": "candidate", "service": name, "candidate": c })
    }

    fn kinds(n: &[Notice]) -> Vec<Kind> {
        n.iter().map(|n| n.kind).collect()
    }

    #[test]
    fn what_the_snapshot_shows_is_the_baseline() {
        let mut s = Scope::default();
        let n = s.apply(&snapshot(json!([
            { "name": "api", "status": { "state": "failed", "reason": "boom" },
              "candidate": { "state": "held", "build_id": "b1", "reason": "schema" } }
        ])));
        assert!(n.is_empty());
        // The same held build again, and the same failure: nothing new.
        assert!(
            s.apply(&candidate(
                "api",
                json!({ "state": "held", "build_id": "b1" })
            ))
            .is_empty()
        );
        assert!(
            s.apply(&service(
                "api",
                json!({ "state": "failed", "reason": "boom" })
            ))
            .is_empty()
        );
    }

    #[test]
    fn a_failed_candidate_build_notifies_once() {
        let mut s = Scope::default();
        s.apply(&snapshot(
            json!([{ "name": "api", "status": { "state": "healthy" } }]),
        ));
        assert!(
            s.apply(&candidate("api", json!({ "state": "building" })))
                .is_empty()
        );
        let n = s.apply(&candidate(
            "api",
            json!({ "state": "failed", "build_id": null, "reason": "error CS1002: ; expected\n\n  at Foo.cs:3" }),
        ));
        assert_eq!(kinds(&n), [Kind::BuildFailed]);
        assert_eq!(n[0].title, "api: build failed");
        assert_eq!(n[0].body, "error CS1002: ; expected\nat Foo.cs:3");
        assert_eq!(n[0].service.as_deref(), Some("api"));
        // Still failed (the same state sent again): nothing.
        assert!(
            s.apply(&candidate(
                "api",
                json!({ "state": "failed", "reason": "x" })
            ))
            .is_empty()
        );
        // The next build fails again: a new notice (folding is the caller's).
        s.apply(&candidate("api", json!({ "state": "building" })));
        assert_eq!(
            kinds(&s.apply(&candidate("api", json!({ "state": "failed" })))),
            [Kind::BuildFailed]
        );
    }

    #[test]
    fn a_held_candidate_and_a_promotion() {
        let mut s = Scope::default();
        s.apply(&snapshot(
            json!([{ "name": "api", "status": { "state": "healthy" } }]),
        ));
        let n = s.apply(&candidate(
            "api",
            json!({ "state": "held", "build_id": "b2", "reason": "migrations changed" }),
        ));
        assert_eq!(kinds(&n), [Kind::Held]);
        assert_eq!(n[0].body, "migrations changed");
        // A newer build held instead: told again.
        assert_eq!(
            kinds(&s.apply(&candidate(
                "api",
                json!({ "state": "held", "build_id": "b3" })
            ))),
            [Kind::Held]
        );
        s.apply(&candidate(
            "api",
            json!({ "state": "switching", "build_id": "b3" }),
        ));
        let n = s.apply(&candidate("api", Value::Null));
        assert_eq!(kinds(&n), [Kind::Live]);
        assert_eq!(n[0].body, "Build b3 serves now.");
        // A candidate dropped (superseded, reverted) is not a promotion.
        s.apply(&candidate("api", json!({ "state": "building" })));
        assert!(s.apply(&candidate("api", Value::Null)).is_empty());
    }

    #[test]
    fn a_build_failing_at_start_is_a_build_failure() {
        let mut s = Scope::default();
        s.apply(&snapshot(
            json!([{ "name": "api", "status": { "state": "stopped" } }]),
        ));
        s.apply(&service(
            "api",
            json!({ "state": "building", "build_id": null }),
        ));
        let n = s.apply(&service(
            "api",
            json!({ "state": "failed", "reason": "build failed: exit 1" }),
        ));
        assert_eq!(kinds(&n), [Kind::BuildFailed]);
    }

    #[test]
    fn crashes_notify_and_stops_do_not() {
        let mut s = Scope::default();
        s.apply(&snapshot(json!([
            { "name": "api", "status": { "state": "healthy" } },
            { "name": "web", "status": { "state": "starting" } },
            { "name": "worker", "status": { "state": "running" } }
        ])));
        let n = s.apply(&service(
            "api",
            json!({ "state": "exited", "exit_code": 3 }),
        ));
        assert_eq!(kinds(&n), [Kind::ServiceFailed]);
        assert_eq!(n[0].title, "api exited");
        // Given up after its restarts: told (folded by the caller).
        s.apply(&service("api", json!({ "state": "starting" })));
        assert_eq!(
            kinds(&s.apply(&service(
                "api",
                json!({ "state": "failed", "reason": "exited by itself 5 times" })
            ))),
            [Kind::ServiceFailed]
        );
        // A start the user stopped, a dependant held back, a plain stop.
        assert!(
            s.apply(&service(
                "web",
                json!({ "state": "failed", "reason": "stopped while it started (`devstack stop`)" })
            ))
            .is_empty()
        );
        assert!(
            s.apply(&service(
                "web",
                json!({ "state": "failed", "reason": "not started: `api` failed" })
            ))
            .is_empty()
        );
        assert!(
            s.apply(&service("worker", json!({ "state": "stopped" })))
                .is_empty()
        );
        assert!(
            s.apply(&service(
                "worker",
                json!({ "state": "exited", "exit_code": 0 })
            ))
            .is_empty()
        );
    }

    #[test]
    fn lanes_failing_and_taking_the_main_ports() {
        let mut l = Lanes::default();
        let lane = |owns: bool, failed_at: Option<u64>| {
            json!([{ "name": "feat-x", "active": true, "owns_main": owns,
                     "moving": failed_at.map(|t| json!({ "kind": "start", "to": "big-box", "error": "no lease: node full", "failed_at": t })) }])
        };
        l.baseline(&lane(false, Some(1)));
        // The failure it started with is old news; the next is not.
        assert!(l.apply(&lane(false, Some(1))).is_empty());
        let n = l.apply(&lane(false, Some(2)));
        assert_eq!(kinds(&n), [Kind::LaneFailed]);
        assert_eq!(n[0].title, "Lane feat-x: start on big-box failed");
        assert_eq!(n[0].lane.as_deref(), Some("feat-x"));
        assert_eq!(kinds(&l.apply(&lane(true, Some(2)))), [Kind::MainPorts]);
        assert_eq!(
            l.apply(&lane(false, Some(2)))[0].title,
            "Lane feat-x gave the main ports back"
        );
        assert_eq!(Lanes::started(&lane(false, None)), ["feat-x"]);
    }

    #[test]
    fn without_a_baseline_the_first_lanes_event_is_one() {
        let mut l = Lanes::default();
        let v =
            json!([{ "name": "a", "owns_main": true, "moving": { "failed_at": 5, "error": "x" } }]);
        assert!(l.apply(&v).is_empty());
    }

    #[test]
    fn kinds_have_stable_keys() {
        for k in Kind::ALL {
            assert_eq!(Kind::from_key(k.key()), Some(k));
        }
        assert!(Kind::BuildFailed.default_on() && !Kind::Live.default_on());
    }
}
