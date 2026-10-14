//! The shared build job (ADR-0020): every build runs in one long-lived job
//! owned by the daemon, so the build servers a build starts (MSBuild nodes,
//! the compiler server) stay warm for the next build and still die with the
//! daemon. A build is finished when its root exits; cancelling it ends the
//! process tree under its root, not the job.

use crate::{ProcError, Result, Spec};
use std::process::Child;

/// One per daemon. Dropping it ends every build and every build server.
pub struct BuildJob {
    #[cfg(windows)]
    job: crate::windows::Job,
    #[cfg(unix)]
    holder: Option<Holder>,
}

#[cfg(unix)]
struct Holder {
    shim: Child,
    cgroup: std::path::PathBuf,
}

/// A running build.
pub struct Build {
    child: Child,
    exit: Option<i32>,
}

impl BuildJob {
    /// `opts.shim` and `opts.cgroup_root` are used on Linux: a shim holds a
    /// cgroup `<cgroup_root>/builds` that every build joins, and kills it when
    /// the daemon dies. Without a cgroup root, builds run in their own process
    /// groups and their servers end by their own idle timeouts after a crash.
    pub fn new(opts: &crate::Options) -> Result<Self> {
        #[cfg(windows)]
        {
            let _ = opts;
            Ok(Self {
                job: crate::windows::Job::new()?,
            })
        }
        #[cfg(unix)]
        {
            let holder = match (&opts.cgroup_root, opts.shim.split_first()) {
                (Some(root), Some((exe, args))) => {
                    let cgroup = root.join("builds");
                    std::fs::create_dir_all(&cgroup).map_err(|e| ProcError::Os {
                        what: format!("create cgroup {}", cgroup.display()),
                        source: e,
                    })?;
                    use std::os::unix::process::CommandExt;
                    let mut cmd = std::process::Command::new(exe);
                    cmd.args(args)
                        .arg("--ppid")
                        .arg(std::process::id().to_string())
                        .arg("--cgroup")
                        .arg(&cgroup)
                        .args(["--", "sh", "-c", "while :; do sleep 3600; done"])
                        .process_group(0)
                        .stdin(std::process::Stdio::null());
                    let shim = crate::unix::spawn_from_keeper(cmd).map_err(|e| ProcError::Os {
                        what: "start build holder".into(),
                        source: e,
                    })?;
                    Some(Holder { shim, cgroup })
                }
                _ => None,
            };
            Ok(Self { holder })
        }
    }

    /// Start a build in the shared job.
    pub fn run(&self, spec: Spec) -> Result<Build> {
        #[cfg(windows)]
        {
            let child = crate::windows::start_in_job(&self.job, spec)?;
            Ok(Build { child, exit: None })
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let err = spec.log.try_clone().map_err(|e| ProcError::Os {
                what: "clone log handle".into(),
                source: e,
            })?;
            let mut cmd = std::process::Command::new("sh");
            cmd.args(["-c", &spec.command])
                .current_dir(&spec.cwd)
                .envs(&spec.env)
                .process_group(0)
                .stdin(std::process::Stdio::null())
                .stdout(spec.log)
                .stderr(err);
            if let Some(h) = &self.holder {
                let procs = std::ffi::CString::new(
                    h.cgroup.join("cgroup.procs").to_string_lossy().into_owned(),
                )
                .map_err(|_| ProcError::Other("cgroup path contains NUL".into()))?;
                // SAFETY: only async-signal-safe calls between fork and exec.
                unsafe {
                    cmd.pre_exec(move || join_cgroup(&procs));
                }
            }
            let child = cmd.spawn().map_err(|e| ProcError::Os {
                what: format!("start build {}", spec.name),
                source: e,
            })?;
            Ok(Build { child, exit: None })
        }
    }

    /// Every process in the build job (builds and their servers).
    pub fn pids(&self) -> Vec<u32> {
        #[cfg(windows)]
        {
            self.job.pids()
        }
        #[cfg(unix)]
        {
            match &self.holder {
                Some(h) => std::fs::read_to_string(h.cgroup.join("cgroup.procs"))
                    .unwrap_or_default()
                    .lines()
                    .filter_map(|l| l.trim().parse().ok())
                    .collect(),
                None => Vec::new(),
            }
        }
    }
}

#[cfg(unix)]
fn join_cgroup(procs: &std::ffi::CStr) -> std::io::Result<()> {
    // Allocation-free: write our pid with raw syscalls.
    let mut buf = [0u8; 16];
    // SAFETY: getpid is async-signal-safe.
    let mut n = unsafe { libc::getpid() } as u32;
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    // SAFETY: valid NUL-terminated path, valid buffer.
    unsafe {
        let fd = libc::open(procs.as_ptr(), libc::O_WRONLY);
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let w = libc::write(fd, buf[i..].as_ptr() as *const _, buf.len() - i);
        libc::close(fd);
        if w < 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

impl Drop for BuildJob {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(h) = &mut self.holder {
            crate::unix::kill_cgroup(&h.cgroup);
            let _ = h.shim.kill();
            let _ = h.shim.wait();
            let _ = std::fs::remove_dir(&h.cgroup);
        }
        // Windows: dropping the job closes its handle, which kills its processes.
    }
}

impl Build {
    pub fn root_pid(&self) -> u32 {
        self.child.id()
    }

    /// Exit code of the build's root process once it has exited.
    pub fn try_exit_code(&mut self) -> Option<i32> {
        if self.exit.is_none()
            && let Ok(Some(status)) = self.child.try_wait()
        {
            self.exit = Some(status.code().unwrap_or(1));
        }
        self.exit
    }

    /// Wait for the build's root to exit.
    pub fn wait(&mut self) -> Result<i32> {
        if let Some(c) = self.exit {
            return Ok(c);
        }
        let status = self.child.wait().map_err(|e| ProcError::Os {
            what: "wait for build".into(),
            source: e,
        })?;
        let code = status.code().unwrap_or(1);
        self.exit = Some(code);
        Ok(code)
    }

    /// End the process tree under the build's root (ADR-0020). Build servers
    /// outside that tree are left alone.
    pub fn cancel(&mut self) {
        #[cfg(windows)]
        crate::windows::kill_tree(self.child.id());
        #[cfg(unix)]
        {
            let _ = crate::unix::kill_group(self.child.id() as i32, libc::SIGKILL);
        }
        let _ = self.child.wait();
        if self.exit.is_none() {
            self.exit = Some(-1);
        }
    }
}
