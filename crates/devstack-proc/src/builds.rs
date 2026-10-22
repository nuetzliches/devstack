//! The shared build job (ADR-0020): every build runs in one long-lived job
//! owned by the daemon, so the build servers a build starts (MSBuild nodes,
//! the compiler server) stay warm for the next build and still die with the
//! daemon. A build is finished when its root exits; cancelling it ends the
//! process tree under its root, not the job.

use crate::{ProcError, Result, Spec};

/// One per daemon. Dropping it ends every build and every build server.
pub struct BuildJob {
    inner: imp::Job,
}

/// A running build.
pub struct Build {
    inner: imp::Root,
    exit: Option<i32>,
}

impl BuildJob {
    /// `opts.shim` and `opts.cgroup_root` are used on Linux: a shim holds a
    /// cgroup `<cgroup_root>/builds` that every build joins, and kills it when
    /// the daemon dies. Without a cgroup root (and on macOS), each build runs
    /// in a process group of its own, which its servers stay in: the job
    /// knows the groups it started, measures them and kills them when it is
    /// dropped. After a crash those servers end by their own idle timeouts.
    pub fn new(opts: &crate::Options) -> Result<Self> {
        Ok(Self {
            inner: imp::Job::new(opts)?,
        })
    }

    /// Start a build in the shared job.
    pub fn run(&self, spec: Spec) -> Result<Build> {
        Ok(Build {
            inner: self.inner.run(spec)?,
            exit: None,
        })
    }

    /// What the build job costs: builds and their servers together (plan §12,
    /// ADR-0020). Without a cgroup (Unix) only what still runs is counted.
    pub fn usage(&self) -> crate::Usage {
        self.inner.usage()
    }

    /// Every process in the build job (builds and their servers).
    pub fn pids(&self) -> Vec<u32> {
        self.inner.pids()
    }
}

impl Build {
    pub fn root_pid(&self) -> u32 {
        self.inner.child.id()
    }

    /// Exit code of the build's root process once it has exited.
    pub fn try_exit_code(&mut self) -> Option<i32> {
        if self.exit.is_none()
            && let Ok(Some(status)) = self.inner.child.try_wait()
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
        let status = self.inner.child.wait().map_err(|e| ProcError::Os {
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
        self.inner.kill_tree();
        let _ = self.inner.child.wait();
        if self.exit.is_none() {
            self.exit = Some(-1);
        }
    }
}

#[cfg(windows)]
mod imp {
    use crate::{Result, Spec, Usage};
    use std::process::Child;

    /// A Job Object; closing it (dropping this) kills its processes.
    pub(super) struct Job(crate::windows::Job);

    pub(super) struct Root {
        pub(super) child: Child,
        /// When the root was created, the bound of its tree for a cancel.
        created: Option<u64>,
    }

    impl Job {
        pub(super) fn new(_opts: &crate::Options) -> Result<Self> {
            Ok(Self(crate::windows::Job::new()?))
        }

        pub(super) fn run(&self, spec: Spec) -> Result<Root> {
            let child = crate::windows::start_in_job(&self.0, spec)?;
            let created = crate::windows::creation_time(child.id());
            Ok(Root { child, created })
        }

        pub(super) fn usage(&self) -> Usage {
            self.0.usage()
        }

        pub(super) fn pids(&self) -> Vec<u32> {
            self.0.pids()
        }
    }

    impl Root {
        pub(super) fn kill_tree(&mut self) {
            crate::windows::kill_tree(self.child.id(), self.created);
        }
    }
}

#[cfg(unix)]
mod imp {
    use crate::posix::{group_members, kill_group, spawn_from_keeper, summed_usage, usage_of};
    use crate::sys::{self, Cgroup};
    use crate::{ProcError, Result, Spec, Usage};
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command, Stdio};
    use std::sync::Mutex;

    pub(super) struct Job {
        holder: Option<Holder>,
        /// Without a holder: the process group of every build started, with
        /// the start time of its leader, until nothing is left in it.
        groups: Mutex<Vec<(i32, u64)>>,
    }

    /// A shim that holds the build cgroup and kills it when the daemon dies.
    struct Holder {
        shim: Child,
        cgroup: Cgroup,
    }

    pub(super) struct Root {
        pub(super) child: Child,
    }

    impl Job {
        pub(super) fn new(opts: &crate::Options) -> Result<Self> {
            let holder = match (&opts.cgroup_root, opts.shim.split_first()) {
                (Some(root), Some((exe, args))) => {
                    let cgroup = Cgroup::create(root.join("builds"))?;
                    let mut cmd = Command::new(exe);
                    cmd.args(args)
                        .arg("--ppid")
                        .arg(std::process::id().to_string())
                        .arg("--cgroup")
                        .arg(cgroup.path())
                        .args(["--", "sh", "-c", "while :; do sleep 3600; done"])
                        .process_group(0)
                        .stdin(Stdio::null());
                    let shim = spawn_from_keeper(cmd).map_err(|e| ProcError::Os {
                        what: "start build holder".into(),
                        source: e,
                    })?;
                    Some(Holder { shim, cgroup })
                }
                _ => None,
            };
            Ok(Self {
                holder,
                groups: Mutex::new(Vec::new()),
            })
        }

        pub(super) fn run(&self, spec: Spec) -> Result<Root> {
            let err = spec.log.try_clone().map_err(|e| ProcError::Os {
                what: "clone log handle".into(),
                source: e,
            })?;
            let mut cmd = Command::new("sh");
            cmd.args(["-c", &spec.command])
                .current_dir(&spec.cwd)
                .envs(&spec.env)
                .process_group(0)
                .stdin(Stdio::null())
                .stdout(spec.log)
                .stderr(err);
            if let Some(h) = &self.holder {
                h.cgroup.join_on_exec(&mut cmd)?;
            }
            let child = cmd.spawn().map_err(|e| ProcError::Os {
                what: format!("start build {}", spec.name),
                source: e,
            })?;
            let pgid = child.id() as i32;
            // A root whose start cannot be read (gone already) is not
            // tracked: its group could not be told from one reusing the number.
            if self.holder.is_none()
                && let Some(start) = sys::start_time(pgid)
            {
                self.groups
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push((pgid, start));
            }
            Ok(Root { child })
        }

        pub(super) fn usage(&self) -> Usage {
            match &self.holder {
                Some(h) => usage_of(Some(&h.cgroup), &self.pids()),
                None => summed_usage(&self.pids()),
            }
        }

        pub(super) fn pids(&self) -> Vec<u32> {
            match &self.holder {
                Some(h) => h.cgroup.listed(),
                None => {
                    let mut pids: Vec<u32> = self
                        .live_groups()
                        .into_iter()
                        .flat_map(|(_, members)| members)
                        .map(|p| p as u32)
                        .collect();
                    pids.sort();
                    pids
                }
            }
        }

        /// The build groups that still have members, with them; forgets the
        /// ones that ended. A group whose number now belongs to a leader with
        /// another start time is not the build's any more (a process group's
        /// number is not reused while the group exists, so a group whose
        /// leader has ended is still the build's).
        fn live_groups(&self) -> Vec<(i32, Vec<i32>)> {
            let mut groups = self.groups.lock().unwrap_or_else(|e| e.into_inner());
            let mut live = Vec::new();
            groups.retain(|&(pgid, start)| {
                if sys::start_time(pgid).is_some_and(|t| t != start) {
                    return false;
                }
                match group_members(pgid) {
                    Some(m) if m.is_empty() => false,
                    Some(m) => {
                        live.push((pgid, m));
                        true
                    }
                    // Cannot be told now: kept, and not counted.
                    None => true,
                }
            });
            live
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            if let Some(h) = &mut self.holder {
                h.cgroup.kill();
                let _ = h.shim.kill();
                let _ = h.shim.wait();
                h.cgroup.remove();
            }
            for (pgid, _) in self.live_groups() {
                let _ = kill_group(pgid, libc::SIGKILL);
            }
        }
    }

    impl Root {
        pub(super) fn kill_tree(&mut self) {
            let _ = kill_group(self.child.id() as i32, libc::SIGKILL);
        }
    }
}
