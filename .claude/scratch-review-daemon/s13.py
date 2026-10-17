import sys; sys.path.insert(0,'D:/Sources/devstack/.claude/scratch-review-daemon')
from rep import apply
W='D:/Sources/devstack/.claude/worktrees/review-daemon/crates/devstack-daemon/src/'
apply(W+'lib.rs',[
("""/// Whether this daemon runs a lane on another machine of the pool, in a
/// node's lane container (`DEVSTACK_REMOTE_LANE`, set by the node agent,
/// ADR-0025): it never holds the main slot, so it runs no workers.
/// A lane on a node that holds the main slot of its requester (ADR-0025
/// point 4): its workers run there. Set by the requester (`POST
/// /v1/main-slot`), told again with every renewal.
pub static REMOTE_MAIN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn remote_lane() -> bool {""",
"""/// A lane on a node that holds the main slot of its requester (ADR-0025
/// point 4): its workers run there. Set by the requester (`POST
/// /v1/main-slot`), told again with every renewal.
pub static REMOTE_MAIN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether this daemon runs a lane on another machine of the pool, in a
/// node's lane container (`DEVSTACK_REMOTE_LANE`, set by the node agent,
/// ADR-0025): it holds the main slot only when its requester gives it.
pub fn remote_lane() -> bool {"""),
("""/// Where the API listens: `DEVSTACK_LISTEN` (`address:port`) in a node's lane
/// container, where the node agent proxies to it from the lane's network;
/// else a free loopback port. The API still requires its token and a
/// loopback `Host` (ADR-0019); the agent sends both.
fn listen_addr() -> Result<Option<std::net::SocketAddr>, String> {
    match std::env::var("DEVSTACK_LISTEN") {
        Ok(v) if !v.trim().is_empty() => v
            .trim()
            .parse()
            .map(Some)
            .map_err(|e| format!("DEVSTACK_LISTEN={v}: {e}")),
        _ => Ok(None),
    }
}""",
"""/// Where the API listens: `DEVSTACK_LISTEN` (`address:port`) in a node's lane
/// container, where the node agent proxies to it from the lane's network;
/// else a free loopback port. The API still requires its token and a
/// loopback `Host` (ADR-0019); the agent sends both. Anywhere but in a lane
/// container (`DEVSTACK_REMOTE_LANE`) the variable is refused: on a
/// developer's machine the API is on loopback only.
fn listen_addr() -> Result<Option<std::net::SocketAddr>, String> {
    listen_addr_from(
        std::env::var("DEVSTACK_LISTEN").ok().as_deref(),
        remote_lane(),
    )
}

fn listen_addr_from(
    value: Option<&str>,
    in_lane_container: bool,
) -> Result<Option<std::net::SocketAddr>, String> {
    let Some(v) = value.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    if !in_lane_container {
        return Err(format!(
            "DEVSTACK_LISTEN={v} is for a lane container on a node only (where DEVSTACK_REMOTE_LANE is set); \\
             on this machine the API listens on loopback only (ADR-0019): unset it"
        ));
    }
    v.parse()
        .map(Some)
        .map_err(|e| format!("DEVSTACK_LISTEN={v}: {e}"))
}"""),
("""/// Run the daemon in the foreground until it is told to shut down. Services
/// are stopped on the way out; infrastructure stays up (ADR-0004).
/// How long a stopping daemon waits for open requests before it stops anyway.
const SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

pub fn serve(stack_file: &Path) -> Result<(), String> {""",
"""/// How long a stopping daemon waits for open requests before it stops anyway.
const SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// How long a stopping daemon waits for its services to stop. What still
/// runs then dies with the daemon (ADR-0009); a second Ctrl-C ends the wait.
const SHUTDOWN_STOP: std::time::Duration = std::time::Duration::from_secs(90);

/// Run the daemon in the foreground until it is told to shut down. Services
/// are stopped on the way out; infrastructure stays up (ADR-0004).
pub fn serve(stack_file: &Path) -> Result<(), String> {"""),
("""    rt.block_on(async move {
        let listener = match listen_addr()? {""",
"""    let served = rt.block_on(async move {
        let listener = match listen_addr()? {"""),
("""        let s = sup.clone();
        let _ = tokio::task::spawn_blocking(move || {
            lanes.down_all();
            s.down(false)
        })
        .await;
        let _ = std::fs::remove_file(info_path(&state_dir));
        diag::info("daemon_stop", serde_json::json!({}));
        served
    })
}""",
"""        let s = sup.clone();
        let down = tokio::task::spawn_blocking(move || {
            lanes.down_all();
            s.down(false)
        });
        tokio::select! {
            r = tokio::time::timeout(SHUTDOWN_STOP, down) => {
                if r.is_err() {
                    diag::warn(
                        "daemon_stop_timeout",
                        serde_json::json!({ "after_s": SHUTDOWN_STOP.as_secs(), "why": "services did not stop in time; they end with the daemon" }),
                    );
                }
            }
            _ = tokio::signal::ctrl_c() => {
                diag::warn("daemon_stop_forced", serde_json::json!({ "reason": "ctrl-c again" }));
            }
        }
        let _ = std::fs::remove_file(info_path(&state_dir));
        diag::info("daemon_stop", serde_json::json!({}));
        served
    });
    // A stop still under way must not keep the process (dropping the
    // runtime would wait for it).
    rt.shutdown_timeout(std::time::Duration::from_secs(1));
    served
}

#[cfg(test)]
mod tests {
    #[test]
    fn devstack_listen_is_for_a_lane_container_only() {
        assert_eq!(super::listen_addr_from(None, false), Ok(None));
        assert_eq!(super::listen_addr_from(Some(" "), false), Ok(None));
        let refused = super::listen_addr_from(Some("0.0.0.0:7600"), false).unwrap_err();
        assert!(refused.contains("DEVSTACK_REMOTE_LANE"), "{refused}");
        assert_eq!(
            super::listen_addr_from(Some("0.0.0.0:7600"), true),
            Ok(Some("0.0.0.0:7600".parse().unwrap()))
        );
        assert!(super::listen_addr_from(Some("nope"), true).is_err());
    }
}"""),
])
print("ok")
