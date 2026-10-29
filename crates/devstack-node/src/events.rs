//! The pool's event stream (ADR-0073): the coordinator tells its members
//! which parts of the pool changed, never what they hold. Each part (a
//! *topic*) has a version, a hash of what the member reads for it; an event
//! names the topics whose version changed, the first one after connecting
//! all of them. A member reads a part again over its own endpoint when its
//! version is not the one it read last, so connecting again is the same as
//! connecting first.
//!
//! `GET /v1/events` (members): `text/event-stream`, one event
//! `{"topics": {"<topic>": "<version>", ...}}` per change (changes of a
//! topic within [`COALESCE`] in one), and a comment every [`PING_EVERY`].

use crate::member::Local;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

/// The members and their states, roles, tags and grants (`GET /v1/members`).
pub const MEMBERS: &str = "members";
/// The nodes, their reports and whether they are online (`GET /v1/nodes`).
pub const NODES: &str = "nodes";
/// The leases and whom their lanes are shared with (`GET /v1/leases`,
/// `GET /v1/shared`).
pub const LEASES: &str = "leases";
/// The MCP upstreams the pool names (`GET /v1/upstreams`, ADR-0069).
pub const UPSTREAMS: &str = "upstreams";
/// Whether the pool collects telemetry, and what (`GET /v1/telemetry`).
pub const TELEMETRY: &str = "telemetry";

/// The topics, in the order the stream names them.
pub const TOPICS: [&str; 5] = [MEMBERS, NODES, LEASES, UPSTREAMS, TELEMETRY];

/// A version per topic.
pub type Versions = BTreeMap<String, String>;

/// How often the coordinator sends a comment on a quiet stream.
pub const PING_EVERY: Duration = Duration::from_secs(15);
/// How long a stream may stay silent before its member takes it for dead
/// (a machine that slept, a network that changed): three pings missed.
pub const SILENT_FOR: Duration = Duration::from_secs(45);
/// Changes within this time go out as one event: a pool of many nodes
/// reports several times a second, and a member reads a part once.
pub const COALESCE: Duration = Duration::from_secs(2);

/// One event of the stream.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub topics: Versions,
}

/// The version of `value`: a short hash of its JSON.
pub fn version(value: &impl Serialize) -> String {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    blake3::hash(&bytes).to_hex()[..16].to_string()
}

/// What a member hears while it follows a pool.
#[derive(Debug, Clone, PartialEq)]
pub enum Heard {
    /// Connected: what follows is live.
    Connected,
    /// These topics have a version other than the one heard before (all of
    /// them after each connect).
    Changed(Versions),
    /// The stream broke; it is tried again after the wait.
    Lost(String),
}

/// Why following a pool ended.
#[derive(Debug, Clone, PartialEq)]
pub enum Ended {
    /// The coordinator has no stream (a release before it): `404`.
    Unsupported,
    /// The coordinator does not take this member (any more): `401`/`403`.
    NotMember(String),
    /// This machine is no approved member of the pool.
    NoMembership(String),
}

/// Follow the pool of the membership in `local` until it ends: connect,
/// pass on what is heard, and connect again after a loss (backing off up to
/// a minute, with jitter so that a restarted coordinator is not asked by
/// everyone at once). Versions are compared across connections: `on` hears
/// `Changed` only for topics whose version is new to it.
pub async fn follow(local: Local, mut on: impl FnMut(Heard) + Send) -> Ended {
    let mut known = Versions::new();
    let mut wait = Duration::from_secs(1);
    loop {
        let conn = match connect(&local).await {
            Ok(c) => c,
            Err(Fail::End(e)) => return e,
            Err(Fail::Retry(e)) => {
                on(Heard::Lost(e));
                tokio::time::sleep(jitter(wait)).await;
                wait = (wait * 2).min(Duration::from_secs(60));
                continue;
            }
        };
        on(Heard::Connected);
        let lost = read(conn, |ev| {
            let changed: Versions = ev
                .topics
                .into_iter()
                .filter(|(t, v)| known.get(t) != Some(v))
                .collect();
            if !changed.is_empty() {
                known.extend(changed.clone());
                on(Heard::Changed(changed));
            }
        })
        .await;
        // It ran: the next connect is tried soon again.
        wait = Duration::from_secs(1);
        on(Heard::Lost(lost));
        tokio::time::sleep(jitter(wait)).await;
    }
}

fn jitter(d: Duration) -> Duration {
    d + Duration::from_millis(rand::random::<u64>() % 1000)
}

enum Fail {
    End(Ended),
    Retry(String),
}

/// Open the stream: the response, once the coordinator answered `200`.
async fn connect(local: &Local) -> Result<hyper::body::Incoming, Fail> {
    use crate::member::MemberError;
    let m = match local.membership() {
        Ok(Some(m)) => m,
        Ok(None) => return Err(Fail::End(Ended::NoMembership("in no pool".into()))),
        Err(e) => return Err(Fail::Retry(e.to_string())),
    };
    let (ca, cert, key) = match local.pems() {
        Ok(p) => p,
        Err(e @ MemberError::Other(_)) => {
            return Err(Fail::End(Ended::NoMembership(e.to_string())));
        }
        Err(e) => return Err(Fail::Retry(e.to_string())),
    };
    let config =
        crate::tls::client_config(&ca, &cert, &key).map_err(|e| Fail::Retry(e.to_string()))?;
    let req = hyper::Request::get("/v1/events")
        .header(hyper::header::ACCEPT, "text/event-stream")
        .body(http_body_util::Empty::<bytes::Bytes>::new())
        .map_err(|e| Fail::Retry(e.to_string()))?;
    let res = crate::tls::send_to(Arc::new(config), &m.coordinator, req)
        .await
        .map_err(|e| Fail::Retry(e.to_string()))?;
    let status = res.status().as_u16();
    match status {
        200 => Ok(res.into_body()),
        404 => Err(Fail::End(Ended::Unsupported)),
        401 | 403 => {
            use http_body_util::BodyExt;
            let body = res
                .into_body()
                .collect()
                .await
                .map(|b| String::from_utf8_lossy(&b.to_bytes()).trim().to_string())
                .unwrap_or_default();
            Err(Fail::End(Ended::NotMember(body)))
        }
        s => Err(Fail::Retry(format!("the coordinator answered {s}"))),
    }
}

/// Read events from `body` until it ends, breaks or stays silent for
/// [`SILENT_FOR`]; why it stopped.
async fn read(mut body: hyper::body::Incoming, mut each: impl FnMut(Event)) -> String {
    use http_body_util::BodyExt;
    let mut buf = Vec::<u8>::new();
    loop {
        let frame = match tokio::time::timeout(SILENT_FOR, body.frame()).await {
            Err(_) => {
                return format!(
                    "no word from the coordinator for {} s",
                    SILENT_FOR.as_secs()
                );
            }
            Ok(None) => return "the coordinator closed the stream".into(),
            Ok(Some(Err(e))) => return e.to_string(),
            Ok(Some(Ok(f))) => f,
        };
        let Ok(data) = frame.into_data() else {
            continue;
        };
        buf.extend_from_slice(&data);
        // An event ends with an empty line.
        while let Some(end) = find_blank(&buf) {
            let block: Vec<u8> = buf.drain(..end).collect();
            if let Some(ev) = parse(&String::from_utf8_lossy(&block)) {
                each(ev);
            }
        }
        if buf.len() > 1 << 20 {
            return "an event over 1 MiB".into();
        }
    }
}

/// Where the first event in `buf` ends, past its blank line.
fn find_blank(buf: &[u8]) -> Option<usize> {
    let lf = buf.windows(2).position(|w| w == b"\n\n").map(|i| i + 2);
    let crlf = buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4);
    match (lf, crlf) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// An event block's `data:` lines, as an [`Event`]; a comment or anything
/// else is `None`.
pub fn parse(block: &str) -> Option<Event> {
    let data: Vec<&str> = block
        .lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .map(|l| l.strip_prefix(' ').unwrap_or(l))
        .collect();
    if data.is_empty() {
        return None;
    }
    serde_json::from_str(&data.join("\n")).ok()
}

/// An event as the stream sends it.
pub fn frame(ev: &Event) -> bytes::Bytes {
    bytes::Bytes::from(format!(
        "data: {}\n\n",
        serde_json::to_string(ev).expect("an event serialises")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_event_reads_back_and_a_comment_is_none() {
        let ev = Event {
            topics: [("nodes".to_string(), "abc".to_string())].into(),
        };
        let text = String::from_utf8(frame(&ev).to_vec()).unwrap();
        assert_eq!(parse(&text), Some(ev));
        assert_eq!(parse(": ping\n\n"), None);
    }

    #[test]
    fn events_split_on_blank_lines_of_either_kind() {
        assert_eq!(find_blank(b"data: {}\n\nrest"), Some(10));
        assert_eq!(find_blank(b"data: {}\r\n\r\nrest"), Some(12));
        assert_eq!(find_blank(b"data: {}\n"), None);
    }

    #[test]
    fn a_version_follows_the_content() {
        assert_eq!(version(&vec![1, 2]), version(&vec![1, 2]));
        assert_ne!(version(&vec![1, 2]), version(&vec![2, 1]));
        assert_eq!(version(&()).len(), 16);
    }
}
