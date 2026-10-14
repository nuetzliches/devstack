//! Windows: one Job Object per tree (spike (a), ADR-0009).

use crate::{Record, Result, Spec, os};
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::{mem, ptr};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Console::{
    AllocConsole, CTRL_BREAK_EVENT, GenerateConsoleCtrlEvent, GetConsoleWindow,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_BASIC_PROCESS_ID_LIST, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
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
struct Job(HANDLE);

// The handle is only used through &self/&mut self of the owning Tree.
unsafe impl Send for Job {}

impl Job {
    fn new() -> Result<Self> {
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

    fn pids(&self) -> Vec<u32> {
        // Room for 4096 ids; JOBOBJECT_BASIC_PROCESS_ID_LIST is two counts and the ids.
        let mut buf = vec![0usize; 2 + 4096];
        // SAFETY: buffer is large enough and correctly aligned for the struct.
        unsafe {
            let list = buf.as_mut_ptr() as *mut JOBOBJECT_BASIC_PROCESS_ID_LIST;
            if QueryInformationJobObject(
                self.0,
                JobObjectBasicProcessIdList,
                list as *mut _,
                (buf.len() * mem::size_of::<usize>()) as u32,
                ptr::null_mut(),
            ) == 0
            {
                return Vec::new();
            }
            let n = (*list).NumberOfProcessIdsInList as usize;
            std::slice::from_raw_parts((*list).ProcessIdList.as_ptr(), n)
                .iter()
                .map(|&p| p as u32)
                .collect()
        }
    }

    fn terminate(&self) {
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
fn resume_process(pid: u32) -> Result<()> {
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

pub(crate) fn spawn(spec: Spec) -> Result<(Tree, Record)> {
    let job = Job::new()?;
    let err = spec.log.try_clone().map_err(|e| crate::ProcError::Os {
        what: "clone log handle".into(),
        source: e,
    })?;
    // Created suspended, so it cannot start anything before it is in the job;
    // in its own process group, so CTRL_BREAK reaches this tree only.
    // `cmd /s /c` strips the first and last quote of the line: wrap the line once more.
    let child = Command::new("cmd")
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
        let mut child = child;
        let _ = child.kill();
        return Err(e);
    }
    resume_process(child.id())?;
    let record = Record {
        name: spec.name,
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

    pub(crate) fn kill(&mut self) {
        self.job.terminate();
        let _ = self.child.wait();
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
