//! Unix: a shim per tree, process group always, a cgroup where the OS has one
//! and it is delegated (spike (a), ADR-0009). What is POSIX lives here
//! (signals, process groups, the shim, `rlimit`); what the OS has to say
//! about processes comes from `crate::sys` (Linux: `/proc` and cgroups;
//! macOS: libproc).

use crate::sys::{self, Cgroup};
use crate::{Options, ProcError, Record, Result, Spec, Usage};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub(crate) struct Tree {
    pgid: i32,
    cgroup: Option<Cgroup>,
    child: Child,
    exit: Option<i32>,
}

/// Live members of a process group; `None` when that cannot be told. A
/// group that is gone costs one `kill(-pgid, 0)`, not a scan of every process.
pub(crate) fn group_members(pgid: i32) -> Option<Vec<i32>> {
    // SAFETY: plain syscall; signal 0 only checks.
    if unsafe { libc::kill(-pgid, 0) } != 0
        && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    {
        return Some(Vec::new());
    }
    sys::group(pgid)
}

pub(crate) fn kill_group(pgid: i32, sig: i32) -> std::io::Result<()> {
    // SAFETY: plain syscall.
    if unsafe { libc::kill(-pgid, sig) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

type SpawnJob = (Command, std::sync::mpsc::Sender<std::io::Result<Child>>);

/// Start a shim from a thread that lives as long as the process.
///
/// `PR_SET_PDEATHSIG` fires when the *thread* that forked the child exits, not
/// only the process. A tree started from a short-lived thread (a command run,
/// a blocking task of the async runtime) would lose its shim when that thread
/// ends. Every shim is therefore forked by this one thread (ADR-0009).
pub(crate) fn spawn_from_keeper(cmd: Command) -> std::io::Result<Child> {
    static KEEPER: std::sync::OnceLock<std::sync::Mutex<std::sync::mpsc::Sender<SpawnJob>>> =
        std::sync::OnceLock::new();
    let keeper = KEEPER.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<SpawnJob>();
        std::thread::Builder::new()
            .name("devstack-spawner".into())
            .spawn(move || {
                for (mut cmd, reply) in rx {
                    let _ = reply.send(cmd.spawn());
                }
            })
            .expect("start the spawner thread");
        std::sync::Mutex::new(tx)
    });
    let (reply, answer) = std::sync::mpsc::channel();
    keeper
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .send((cmd, reply))
        .map_err(|_| std::io::Error::other("spawner thread is gone"))?;
    answer
        .recv()
        .map_err(|_| std::io::Error::other("spawner thread is gone"))?
}

pub(crate) fn spawn(spec: Spec, opts: &Options) -> Result<(Tree, Record)> {
    let Some((shim_exe, shim_args)) = opts.shim.split_first() else {
        return Err(ProcError::Other(
            "Options::shim must name the program that runs shim_main".into(),
        ));
    };
    let cgroup = match &opts.cgroup_root {
        Some(root) => Some(Cgroup::create(root.join(&spec.name))?),
        None => None,
    };
    let err = spec.log.try_clone().map_err(|e| ProcError::Os {
        what: "clone log handle".into(),
        source: e,
    })?;
    let mut cmd = Command::new(shim_exe);
    cmd.args(shim_args)
        .arg("--ppid")
        .arg(std::process::id().to_string());
    if let Some(cg) = &cgroup {
        cmd.arg("--cgroup").arg(cg.path());
    }
    cmd.arg("--")
        .args(["sh", "-c", &spec.command])
        .current_dir(&spec.cwd)
        .envs(&spec.env)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(spec.log)
        .stderr(err);
    let child = spawn_from_keeper(cmd).map_err(|e| ProcError::Os {
        what: format!("start {}", spec.name),
        source: e,
    })?;
    let pgid = child.id() as i32;
    let record = Record {
        name: spec.name,
        pgid: pgid as i64,
        leader_start: sys::start_time(pgid).unwrap_or(0),
        cgroup: cgroup.as_ref().map(|cg| cg.path().to_path_buf()),
    };
    Ok((
        Tree {
            pgid,
            cgroup,
            child,
            exit: None,
        },
        record,
    ))
}

impl Tree {
    pub(crate) fn root_pid(&self) -> u32 {
        self.child.id()
    }

    pub(crate) fn pids(&mut self) -> Vec<u32> {
        let _ = self.try_exit_code(); // reap the shim so it does not linger as a zombie
        let mut v = self.members().unwrap_or_default();
        v.sort();
        v.into_iter().map(|p| p as u32).collect()
    }

    fn members(&self) -> Option<Vec<i32>> {
        match &self.cgroup {
            Some(cg) => cg.members(),
            None => group_members(self.pgid),
        }
    }

    /// Whether any process of the tree lives; `None` when that cannot be told.
    pub(crate) fn alive(&mut self) -> Option<bool> {
        let _ = self.try_exit_code();
        self.members().map(|m| !m.is_empty())
    }

    pub(crate) fn usage(&mut self) -> Usage {
        let pids = self.pids();
        usage_of(self.cgroup.as_ref(), &pids)
    }

    pub(crate) fn try_exit_code(&mut self) -> Option<i32> {
        if self.exit.is_none()
            && let Ok(Some(status)) = self.child.try_wait()
        {
            self.exit = Some(status.code().unwrap_or(1));
        }
        self.exit
    }

    pub(crate) fn request_stop(&mut self) -> Result<()> {
        kill_group(self.pgid, libc::SIGTERM).map_err(|e| ProcError::Os {
            what: format!("SIGTERM to group {}", self.pgid),
            source: e,
        })
    }

    pub(crate) fn kill(&mut self) {
        if let Some(cg) = &self.cgroup {
            cg.kill();
        }
        let _ = kill_group(self.pgid, libc::SIGKILL);
        let _ = self.child.wait();
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        // Dropping the handle ends the tree, as closing the job does on Windows.
        if self.exit.is_none() {
            self.kill();
        }
        if let Some(cg) = &self.cgroup {
            cg.remove();
        }
    }
}

// --------------------------------------------------------------------- shim

static PARENT_DIED: AtomicBool = AtomicBool::new(false);
extern "C" fn on_parent_death(_: libc::c_int) {
    PARENT_DIED.store(true, Ordering::SeqCst);
}
extern "C" fn ignore(_: libc::c_int) {}

/// Entry point of the shim (`devstack shim --ppid <pid> [--cgroup <dir>] -- <cmd…>`).
///
/// Survives the graceful stop signal through a no-op handler (not `SIG_IGN`,
/// which the service would inherit across `exec`), and when the supervisor
/// dies kills the tree's cgroup and process group. Exits when nothing but
/// itself is left in the tree, with the service's exit code.
pub fn shim_main(args: &[String]) -> ! {
    let mut ppid = None;
    let mut cgroup = None;
    let mut it = args.iter();
    let mut cmd = Vec::new();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--ppid" => ppid = it.next().and_then(|v| v.parse::<i32>().ok()),
            "--cgroup" => cgroup = it.next().map(PathBuf::from),
            "--" => {
                cmd = it.by_ref().cloned().collect();
                break;
            }
            other => fail(&format!("unexpected argument {other}")),
        }
    }
    let Some(ppid) = ppid else {
        fail("--ppid is required")
    };
    if cmd.is_empty() {
        fail("no command after --");
    }
    let cgroup = cgroup.map(|dir| {
        Cgroup::at(&dir)
            .unwrap_or_else(|| fail(&format!("--cgroup {}: no cgroups here", dir.display())))
    });
    // SAFETY: installing async-signal-safe handlers.
    unsafe {
        let death: extern "C" fn(libc::c_int) = on_parent_death;
        let noop: extern "C" fn(libc::c_int) = ignore;
        libc::signal(libc::SIGUSR2, death as libc::sighandler_t);
        libc::signal(libc::SIGTERM, noop as libc::sighandler_t);
        libc::signal(libc::SIGINT, noop as libc::sighandler_t);
    }
    sys::arm_parent_death(ppid, libc::SIGUSR2);
    // The supervisor may have died before the death signal was armed.
    // SAFETY: plain syscall.
    if unsafe { libc::getppid() } != ppid {
        PARENT_DIED.store(true, Ordering::SeqCst);
    }
    if let Some(cg) = &cgroup
        && let Err(e) = cg.join(std::process::id())
    {
        fail(&format!("join cgroup {}: {e}", cg.path().display()));
    }
    let me = std::process::id() as i32;
    // SAFETY: plain syscall.
    let pgid = unsafe { libc::getpgrp() };
    let mut child = match Command::new(&cmd[0]).args(&cmd[1..]).spawn() {
        Ok(c) => c,
        Err(e) => fail(&format!("start {}: {e}", cmd[0])),
    };
    let mut code: Option<i32> = None;
    loop {
        if PARENT_DIED.load(Ordering::SeqCst) {
            if let Some(cg) = &cgroup {
                cg.kill(); // includes this shim
            }
            let _ = kill_group(pgid, libc::SIGKILL);
            std::process::exit(137);
        }
        if code.is_none()
            && let Ok(Some(status)) = child.try_wait()
        {
            code = Some(status.code().unwrap_or(1));
        }
        let others = match &cgroup {
            Some(cg) => cg.members(),
            None => group_members(pgid),
        };
        if others.is_some_and(|o| o.iter().all(|&p| p == me)) {
            // The service may have exited between try_wait and the membership
            // check (zombies do not count as members): collect its code now,
            // never report a guess.
            let code =
                code.unwrap_or_else(|| child.wait().ok().and_then(|s| s.code()).unwrap_or(1));
            std::process::exit(code);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn fail(msg: &str) -> ! {
    eprintln!("devstack shim: {msg}");
    std::process::exit(2)
}

// ------------------------------------------------------------ restart cleanup

/// Only processes of the recorded group that started no earlier than the
/// recorded leader are hit, so a process that merely reuses a number is spared.
/// Without a recorded start (`leader_start == 0`) the group is left alone:
/// after a reboot its number may belong to anyone.
pub(crate) fn cleanup(records: &[Record]) -> Vec<(String, usize)> {
    records
        .iter()
        .map(|r| {
            let mut n = 0;
            if let Some(cg) = r.cgroup.as_deref().and_then(Cgroup::at) {
                n += cg.members().map_or(0, |m| m.len());
                cg.kill();
            }
            if r.leader_start == 0 {
                return (r.name.clone(), n);
            }
            for pid in group_members(r.pgid as i32).unwrap_or_default() {
                if sys::start_time(pid).is_some_and(|t| t >= r.leader_start) {
                    // SAFETY: plain syscall.
                    unsafe {
                        libc::kill(pid, libc::SIGKILL);
                    }
                    n += 1;
                }
            }
            (r.name.clone(), n)
        })
        .collect()
}

/// Usage from the cgroup where there is one, else summed over the processes.
pub(crate) fn usage_of(cgroup: Option<&Cgroup>, pids: &[u32]) -> Usage {
    match cgroup {
        Some(cg) => cg.usage(pids),
        None => summed_usage(pids),
    }
}

/// Usage summed over live processes: what has exited is not counted.
pub(crate) fn summed_usage(pids: &[u32]) -> Usage {
    let (cpu_us, memory) = pids
        .iter()
        .map(|&p| sys::process_usage(p))
        .fold((0, 0), |(c, m), (pc, pm)| (c + pc, m + pm));
    Usage {
        cpu_us,
        memory,
        processes: pids.len() as u32,
    }
}

// -------------------------------------------------------------------- misc

pub(crate) fn pid_alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 only checks that the process exists.
    let sent = unsafe { libc::kill(pid, 0) } == 0;
    sent || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// No console to give: services get `SIGTERM` to their process group.
pub(crate) fn ensure_console() -> Result<bool> {
    Ok(false)
}

/// Unix paths have no short form.
pub(crate) fn long_path(_path: &Path) -> Option<PathBuf> {
    None
}

pub(crate) fn disk_free(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a NUL-terminated path, `s` a statvfs to fill.
    let ok = unsafe { libc::statvfs(c.as_ptr(), &mut s) } == 0;
    #[allow(clippy::unnecessary_cast)]
    ok.then(|| s.f_bavail as u64 * s.f_frsize as u64)
}

pub(crate) fn raise_open_files() -> Option<u64> {
    let mut l: libc::rlimit = unsafe { std::mem::zeroed() };
    // SAFETY: `l` is an rlimit to fill.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut l) } != 0 {
        return None;
    }
    // macOS refuses more than OPEN_MAX for an unlimited hard limit.
    let want = if l.rlim_max == libc::RLIM_INFINITY {
        l.rlim_cur.max(10240)
    } else {
        l.rlim_max
    };
    if want > l.rlim_cur {
        let raised = libc::rlimit {
            rlim_cur: want,
            rlim_max: l.rlim_max,
        };
        // SAFETY: a valid rlimit no higher than the hard limit.
        if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raised) } == 0 {
            l.rlim_cur = want;
        }
    }
    #[allow(clippy::unnecessary_cast)]
    Some(l.rlim_cur as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// F-35: a record without the leader's start time cannot tell its group
    /// from one that reuses the number; it is left alone.
    #[test]
    fn cleanup_without_a_start_time_leaves_the_group_alone() {
        let mut child = Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap();
        let pgid = child.id() as i64;
        let record = Record {
            name: "x".into(),
            pgid,
            leader_start: 0,
            cgroup: None,
        };
        assert_eq!(
            cleanup(std::slice::from_ref(&record)),
            vec![("x".to_string(), 0)]
        );
        assert!(child.try_wait().unwrap().is_none(), "still running");
        let known = Record {
            leader_start: sys::start_time(pgid as i32).unwrap(),
            ..record
        };
        assert_eq!(cleanup(&[known]), vec![("x".to_string(), 1)]);
        let _ = child.wait();
        assert_eq!(group_members(pgid as i32), Some(Vec::new()));
    }
}
