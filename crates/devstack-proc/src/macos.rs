//! macOS: what the POSIX backend needs from the OS, from libproc (process
//! lists, start times, CPU time and resident sets), and the machine's memory
//! and CPU time from `sysctl` and the Mach host statistics (ADR-0054).
//! There are no cgroups: a tree is its process group, and the shim notices
//! the supervisor's death by polling `getppid()`.

use crate::{ProcError, Result, Usage};
use std::ffi::{c_int, c_void};
use std::mem;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

/// `proc_listpids` filter for the members of one process group
/// (`sys/proc_info.h`; libc does not define it).
const PROC_PGRP_ONLY: u32 = 2;

// ------------------------------------------------------------------ machine

/// `hw.memsize`.
pub(crate) fn memory_total() -> Option<u64> {
    let mut bytes: u64 = 0;
    let mut len = mem::size_of::<u64>();
    // SAFETY: a NUL-terminated name and an out buffer of the length given.
    let ok = unsafe {
        libc::sysctlbyname(
            c"hw.memsize".as_ptr(),
            (&raw mut bytes).cast::<c_void>(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    } == 0;
    (ok && len == mem::size_of::<u64>()).then_some(bytes)
}

/// What Activity Monitor shows as physical memory minus "Memory Used": the
/// total less app memory (anonymous pages that cannot be purged), wired
/// memory and what the compressor holds. What remains is free memory and
/// the file cache, which the system gives up for new work: the closest
/// match to Linux's `MemAvailable`. Free plus inactive pages would count
/// anonymous memory that has to be compressed or swapped first.
pub(crate) fn memory_available() -> Option<u64> {
    // SAFETY: plain data, all zeroes is a valid value.
    let mut vm: libc::vm_statistics64 = unsafe { mem::zeroed() };
    let mut count = libc::HOST_VM_INFO64_COUNT;
    // SAFETY: `vm` has room for `count` integers; the kernel fills what it
    // has (an older one fewer fields) and says how many.
    let kr = unsafe {
        libc::host_statistics64(
            host(),
            libc::HOST_VM_INFO64,
            (&raw mut vm).cast::<libc::integer_t>(),
            &mut count,
        )
    };
    if kr != libc::KERN_SUCCESS {
        return None;
    }
    let app = u64::from(vm.internal_page_count).saturating_sub(u64::from(vm.purgeable_count));
    let used = (app + u64::from(vm.wire_count) + u64::from(vm.compressor_page_count)) * page_size();
    Some(memory_total()?.saturating_sub(used))
}

/// `(idle, total)` in scheduler ticks over all CPUs (`HOST_CPU_LOAD_INFO`).
/// The kernel counts them in 32 bits: a reading taken after a counter wrapped
/// (some weeks at 100 ticks per second and core) gives one sample that says
/// nothing (the callers' differences saturate at 0); the next is right again.
pub(crate) fn cpu_times() -> Option<(u64, u64)> {
    // SAFETY: plain data, all zeroes is a valid value.
    let mut load: libc::host_cpu_load_info = unsafe { mem::zeroed() };
    let mut count = libc::HOST_CPU_LOAD_INFO_COUNT;
    // SAFETY: `load` has room for `count` integers.
    let kr = unsafe {
        libc::host_statistics(
            host(),
            libc::HOST_CPU_LOAD_INFO,
            (&raw mut load).cast::<libc::integer_t>(),
            &mut count,
        )
    };
    if kr != libc::KERN_SUCCESS {
        return None;
    }
    let ticks = load.cpu_ticks.map(u64::from);
    Some((ticks[libc::CPU_STATE_IDLE as usize], ticks.iter().sum()))
}

/// The host port, asked for once: every `mach_host_self()` adds a reference
/// to it.
fn host() -> libc::host_t {
    static HOST: OnceLock<libc::host_t> = OnceLock::new();
    // SAFETY: plain Mach call without arguments.
    *HOST.get_or_init(|| unsafe { mach2::mach_init::mach_host_self() })
}

fn page_size() -> u64 {
    // SAFETY: plain libc call.
    unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(1) as u64
}

/// Mach absolute time units in nanoseconds (1 on Intel, 125/3 on Apple
/// silicon).
fn mach_to_ns(t: u64) -> u64 {
    static TIMEBASE: OnceLock<(u64, u64)> = OnceLock::new();
    let &(numer, denom) = TIMEBASE.get_or_init(|| {
        let mut tb = mach2::mach_time::mach_timebase_info { numer: 0, denom: 0 };
        // SAFETY: a valid out pointer.
        let kr = unsafe { mach2::mach_time::mach_timebase_info(&mut tb) };
        if kr != libc::KERN_SUCCESS || tb.numer == 0 || tb.denom == 0 {
            (1, 1)
        } else {
            (u64::from(tb.numer), u64::from(tb.denom))
        }
    });
    (u128::from(t) * u128::from(numer) / u128::from(denom)) as u64
}

// ---------------------------------------------------------------- processes

/// Start of a process in Mach absolute time since boot
/// (`ri_proc_start_abstime`). Like Linux's clock ticks since boot it only
/// grows during a boot and starts again at zero after a reboot, so the
/// cleanup's comparison with a recorded leader means the same on both
/// (`pbi_start_tvsec` is wall-clock time: it would neither restart at a
/// reboot nor stay put when the clock is set).
pub(crate) fn start_time(pid: i32) -> Option<u64> {
    // SAFETY: plain data, all zeroes is a valid value.
    let mut ri: libc::rusage_info_v2 = unsafe { mem::zeroed() };
    // SAFETY: `ri` is the struct the flavor asks for; the API takes it as a
    // `rusage_info_t *`.
    let r = unsafe {
        libc::proc_pid_rusage(
            pid,
            libc::RUSAGE_INFO_V2,
            (&raw mut ri).cast::<libc::rusage_info_t>(),
        )
    };
    (r == 0 && ri.ri_proc_start_abstime != 0).then_some(ri.ri_proc_start_abstime)
}

/// `proc_pidinfo` for a fixed-size flavor; `None` when the process is gone
/// or may not be inspected.
fn pidinfo<T>(pid: i32, flavor: c_int) -> Option<T> {
    let size = c_int::try_from(mem::size_of::<T>()).ok()?;
    let mut info = mem::MaybeUninit::<T>::zeroed();
    // SAFETY: the buffer has `size` bytes; the kernel fills them all or the
    // call reports fewer.
    let n = unsafe { libc::proc_pidinfo(pid, flavor, 0, info.as_mut_ptr().cast(), size) };
    // SAFETY: the kernel filled the whole struct (plain data).
    (n == size).then(|| unsafe { info.assume_init() })
}

/// What `proc_listpids` lists for a filter; `None` when it cannot be asked.
fn list_pids(kind: u32, arg: u32) -> Option<Vec<i32>> {
    // Without a buffer the call tells how many bytes a list may take (every
    // process and some room); with one it returns how many it filled.
    // SAFETY: a null buffer is how the size is asked for.
    let need = unsafe { libc::proc_listpids(kind, arg, std::ptr::null_mut(), 0) };
    if need <= 0 {
        return None;
    }
    let mut room = need as usize / mem::size_of::<i32>() + 16;
    loop {
        let mut buf = vec![0i32; room];
        let bytes = c_int::try_from(room * mem::size_of::<i32>()).ok()?;
        // SAFETY: `buf` has `bytes` bytes.
        let filled = unsafe { libc::proc_listpids(kind, arg, buf.as_mut_ptr().cast(), bytes) };
        if filled < 0 {
            return None;
        }
        let n = filled as usize / mem::size_of::<i32>();
        if n < room || room >= 1 << 20 {
            buf.truncate(n);
            buf.retain(|&p| p > 0);
            return Some(buf);
        }
        // Full: processes were started meanwhile; ask again with more room.
        room *= 2;
    }
}

/// Live (no zombie) members of a process group; `None` when the process
/// list cannot be read.
pub(crate) fn group(pgid: i32) -> Option<Vec<i32>> {
    let Ok(id) = u32::try_from(pgid) else {
        return Some(Vec::new());
    };
    Some(
        list_pids(PROC_PGRP_ONLY, id)?
            .into_iter()
            .filter(|&p| {
                // The list includes zombies; and a process may have left the
                // group (or ended) since it was listed.
                pidinfo::<libc::proc_bsdinfo>(p, libc::PROC_PIDTBSDINFO)
                    .is_some_and(|i| i.pbi_pgid == id && i.pbi_status != libc::SZOMB)
            })
            .collect(),
    )
}

/// CPU time (user plus kernel, microseconds) and resident set (bytes) of
/// one process; zero for what cannot be read.
pub(crate) fn process_usage(pid: u32) -> (u64, u64) {
    let Ok(pid) = i32::try_from(pid) else {
        return (0, 0);
    };
    match pidinfo::<libc::proc_taskinfo>(pid, libc::PROC_PIDTASKINFO) {
        // The times are Mach absolute time units, not nanoseconds.
        Some(t) => (
            mach_to_ns(t.pti_total_user.saturating_add(t.pti_total_system)) / 1000,
            t.pti_resident_size,
        ),
        None => (0, 0),
    }
}

/// macOS has no parent-death signal: a thread asks `getppid()` every 500 ms
/// and sends `signal` to this process once the parent changed (the shim
/// then belongs to launchd).
pub(crate) fn arm_parent_death(ppid: i32, signal: c_int) {
    let watch = std::thread::Builder::new()
        .name("parent-watch".into())
        .spawn(move || {
            loop {
                // SAFETY: plain syscalls.
                unsafe {
                    if libc::getppid() != ppid {
                        libc::kill(libc::getpid(), signal);
                        return;
                    }
                }
                std::thread::sleep(Duration::from_millis(500));
            }
        });
    if let Err(e) = watch {
        eprintln!("devstack shim: cannot watch the supervisor: {e}");
    }
}

// ------------------------------------------------------------------ cgroups

/// macOS has no cgroups: a value of this type cannot exist.
pub(crate) enum Cgroup {}

impl Cgroup {
    pub(crate) fn create(dir: PathBuf) -> Result<Self> {
        Err(ProcError::Other(format!(
            "cgroup {}: cgroups exist on Linux only",
            dir.display()
        )))
    }

    pub(crate) fn at(_dir: &Path) -> Option<Self> {
        None
    }

    pub(crate) fn path(&self) -> &Path {
        match *self {}
    }

    pub(crate) fn members(&self) -> Option<Vec<i32>> {
        match *self {}
    }

    pub(crate) fn listed(&self) -> Vec<u32> {
        match *self {}
    }

    pub(crate) fn kill(&self) {
        match *self {}
    }

    pub(crate) fn remove(&self) {
        match *self {}
    }

    pub(crate) fn join(&self, _pid: u32) -> std::io::Result<()> {
        match *self {}
    }

    pub(crate) fn join_on_exec(&self, _cmd: &mut Command) -> Result<()> {
        match *self {}
    }

    pub(crate) fn usage(&self, _pids: &[u32]) -> Usage {
        match *self {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt;
    use std::time::Instant;

    #[test]
    fn the_machine_reports_memory_and_cpu_time() {
        let total = memory_total().unwrap();
        assert!(total >= 1 << 30, "{total}");
        let free = memory_available().unwrap();
        assert!(free > 0 && free <= total, "{free} of {total}");
        let (idle1, total1) = cpu_times().unwrap();
        // Busy until the ticks move: on a virtual machine (CI) the host
        // counts them in steps, 300 ms could see none.
        let t0 = Instant::now();
        let (idle2, total2) = loop {
            std::hint::black_box(t0.elapsed());
            let now = cpu_times().unwrap();
            if (now.1 > total1 && t0.elapsed() >= Duration::from_millis(300))
                || t0.elapsed() > Duration::from_secs(5)
            {
                break now;
            }
        };
        assert!(total2 > total1, "{total1} -> {total2}");
        assert!(idle2 >= idle1 && idle2 - idle1 <= total2 - total1);
    }

    #[test]
    fn a_process_group_is_listed_with_start_times_and_usage() {
        let mut child = Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap();
        let pgid = child.id() as i32;
        let me = std::process::id() as i32;
        let started = start_time(pgid).unwrap();
        assert!(started >= start_time(me).unwrap());
        assert_eq!(group(pgid), Some(vec![pgid]));
        // This process is in a group of its own choosing, not in the child's.
        assert!(!group(pgid).unwrap().contains(&me));
        let _ = child.kill();
        // Not reaped yet: a zombie, which is no member.
        let t0 = Instant::now();
        while group(pgid) != Some(Vec::new()) && t0.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(group(pgid), Some(Vec::new()));
        let _ = child.wait();
    }

    /// CPU time comes in Mach units; unscaled, 300 ms would read as 7 ms on
    /// Apple silicon. Other tests run in this process meanwhile: only the
    /// lower bound is tight.
    #[test]
    fn cpu_time_and_resident_set_are_in_microseconds_and_bytes() {
        let me = std::process::id();
        let (before, rss) = process_usage(me);
        assert!(rss >= 1 << 20, "{rss}");
        let t0 = Instant::now();
        let mut x = 0u64;
        while t0.elapsed() < Duration::from_millis(300) {
            x = std::hint::black_box(x.wrapping_mul(31).wrapping_add(7));
        }
        let (after, _) = process_usage(me);
        let spent = after - before;
        assert!(
            (150_000..5_000_000).contains(&spent),
            "300 ms of work read as {spent} µs"
        );
    }
}
