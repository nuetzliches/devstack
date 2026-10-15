//! Process ownership (ADR-0009, ADR-0016), measured in spike (a).
//!
//! Every service runs as one owned process tree:
//!
//! - **Windows:** a Job Object with kill-on-close (handle never inheritable,
//!   breakaway refused), the service in its own console process group; graceful
//!   stop is `CTRL_BREAK` to that group. The supervisor needs a console of its
//!   own for that ([`ensure_console`]).
//! - **Linux:** a shim between supervisor and service holds the service in its
//!   own process group and, where delegated, its own cgroup v2; it kills both
//!   when the supervisor dies (`PR_SET_PDEATHSIG`). Graceful stop is `SIGTERM`
//!   to the group. What a dead instance left is removed by [`cleanup`] from the
//!   [`Record`]s it wrote.
//!
//! A tree's lifetime ends when the supervisor drops its [`Owned`] handle or
//! dies, whichever comes first.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::PathBuf;
use std::time::{Duration, Instant};

mod builds;
mod detach;
mod lock;
pub use builds::{Build, BuildJob};
pub use detach::spawn_detached;
pub use lock::{LockError, StateLock};

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
pub use unix::shim_main;

#[derive(Debug, thiserror::Error)]
pub enum ProcError {
    #[error("{what}: {source}")]
    Os {
        what: String,
        source: std::io::Error,
    },
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, ProcError>;

#[cfg(windows)]
pub(crate) fn os(what: impl Into<String>) -> ProcError {
    ProcError::Os {
        what: what.into(),
        source: std::io::Error::last_os_error(),
    }
}

/// What a process tree costs right now (plan §12). Callers derive CPU load
/// from two samples of `cpu_us`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct Usage {
    /// CPU time spent so far, user plus kernel, in microseconds. Windows and
    /// Linux with a cgroup count processes that already exited; without a
    /// cgroup it is the sum over the live processes and may drop.
    pub cpu_us: u64,
    /// Bytes: the summed working sets (Windows), `memory.current` of the
    /// cgroup, else the summed resident sets (Linux).
    pub memory: u64,
    pub processes: u32,
}

/// What to start.
#[derive(Debug)]
pub struct Spec {
    /// Name, used for logs and the cgroup.
    pub name: String,
    /// Command line, run by the platform shell (`cmd /d /s /c`, `sh -c`).
    pub command: String,
    pub cwd: PathBuf,
    /// Variables added to (or overriding) the supervisor's environment.
    pub env: BTreeMap<String, String>,
    /// Receives stdout and stderr.
    pub log: File,
}

/// How to own trees on this machine.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Linux: parent directory for per-service cgroups. `None`: process group only.
    pub cgroup_root: Option<PathBuf>,
    /// Linux: program and leading arguments that run [`shim_main`], e.g.
    /// `["/usr/bin/devstack", "shim"]`. Required on Linux.
    pub shim: Vec<String>,
}

/// What a later instance needs to clean up after this one (ADR-0016).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct Record {
    pub name: String,
    /// Process group id (Linux); root process id (Windows, informational).
    pub pgid: i64,
    /// Start time of the group leader in clock ticks since boot (Linux).
    pub leader_start: u64,
    pub cgroup: Option<PathBuf>,
}

/// An owned process tree.
pub struct Owned {
    pub name: String,
    #[cfg(windows)]
    inner: windows::Tree,
    #[cfg(unix)]
    inner: unix::Tree,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopOutcome {
    /// The tree ended within the timeout after the graceful signal.
    pub graceful: bool,
    pub elapsed: Duration,
    /// Error delivering the graceful signal, if any (the tree was then killed).
    pub signal_error: Option<String>,
}

/// Start a service as an owned tree.
pub fn spawn(spec: Spec, opts: &Options) -> Result<(Owned, Record)> {
    let name = spec.name.clone();
    #[cfg(windows)]
    {
        let _ = opts;
        let (inner, record) = windows::spawn(spec)?;
        Ok((Owned { name, inner }, record))
    }
    #[cfg(unix)]
    {
        let (inner, record) = unix::spawn(spec, opts)?;
        Ok((Owned { name, inner }, record))
    }
}

impl Owned {
    /// Process id of the tree's root (the shell, or the shim on Linux).
    pub fn root_pid(&self) -> u32 {
        self.inner.root_pid()
    }

    /// Every live process of the tree (exact membership, not a tree walk).
    pub fn pids(&mut self) -> Vec<u32> {
        self.inner.pids()
    }

    /// CPU time, memory and process count of the whole tree (plan §12).
    pub fn usage(&mut self) -> Usage {
        self.inner.usage()
    }

    /// True while any process of the tree is alive.
    pub fn is_alive(&mut self) -> bool {
        !self.pids().is_empty()
    }

    /// Exit code of the root process once it has exited.
    pub fn try_exit_code(&mut self) -> Option<i32> {
        self.inner.try_exit_code()
    }

    /// Deliver the graceful stop signal to this tree only.
    pub fn request_stop(&mut self) -> Result<()> {
        self.inner.request_stop()
    }

    /// End the tree now.
    pub fn kill(&mut self) {
        self.inner.kill();
    }

    /// Graceful signal, wait up to `timeout`, then kill (ADR-0009).
    pub fn stop(&mut self, timeout: Duration) -> StopOutcome {
        let t0 = Instant::now();
        let signal_error = self.request_stop().err().map(|e| e.to_string());
        if signal_error.is_none() {
            while self.is_alive() && t0.elapsed() < timeout {
                std::thread::sleep(Duration::from_millis(25));
            }
        }
        let graceful = !self.is_alive();
        if !graceful {
            self.kill();
            while self.is_alive() && t0.elapsed() < timeout + Duration::from_secs(5) {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        StopOutcome {
            graceful,
            elapsed: t0.elapsed(),
            signal_error,
        }
    }
}

/// Windows: give a supervisor without a console a hidden one, so services can
/// share it and receive `CTRL_BREAK`. No-op elsewhere and when a console exists.
/// Returns true if a console was allocated.
pub fn ensure_console() -> Result<bool> {
    #[cfg(windows)]
    {
        windows::ensure_console()
    }
    #[cfg(not(windows))]
    {
        Ok(false)
    }
}

/// Windows: the long form of a path (`C:\Users\RUNNER~1` becomes
/// `C:\Users\runneradmin`). Compilers report long paths; a repository known
/// by its 8.3 name would not be recognised in their messages. Unlike
/// `canonicalize`, `subst` drives and junctions stay as they are. Returns the
/// path unchanged elsewhere and when the lookup fails.
pub fn long_path(path: &std::path::Path) -> std::path::PathBuf {
    #[cfg(windows)]
    {
        windows::long_path(path).unwrap_or_else(|| path.to_path_buf())
    }
    #[cfg(not(windows))]
    {
        path.to_path_buf()
    }
}

/// Kill what a previous instance recorded and left behind (ADR-0016). Returns
/// the number of processes signalled per record. Windows needs nothing: its
/// trees die with their owner.
pub fn cleanup(records: &[Record]) -> Vec<(String, usize)> {
    #[cfg(unix)]
    {
        unix::cleanup(records)
    }
    #[cfg(not(unix))]
    {
        records.iter().map(|r| (r.name.clone(), 0)).collect()
    }
}
