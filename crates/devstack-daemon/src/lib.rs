//! The devstack daemon (phase 1): one per stack, owning the stack's processes
//! (ADR-0009), restarting cleanly (ADR-0016), controlled through one HTTP API
//! (ADR-0007, ADR-0019).

pub mod ansi;
pub mod api;
pub mod commands;
pub mod diag;
pub mod external;
pub mod health;
pub mod lanes;
pub mod logfiles;
pub mod proxy;
pub mod registry;
pub mod remote;
pub mod state;
pub mod staticsrv;
pub mod supervisor;
pub mod tcpfwd;
pub mod telemetry;
pub mod timeline;
pub mod token;
pub mod usage;
pub mod watch;

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A lane on a node that holds the main slot of its requester (ADR-0025
/// point 4): its workers run there. Set by the requester (`POST
/// /v1/main-slot`), told again with every renewal.
pub static REMOTE_MAIN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether this daemon runs a lane on another machine of the pool, in a
/// node's lane container (`DEVSTACK_REMOTE_LANE`, set by the node agent,
/// ADR-0025): it holds the main slot only when its requester gives it.
pub fn remote_lane() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("DEVSTACK_REMOTE_LANE").is_some_and(|v| !v.is_empty()))
}

/// Where the files of a lane on a node are for the developer
/// (`DEVSTACK_DISPLAY_ROOT`, the worktree the node mirrors): compiler paths
/// in logs point there instead of into the mirror (ADR-0029 point 5).
pub fn display_root() -> Option<&'static str> {
    static ROOT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    ROOT.get_or_init(|| {
        std::env::var("DEVSTACK_DISPLAY_ROOT")
            .ok()
            .filter(|v| !v.trim().is_empty())
    })
    .as_deref()
}

/// Where the API listens: `DEVSTACK_LISTEN` (`address:port`) in a node's lane
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
            "DEVSTACK_LISTEN={v} is for a lane container on a node only (where DEVSTACK_REMOTE_LANE is set); \
             on this machine the API listens on loopback only (ADR-0019): unset it"
        ));
    }
    v.parse()
        .map(Some)
        .map_err(|e| format!("DEVSTACK_LISTEN={v}: {e}"))
}

/// Written to `<state>/daemon.json` while the daemon runs: how clients find it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonInfo {
    pub pid: u32,
    pub port: u16,
    pub started: u128,
    #[serde(default)]
    pub version: String,
    /// It was told to stop and stops its services now; the file goes when
    /// it is done. A daemon starting meanwhile waits for it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stopping: bool,
}

pub fn info_path(state_dir: &Path) -> PathBuf {
    state_dir.join("daemon.json")
}

pub fn read_info(state_dir: &Path) -> Option<DaemonInfo> {
    serde_json::from_slice(&std::fs::read(info_path(state_dir)).ok()?).ok()
}

/// Linux: a per-stack cgroup under the daemon's own one, when it is writable
/// (cgroup delegation); otherwise process groups only (ADR-0009).
fn cgroup_root(stack: &str) -> Option<PathBuf> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let own = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    let path = own
        .lines()
        .find_map(|l| l.strip_prefix("0::"))?
        .trim()
        .trim_start_matches('/')
        .to_string();
    let root = PathBuf::from("/sys/fs/cgroup")
        .join(path)
        .join(format!("devstack-{stack}"));
    std::fs::create_dir_all(&root).ok().map(|_| root)
}

/// How long a stopping daemon waits for open requests before it stops anyway.
const SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// How long a stopping daemon waits for its services to stop. What still
/// runs then dies with the daemon (ADR-0009); a second Ctrl-C ends the wait.
const SHUTDOWN_STOP: std::time::Duration = std::time::Duration::from_secs(90);

/// How long a starting daemon waits for the state lock of an instance that
/// is on its way out.
const STATE_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// The state lock: one daemon per stack (ADR-0016).
///
/// An instance that is stopping (`devstack down` + `up`; its `daemon.json`
/// says `stopping`), or that just died and is still being torn down (a
/// process killed hard), holds the lock a moment longer: wait for it, at
/// most `wait`. But not for one that serves: when `daemon.json` names a live
/// process other than `own_pid` that is not stopping, this one lost a race
/// (two `devstack up` at once) and ends at once, also when that shows only
/// during the wait. Otherwise it would take the lock over should the other
/// stop within the wait, and run although nobody asked for it.
fn acquire_state_lock(
    state_dir: &Path,
    wait: std::time::Duration,
    own_pid: u32,
    alive: impl Fn(u32) -> bool,
) -> Result<devstack_proc::StateLock, String> {
    let t0 = std::time::Instant::now();
    loop {
        match devstack_proc::StateLock::acquire(state_dir) {
            Ok(l) => return Ok(l),
            Err(devstack_proc::LockError::Held(path)) => {
                if let Some(info) = read_info(state_dir)
                    && info.pid != own_pid
                    && !info.stopping
                    && alive(info.pid)
                {
                    return Err(format!(
                        "a daemon for this stack runs already (pid {}, port {})",
                        info.pid, info.port
                    ));
                }
                if t0.elapsed() >= wait {
                    return Err(devstack_proc::LockError::Held(path).to_string());
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// Run the daemon in the foreground until it is told to shut down. Services
/// are stopped on the way out; infrastructure stays up (ADR-0004).
pub fn serve(stack_file: &Path) -> Result<(), String> {
    let abs = std::path::absolute(stack_file).map_err(|e| e.to_string())?;
    let abs_file = devstack_proc::long_path(&abs);
    let repo = abs.parent().map(Path::to_path_buf).unwrap_or(abs);
    // One spelling of the repository everywhere (compiler messages use the long one).
    let repo = devstack_proc::long_path(&repo);
    let state_dir = repo.join(".devstack");
    let _lock = acquire_state_lock(
        &state_dir,
        STATE_LOCK_WAIT,
        std::process::id(),
        devstack_proc::pid_alive,
    )?;
    // Only the lock holder writes the diagnostic log (plan §11).
    diag::init(state_dir.join("daemon.log"));
    // Read after the log is open: a stack file this devstack cannot run (one
    // that needs a newer devstack) says so in `devstack logs --daemon` (#11).
    let stack = match devstack_config::load(stack_file) {
        Ok(s) => s,
        Err(e) => {
            diag::warn(
                "daemon_start_failed",
                serde_json::json!({ "error": e.to_string() }),
            );
            return Err(e.to_string());
        }
    };
    // A lane on a node runs on the ports it has on its developer's machine
    // (`DEVSTACK_PORT_OFFSET`): links, `{service.x.port}` and the forwarded
    // ports there are the same (ADR-0029 point 4).
    let stack = match std::env::var("DEVSTACK_PORT_OFFSET")
        .ok()
        .and_then(|v| v.trim().parse::<u16>().ok())
        .filter(|o| *o > 0)
    {
        Some(offset) => lanes::lane_stack(&stack, offset)?,
        None => stack,
    };
    devstack_proc::ensure_console().map_err(|e| e.to_string())?;
    let token = token::load_or_create().map_err(|e| format!("token: {e}"))?;
    // Known to the hub from now on (ADR-0021); a failure costs only the listing.
    let _ = registry::register(&registry::Entry {
        id: registry::id_for(&abs_file),
        name: stack.stack.name.clone(),
        file: abs_file.clone(),
        repo: repo.clone(),
    });
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let proc_opts = devstack_proc::Options {
        cgroup_root: cgroup_root(&stack.stack.name),
        shim: vec![exe.to_string_lossy().into_owned(), "shim".into()],
    };
    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    let sup = supervisor::Supervisor::new(
        stack,
        abs_file.clone(),
        repo,
        state_dir.clone(),
        proc_opts.clone(),
        rt.handle().clone(),
    )?;
    sup.refresh_infra();
    let usage = usage::Sampler::start(sup.clone());
    // Kept for the daemon's lifetime; changes become candidates (ADR-0003).
    let _watcher = watch::start(sup.clone());

    let served = rt.block_on(async move {
        let listener = match listen_addr()? {
            Some(addr) => tokio::net::TcpListener::bind(addr).await,
            None => tokio::net::TcpListener::bind(("127.0.0.1", 0)).await,
        }
        .map_err(|e| e.to_string())?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let info = DaemonInfo {
            pid: std::process::id(),
            port,
            started: state::now_ms(),
            version: devstack_config::VERSION.into(),
            stopping: false,
        };
        std::fs::write(
            info_path(&state_dir),
            serde_json::to_vec_pretty(&info).unwrap(),
        )
        .map_err(|e| e.to_string())?;
        diag::info(
            "daemon_start",
            serde_json::json!({
                "version": devstack_config::VERSION,
                "pid": info.pid,
                "port": port,
                "stack": sup.stack.stack.name,
            }),
        );
        let shutdown = Arc::new(tokio::sync::Notify::new());
        let token = Arc::new(token);
        let lanes = lanes::Lanes::load(
            state_dir.clone(),
            sup.clone(),
            proc_opts.clone(),
            tokio::runtime::Handle::current(),
            token.clone(),
            port,
        );
        // What the pool's collector gets every minute, if this machine
        // agreed (ADR-0037): lanes and services running.
        {
            let (sup, lanes) = (sup.clone(), lanes.clone());
            telemetry::set_gauges(move || {
                use devstack_node::telemetry::Gauge;
                let v = sup.view();
                let running = v.services.iter().filter(|s| s.status.is_active()).count();
                let failed = v
                    .services
                    .iter()
                    .filter(|s| matches!(s.status, state::Status::Failed { .. }))
                    .count();
                let ls = lanes.views();
                let started = |remote: bool| {
                    ls.iter()
                        .filter(|l| l.active && l.node.is_some() == remote)
                        .count() as f64
                };
                let stack = vec![("devstack.repository".to_string(), v.name.clone())];
                let g = |name: &str, value: f64, extra: Option<(&str, &str)>| {
                    let mut labels = stack.clone();
                    if let Some((k, x)) = extra {
                        labels.push((k.into(), x.into()));
                    }
                    Gauge {
                        name: name.into(),
                        unit: "1".into(),
                        value,
                        labels,
                    }
                };
                vec![
                    g("devstack.services", running as f64, Some(("devstack.state", "running"))),
                    g("devstack.services", failed as f64, Some(("devstack.state", "failed"))),
                    g("devstack.lanes", started(false), Some(("devstack.place", "local"))),
                    g("devstack.lanes", started(true), Some(("devstack.place", "remote"))),
                ]
            });
        }
        let app = api::router(api::AppState {
            sup: sup.clone(),
            runs: Arc::new(commands::Runs::default()),
            usage,
            token,
            port,
            shutdown: shutdown.clone(),
            lanes: Some(lanes.clone()),
        });
        let (stopping_tx, stopping_rx) = tokio::sync::oneshot::channel::<()>();
        let stopping_info = (info_path(&state_dir), info.clone());
        let stop = async move {
            let why = tokio::select! {
                _ = shutdown.notified() => "shutdown requested",
                _ = tokio::signal::ctrl_c() => "ctrl-c",
            };
            diag::info("daemon_stopping", serde_json::json!({ "reason": why }));
            // A daemon started now (`down` + `up`) waits for this one instead
            // of taking it for one that serves (`acquire_state_lock`).
            let (path, mut info) = stopping_info;
            info.stopping = true;
            let _ = std::fs::write(&path, serde_json::to_vec_pretty(&info).unwrap_or_default());
            let _ = stopping_tx.send(());
        };
        // Small writes go out at once: a tunnel to a lane's port is this API
        // too (ADR-0029 point 4), and its turns would wait for delayed ACKs.
        use axum::serve::ListenerExt;
        let listener = listener.tap_io(|tcp| {
            let _ = tcp.set_nodelay(true);
        });
        let serve = axum::serve(listener, app).with_graceful_shutdown(stop);
        // A graceful shutdown waits for every open request, and event and log
        // streams (dashboard, `logs --follow`) never end on their own: give them
        // a moment, then stop anyway. Dropping the runtime closes them.
        let served = tokio::select! {
            r = serve => r.map_err(|e| e.to_string()),
            _ = async {
                let _ = stopping_rx.await;
                tokio::time::sleep(SHUTDOWN_GRACE).await;
            } => {
                diag::info("daemon_streams_cut", serde_json::json!({ "after_s": SHUTDOWN_GRACE.as_secs() }));
                Ok(())
            }
        };
        let s = sup.clone();
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

    fn write_info(dir: &std::path::Path, pid: u32) {
        write_info_as(dir, pid, false);
    }

    fn write_info_as(dir: &std::path::Path, pid: u32, stopping: bool) {
        let info = super::DaemonInfo {
            pid,
            port: 7000,
            started: 0,
            version: String::new(),
            stopping,
        };
        std::fs::write(super::info_path(dir), serde_json::to_vec(&info).unwrap()).unwrap();
    }

    #[test]
    fn a_daemon_that_lost_the_race_does_not_wait_to_take_over() {
        use std::time::{Duration, Instant};
        let d = tempfile::tempdir().unwrap();
        let wait = Duration::from_secs(3);
        let first = devstack_proc::StateLock::acquire(d.path()).unwrap();
        // The other one serves: give up at once, even though it may stop
        // within the wait.
        write_info(d.path(), 4242);
        let t0 = Instant::now();
        let e = super::acquire_state_lock(d.path(), wait, 1, |p| p == 4242).unwrap_err();
        assert!(e.contains("runs already"), "{e}");
        assert!(t0.elapsed() < wait);
        // A `daemon.json` of a dead process says nothing: wait, then give up.
        let t0 = Instant::now();
        let e = super::acquire_state_lock(d.path(), Duration::from_millis(300), 1, |_| false)
            .unwrap_err();
        assert!(!e.contains("runs already"), "{e}");
        assert!(t0.elapsed() >= Duration::from_millis(300));
        // One on its way out (`daemon.json` says so, then goes): wait for it.
        write_info_as(d.path(), 4242, true);
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            drop(first);
        });
        super::acquire_state_lock(d.path(), wait, 1, |_| true).unwrap();
        t.join().unwrap();
    }

    #[test]
    fn a_daemon_that_shows_up_during_the_wait_ends_it() {
        use std::time::{Duration, Instant};
        let d = tempfile::tempdir().unwrap();
        let wait = Duration::from_secs(5);
        let first = devstack_proc::StateLock::acquire(d.path()).unwrap();
        // The holder is still starting; it serves after a moment.
        let dir = d.path().to_path_buf();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            write_info(&dir, 4242);
        });
        let t0 = Instant::now();
        let e = super::acquire_state_lock(d.path(), wait, 1, |p| p == 4242).unwrap_err();
        assert!(e.contains("runs already"), "{e}");
        assert!(t0.elapsed() < wait);
        t.join().unwrap();
        drop(first);
    }
}
