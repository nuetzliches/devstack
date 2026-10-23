//! Linux: what the POSIX backend needs from the OS, read from `/proc`, and
//! cgroup v2 where it is delegated (spike (a), ADR-0009); the machine's
//! memory and CPU time.

use crate::{ProcError, Result, Usage};
use std::fs;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

// ------------------------------------------------------------------ machine

pub(crate) fn memory_total() -> Option<u64> {
    meminfo("MemTotal:")
}

pub(crate) fn memory_available() -> Option<u64> {
    meminfo("MemAvailable:")
}

fn meminfo(key: &str) -> Option<u64> {
    let text = fs::read_to_string("/proc/meminfo").ok()?;
    let kb: u64 = text
        .lines()
        .find_map(|l| l.strip_prefix(key))?
        .trim()
        .trim_end_matches("kB")
        .trim()
        .parse()
        .ok()?;
    Some(kb * 1024)
}

/// `(idle, total)` in clock ticks, from the first line of `/proc/stat`.
pub(crate) fn cpu_times() -> Option<(u64, u64)> {
    let text = fs::read_to_string("/proc/stat").ok()?;
    let v: Vec<u64> = text
        .lines()
        .next()?
        .strip_prefix("cpu ")?
        .split_whitespace()
        .filter_map(|x| x.parse().ok())
        .collect();
    // user nice system idle iowait irq softirq steal ...
    let idle = v.get(3)? + v.get(4).copied().unwrap_or(0);
    Some((idle, v.iter().take(8).sum()))
}

// ---------------------------------------------------------------- processes

/// Start time of a process in clock ticks since boot (/proc/<pid>/stat field 22).
pub(crate) fn start_time(pid: i32) -> Option<u64> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = &stat[stat.rfind(')')? + 2..];
    rest.split_whitespace().nth(19)?.parse().ok()
}

/// (pid, process group, state) of every process; `None` when `/proc`
/// cannot be read.
fn processes() -> Option<Vec<(i32, i32, char)>> {
    let mut out = Vec::new();
    let dir = fs::read_dir("/proc").ok()?;
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
    Some(out)
}

/// Live (no zombie) members of a process group, from a scan of `/proc`;
/// `None` when it cannot be read.
pub(crate) fn group(pgid: i32) -> Option<Vec<i32>> {
    Some(
        processes()?
            .into_iter()
            .filter(|&(_, g, s)| g == pgid && s != 'Z')
            .map(|(p, _, _)| p)
            .collect(),
    )
}

/// The process exists and is no zombie.
fn live(pid: i32) -> bool {
    fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
        stat.rfind(')')
            .and_then(|i| stat[i + 1..].split_whitespace().next())
            .is_some_and(|state| state != "Z")
    })
}

/// CPU time (user plus kernel, microseconds) and resident set (bytes) of
/// one process; zero for what cannot be read.
pub(crate) fn process_usage(pid: u32) -> (u64, u64) {
    (proc_cpu_us(pid), proc_rss(pid))
}

fn proc_cpu_us(pid: u32) -> u64 {
    let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return 0;
    };
    // After the command name: state is field 3, utime 14, stime 15.
    let Some(rest) = stat.rfind(')').map(|i| &stat[i + 2..]) else {
        return 0;
    };
    let f: Vec<&str> = rest.split_whitespace().collect();
    let ticks: u64 = [11, 12]
        .iter()
        .filter_map(|&i| f.get(i).and_then(|v| v.parse::<u64>().ok()))
        .sum();
    // SAFETY: plain libc call.
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as u64;
    ticks * 1_000_000 / hz
}

fn proc_rss(pid: u32) -> u64 {
    let pages: u64 = fs::read_to_string(format!("/proc/{pid}/statm"))
        .ok()
        .and_then(|s| s.split_whitespace().nth(1).and_then(|v| v.parse().ok()))
        .unwrap_or(0);
    // SAFETY: plain libc call.
    pages * unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(1) as u64
}

/// Make the death of the parent deliver `signal` to this process
/// (`PR_SET_PDEATHSIG`). It fires when the *thread* that forked this process
/// ends, which is why every shim is forked by one long-lived thread
/// (`posix::spawn_from_keeper`).
pub(crate) fn arm_parent_death(_ppid: i32, signal: libc::c_int) {
    // SAFETY: plain syscall.
    unsafe {
        libc::prctl(libc::PR_SET_PDEATHSIG, signal);
    }
}

// ------------------------------------------------------------------ cgroups

/// A cgroup v2 that holds a tree (or the build job) where the daemon's
/// cgroup is delegated: exact membership, and one write kills every member.
pub(crate) struct Cgroup(PathBuf);

impl Cgroup {
    /// Create the cgroup `dir`.
    pub(crate) fn create(dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(&dir).map_err(|e| ProcError::Os {
            what: format!("create cgroup {}", dir.display()),
            source: e,
        })?;
        Ok(Self(dir))
    }

    /// A cgroup made before: a record's, or the one the shim is told to join.
    pub(crate) fn at(dir: &Path) -> Option<Self> {
        Some(Self(dir.to_path_buf()))
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }

    /// Live members; `None` when they cannot be read.
    pub(crate) fn members(&self) -> Option<Vec<i32>> {
        Some(
            fs::read_to_string(self.0.join("cgroup.procs"))
                .ok()?
                .lines()
                .filter_map(|l| l.trim().parse().ok())
                .filter(|&p| live(p))
                .collect(),
        )
    }

    /// Every process `cgroup.procs` lists; empty when it cannot be read.
    pub(crate) fn listed(&self) -> Vec<u32> {
        fs::read_to_string(self.0.join("cgroup.procs"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.trim().parse().ok())
            .collect()
    }

    pub(crate) fn kill(&self) {
        // cgroup.kill (Linux 5.14+) SIGKILLs every process in the cgroup and below.
        let _ = fs::write(self.0.join("cgroup.kill"), "1");
    }

    pub(crate) fn remove(&self) {
        let _ = fs::remove_dir(&self.0);
    }

    /// Move a process into the cgroup.
    pub(crate) fn join(&self, pid: u32) -> std::io::Result<()> {
        fs::write(self.0.join("cgroup.procs"), pid.to_string())
    }

    /// Have the process `cmd` starts join the cgroup before it runs anything.
    pub(crate) fn join_on_exec(&self, cmd: &mut Command) -> Result<()> {
        let procs =
            std::ffi::CString::new(self.0.join("cgroup.procs").to_string_lossy().into_owned())
                .map_err(|_| ProcError::Other("cgroup path contains NUL".into()))?;
        // SAFETY: only async-signal-safe calls between fork and exec.
        unsafe {
            cmd.pre_exec(move || join_cgroup(&procs));
        }
        Ok(())
    }

    /// Usage from the cgroup (`cpu.stat`, `memory.current`), else summed
    /// over the processes.
    pub(crate) fn usage(&self, pids: &[u32]) -> Usage {
        let read = |name: &str| fs::read_to_string(self.0.join(name)).ok();
        let cpu_us = read("cpu.stat").and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("usage_usec "))
                .and_then(|v| v.trim().parse().ok())
        });
        let memory = read("memory.current").and_then(|s| s.trim().parse().ok());
        let summed = || crate::posix::summed_usage(pids);
        Usage {
            cpu_us: cpu_us.unwrap_or_else(|| summed().cpu_us),
            memory: memory.unwrap_or_else(|| summed().memory),
            processes: pids.len() as u32,
        }
    }
}

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
