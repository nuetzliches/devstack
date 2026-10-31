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
//! - **macOS:** the same shim and process group, without cgroups; the shim
//!   notices the supervisor's death by polling `getppid()`, and process lists,
//!   start times and usage come from libproc (ADR-0054).
//!
//! A tree's lifetime ends when the supervisor drops its [`Owned`] handle or
//! dies, whichever comes first.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::PathBuf;
use std::time::{Duration, Instant};

mod builds;
mod call;
mod detach;
mod lock;
pub use builds::{Build, BuildJob};
pub use call::Call;
pub use detach::{keep_std_handles, spawn_detached};
pub use lock::{LockError, StateLock};

// One module per operating system, each with the same set of functions
// (`sys`): the machine's memory and CPU time, and on Unix what the POSIX
// backend needs from the OS (process lists, start times, per-process usage,
// the shim's parent-death signal, cgroups). The process-ownership backend
// (`backend`) is Windows' job objects or POSIX process groups; both have the
// same set of functions too. An OS without a module does not build.
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(unix)]
mod posix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
use posix as backend;
#[cfg(windows)]
use windows as backend;

#[cfg(target_os = "linux")]
use linux as sys;
#[cfg(target_os = "macos")]
use macos as sys;
#[cfg(windows)]
use windows as sys;

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
compile_error!("devstack-proc supports Windows, Linux and macOS: another OS needs a module");

#[cfg(unix)]
pub use posix::shim_main;

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

/// Is a process with this id running? For processes devstack did not start
/// itself (no handle to wait on); a reused id answers yes.
pub fn pid_alive(pid: u32) -> bool {
    backend::pid_alive(pid)
}

/// Write a minidump of this process to `path`: its threads' stacks and
/// what they point at, for reading a hang afterwards with the build's PDB
/// (#398). Windows only; elsewhere `Unsupported`.
pub fn write_dump(path: &std::path::Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::Diagnostics::Debug::{
            MiniDumpWithIndirectlyReferencedMemory, MiniDumpWithThreadInfo, MiniDumpWriteDump,
        };
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetCurrentProcessId};
        let file = std::fs::File::create(path)?;
        // SAFETY: a pseudo handle to this process and a file handle we own
        // for the duration of the call; no optional parameters.
        let ok = unsafe {
            MiniDumpWriteDump(
                GetCurrentProcess(),
                GetCurrentProcessId(),
                file.as_raw_handle(),
                MiniDumpWithThreadInfo | MiniDumpWithIndirectlyReferencedMemory,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "a minidump is written on Windows only",
        ))
    }
}

/// End the process `pid`, one devstack did not start (a daemon or a hub
/// that does not answer): Windows terminates it, which closes its job
/// objects and with them every process tree it owns (ADR-0009); elsewhere
/// `SIGTERM`, then `SIGKILL` after 5 s.
pub fn end_pid(pid: u32) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_TERMINATE, TerminateProcess,
        };
        // SAFETY: plain Win32 calls on a handle we open and close here.
        unsafe {
            let h = OpenProcess(PROCESS_TERMINATE, 0, pid);
            if h.is_null() {
                return Err(std::io::Error::last_os_error());
            }
            let ok = TerminateProcess(h, 1) != 0;
            let err = std::io::Error::last_os_error();
            CloseHandle(h);
            if ok { Ok(()) } else { Err(err) }
        }
    }
    #[cfg(not(windows))]
    {
        // SAFETY: a signal to one process id.
        if unsafe { libc::kill(pid as i32, libc::SIGTERM) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let t0 = std::time::Instant::now();
        while pid_alive(pid) && t0.elapsed() < std::time::Duration::from_secs(5) {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        if pid_alive(pid) {
            // SAFETY: as above.
            unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        }
        Ok(())
    }
}

/// What a process tree costs right now (plan §12). Callers derive CPU load
/// from two samples of `cpu_us`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct Usage {
    /// CPU time spent so far, user plus kernel, in microseconds. Windows and
    /// Linux with a cgroup count processes that already exited; without a
    /// cgroup (and on macOS) it is the sum over the live processes and may
    /// drop.
    pub cpu_us: u64,
    /// Bytes: the summed working sets (Windows), `memory.current` of the
    /// cgroup, else the summed resident sets (Linux, macOS).
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
    /// Linux: parent directory for per-service cgroups. `None`: process group
    /// only. macOS has no cgroups: a root is refused there.
    pub cgroup_root: Option<PathBuf>,
    /// Unix: program and leading arguments that run [`shim_main`], e.g.
    /// `["/usr/bin/devstack", "shim"]`. Required on Linux and macOS.
    pub shim: Vec<String>,
}

/// What a later instance needs to clean up after this one (ADR-0016).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct Record {
    pub name: String,
    /// Process group id (Unix); root process id (Windows, informational).
    pub pgid: i64,
    /// Start time of the group leader since boot: clock ticks (Linux), Mach
    /// absolute time units (macOS).
    pub leader_start: u64,
    pub cgroup: Option<PathBuf>,
}

/// An owned process tree.
pub struct Owned {
    pub name: String,
    inner: backend::Tree,
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
    let (inner, record) = backend::spawn(spec, opts)?;
    Ok((Owned { name, inner }, record))
}

impl Owned {
    /// Process id of the tree's root (the shell, or the shim on Unix).
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

    /// True while any process of the tree is alive, and when that cannot
    /// be told (a failed query is no ended tree, ADR-0009).
    pub fn is_alive(&mut self) -> bool {
        self.inner.alive() != Some(false)
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
            // Short at first (most trees end at once), then less often.
            let mut pause = Duration::from_millis(10);
            while self.is_alive() && t0.elapsed() < timeout {
                std::thread::sleep(pause.min(timeout.saturating_sub(t0.elapsed())));
                pause = (pause * 2).min(Duration::from_millis(100));
            }
        }
        // Graceful only when the tree is known to be gone: unless the query
        // said so, it is killed.
        let graceful = self.inner.alive() == Some(false);
        if !graceful {
            self.kill();
            while self.inner.alive() == Some(true)
                && t0.elapsed() < timeout + Duration::from_secs(5)
            {
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
    backend::ensure_console()
}

/// Physical memory of the machine, in bytes; `None` where devstack cannot
/// tell.
pub fn memory_total() -> Option<u64> {
    sys::memory_total()
}

/// The machine's CPU time so far, all cores together: `(idle, total)` in
/// the platform's ticks. Two readings give the share that was idle between
/// them; `None` where devstack cannot tell.
pub fn cpu_times() -> Option<(u64, u64)> {
    sys::cpu_times()
}

/// Physical memory available for new work, in bytes; `None` where devstack
/// cannot tell (the lane memory check, ADR-0030, then does not refuse).
pub fn memory_available() -> Option<u64> {
    sys::memory_available()
}

/// Bytes free for this user on the filesystem that holds `path`.
pub fn disk_free(path: &std::path::Path) -> Option<u64> {
    backend::disk_free(path)
}

/// Unix: raise this process's soft limit of open files to its hard limit,
/// and return it. A daemon holds a connection per tunnel, proxy and stream:
/// the soft limit of 1024 a container gets by default ran out in a lane's
/// daemon on a node ("No file descriptors available", os error 24). `None`
/// on Windows (no such limit) and when it cannot be read.
pub fn raise_open_files() -> Option<u64> {
    backend::raise_open_files()
}

/// Windows: does a process listen on TCP `port` on the wildcard address
/// (`0.0.0.0`, `::`) or on loopback? Read from the system's table of
/// listeners. There a bind on `127.0.0.1` succeeds while a process of the
/// same user listens on `0.0.0.0`, and a connect to a closed loopback port
/// takes seconds of retries, so neither tells. `false` elsewhere, where a
/// bind tells (Linux) or a connect (macOS), and when the table cannot be
/// read.
pub fn tcp_listening(port: u16) -> bool {
    backend::tcp_listening(port)
}

/// Windows: the long form of a path (`C:\Users\RUNNER~1` becomes
/// `C:\Users\runneradmin`). Compilers report long paths; a repository known
/// by its 8.3 name would not be recognised in their messages. Unlike
/// `canonicalize`, `subst` drives and junctions stay as they are. Returns the
/// path unchanged elsewhere and when the lookup fails.
pub fn long_path(path: &std::path::Path) -> std::path::PathBuf {
    backend::long_path(path).unwrap_or_else(|| path.to_path_buf())
}

/// Kill what a previous instance recorded and left behind (ADR-0016). Returns
/// the number of processes signalled per record. Windows needs nothing: its
/// trees die with their owner.
pub fn cleanup(records: &[Record]) -> Vec<(String, usize)> {
    backend::cleanup(records)
}
