//! Is the Docker engine there, and can devstack start it? A stack with Compose
//! groups needs a running engine; without one every `docker compose` call fails
//! with a message about a pipe or socket that says little.
//!
//! What "start" means depends on how Docker came onto the machine, and is the
//! desktop's (`devstack_desktop::docker_starter`):
//!
//! | Platform | Engine | devstack starts it with |
//! | --- | --- | --- |
//! | Windows | Docker Desktop | `Docker Desktop.exe` (no admin needed) |
//! | Linux | Docker Desktop for Linux | `systemctl --user start docker-desktop` |
//! | Linux | Docker Engine (system service) | nothing: it needs root, so devstack names `sudo systemctl start docker` |
//! | macOS | Docker Desktop, else OrbStack | `open -g -a Docker` / `open -g -a OrbStack` |
//! | macOS | Colima | nothing: devstack names `colima start` |

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
    look(program).0
}

/// [`status`], and why it is not `Running`: `docker` could not be run (not
/// found is `Missing`, anything else counts as an engine that does not
/// answer), it gave no answer within 5 s, or it answered without a version.
pub fn look(program: &str) -> (Engine, Option<String>) {
    let mut cmd = Command::new(program);
    cmd.args(["version", "--format", "{{.Server.Version}}"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    no_window(&mut cmd);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Engine::Missing, None),
        Err(e) => {
            return (
                stopped(),
                Some(format!("`{program}` could not be run: {e}")),
            );
        }
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
    let why = match exit {
        None => "no answer within 5 s".to_string(),
        Some(s) if !s.success() => format!("`{program} version` ended with {s}"),
        Some(_) => {
            let mut out = String::new();
            if let Some(mut so) = child.stdout.take() {
                use std::io::Read;
                let _ = so.read_to_string(&mut out);
            }
            let version = out.trim().to_string();
            if !version.is_empty() {
                return (Engine::Running { version }, None);
            }
            "the engine named no version".to_string()
        }
    };
    (stopped(), Some(why))
}

/// The CLI is there, the engine does not answer: what can start it.
fn stopped() -> Engine {
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
    devstack_desktop::start_docker().map_err(|e| format!("start {name}: {e}"))?;
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
    devstack_desktop::docker_starter()
}

fn manual_hint() -> &'static str {
    devstack_desktop::docker_hint()
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_a_docker_that_is_not_there_is_missing() {
        assert_eq!(
            super::look("devstack-no-such-docker-cli"),
            (super::Engine::Missing, None)
        );
        // A program that runs and names no version: an engine that does
        // not answer, with why.
        let (engine, why) = super::look(if cfg!(windows) { "whoami" } else { "true" });
        assert!(
            matches!(engine, super::Engine::Stopped { .. }),
            "{engine:?}"
        );
        assert!(why.is_some());
    }
}
