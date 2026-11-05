//! The daemon ends with the last tree it runs (ADR-0095): it has no start or
//! stop of its own. The first tree that starts brings it up; once nothing
//! runs in it any more (no tree with a service or container up, no lane on a
//! node, no command), it ends by itself.
//!
//! Only a daemon that ran something ends: one just started for a tree is
//! not ended before that tree's start arrived. A request that may start
//! something holds it while it is answered and for [`GRACE`] after, so a
//! client's look at the daemon and its start that follows are not split by
//! its end. Once it ends, such a request is refused with `503` and
//! `"ending": true`: the client waits for it and starts a new one.

use axum::extract::{Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long a request holds the daemon after it was answered.
pub const GRACE: Duration = Duration::from_secs(5);

/// How often the daemon looks whether anything still runs in it.
pub const TICK: Duration = Duration::from_secs(1);

#[derive(Debug)]
pub struct Idle {
    inner: Mutex<Inner>,
}

#[derive(Debug)]
struct Inner {
    /// Requests being answered that hold the daemon.
    holds: usize,
    /// When the last of them was answered (or the daemon started).
    last: Instant,
    /// Something ran in it since it started.
    armed: bool,
    /// It decided to end.
    ending: bool,
}

impl Default for Idle {
    fn default() -> Self {
        Self {
            inner: Mutex::new(Inner {
                holds: 0,
                last: Instant::now(),
                armed: false,
                ending: false,
            }),
        }
    }
}

/// A request that holds the daemon until it is dropped.
pub struct Hold(Arc<Idle>);

impl Drop for Hold {
    fn drop(&mut self) {
        let mut i = self.0.inner.lock().unwrap();
        i.holds = i.holds.saturating_sub(1);
        i.last = Instant::now();
    }
}

impl Idle {
    /// Hold the daemon while a request is answered; `None` once it ends.
    pub fn hold(self: &Arc<Self>) -> Option<Hold> {
        let mut i = self.inner.lock().unwrap();
        if i.ending {
            return None;
        }
        i.holds += 1;
        Some(Hold(self.clone()))
    }

    pub fn ending(&self) -> bool {
        self.inner.lock().unwrap().ending
    }

    /// One look: `runs` says whether anything runs in the daemon now.
    /// `true` once it is to end: it ran something, nothing runs now, no
    /// request holds it and none did for `grace`. From then on it refuses
    /// what would hold it.
    pub fn settle(&self, runs: bool, grace: Duration) -> bool {
        let mut i = self.inner.lock().unwrap();
        if runs {
            i.armed = true;
            return false;
        }
        if !i.armed || i.ending || i.holds > 0 || i.last.elapsed() < grace {
            return false;
        }
        i.ending = true;
        true
    }
}

/// A request that may start something: every one but a `GET` or `HEAD`, and
/// a client's (the CLI's, an agent's through `devstack mcp`) of any method,
/// since its look at the daemon comes before what it asks of it. Not the
/// dashboard's reads or the tray's: an open page does not keep a daemon
/// that runs nothing.
fn holds(req: &Request) -> bool {
    if !matches!(*req.method(), Method::GET | Method::HEAD) {
        return true;
    }
    matches!(
        req.headers()
            .get("x-devstack-client")
            .and_then(|v| v.to_str().ok()),
        Some("cli" | "mcp")
    )
}

/// What stops something, which a daemon that ends still answers.
fn stops(path: &str) -> bool {
    ["/down", "/stop", "/shutdown", "/cancel"]
        .iter()
        .any(|s| path.ends_with(s))
}

/// The middleware: hold the daemon for a request that may start something,
/// refuse it once the daemon ends.
pub async fn keep(State(idle): State<Arc<Idle>>, req: Request, next: Next) -> Response {
    if !holds(&req) {
        return next.run(req).await;
    }
    let Some(hold) = idle.hold() else {
        if stops(req.uri().path()) {
            return next.run(req).await;
        }
        return ending_response();
    };
    let res = next.run(req).await;
    drop(hold);
    res
}

/// `503` for a request to a daemon that ends: start a new one, then ask again.
pub fn ending_response() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [(axum::http::header::RETRY_AFTER, "1")],
        axum::Json(serde_json::json!({
            "error": "the stack's daemon is ending: nothing runs in it any more (ADR-0095); start again, a new one comes up",
            "ending": true,
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_daemon_that_never_ran_anything_stays() {
        let idle = Idle::default();
        assert!(!idle.settle(false, Duration::ZERO));
        assert!(!idle.ending());
    }

    #[test]
    fn it_ends_once_nothing_runs_after_something_did() {
        let idle = Idle::default();
        assert!(!idle.settle(true, Duration::ZERO));
        assert!(idle.settle(false, Duration::ZERO));
        assert!(idle.ending());
        // Once ending, a start is refused and it decides only once.
        assert!(Arc::new(idle).hold().is_none());
    }

    #[test]
    fn a_request_under_way_or_just_answered_holds_it() {
        let idle = Arc::new(Idle::default());
        idle.settle(true, Duration::ZERO);
        let hold = idle.hold().unwrap();
        assert!(!idle.settle(false, Duration::ZERO));
        drop(hold);
        assert!(!idle.settle(false, Duration::from_secs(60)));
        assert!(idle.settle(false, Duration::ZERO));
    }

    #[test]
    fn a_stop_is_still_answered_while_it_ends() {
        assert!(stops("/v1/down"));
        assert!(stops("/v1/lanes/x/stop"));
        assert!(!stops("/v1/lanes"));
        assert!(!stops("/v1/up"));
    }
}
