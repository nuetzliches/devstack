//! A short call to another program (the Docker CLI) whose whole process tree
//! can be ended: `docker compose` runs Compose's plugin as a child of the
//! CLI, and ending the CLI alone leaves the plugin running, with the pipes
//! the caller reads still open.
//!
//! - **Windows:** the program starts suspended, joins a Job Object with
//!   kill-on-close, then runs; without a console window. Ending the call, or
//!   dropping it, ends every process of the job.
//! - **Unix:** the program leads a process group of its own; ending the call
//!   sends `SIGKILL` to the group.

use std::process::{Child, Command};

/// A started call; see the module documentation.
pub struct Call {
    child: Child,
    #[cfg(windows)]
    job: Option<crate::windows::Job>,
    #[cfg(windows)]
    created: Option<u64>,
}

impl Call {
    /// Start `cmd` as a call of its own tree. On Windows it runs without a
    /// console window (its creation flags are set here).
    pub fn spawn(cmd: &mut Command) -> std::io::Result<Self> {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            use windows_sys::Win32::System::Threading::CREATE_SUSPENDED;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED);
            let mut child = cmd.spawn()?;
            // Without a job (one that cannot be created, a process that
            // cannot join it) the call still runs; ending it then walks its
            // descendants instead.
            let job = crate::windows::Job::new()
                .ok()
                .filter(|job| job.assign(&child));
            let created = crate::windows::creation_time(child.id());
            if let Err(e) = crate::windows::resume_process(child.id()) {
                let _ = child.kill();
                let _ = child.wait();
                return Err(std::io::Error::other(e.to_string()));
            }
            Ok(Self {
                child,
                job,
                created,
            })
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
            Ok(Self {
                child: cmd.spawn()?,
            })
        }
    }

    /// The program started (its pipes, its exit status).
    pub fn child(&mut self) -> &mut Child {
        &mut self.child
    }

    /// End the program and every process it started, and reap it.
    pub fn kill(&mut self) {
        #[cfg(windows)]
        match &self.job {
            Some(job) => job.terminate(),
            None => crate::windows::kill_tree(self.child.id(), self.created),
        }
        #[cfg(unix)]
        {
            // SAFETY: plain syscall; the group is the child's own (it leads it).
            unsafe {
                libc::kill(-(self.child.id() as i32), libc::SIGKILL);
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::time::{Duration, Instant};

    /// A grandchild that holds the call's stdout ends with the call: the
    /// pipe reaches end of file at once instead of when the grandchild ends.
    #[test]
    fn ending_a_call_ends_what_it_started() {
        let mut cmd = if cfg!(windows) {
            let mut c = Command::new("cmd");
            c.args([
                "/d",
                "/c",
                "start /b ping -n 60 127.0.0.1 & ping -n 60 127.0.0.1",
            ]);
            c
        } else {
            let mut c = Command::new("sh");
            c.args(["-c", "sleep 60 & sleep 60"]);
            c
        };
        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .stdin(std::process::Stdio::null());
        let mut call = Call::spawn(&mut cmd).unwrap();
        let mut out = call.child().stdout.take().unwrap();
        let reader = std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = out.read_to_end(&mut buf);
        });
        std::thread::sleep(Duration::from_millis(500));
        let t0 = Instant::now();
        call.kill();
        while !reader.is_finished() && t0.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            reader.is_finished(),
            "the grandchild still holds the pipe after the call was ended"
        );
    }
}
