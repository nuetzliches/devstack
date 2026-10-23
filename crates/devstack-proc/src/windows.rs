//! Windows: one Job Object per tree (spike (a), ADR-0009).

use crate::{Options, Record, Result, Spec, Usage, os};
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::{mem, ptr};
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_MORE_DATA, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Console::{
    AllocConsole, CTRL_BREAK_EVENT, GenerateConsoleCtrlEvent, GetConsoleWindow,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_BASIC_PROCESS_ID_LIST,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicAccountingInformation,
    JobObjectBasicProcessIdList, JobObjectExtendedLimitInformation, QueryInformationJobObject,
    SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NEW_PROCESS_GROUP, CREATE_SUSPENDED, OpenThread, ResumeThread, THREAD_SUSPEND_RESUME,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{SW_HIDE, ShowWindow};

/// A Job Object with kill-on-close. The handle is created non-inheritable and
/// breakaway stays forbidden: the two conditions spike (a) showed the guarantee
/// depends on.
pub(crate) struct Job(HANDLE);

/// The most process ids one query asks room for.
const MAX_JOB_PIDS: usize = 1 << 20;

// The handle is only used through &self/&mut self of the owning Tree.
unsafe impl Send for Job {}
// Kernel handles may be used from several threads at once; the calls made on
// it (query, assign, terminate) are thread-safe Win32 functions.
unsafe impl Sync for Job {}

impl Job {
    pub(crate) fn new() -> Result<Self> {
        // SAFETY: plain Win32 calls with valid arguments; the handle is checked.
        unsafe {
            let h = CreateJobObjectW(ptr::null(), ptr::null());
            if h.is_null() {
                return Err(os("CreateJobObjectW"));
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                h,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            ) == 0
            {
                let e = os("SetInformationJobObject");
                CloseHandle(h);
                return Err(e);
            }
            Ok(Job(h))
        }
    }

    /// Every process in the job, or `None` when the query failed: that is
    /// not an empty job, and a stop must not take it for one (ADR-0009).
    pub(crate) fn query_pids(&self) -> Option<Vec<u32>> {
        self.query_pids_from(1024)
    }

    /// [`Self::query_pids`] with room for `room` ids at first; grows when
    /// the job holds more.
    fn query_pids_from(&self, mut room: usize) -> Option<Vec<u32>> {
        loop {
            // JOBOBJECT_BASIC_PROCESS_ID_LIST is two counts and the ids.
            let mut buf = vec![0usize; 2 + room];
            // SAFETY: buffer is large enough and correctly aligned for the struct.
            unsafe {
                let list = buf.as_mut_ptr() as *mut JOBOBJECT_BASIC_PROCESS_ID_LIST;
                let ok = QueryInformationJobObject(
                    self.0,
                    JobObjectBasicProcessIdList,
                    list as *mut _,
                    (buf.len() * mem::size_of::<usize>()) as u32,
                    ptr::null_mut(),
                ) != 0;
                let assigned = (*list).NumberOfAssignedProcesses as usize;
                let n = (*list).NumberOfProcessIdsInList as usize;
                let more = if ok {
                    n < assigned
                } else {
                    std::io::Error::last_os_error().raw_os_error() == Some(ERROR_MORE_DATA as i32)
                };
                if more && room < MAX_JOB_PIDS {
                    room = (assigned.max(room) * 2).min(MAX_JOB_PIDS);
                    continue;
                }
                if !ok {
                    return None;
                }
                // The ids run past the struct's one-element array: read them
                // through a pointer derived from `list` (the whole buffer),
                // not from a reference to that array.
                let ids = (&raw const (*list).ProcessIdList).cast::<usize>();
                return Some(
                    std::slice::from_raw_parts(ids, n.min(room))
                        .iter()
                        .map(|&p| p as u32)
                        .collect(),
                );
            }
        }
    }

    pub(crate) fn pids(&self) -> Vec<u32> {
        self.query_pids().unwrap_or_default()
    }

    /// Job accounting for CPU time (it includes exited processes), the summed
    /// working sets of the live ones for memory.
    pub(crate) fn usage(&self) -> Usage {
        // SAFETY: valid job handle, correctly sized out struct.
        let cpu_us = unsafe {
            let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = mem::zeroed();
            if QueryInformationJobObject(
                self.0,
                JobObjectBasicAccountingInformation,
                &mut info as *mut _ as *mut _,
                mem::size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                ptr::null_mut(),
            ) == 0
            {
                0
            } else {
                // 100 ns units.
                (info.TotalUserTime as u64 + info.TotalKernelTime as u64) / 10
            }
        };
        let pids = self.pids();
        Usage {
            cpu_us,
            memory: pids.iter().map(|&p| working_set(p)).sum(),
            processes: pids.len() as u32,
        }
    }

    /// Put a process (created suspended, so it has started nothing yet)
    /// into the job.
    pub(crate) fn assign(&self, child: &Child) -> bool {
        // SAFETY: valid job and process handles.
        unsafe { AssignProcessToJobObject(self.0, child.as_raw_handle() as HANDLE) != 0 }
    }

    pub(crate) fn terminate(&self) {
        // SAFETY: valid job handle.
        unsafe {
            TerminateJobObject(self.0, 1);
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: valid job handle, closed once. Closing it kills the tree.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

pub(crate) struct Tree {
    job: Job,
    child: Child,
    exit: Option<i32>,
}

/// Resume every thread of a process created suspended (std's Child does not
/// expose the main thread handle).
pub(crate) fn resume_process(pid: u32) -> Result<()> {
    // SAFETY: snapshot and thread handles are checked and closed.
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        if snap == INVALID_HANDLE_VALUE {
            return Err(os("CreateToolhelp32Snapshot"));
        }
        let mut te: THREADENTRY32 = mem::zeroed();
        te.dwSize = mem::size_of::<THREADENTRY32>() as u32;
        let mut resumed = 0;
        if Thread32First(snap, &mut te) != 0 {
            loop {
                if te.th32OwnerProcessID == pid {
                    let t = OpenThread(THREAD_SUSPEND_RESUME, 0, te.th32ThreadID);
                    if !t.is_null() {
                        ResumeThread(t);
                        CloseHandle(t);
                        resumed += 1;
                    }
                }
                if Thread32Next(snap, &mut te) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snap);
        if resumed == 0 {
            return Err(crate::ProcError::Other(format!(
                "no thread of process {pid} could be resumed"
            )));
        }
        Ok(())
    }
}

/// Start a command line in `job`: created suspended, so it cannot start
/// anything before it is in the job; in its own process group, so CTRL_BREAK
/// reaches this tree only.
pub(crate) fn start_in_job(job: &Job, spec: Spec) -> Result<Child> {
    let err = spec.log.try_clone().map_err(|e| crate::ProcError::Os {
        what: "clone log handle".into(),
        source: e,
    })?;
    // `cmd /s /c` strips the first and last quote of the line: wrap the line once more.
    let mut child = Command::new("cmd")
        .args(["/d", "/s", "/c"])
        .raw_arg(format!("\"{}\"", spec.command))
        .current_dir(&spec.cwd)
        .envs(&spec.env)
        .stdin(Stdio::null())
        .stdout(spec.log)
        .stderr(err)
        .creation_flags(CREATE_SUSPENDED | CREATE_NEW_PROCESS_GROUP)
        .spawn()
        .map_err(|e| crate::ProcError::Os {
            what: format!("start {}", spec.name),
            source: e,
        })?;
    // SAFETY: valid job and process handles.
    let assigned = unsafe { AssignProcessToJobObject(job.0, child.as_raw_handle() as HANDLE) };
    if assigned == 0 {
        let e = os("AssignProcessToJobObject");
        let _ = child.kill();
        return Err(e);
    }
    if let Err(e) = resume_process(child.id()) {
        // Still suspended in the job: in the shared build job it would stay
        // until the daemon ends.
        let _ = child.kill();
        let _ = child.wait();
        return Err(e);
    }
    Ok(child)
}

/// Windows needs no shim and no cgroup: `Options` is not used.
pub(crate) fn spawn(spec: Spec, _opts: &Options) -> Result<(Tree, Record)> {
    let job = Job::new()?;
    let name = spec.name.clone();
    let child = start_in_job(&job, spec)?;
    let record = Record {
        name,
        pgid: child.id() as i64,
        leader_start: 0,
        cgroup: None,
    };
    Ok((
        Tree {
            job,
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
        let _ = self.try_exit_code();
        self.job.pids()
    }

    /// Whether any process of the tree lives; `None` when the job cannot tell.
    pub(crate) fn alive(&mut self) -> Option<bool> {
        let _ = self.try_exit_code();
        self.job.query_pids().map(|p| !p.is_empty())
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
        // SAFETY: plain Win32 call; the group id is the root's process id.
        if unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, self.child.id()) } == 0 {
            return Err(os(
                "GenerateConsoleCtrlEvent (does the supervisor have a console? see ensure_console)",
            ));
        }
        Ok(())
    }

    pub(crate) fn usage(&mut self) -> Usage {
        let _ = self.try_exit_code();
        self.job.usage()
    }

    pub(crate) fn kill(&mut self) {
        self.job.terminate();
        let _ = self.child.wait();
    }
}

fn working_set(pid: u32) -> u64 {
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
    };
    // SAFETY: plain Win32 calls; handle checked and closed.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ, 0, pid);
        if h.is_null() {
            return 0;
        }
        let mut c: PROCESS_MEMORY_COUNTERS = mem::zeroed();
        c.cb = mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        let ok = K32GetProcessMemoryInfo(h, &mut c, c.cb);
        CloseHandle(h);
        if ok == 0 { 0 } else { c.WorkingSetSize as u64 }
    }
}

pub(crate) fn ensure_console() -> Result<bool> {
    // SAFETY: plain Win32 calls.
    unsafe {
        if !GetConsoleWindow().is_null() {
            return Ok(false);
        }
        if AllocConsole() == 0 {
            // A process attached to a console without a window (a pseudo console,
            // or one created with CREATE_NO_WINDOW) has no window handle but
            // still a console; AllocConsole then fails with ACCESS_DENIED.
            let err = std::io::Error::last_os_error();
            if err.raw_os_error()
                == Some(windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED as i32)
            {
                return Ok(false);
            }
            return Err(crate::ProcError::Os {
                what: "AllocConsole".into(),
                source: err,
            });
        }
        let w = GetConsoleWindow();
        if !w.is_null() {
            ShowWindow(w, SW_HIDE);
        }
        Ok(true)
    }
}

/// Creation time of a process (100 ns ticks), or None if it cannot be opened.
pub(crate) fn creation_time(pid: u32) -> Option<u64> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    // SAFETY: handle checked and closed; out-parameters are valid.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let mut c: FILETIME = mem::zeroed();
        let (mut e, mut k, mut u): (FILETIME, FILETIME, FILETIME) =
            (mem::zeroed(), mem::zeroed(), mem::zeroed());
        let ok = GetProcessTimes(h, &mut c, &mut e, &mut k, &mut u);
        CloseHandle(h);
        (ok != 0).then_some(((c.dwHighDateTime as u64) << 32) | c.dwLowDateTime as u64)
    }
}

/// End `root` and every descendant (ADR-0020: cancelling one build in the
/// shared build job). A child only counts if it was created after its parent,
/// so a process that reuses a dead parent's id is never mistaken for a child.
/// Where a parent has exited, the nearest ancestor still known stands in for
/// it, down to `root_created` (when the root was started): a process
/// older than that is never part of the tree.
pub(crate) fn kill_tree(root: u32, root_created: Option<u64>) {
    use std::collections::HashMap;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    // SAFETY: snapshot handle checked and closed.
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return;
        }
        let mut pe: PROCESSENTRY32W = mem::zeroed();
        pe.dwSize = mem::size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(snap, &mut pe) != 0 {
            loop {
                children
                    .entry(pe.th32ParentProcessID)
                    .or_default()
                    .push(pe.th32ProcessID);
                if Process32NextW(snap, &mut pe) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snap);
    }
    // (pid, the creation time a child of it must not precede)
    let mut tree: Vec<(u32, Option<u64>)> = vec![(root, creation_time(root).or(root_created))];
    let mut i = 0;
    while i < tree.len() {
        let (parent, bound) = tree[i];
        for &c in children.get(&parent).map(Vec::as_slice).unwrap_or_default() {
            let created = creation_time(c);
            let newer = matches!((bound, created), (Some(b), Some(t)) if t >= b);
            if newer && !tree.iter().any(|(p, _)| *p == c) {
                tree.push((c, created));
            }
        }
        i += 1;
    }
    for (pid, _) in tree {
        // SAFETY: handle checked and closed.
        unsafe {
            let h = OpenProcess(PROCESS_TERMINATE, 0, pid);
            if !h.is_null() {
                TerminateProcess(h, 1);
                CloseHandle(h);
            }
        }
    }
}

pub(crate) fn long_path(path: &std::path::Path) -> Option<std::path::PathBuf> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows_sys::Win32::Storage::FileSystem::GetLongPathNameW;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    let mut buf = vec![0u16; 512];
    loop {
        // SAFETY: `wide` is NUL-terminated, `buf` is writable for its length.
        let n =
            unsafe { GetLongPathNameW(wide.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) } as usize;
        if n == 0 {
            return None;
        }
        if n < buf.len() {
            buf.truncate(n);
            return Some(std::ffi::OsString::from_wide(&buf).into());
        }
        buf.resize(n, 0);
    }
}

/// Nothing to clean up: a tree dies with the job handle of its owner.
pub(crate) fn cleanup(records: &[Record]) -> Vec<(String, usize)> {
    records.iter().map(|r| (r.name.clone(), 0)).collect()
}

pub(crate) fn pid_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::STILL_ACTIVE;
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    // SAFETY: plain Win32 calls on a handle opened and closed here.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return false;
        }
        let mut code = 0u32;
        let ok = GetExitCodeProcess(h, &mut code) != 0;
        CloseHandle(h);
        ok && code == STILL_ACTIVE as u32
    }
}

pub(crate) fn disk_free(path: &std::path::Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut free: u64 = 0;
    // SAFETY: `wide` is a NUL-terminated UTF-16 path; the other out
    // parameters may be null.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    } != 0;
    ok.then_some(free)
}

/// Windows has no limit of open files to raise.
pub(crate) fn raise_open_files() -> Option<u64> {
    None
}

/// A listener on `port` in the TCP tables (IPv4, then IPv6) whose address is
/// the wildcard or loopback. A listener on another address of the machine
/// (`192.168.1.5`) does not hold the port for a service on loopback.
pub(crate) fn tcp_listening(port: u16) -> bool {
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        MIB_TCP_STATE_LISTEN, MIB_TCP6ROW_OWNER_PID, MIB_TCPROW_OWNER_PID,
    };
    // `AF_INET`, `AF_INET6` (WinSock).
    const AF_INET: u32 = 2;
    const AF_INET6: u32 = 23;
    // Ports are in network byte order in the low 16 bits.
    let port_of = |p: u32| u16::from_be(p as u16);
    let listen = MIB_TCP_STATE_LISTEN as u32;
    let v4 = listeners::<MIB_TCPROW_OWNER_PID>(AF_INET).is_some_and(|rows| {
        rows.iter().any(|r| {
            // The address in network byte order: its bytes as they are.
            let a = r.dwLocalAddr.to_ne_bytes();
            r.dwState == listen && port_of(r.dwLocalPort) == port && (a == [0; 4] || a[0] == 127)
        })
    });
    v4 || listeners::<MIB_TCP6ROW_OWNER_PID>(AF_INET6).is_some_and(|rows| {
        rows.iter().any(|r| {
            let a = std::net::Ipv6Addr::from(r.ucLocalAddr);
            r.dwState == listen
                && port_of(r.dwLocalPort) == port
                && (a.is_unspecified()
                    || a.is_loopback()
                    || a.to_ipv4_mapped().is_some_and(|v| v.is_loopback()))
        })
    })
}

/// The rows of `GetExtendedTcpTable(TCP_TABLE_OWNER_PID_LISTENER)` for one
/// address family; `Row` is that family's `MIB_TCP*ROW_OWNER_PID`. The table
/// is a row count followed by the rows; it is read into `u32`s, which aligns
/// both row types.
fn listeners<Row: Copy>(family: u32) -> Option<Vec<Row>> {
    use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, NO_ERROR};
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, TCP_TABLE_OWNER_PID_LISTENER,
    };
    let mut size: u32 = 0;
    let mut buf: Vec<u32> = Vec::new();
    // The table can grow between asking its size and reading it.
    for _ in 0..4 {
        // SAFETY: `buf` holds at least `size` bytes, as the call is told.
        let r = unsafe {
            GetExtendedTcpTable(
                buf.as_mut_ptr().cast(),
                &mut size,
                0,
                family,
                TCP_TABLE_OWNER_PID_LISTENER,
                0,
            )
        };
        match r {
            NO_ERROR => {
                let n = *buf.first()? as usize;
                let bytes = buf.len() * 4;
                if 4 + n * mem::size_of::<Row>() > bytes || mem::align_of::<Row>() > 4 {
                    return None;
                }
                // SAFETY: `n` rows of `Row` follow the count, within `buf`
                // (checked above), at an offset aligned for `Row`.
                let rows = unsafe { std::slice::from_raw_parts(buf.as_ptr().add(1).cast(), n) };
                return Some(rows.to_vec());
            }
            ERROR_INSUFFICIENT_BUFFER => buf = vec![0; (size as usize).div_ceil(4) + 1],
            _ => return None,
        }
    }
    None
}

fn memory_status() -> Option<windows_sys::Win32::System::SystemInformation::MEMORYSTATUSEX> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut m: MEMORYSTATUSEX = unsafe { mem::zeroed() };
    m.dwLength = mem::size_of::<MEMORYSTATUSEX>() as u32;
    // SAFETY: `m` is a properly sized MEMORYSTATUSEX with dwLength set.
    let ok = unsafe { GlobalMemoryStatusEx(&mut m) } != 0;
    ok.then_some(m)
}

pub(crate) fn memory_total() -> Option<u64> {
    memory_status().map(|m| m.ullTotalPhys)
}

pub(crate) fn memory_available() -> Option<u64> {
    memory_status().map(|m| m.ullAvailPhys)
}

/// `(idle, kernel + user)` in 100 ns units.
pub(crate) fn cpu_times() -> Option<(u64, u64)> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::GetSystemTimes;
    let (mut idle, mut kernel, mut user): (FILETIME, FILETIME, FILETIME) = unsafe { mem::zeroed() };
    // SAFETY: three valid FILETIME out-pointers.
    let ok = unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) } != 0;
    let t = |f: FILETIME| (u64::from(f.dwHighDateTime) << 32) | u64::from(f.dwLowDateTime);
    // Kernel time includes the idle time.
    ok.then(|| (t(idle), t(kernel) + t(user)))
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_job_with_more_processes_than_room_lists_them_all() {
        let job = super::Job::new().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let spec = crate::Spec {
            name: "many".into(),
            // Three processes at least: cmd, and two pings started from it.
            command: "start /b ping -n 30 127.0.0.1 >nul & ping -n 30 127.0.0.1 >nul".into(),
            cwd: dir.path().into(),
            env: Default::default(),
            log: std::fs::File::create(dir.path().join("log")).unwrap(),
        };
        let mut child = super::start_in_job(&job, spec).unwrap();
        let t0 = std::time::Instant::now();
        let mut all = Vec::new();
        while t0.elapsed() < std::time::Duration::from_secs(10) {
            all = job.query_pids().unwrap();
            if all.len() >= 3 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(all.len() >= 3, "{all:?}");
        let mut small = job.query_pids_from(1).unwrap();
        small.sort();
        all.sort();
        assert!(small.len() >= all.len().min(3), "{small:?} vs {all:?}");
        job.terminate();
        let _ = child.wait();
        assert_eq!(job.query_pids(), Some(Vec::new()));
    }

    #[test]
    fn a_short_path_becomes_the_long_one() {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        use windows_sys::Win32::Storage::FileSystem::GetShortPathNameW;
        let dir = tempfile::tempdir().unwrap();
        let long = dir.path().join("a rather long directory name");
        std::fs::create_dir(&long).unwrap();
        let wide: Vec<u16> = long.as_os_str().encode_wide().chain([0]).collect();
        let mut buf = vec![0u16; 512];
        // SAFETY: `wide` is NUL-terminated, `buf` is writable for its length.
        let n = unsafe { GetShortPathNameW(wide.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) }
            as usize;
        buf.truncate(n);
        let short = std::path::PathBuf::from(std::ffi::OsString::from_wide(&buf));
        if n == 0 || short.file_name() == long.file_name() {
            eprintln!("skipped: the volume keeps no 8.3 names");
            return;
        }
        let back = super::long_path(&short).unwrap();
        assert_eq!(
            back.file_name(),
            long.file_name(),
            "{} -> {}",
            short.display(),
            back.display()
        );
    }

    #[test]
    fn a_listener_on_the_wildcard_or_loopback_is_in_the_table() {
        use std::net::TcpListener;
        // A listener on every address makes the firewall ask the person at
        // the machine, for each new build of this test binary: only in CI.
        let wildcard = std::env::var_os("CI").is_some();
        for addr in ["0.0.0.0", "127.0.0.1", "::", "::1"] {
            if !wildcard && matches!(addr, "0.0.0.0" | "::") {
                continue;
            }
            let Ok(l) = TcpListener::bind((addr, 0)) else {
                continue; // no IPv6 on this host
            };
            let port = l.local_addr().unwrap().port();
            assert!(super::tcp_listening(port), "{addr}:{port}");
            drop(l);
            assert!(!super::tcp_listening(port), "{addr}:{port} after close");
        }
        // Cheap enough for a scan of a range of ports.
        let t0 = std::time::Instant::now();
        for p in 0..100 {
            super::tcp_listening(40000 + p);
        }
        eprintln!("100 reads of the table: {:?}", t0.elapsed());
    }
}
