//! Linux: a shim per tree, process group always, cgroup v2 where delegated
//! (spike (a), ADR-0009). Other Unixes get the process group and the shim; the
//! cgroup parts are Linux-only.

use crate::{Options, ProcError, Record, Result, Spec};
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

/// (pid, process group, state) of every process.
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
    let live: Vec<i32> = processes()
        .into_iter()
        .filter(|&(_, _, s)| s != 'Z')
        .map(|(p, _, _)| p)
        .collect();
    fs::read_to_string(cg.join("cgroup.procs"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .filter(|p| live.contains(p))
        .collect()
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
        let mut v = match &self.cgroup {
            Some(cg) => cgroup_members(cg),
            None => group_members(self.pgid),
        };
        v.sort();
        v.into_iter().map(|p| p as u32).collect()
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
        if others.iter().all(|&p| p == me) {
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
pub(crate) fn cleanup(records: &[Record]) -> Vec<(String, usize)> {
    records
        .iter()
        .map(|r| {
            let mut n = 0;
            if let Some(cg) = &r.cgroup {
                n += cgroup_members(cg).len();
                kill_cgroup(cg);
            }
            for pid in group_members(r.pgid as i32) {
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
