//! Is the Docker engine there, and can devstack start it? A stack with Compose
//! groups needs a running engine; without one every `docker compose` call fails
//! with a message about a pipe or socket that says little.
//!
//! What "start" means depends on how Docker came onto the machine:
//!
//! | Platform | Engine | devstack starts it with |
//! | --- | --- | --- |
//! | Windows | Docker Desktop | `Docker Desktop.exe` (no admin needed) |
//! | Linux | Docker Desktop for Linux | `systemctl --user start docker-desktop` |
//! | Linux | Docker Engine (system service) | nothing: it needs root, so devstack names `sudo systemctl start docker` |

use crate::no_window;
use serde::Serialize;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum Engine {
    /// The engine answers.
    Running { version: String },
    /// The CLI is there, the engine does not answer.
    Stopped {
        /// What devstack would run to start it, if it can (`Docker Desktop`).
        starter: Option<String>,
        /// What to tell the user when devstack cannot start it.
        hint: String,
    },
    /// No `docker` on PATH.
    Missing,
}

impl Engine {
    pub fn is_running(&self) -> bool {
        matches!(self, Engine::Running { .. })
    }
}

/// Ask the engine for its version (5 s at most: a starting Docker Desktop
/// can hold the call).
pub fn status(program: &str) -> Engine {
    let mut cmd = Command::new(program);
    cmd.args(["version", "--format", "{{.Server.Version}}"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    no_window(&mut cmd);
    let Ok(mut child) = cmd.spawn() else {
        return Engine::Missing;
    };
    let t0 = Instant::now();
    let exit = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if t0.elapsed() < Duration::from_secs(5) => {
                std::thread::sleep(Duration::from_millis(50))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    if exit.is_some_and(|s| s.success()) {
        let mut out = String::new();
        if let Some(mut so) = child.stdout.take() {
            use std::io::Read;
            let _ = so.read_to_string(&mut out);
        }
        let version = out.trim().to_string();
        if !version.is_empty() {
            return Engine::Running { version };
        }
    }
    match starter() {
        Some(s) => Engine::Stopped {
            hint: format!("start {s}"),
            starter: Some(s.to_string()),
        },
        None => Engine::Stopped {
            starter: None,
            hint: manual_hint().into(),
        },
    }
}

/// Start the engine where devstack can. Returns what it started.
pub fn start() -> Result<String, String> {
    let Some(name) = starter() else {
        return Err(format!(
            "devstack cannot start Docker here: {}",
            manual_hint()
        ));
    };
    imp::start().map_err(|e| format!("start {name}: {e}"))?;
    Ok(name.to_string())
}

/// Wait until the engine answers, up to `timeout`.
pub fn wait(program: &str, timeout: Duration) -> Result<String, String> {
    let t0 = Instant::now();
    loop {
        if let Engine::Running { version } = status(program) {
            return Ok(version);
        }
        if t0.elapsed() > timeout {
            return Err(format!(
                "Docker did not answer within {} s",
                timeout.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

fn starter() -> Option<&'static str> {
    imp::installed().then_some("Docker Desktop")
}

fn manual_hint() -> &'static str {
    if cfg!(windows) {
        "install Docker Desktop, or start your Docker engine"
    } else {
        "start the Docker service (`sudo systemctl start docker`)"
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::path::PathBuf;

    /// `Docker Desktop.exe`: where a machine-wide installation puts it,
    /// beside the `docker` on PATH (`<install>\resources\bin\docker.exe`),
    /// or where a per-user installation puts it.
    fn exe() -> Option<PathBuf> {
        let mut candidates = Vec::new();
        if let Some(base) = std::env::var_os("ProgramFiles") {
            candidates.push(PathBuf::from(base).join("Docker").join("Docker"));
        }
        if let Some(path) = std::env::var_os("PATH") {
            for dir in std::env::split_paths(&path) {
                if dir.join("docker.exe").is_file()
                    && let Some(install) = dir.parent().and_then(|r| r.parent())
                {
                    candidates.push(install.to_path_buf());
                }
            }
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            let programs = PathBuf::from(local).join("Programs");
            candidates.push(programs.join("Docker").join("Docker"));
            candidates.push(programs.join("DockerDesktop"));
        }
        candidates
            .into_iter()
            .map(|d| d.join("Docker Desktop.exe"))
            .find(|p| p.is_file())
    }

    pub fn installed() -> bool {
        exe().is_some()
    }

    pub fn start() -> std::io::Result<()> {
        let exe = exe().ok_or_else(|| std::io::Error::other("Docker Desktop is not installed"))?;
        // Docker Desktop shows its own window and tray icon; it must outlive us.
        let mut cmd = Command::new(exe);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        devstack_detach(&mut cmd)
    }

    fn devstack_detach(cmd: &mut Command) -> std::io::Result<()> {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        match cmd
            .creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB)
            .spawn()
        {
            Err(e) if e.raw_os_error() == Some(5) => cmd
                .creation_flags(CREATE_NEW_PROCESS_GROUP)
                .spawn()
                .map(|_| ()),
            other => other.map(|_| ()),
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    /// Docker Desktop for Linux runs as a systemd user unit.
    pub fn installed() -> bool {
        Command::new("systemctl")
            .args(["--user", "cat", "docker-desktop"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    pub fn start() -> std::io::Result<()> {
        let s = Command::new("systemctl")
            .args(["--user", "start", "docker-desktop"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;
        if s.success() {
            Ok(())
        } else {
            Err(std::io::Error::other(format!("systemctl answered {s}")))
        }
    }
}
