//! macOS: what the POSIX backend needs from the OS, and the machine's memory
//! and CPU time. Not implemented yet: every query answers that it cannot
//! tell, as the Linux code did when it found no `/proc` here.

use crate::{ProcError, Result, Usage};
use std::path::{Path, PathBuf};
use std::process::Command;

// ------------------------------------------------------------------ machine

pub(crate) fn memory_total() -> Option<u64> {
    None
}

pub(crate) fn memory_available() -> Option<u64> {
    None
}

pub(crate) fn cpu_times() -> Option<(u64, u64)> {
    None
}

// ---------------------------------------------------------------- processes

pub(crate) fn start_time(_pid: i32) -> Option<u64> {
    None
}

pub(crate) fn group(_pgid: i32) -> Option<Vec<i32>> {
    None
}

pub(crate) fn process_usage(_pid: u32) -> (u64, u64) {
    (0, 0)
}

pub(crate) fn arm_parent_death(_ppid: i32, _signal: libc::c_int) {}

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
