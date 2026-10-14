//! The devstack daemon (phase 1): one per stack, owning the stack's processes
//! (ADR-0009), restarting cleanly (ADR-0016), controlled through one HTTP API
//! (ADR-0007, ADR-0019).

pub mod api;
pub mod commands;
pub mod health;
pub mod proxy;
pub mod state;
pub mod supervisor;
pub mod token;
pub mod usage;
pub mod watch;

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Written to `<state>/daemon.json` while the daemon runs: how clients find it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonInfo {
    pub pid: u32,
    pub port: u16,
    pub started: u128,
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

/// Run the daemon in the foreground until it is told to shut down. Services
/// are stopped on the way out; infrastructure stays up (ADR-0004).
pub fn serve(stack_file: &Path) -> Result<(), String> {
    let stack = devstack_config::load(stack_file).map_err(|e| e.to_string())?;
    let abs = std::path::absolute(stack_file).map_err(|e| e.to_string())?;
    let repo = abs.parent().map(Path::to_path_buf).unwrap_or(abs);
    // One spelling of the repository everywhere (compiler messages use the long one).
    let repo = devstack_proc::long_path(&repo);
    let state_dir = repo.join(".devstack");
    // A previous instance that just died may still be closing its handles (a
    // process killed hard is torn down asynchronously): wait a moment for it.
    let t0 = std::time::Instant::now();
    let _lock = loop {
        match devstack_proc::StateLock::acquire(&state_dir) {
            Ok(l) => break l,
            Err(devstack_proc::LockError::Held(_))
                if t0.elapsed() < std::time::Duration::from_secs(5) =>
            {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(e) => return Err(e.to_string()),
        }
    };
    devstack_proc::ensure_console().map_err(|e| e.to_string())?;
    let token = token::load_or_create().map_err(|e| format!("token: {e}"))?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let proc_opts = devstack_proc::Options {
        cgroup_root: cgroup_root(&stack.stack.name),
        shim: vec![exe.to_string_lossy().into_owned(), "shim".into()],
    };
    let rt = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    let sup = supervisor::Supervisor::new(
        stack,
        repo,
        state_dir.clone(),
        proc_opts,
        rt.handle().clone(),
    )?;
    sup.refresh_infra();
    let usage = usage::Sampler::start(sup.clone());
    // Kept for the daemon's lifetime; changes become candidates (ADR-0003).
    let _watcher = watch::start(sup.clone());

    rt.block_on(async move {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(|e| e.to_string())?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let info = DaemonInfo {
            pid: std::process::id(),
            port,
            started: state::now_ms(),
        };
        std::fs::write(
            info_path(&state_dir),
            serde_json::to_vec_pretty(&info).unwrap(),
        )
        .map_err(|e| e.to_string())?;
        let shutdown = Arc::new(tokio::sync::Notify::new());
        let app = api::router(api::AppState {
            sup: sup.clone(),
            runs: Arc::new(commands::Runs::default()),
            usage,
            token: Arc::new(token),
            port,
            shutdown: shutdown.clone(),
        });
        let stop = async move {
            tokio::select! {
                _ = shutdown.notified() => {}
                _ = tokio::signal::ctrl_c() => {}
            }
        };
        let served = axum::serve(listener, app)
            .with_graceful_shutdown(stop)
            .await
            .map_err(|e| e.to_string());
        let s = sup.clone();
        let _ = tokio::task::spawn_blocking(move || s.down(false)).await;
        let _ = std::fs::remove_file(info_path(&state_dir));
        served
    })
}
