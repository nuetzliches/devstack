//! Starting a process that outlives the terminal it was started from: the
//! daemon and the hub. Three things tie a child to its terminal, and each one
//! has cost a bug:
//!
//! - **The console** (Windows): a child on the caller's console gets its
//!   `CTRL_CLOSE` when the window closes. The child gets a console of its own
//!   without a window (it needs one to send `CTRL_BREAK` to services, ADR-0009).
//! - **A job object** (Windows): IDE terminals (VS Code, Visual Studio, Rider)
//!   put the shell into a job that kills every member when the terminal
//!   closes. The child breaks away from it where the job allows that.
//! - **Inherited handles**: the caller's stdout may be a pipe
//!   (`devstack ui --print | …`). Inherited, the long-lived child holds it open
//!   and the reader never sees the end.
//!
//! On Unix the child starts its own session (`setsid`), which detaches it from
//! the terminal and its hangup.

use std::io;
use std::process::{Child, Command};

/// Make this process's standard handles non-inheritable. A child started
/// with redirected stdio inherits every inheritable handle, and a caller's
/// pipe handed on that way to a process that starts the hub (`devstack
/// update` runs `devstack hub start`) ends up held by the hub, so the reader
/// never sees the end. Only for a short-lived CLI that does not need them
/// inheritable afterwards; `Stdio::inherit` still works (it duplicates).
pub fn keep_std_handles() {
    imp::keep_std_handles();
}

/// Spawn `cmd` detached from the caller's terminal. Set the child's stdio
/// (null or a log file) before calling.
pub fn spawn_detached(cmd: &mut Command) -> io::Result<Child> {
    imp::spawn_detached(cmd)
}

#[cfg(windows)]
mod imp {
    use std::io;
    use std::os::windows::process::CommandExt;
    use std::process::{Child, Command};
    use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, SetHandleInformation};
    use windows_sys::Win32::System::Console::{
        GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };

    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const ERROR_ACCESS_DENIED: i32 = 5;

    pub fn keep_std_handles() {
        for which in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            // SAFETY: plain Win32 calls on this process's standard handles.
            unsafe {
                let h = GetStdHandle(which);
                if !h.is_null() {
                    SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0);
                }
            }
        }
    }

    pub fn spawn_detached(cmd: &mut Command) -> io::Result<Child> {
        // Only the standard handles are a problem: Rust opens its own handles
        // non-inheritable, but the ones this process got from its parent may
        // be inheritable, and with redirected stdio every inheritable handle
        // goes to the child.
        keep_std_handles();
        // Letting the child AllocConsole and hide the window does not work where
        // Windows Terminal is the default terminal: the console then opens a
        // terminal window that the console's window handle does not refer to.
        let base = CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP;
        match cmd.creation_flags(base | CREATE_BREAKAWAY_FROM_JOB).spawn() {
            // A job that forbids breakaway refuses the flag; then the child stays
            // in it, and closing that terminal ends it (nothing better possible).
            Err(e) if e.raw_os_error() == Some(ERROR_ACCESS_DENIED) => {
                cmd.creation_flags(base).spawn()
            }
            other => other,
        }
    }
}

#[cfg(unix)]
mod imp {
    use std::io;
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command};

    /// Nothing to do: Rust opens its descriptors close-on-exec, and a child
    /// gets only the standard ones it is given.
    pub fn keep_std_handles() {}

    pub fn spawn_detached(cmd: &mut Command) -> io::Result<Child> {
        // SAFETY: setsid is async-signal-safe; it detaches from the terminal's session.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        cmd.spawn()
    }
}
