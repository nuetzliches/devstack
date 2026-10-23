//! The hub starts the tray (ADR-0060 point 2): `devstack tray` in the
//! background when the hub starts, unless it is switched off (`tray.json`
//! beside the hub's state, `DEVSTACK_NO_TRAY`) or there is no desktop. The
//! tray is its own process and outlives the hub; a second one exits.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const FILE: &str = "tray.json";

#[derive(Debug, Default, Serialize, Deserialize)]
struct Settings {
    #[serde(default)]
    off: bool,
}

fn path() -> PathBuf {
    crate::state_dir().join(FILE)
}

/// Switched off with `devstack hub tray off`.
pub fn is_off() -> bool {
    std::fs::read(path())
        .ok()
        .and_then(|b| serde_json::from_slice::<Settings>(&b).ok())
        .is_some_and(|s| s.off)
}

/// `devstack hub tray on|off`: kept per machine.
pub fn set_off(off: bool) -> Result<(), String> {
    let dir = crate::state_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let json = serde_json::to_vec_pretty(&Settings { off }).map_err(|e| e.to_string())?;
    std::fs::write(path(), json).map_err(|e| format!("{}: {e}", path().display()))
}

/// `DEVSTACK_NO_TRAY` is set and not `0` (tests, a machine nobody sits at).
pub fn env_off() -> bool {
    std::env::var_os("DEVSTACK_NO_TRAY").is_some_and(|v| !v.is_empty() && v != "0")
}

/// The lock a running tray holds, so a second one exits.
pub fn lock_path() -> PathBuf {
    crate::state_dir().join("tray.lock")
}

/// The running tray's process id, written once it holds the lock.
pub fn pid_path() -> PathBuf {
    crate::state_dir().join("tray.pid")
}

/// Asks the running tray to end (`devstack hub tray off`, `self uninstall`):
/// it looks every two seconds and removes the file when it goes.
pub fn stop_path() -> PathBuf {
    crate::state_dir().join("tray.stop")
}

/// The process id of the tray that runs, if one does.
pub fn running() -> Option<u32> {
    let pid: u32 = std::fs::read_to_string(pid_path())
        .ok()?
        .trim()
        .parse()
        .ok()?;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(lock_path())
        .ok()?;
    match lock.try_lock() {
        Err(std::fs::TryLockError::WouldBlock) => devstack_proc::pid_alive(pid).then_some(pid),
        _ => None,
    }
}

/// End the running tray, and wait up to `wait` for it to go. Whether none
/// runs now.
pub fn stop(wait: std::time::Duration) -> bool {
    if running().is_none() {
        return true;
    }
    if std::fs::write(stop_path(), b"").is_err() {
        return false;
    }
    let t0 = std::time::Instant::now();
    while t0.elapsed() < wait {
        if running().is_none() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    running().is_none()
}

/// Why the tray does not start with the hub, or `None`.
pub fn why_not() -> Option<String> {
    if env_off() {
        return Some("switched off (DEVSTACK_NO_TRAY)".into());
    }
    if is_off() {
        return Some("switched off (`devstack hub tray on` starts it)".into());
    }
    devstack_desktop::tray::unavailable()
}

/// Start `devstack tray` in the background; its output goes to `tray.out`.
pub fn spawn() -> Result<(), String> {
    let exe = crate::devstack_exe().map_err(|e| e.to_string())?;
    let dir = crate::state_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    // Appended: a second tray (it exits) must not wipe what the first said.
    let out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("tray.out"))
        .map_err(|e| e.to_string())?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("tray")
        .current_dir(&dir)
        .stdin(std::process::Stdio::null())
        .stdout(out.try_clone().map_err(|e| e.to_string())?)
        .stderr(out);
    let mut child =
        devstack_proc::spawn_detached(&mut cmd).map_err(|e| format!("start the tray: {e}"))?;
    // A second tray exits at once; a long-lived hub must not keep it as a
    // zombie.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// At the hub's start: the tray, unless it is off; a failure is logged.
pub(crate) fn start_quietly() {
    if let Some(why) = why_not() {
        devstack_daemon::diag::info("tray_not_started", serde_json::json!({ "why": why }));
        return;
    }
    if let Err(e) = spawn() {
        devstack_daemon::diag::warn("tray_start_failed", serde_json::json!({ "error": e }));
    }
}
