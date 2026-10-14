//! Linux process ownership for spike (a).
//!
//! Linux has no counterpart to a Job Object with kill-on-close: neither a
//! process group nor a cgroup dies with the process that created it. Options
//! measured here:
//!
//! - **process group** per service (`setpgid`): signals reach the whole group,
//!   but a process that calls `setsid` (a "detached" child) leaves it.
//! - **cgroup v2** per service: holds every descendant, `setsid` or not, and
//!   `cgroup.kill` ends them all. Needs a writable cgroup hierarchy.
//! - **shim**: a small `procspike shim` process between supervisor and service.
//!   It asks the kernel for a signal when its parent dies (`PR_SET_PDEATHSIG`)
//!   and then kills its service's group and cgroup: kill-on-close semantics.
//! - **restart cleanup** (ADR-0016): the supervisor records group id, leader
//!   start time and cgroup; `procspike cleanup` kills what a dead instance left.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

#[derive(Clone, Default)]
pub struct Options {
    /// Parent directory for per-service cgroups (e.g. /sys/fs/cgroup/procspike).
    pub cgroup_root: Option<PathBuf>,
    /// Put a shim between supervisor and service.
    pub shim: bool,
}

/// What a later instance needs to clean up after this one (ADR-0016).
#[derive(Serialize, Deserialize, Clone)]
pub struct Record {
    pub name: String,
    pub pgid: i32,
    /// Start time of the group leader in clock ticks since boot (/proc/<pid>/stat field 22).
    pub leader_start: u64,
    pub cgroup: Option<PathBuf>,
}

pub struct Service {
    pub name: String,
    pub pgid: i32,
    cgroup: Option<PathBuf>,
    child: Child,
}

/// Start time of a process in clock ticks since boot, or None if it is gone.
pub fn start_time(pid: i32) -> Option<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command name may contain spaces and parentheses; fields start after the last ')'.
    let rest = &stat[stat.rfind(')')? + 2..];
    rest.split_whitespace().nth(19)?.parse().ok()
}

/// (pid, pgid, state) of every process.
fn processes() -> Vec<(i32, i32, char)> {
    let mut out = Vec::new();
    let Ok(dir) = fs::read_dir("/proc") else {
        return out;
    };
    for e in dir.flatten() {
        let Ok(pid) = e.file_name().to_string_lossy().parse::<i32>() else {
            continue;
        };
        let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        let Some(close) = stat.rfind(')') else {
            continue;
        };
        let mut f = stat[close + 2..].split_whitespace();
        let state = f.next().and_then(|s| s.chars().next()).unwrap_or('?');
        let _ppid = f.next();
        let Some(pgrp) = f.next().and_then(|s| s.parse().ok()) else {
            continue;
        };
        out.push((pid, pgrp, state));
    }
    out
}

fn group_members(pgid: i32) -> Vec<i32> {
    processes()
        .into_iter()
        .filter(|&(_, g, s)| g == pgid && s != 'Z')
        .map(|(p, _, _)| p)
        .collect()
}

fn cgroup_members(cg: &Path) -> Vec<i32> {
    fs::read_to_string(cg.join("cgroup.procs"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .filter(|&p| processes().iter().any(|&(pid, _, s)| pid == p && s != 'Z'))
        .collect()
}

fn kill_group(pgid: i32, sig: i32) {
    unsafe {
        libc::kill(-pgid, sig);
    }
}

fn kill_cgroup(cg: &Path) {
    // cgroup.kill (Linux 5.14+) SIGKILLs every process in the cgroup and below.
    let _ = fs::write(cg.join("cgroup.kill"), "1");
}

fn make_cgroup(root: &Path, name: &str) -> Result<PathBuf> {
    let cg = root.join(name);
    fs::create_dir_all(&cg).with_context(|| format!("create cgroup {}", cg.display()))?;
    Ok(cg)
}

pub fn spawn(name: &str, cmdline: &str, log: File, opts: &Options) -> Result<(Service, Record)> {
    let err = log.try_clone()?;
    let cgroup = match &opts.cgroup_root {
        Some(root) => Some(make_cgroup(root, name)?),
        None => None,
    };
    let mut cmd = if opts.shim {
        let mut c = Command::new(std::env::current_exe()?);
        c.args(["shim", "--ppid", &std::process::id().to_string()]);
        if let Some(cg) = &cgroup {
            c.arg("--cgroup").arg(cg);
        }
        c.args(["--", "sh", "-c", cmdline]);
        c
    } else {
        let mut c = Command::new("sh");
        c.args(["-c", cmdline]);
        let procs = cgroup.as_ref().map(|cg| cg.join("cgroup.procs"));
        unsafe {
            c.pre_exec(move || {
                // Leader only: dies with the supervisor. Its children do not.
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                if let Some(p) = &procs {
                    // Join the cgroup before exec, so every descendant is born in it.
                    std::fs::write(p, libc::getpid().to_string())?;
                }
                Ok(())
            });
        }
        c
    };
    let child = cmd
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(err)
        .spawn()
        .with_context(|| format!("spawn {name}"))?;
    let pgid = child.id() as i32;
    let record = Record {
        name: name.into(),
        pgid,
        leader_start: start_time(pgid).unwrap_or(0),
        cgroup: cgroup.clone(),
    };
    Ok((
        Service {
            name: name.into(),
            pgid,
            cgroup,
            child,
        },
        record,
    ))
}

impl Service {
    /// Members: the cgroup when there is one (it also holds processes that left
    /// the group with setsid), the process group otherwise.
    pub fn pids(&mut self) -> Vec<u32> {
        let _ = self.child.try_wait(); // reap the leader so it does not linger as a zombie
        let mut v = match &self.cgroup {
            Some(cg) => cgroup_members(cg),
            None => group_members(self.pgid),
        };
        v.sort();
        v.into_iter().map(|p| p as u32).collect()
    }

    pub fn active_processes(&mut self) -> u32 {
        self.pids().len() as u32
    }

    pub fn request_stop(&mut self) -> Result<()> {
        if unsafe { libc::kill(-self.pgid, libc::SIGTERM) } != 0 {
            bail!(
                "kill(-{}, SIGTERM): {}",
                self.pgid,
                std::io::Error::last_os_error()
            );
        }
        Ok(())
    }

    pub fn terminate(&mut self) {
        if let Some(cg) = &self.cgroup {
            kill_cgroup(cg);
        }
        kill_group(self.pgid, libc::SIGKILL);
        let _ = self.child.try_wait();
    }
}

// ---------------------------------------------------------------- the shim

static PARENT_DIED: AtomicBool = AtomicBool::new(false);
extern "C" fn on_parent_death(_: libc::c_int) {
    PARENT_DIED.store(true, Ordering::SeqCst);
}
extern "C" fn ignore(_: libc::c_int) {}

/// Sits between supervisor and service. Survives the service's graceful stop
/// signal (a handler, not SIG_IGN, so the service does not inherit "ignore"),
/// and when the supervisor dies, kills the service's cgroup and process group.
pub fn shim(ppid: u32, cgroup: Option<PathBuf>, cmd: Vec<String>) -> Result<()> {
    unsafe {
        libc::signal(libc::SIGUSR2, on_parent_death as usize);
        libc::signal(libc::SIGTERM, ignore as usize);
        libc::signal(libc::SIGINT, ignore as usize);
        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGUSR2);
        // The supervisor may have died before prctl took effect.
        if libc::getppid() as u32 != ppid {
            PARENT_DIED.store(true, Ordering::SeqCst);
        }
    }
    if let Some(cg) = &cgroup {
        fs::write(cg.join("cgroup.procs"), std::process::id().to_string())?;
    }
    let me = std::process::id() as i32;
    let pgid = unsafe { libc::getpgrp() };
    let mut child = Command::new(&cmd[0]).args(&cmd[1..]).spawn()?;
    let mut code = 0;
    loop {
        if PARENT_DIED.load(Ordering::SeqCst) {
            if let Some(cg) = &cgroup {
                kill_cgroup(cg); // includes this shim
            }
            kill_group(pgid, libc::SIGKILL);
            std::process::exit(137);
        }
        if let Ok(Some(status)) = child.try_wait() {
            code = status.code().unwrap_or(1);
        }
        // Done when nothing but the shim is left in the service.
        let others = match &cgroup {
            Some(cg) => cgroup_members(cg),
            None => group_members(pgid),
        };
        if others.iter().all(|&p| p == me) {
            std::process::exit(code);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

// ------------------------------------------------------ restart cleanup

/// Kill what a previous instance recorded and left behind. Only processes in
/// the recorded group that started no earlier than the recorded leader are hit,
/// so a process that merely reuses a number is left alone.
pub fn cleanup(records: &[Record]) -> Vec<(String, usize)> {
    let mut killed = Vec::new();
    for r in records {
        let mut n = 0;
        if let Some(cg) = &r.cgroup {
            n += cgroup_members(cg).len();
            kill_cgroup(cg);
        }
        for pid in group_members(r.pgid) {
            if start_time(pid).is_some_and(|t| t >= r.leader_start) {
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                }
                n += 1;
            }
        }
        killed.push((r.name.clone(), n));
    }
    killed
}
