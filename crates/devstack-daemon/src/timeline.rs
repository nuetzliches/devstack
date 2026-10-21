//! The builds of a service over time, read back from the diagnostic log
//! (plan §6 "build timeline"): every candidate state and every start of the
//! main instance is there with its build id, across daemon restarts.

use serde::Serialize;
use serde_json::Value;
use std::path::Path;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct BuildEntry {
    pub build: String,
    /// When devstack started on it: the build start, or the first time it was seen.
    pub started_ms: u64,
    /// The last time its state changed.
    pub updated_ms: u64,
    /// `live` (it ran as the main instance), `failed`, `superseded` (a newer
    /// change replaced it before it went live), `held` (waits for promote),
    /// `interrupted` (it was on its way and is no longer: the daemon stopped,
    /// or a stop or restart came between), or the state it is in now
    /// (`building`, `starting`, `switching`).
    pub outcome: String,
    pub reason: Option<String>,
    /// It runs as the main instance now.
    pub current: bool,
    /// The store still has it: it can be gone back to (the newest five per
    /// service and what something runs or may go back to, ADR-0002).
    pub artifact: bool,
    /// Its build log is still kept (the newest ten per service).
    pub log: bool,
}

fn at_ms(v: &Value) -> Option<u64> {
    let ts: jiff::Timestamp = v.get("at")?.as_str()?.parse().ok()?;
    u64::try_from(ts.as_millisecond()).ok()
}

/// The builds of `service` in the diagnostic log lines, newest first, at most
/// `limit`. `on_way` is the build the service's candidate has now while it
/// builds, starts or switches: only that one is still on its way.
pub fn builds(
    lines: &[String],
    service: &str,
    lane: &str,
    current: Option<&str>,
    on_way: Option<&str>,
    limit: usize,
) -> Vec<BuildEntry> {
    let mut out: Vec<BuildEntry> = Vec::new();
    // A build starts before its id is known (`building` without one).
    let mut pending_start: Option<u64> = None;
    // The main working tree: `main` in the log since 2026-10-19; before,
    // no lane or an empty one.
    let main = |l: &str| if l.is_empty() { "main" } else { l }.to_string();
    let lane = main(lane);
    for line in lines {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v["service"].as_str() != Some(service) || main(v["lane"].as_str().unwrap_or("")) != lane
        {
            continue;
        }
        let Some(at) = at_ms(&v) else { continue };
        match v["event"].as_str() {
            Some("candidate") => {
                let c = &v["candidate"];
                if c.is_null() {
                    // Done: a switch made it live.
                    if let Some(e) = out.last_mut().filter(|e| e.outcome == "switching") {
                        e.outcome = "live".into();
                        e.updated_ms = at;
                    }
                    continue;
                }
                let state = c["state"].as_str().unwrap_or("");
                let reason = c["reason"].as_str().map(str::to_string);
                match c["build_id"].as_str() {
                    None => {
                        if state == "building" {
                            pending_start.get_or_insert(at);
                        }
                        // A failed build has no id: nothing to show but the reason.
                        if state == "failed" {
                            pending_start = None;
                        }
                    }
                    Some(id) => {
                        let idx = match out.iter().position(|e| e.build == id) {
                            Some(i) => i,
                            None => {
                                // Whatever was on its way and is not done was replaced.
                                for e in out.iter_mut() {
                                    if matches!(e.outcome.as_str(), "building" | "starting") {
                                        e.outcome = "superseded".into();
                                    }
                                }
                                out.push(BuildEntry {
                                    build: id.into(),
                                    started_ms: pending_start.take().unwrap_or(at),
                                    updated_ms: at,
                                    outcome: state.into(),
                                    reason: None,
                                    current: false,
                                    artifact: false,
                                    log: false,
                                });
                                out.len() - 1
                            }
                        };
                        let e = &mut out[idx];
                        e.outcome = state.into();
                        e.updated_ms = at;
                        if reason.is_some() {
                            e.reason = reason;
                        }
                    }
                }
            }
            Some("service") => {
                let state = v["status"]["state"].as_str().unwrap_or("");
                if state == "building" && v["status"]["build_id"].is_null() {
                    pending_start.get_or_insert(at);
                }
                // The main instance runs a build: it is live.
                if let Some(id) = v["build_id"]
                    .as_str()
                    .filter(|_| state == "healthy" || state == "running")
                {
                    match out.iter_mut().find(|e| e.build == id) {
                        Some(e) => {
                            if e.outcome != "live" {
                                e.outcome = "live".into();
                                e.updated_ms = at;
                            }
                        }
                        None => out.push(BuildEntry {
                            build: id.into(),
                            started_ms: pending_start.take().unwrap_or(at),
                            updated_ms: at,
                            outcome: "live".into(),
                            reason: None,
                            current: false,
                            artifact: false,
                            log: false,
                        }),
                    }
                    pending_start = None;
                }
            }
            _ => {}
        }
    }
    for e in out.iter_mut() {
        e.current = current == Some(e.build.as_str());
        // The log says where it got to, not that it got no further.
        if matches!(e.outcome.as_str(), "building" | "starting" | "switching")
            && on_way != Some(e.build.as_str())
        {
            e.outcome = "interrupted".into();
        }
    }
    out.reverse();
    out.truncate(limit);
    out
}

/// Reads the diagnostic log (with its rotated part) for `service`.
pub fn from_log(
    path: &Path,
    service: &str,
    lane: &str,
    current: Option<&str>,
    on_way: Option<&str>,
    limit: usize,
) -> Vec<BuildEntry> {
    let needle = format!("\"service\":\"{service}\"");
    let lines = crate::diag::tail(path, 50_000, Some(&needle));
    builds(&lines, service, lane, current, on_way, limit)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(t: u32, json: &str) -> String {
        format!("{{\"at\":\"2026-10-15T10:00:{t:02}+00:00\",{json}}}")
    }

    #[test]
    fn follows_builds_through_their_states() {
        let c = |t, state: &str, id: Option<&str>, reason: Option<&str>| {
            let id = id.map(|i| format!("\"{i}\"")).unwrap_or("null".into());
            let r = reason.map(|r| format!("\"{r}\"")).unwrap_or("null".into());
            ev(
                t,
                &format!(
                    "\"event\":\"candidate\",\"service\":\"api\",\"candidate\":{{\"state\":\"{state}\",\"build_id\":{id},\"reason\":{r}}}"
                ),
            )
        };
        let lines = vec![
            // up: the main instance's first build
            ev(
                0,
                r#""event":"service","service":"api","build_id":null,"status":{"state":"building","build_id":null}"#,
            ),
            ev(
                3,
                r#""event":"service","service":"api","build_id":"aaa","status":{"state":"healthy"}"#,
            ),
            // an edit goes live
            c(10, "building", None, None),
            c(11, "building", Some("bbb"), None),
            c(13, "starting", Some("bbb"), None),
            c(14, "switching", Some("bbb"), None),
            ev(
                15,
                r#""event":"candidate","service":"api","candidate":null"#,
            ),
            // one is superseded by the next
            c(20, "building", None, None),
            c(21, "building", Some("ccc"), None),
            c(22, "building", None, None),
            c(23, "building", Some("ddd"), None),
            c(25, "failed", Some("ddd"), Some("exited with code 3")),
            // another service is ignored
            ev(
                30,
                r#""event":"service","service":"web","build_id":"zzz","status":{"state":"healthy"}"#,
            ),
        ];
        let b = builds(&lines, "api", "", Some("bbb"), None, 10);
        assert!(
            builds(&lines, "api", "feat", None, None, 10).is_empty(),
            "another lane stays out"
        );
        let got: Vec<(&str, &str, bool)> = b
            .iter()
            .map(|e| (e.build.as_str(), e.outcome.as_str(), e.current))
            .collect();
        assert_eq!(
            got,
            [
                ("ddd", "failed", false),
                ("ccc", "superseded", false),
                ("bbb", "live", true),
                ("aaa", "live", false)
            ]
        );
        let bbb = &b[2];
        assert_eq!(
            bbb.updated_ms - bbb.started_ms,
            5_000,
            "from the build start to the switch"
        );
        assert_eq!(b[0].reason.as_deref(), Some("exited with code 3"));
        assert_eq!(builds(&lines, "api", "", None, None, 2).len(), 2);
    }

    #[test]
    fn the_main_lane_is_read_by_either_name() {
        let lines = vec![
            // Before 2026-10-19: no lane for the main working tree.
            ev(
                1,
                r#""event":"service","service":"api","build_id":"aaa","status":{"state":"healthy"}"#,
            ),
            // Since: `main`.
            ev(
                2,
                r#""event":"service","service":"api","lane":"main","build_id":"bbb","status":{"state":"healthy"}"#,
            ),
            ev(
                3,
                r#""event":"service","service":"api","lane":"feat","build_id":"ccc","status":{"state":"healthy"}"#,
            ),
        ];
        for lane in ["", "main"] {
            let ids: Vec<String> = builds(&lines, "api", lane, None, None, 10)
                .into_iter()
                .map(|e| e.build)
                .collect();
            assert_eq!(ids, ["bbb", "aaa"], "lane {lane:?}");
        }
        assert_eq!(builds(&lines, "api", "feat", None, None, 10).len(), 1);
    }

    #[test]
    fn a_build_no_longer_on_its_way_was_interrupted() {
        let c = |t, state: &str, id: &str| {
            ev(
                t,
                &format!(
                    "\"event\":\"candidate\",\"service\":\"api\",\"candidate\":{{\"state\":\"{state}\",\"build_id\":\"{id}\"}}"
                ),
            )
        };
        // A switch the daemon's stop cut short, then a build under way.
        let lines = vec![
            c(1, "switching", "aaa"),
            ev(
                2,
                r#""event":"service","service":"api","build_id":"zzz","status":{"state":"healthy"}"#,
            ),
            c(3, "building", "bbb"),
        ];
        let outcomes = |on_way| {
            builds(&lines, "api", "", Some("zzz"), on_way, 10)
                .into_iter()
                .map(|e| (e.build, e.outcome))
                .collect::<Vec<_>>()
        };
        let s = |b: &str, o: &str| (b.to_string(), o.to_string());
        assert_eq!(
            outcomes(Some("bbb")),
            [
                s("bbb", "building"),
                s("zzz", "live"),
                s("aaa", "interrupted")
            ]
        );
        assert_eq!(
            outcomes(None),
            [
                s("bbb", "interrupted"),
                s("zzz", "live"),
                s("aaa", "interrupted")
            ],
            "nothing on its way now"
        );
    }
}
