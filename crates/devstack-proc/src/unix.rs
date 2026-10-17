//! Linux: a shim per tree, process group always, cgroup v2 where delegated
//! (spike (a), ADR-0009). Other Unixes get the process group and the shim; the
//! cgroup parts are Linux-only.

use crate::{Options, ProcError, Record, Result, Spec, Usage};
use std::fs;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub(crate) struct Tree {
    pgid: i32,
    cgroup: Option<PathBuf>,
    child: Child,
    exit: Option<i32>,
}

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

/// The process exists and is no zombie.
fn live(pid: i32) -> bool {
    fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
        stat.rfind(')')
            .and_then(|i| stat[i + 1..].split_whitespace().next())
            .is_some_and(|state| state != "Z")
    })
}

/// Live members of a process group; `None` when that cannot be told. A
/// group that is gone costs one `kill(-pgid, 0)`, not a scan of `/proc`.
fn group_members(pgid: i32) -> Option<Vec<i32>> {
    // SAFETY: plain syscall; signal 0 only checks.
    if unsafe { libc::kill(-pgid, 0) } != 0
        && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    {
        return Some(Vec::new());
    }
    Some(
        processes()?
            .into_iter()
            .filter(|&(_, g, s)| g == pgid && s != 'Z')
            .map(|(p, _, _)| p)
            .collect(),
    )
}

/// Live members of a cgroup; `None` when it cannot be read.
fn cgroup_members(cg: &Path) -> Option<Vec<i32>> {
    Some(
        fs::read_to_string(cg.join("cgroup.procs"))
            .ok()?
            .lines()
            .filter_map(|l| l.trim().parse().ok())
            .filter(|&p| live(p))
            .collect(),
    )
}

pub(crate) fn kill_group(pgid: i32, sig: i32) -> std::io::Result<()> {
    // SAFETY: plain syscall.
    if unsafe { libc::kill(-pgid, sig) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

pub(crate) fn kill_cgroup(cg: &Path) {
    // cgroup.kill (Linux 5.14+) SIGKILLs every process in the cgroup and below.
    let _ = fs::write(cg.join("cgroup.kill"), "1");
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
        Some(root) => {
            let cg = root.join(&spec.name);
            fs::create_dir_all(&cg).map_err(|e| ProcError::Os {
                what: format!("create cgroup {}", cg.display()),
                source: e,
            })?;
            Some(cg)
        }
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
        cmd.arg("--cgroup").arg(cg);
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
        leader_start: start_time(pgid).unwrap_or(0),
        cgroup: cgroup.clone(),
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
            Some(cg) => cgroup_members(cg),
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
        usage_of(self.cgroup.as_deref(), &pids)
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
            kill_cgroup(cg);
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
            let _ = fs::remove_dir(cg);
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
    // SAFETY: installing async-signal-safe handlers and setting the death signal.
    unsafe {
        let death: extern "C" fn(libc::c_int) = on_parent_death;
        let noop: extern "C" fn(libc::c_int) = ignore;
        libc::signal(libc::SIGUSR2, death as libc::sighandler_t);
        libc::signal(libc::SIGTERM, noop as libc::sighandler_t);
        libc::signal(libc::SIGINT, noop as libc::sighandler_t);
        #[cfg(target_os = "linux")]
        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGUSR2);
        // The supervisor may have died before the death signal was armed.
        if libc::getppid() != ppid {
            PARENT_DIED.store(true, Ordering::SeqCst);
        }
    }
    if let Some(cg) = &cgroup
        && let Err(e) = fs::write(cg.join("cgroup.procs"), std::process::id().to_string())
    {
        fail(&format!("join cgroup {}: {e}", cg.display()));
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
                kill_cgroup(cg); // includes this shim
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
            Some(cg) => cgroup_members(cg),
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
            if let Some(cg) = &r.cgroup {
                n += cgroup_members(cg).map_or(0, |m| m.len());
                kill_cgroup(cg);
            }
            if r.leader_start == 0 {
                return (r.name.clone(), n);
            }
            for pid in group_members(r.pgid as i32).unwrap_or_default() {
                if start_time(pid).is_some_and(|t| t >= r.leader_start) {
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

/// Usage from the cgroup where there is one (`cpu.stat`, `memory.current`),
/// else summed over the processes from `/proc`.
pub(crate) fn usage_of(cgroup: Option<&Path>, pids: &[u32]) -> Usage {
    let read = |name: &str| cgroup.and_then(|cg| fs::read_to_string(cg.join(name)).ok());
    let cpu_us = read("cpu.stat")
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("usage_usec "))
                .and_then(|v| v.trim().parse().ok())
        })
        .unwrap_or_else(|| pids.iter().map(|&p| proc_cpu_us(p)).sum());
    let memory = read("memory.current")
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or_else(|| pids.iter().map(|&p| proc_rss(p)).sum());
    Usage {
        cpu_us,
        memory,
        processes: pids.len() as u32,
    }
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

#[cfg(all(test, target_os = "linux"))]
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
            leader_start: start_time(pgid as i32).unwrap(),
            ..record
        };
        assert_eq!(cleanup(&[known]), vec![("x".to_string(), 1)]);
        let _ = child.wait();
        assert_eq!(group_members(pgid as i32), Some(Vec::new()));
    }
}
