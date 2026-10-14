//! Windows process ownership: one Job Object per service.
//!
//! The guarantees ADR-0009 relies on, and the switches to break them on
//! purpose (negative controls for the spike):
//! - `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`: closing the last handle to the job
//!   terminates every process in it. The supervisor holds the only handle, so
//!   its death (any kind) ends the tree.
//! - The job handle is not inheritable. With `inheritable_job` it is, and every
//!   child started with handle inheritance keeps the job alive.
//! - Breakaway is not allowed. With `allow_breakaway` a child may leave the job
//!   with `CREATE_BREAKAWAY_FROM_JOB`.

use anyhow::{Context, Result, bail};
use std::io::Write;
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::{fs::File, mem, ptr};
use windows_sys::Win32::Foundation::{
    CloseHandle, HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation,
};
use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, GenerateConsoleCtrlEvent};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_BREAKAWAY_OK,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
    JOBOBJECT_BASIC_PROCESS_ID_LIST, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectBasicAccountingInformation, JobObjectBasicProcessIdList,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
    TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NEW_PROCESS_GROUP, CREATE_SUSPENDED, OpenProcess, OpenThread, PROCESS_SET_QUOTA,
    PROCESS_TERMINATE, ResumeThread, THREAD_SUSPEND_RESUME,
};

pub struct Job {
    handle: HANDLE,
}

// The handle is only used from the thread that owns the Job.
unsafe impl Send for Job {}

impl Job {
    pub fn new(inheritable: bool, allow_breakaway: bool) -> Result<Self> {
        unsafe {
            let handle = CreateJobObjectW(ptr::null(), ptr::null());
            if handle.is_null() {
                bail!(
                    "CreateJobObjectW failed: {}",
                    std::io::Error::last_os_error()
                );
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
                | if allow_breakaway {
                    JOB_OBJECT_LIMIT_BREAKAWAY_OK
                } else {
                    0
                };
            if SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            ) == 0
            {
                bail!(
                    "SetInformationJobObject failed: {}",
                    std::io::Error::last_os_error()
                );
            }
            if inheritable
                && SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) == 0
            {
                bail!(
                    "SetHandleInformation failed: {}",
                    std::io::Error::last_os_error()
                );
            }
            Ok(Self { handle })
        }
    }

    pub fn assign_pid(&self, pid: u32) -> Result<()> {
        unsafe {
            let p = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
            if p.is_null() {
                bail!(
                    "OpenProcess({pid}) failed: {}",
                    std::io::Error::last_os_error()
                );
            }
            let ok = AssignProcessToJobObject(self.handle, p);
            CloseHandle(p);
            if ok == 0 {
                bail!(
                    "AssignProcessToJobObject({pid}) failed: {}",
                    std::io::Error::last_os_error()
                );
            }
            Ok(())
        }
    }

    pub fn assign_child(&self, child: &Child) -> Result<()> {
        unsafe {
            if AssignProcessToJobObject(self.handle, child.as_raw_handle() as HANDLE) == 0 {
                bail!(
                    "AssignProcessToJobObject failed: {}",
                    std::io::Error::last_os_error()
                );
            }
        }
        Ok(())
    }

    /// Every process currently in the job (not a tree walk: exact membership).
    pub fn pids(&self) -> Vec<u32> {
        unsafe {
            // Room for 1024 ids; enough for a spike.
            let mut buf = vec![0usize; 2 + 1024];
            let list = buf.as_mut_ptr() as *mut JOBOBJECT_BASIC_PROCESS_ID_LIST;
            let ok = QueryInformationJobObject(
                self.handle,
                JobObjectBasicProcessIdList,
                list as *mut _,
                (buf.len() * mem::size_of::<usize>()) as u32,
                ptr::null_mut(),
            );
            if ok == 0 {
                return vec![];
            }
            let n = (*list).NumberOfProcessIdsInList as usize;
            let ids = std::slice::from_raw_parts((*list).ProcessIdList.as_ptr(), n);
            ids.iter().map(|&id| id as u32).collect()
        }
    }

    pub fn active_processes(&self) -> u32 {
        unsafe {
            let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = mem::zeroed();
            if QueryInformationJobObject(
                self.handle,
                JobObjectBasicAccountingInformation,
                &mut info as *mut _ as *mut _,
                mem::size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                ptr::null_mut(),
            ) == 0
            {
                return 0;
            }
            info.ActiveProcesses
        }
    }

    pub fn terminate(&self) {
        unsafe {
            TerminateJobObject(self.handle, 1);
        }
    }
}

/// Give a supervisor without a console (started detached, e.g. as a background
/// daemon) a hidden one. Services in "break" mode share it, which is what
/// GenerateConsoleCtrlEvent needs; without it, every console child would also
/// open a console window of its own.
pub fn ensure_hidden_console() -> Result<bool> {
    use windows_sys::Win32::System::Console::{AllocConsole, GetConsoleWindow};
    use windows_sys::Win32::UI::WindowsAndMessaging::{SW_HIDE, ShowWindow};
    unsafe {
        if !GetConsoleWindow().is_null() {
            return Ok(false);
        }
        if AllocConsole() == 0 {
            bail!("AllocConsole failed: {}", std::io::Error::last_os_error());
        }
        let w = GetConsoleWindow();
        if !w.is_null() {
            ShowWindow(w, SW_HIDE);
        }
        Ok(true)
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

/// Resume every thread of a process created with CREATE_SUSPENDED. std's Child
/// does not expose the main thread handle, so the threads are enumerated.
fn resume_process(pid: u32) -> Result<()> {
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        if snap as isize == -1 {
            bail!("CreateToolhelp32Snapshot failed");
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
            bail!("no thread of {pid} resumed");
        }
        Ok(())
    }
}

/// A started service: its job plus whatever the stop method needs.
pub struct Service {
    pub name: String,
    pub job: Job,
    pub root_pid: u32,
    stop: StopHandle,
}

enum StopHandle {
    Break,
    Conpty {
        writer: std::sync::Arc<std::sync::Mutex<Box<dyn Write + Send>>>,
        // Keep master and child alive for the lifetime of the service.
        _master: Box<dyn portable_pty::MasterPty + Send>,
        _child: Box<dyn portable_pty::Child + Send + Sync>,
    },
}

/// Method "break": child shares our console in its own process group, created
/// suspended so it cannot spawn anything before it is in the job.
pub fn spawn_break(name: &str, cmdline: &str, log: File, job: Job) -> Result<Service> {
    let err = log.try_clone()?;
    let child = Command::new("cmd")
        .args(["/d", "/s", "/c"])
        // `cmd /s /c` strips the first and the last quote of the rest of the line;
        // wrapping the whole line keeps a command that starts with a quoted path intact.
        .raw_arg(format!("\"{cmdline}\""))
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(err)
        .creation_flags(CREATE_SUSPENDED | CREATE_NEW_PROCESS_GROUP)
        .spawn()
        .with_context(|| format!("spawn {name}"))?;
    job.assign_child(&child)?;
    resume_process(child.id())?;
    let root_pid = child.id();
    // The Child handle is dropped; the job owns the lifetime from here.
    drop(child);
    Ok(Service {
        name: name.into(),
        job,
        root_pid,
        stop: StopHandle::Break,
    })
}

/// Method "conpty": every service gets its own pseudo console. Ctrl+C is
/// written into that console's input. The process is assigned to the job right
/// after creation (portable-pty cannot create it suspended), which leaves a
/// short window in which a very fast child could spawn outside the job.
pub fn spawn_conpty(name: &str, cmdline: &str, mut log: File, job: Job) -> Result<Service> {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};
    let pty = native_pty_system().openpty(PtySize {
        rows: 50,
        cols: 200,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    let mut cmd = CommandBuilder::new("cmd");
    cmd.args(["/d", "/s", "/c", cmdline]);
    cmd.cwd(std::env::current_dir()?);
    let child = pty
        .slave
        .spawn_command(cmd)
        .with_context(|| format!("spawn {name}"))?;
    let root_pid = child.process_id().context("no pid from pty child")?;
    job.assign_pid(root_pid)?;
    let mut reader = pty.master.try_clone_reader()?;
    let writer = std::sync::Arc::new(std::sync::Mutex::new(pty.master.take_writer()?));
    // ConPTY asks the "terminal" for the cursor position (ESC[6n) when it starts
    // and blocks the console until it gets an answer. There is no terminal, so
    // the supervisor answers itself.
    let answer = writer.clone();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = [0u8; 8192];
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            if buf[..n].windows(4).any(|w| w == b"\x1b[6n") {
                if let Ok(mut w) = answer.lock() {
                    let _ = w.write_all(b"\x1b[1;1R");
                    let _ = w.flush();
                }
            }
            let _ = log.write_all(&buf[..n]);
        }
    });
    Ok(Service {
        name: name.into(),
        job,
        root_pid,
        stop: StopHandle::Conpty {
            writer,
            _master: pty.master,
            _child: child,
        },
    })
}

impl Service {
    pub fn pids(&mut self) -> Vec<u32> {
        self.job.pids()
    }

    pub fn active_processes(&mut self) -> u32 {
        self.job.active_processes()
    }

    pub fn terminate(&mut self) {
        self.job.terminate();
    }

    /// Ask the service to stop the way a user in its terminal would.
    pub fn request_stop(&mut self) -> Result<()> {
        match &mut self.stop {
            StopHandle::Break => unsafe {
                if GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, self.root_pid) == 0 {
                    bail!(
                        "GenerateConsoleCtrlEvent failed: {}",
                        std::io::Error::last_os_error()
                    );
                }
                Ok(())
            },
            StopHandle::Conpty { writer, .. } => {
                let mut w = writer
                    .lock()
                    .map_err(|_| anyhow::anyhow!("writer poisoned"))?;
                // The console switches ConPTY to win32-input-mode (ESC[?9001h), so
                // a key press is expected as ESC[Vk;Sc;Uc;Kd;Cs;Rc_. Ctrl+C:
                // Vk 0x43, scan 0x2E, char 3, LEFT_CTRL_PRESSED (8), down then up.
                // The plain ETX byte follows for consoles not in that mode.
                w.write_all(b"\x1b[67;46;3;1;8;1_\x1b[67;46;3;0;8;1_\x03")?;
                w.flush()?;
                Ok(())
            }
        }
    }
}
