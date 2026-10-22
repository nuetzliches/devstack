//! macOS: what the `cfg(not(windows))` branches did there before this crate
//! existed, which is Linux's (XDG files, `xdg-open`, `systemctl --user`) or
//! nothing at all. Wrong for a Mac, and kept as it was until the macOS
//! desktop replaces it (ADR-0054 point 3).

use crate::{Local, Platform};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub(crate) const PLATFORM: Platform = Platform::Linux;

pub fn no_window(_cmd: &mut Command) {}

pub struct Front;

impl Front {
    pub fn take() -> Option<Self> {
        None
    }
}

// ------------------------------------------- autostart and the devstack:// link

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

fn data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/share"))
}

fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"))
}

fn link_file() -> PathBuf {
    data_dir().join("applications/devstack-url.desktop")
}

fn autostart_file() -> PathBuf {
    config_dir().join("autostart/devstack-hub.desktop")
}

fn write(path: &Path, text: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn install_link(exe: &Path) -> Result<(), String> {
    write(
        &link_file(),
        &format!(
            "[Desktop Entry]\nType=Application\nName=devstack\nExec=\"{}\" open-url %u\nNoDisplay=true\nTerminal=false\nMimeType=x-scheme-handler/devstack;\n",
            exe.display()
        ),
    )?;
    let _ = Command::new("xdg-mime")
        .args([
            "default",
            "devstack-url.desktop",
            "x-scheme-handler/devstack",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    Ok(())
}

pub fn remove_link() -> Result<(), String> {
    let _ = std::fs::remove_file(link_file());
    Ok(())
}

pub fn has_link() -> bool {
    link_file().is_file()
}

pub fn install_autostart(exe: &Path) -> Result<(), String> {
    write(
        &autostart_file(),
        &format!(
            "[Desktop Entry]\nType=Application\nName=devstack hub\nComment=Serves the devstack dashboard\nExec=\"{}\" hub start\nTerminal=false\nNoDisplay=true\nX-GNOME-Autostart-enabled=true\n",
            exe.display()
        ),
    )
}

pub fn remove_autostart() -> Result<(), String> {
    let _ = std::fs::remove_file(autostart_file());
    Ok(())
}

pub fn has_autostart() -> bool {
    autostart_file().is_file()
}

// ------------------------------------------------------------ notifications

pub fn notify_sink(_port: u16, _hooks: crate::notify::Hooks) -> Box<dyn crate::notify::Sink> {
    Box::new(crate::notify::Off("not on this platform yet"))
}

pub fn notify_unregister() {}

// ------------------------------------------------------------- folder dialog

pub fn pick_folder() -> Result<Option<String>, String> {
    if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none() {
        return Err("no desktop session for a folder dialog".into());
    }
    Ok(rfd::FileDialog::new()
        .set_title("Choose the repository folder (devstack)")
        .pick_folder()
        .map(|p| p.to_string_lossy().into_owned()))
}

// ------------------------------------------------------------------ browsers

pub const URL_OPENER: &str = "xdg-open";

pub fn open_browser(url: &str) -> std::io::Result<()> {
    Command::new("xdg-open").arg(url).spawn().map(drop)
}

pub fn start_browser(program: &str, url: &str) -> std::io::Result<bool> {
    Command::new(program).arg(url).status().map(|_| true)
}

// -------------------------------------------- the user's programs (ADR-0046)

/// Nothing to open with.
#[derive(Debug, Clone)]
pub enum Launch {}

pub fn files_and_terminals() -> Vec<Local> {
    Vec::new()
}

pub fn launch(how: &Launch, _dir: &Path, _path: &str) -> Result<(), String> {
    match *how {}
}

pub fn registered_schemes<'a>(_schemes: &[&'a str]) -> Vec<&'a str> {
    Vec::new()
}

pub fn orca() -> Option<PathBuf> {
    None
}

pub fn github_desktop() -> Option<PathBuf> {
    None
}

pub fn github_desktop_open(_exe: &Path, _path: &str) -> Result<(), String> {
    Err("not supported on this platform".into())
}

pub fn shell_open(_target: &str) -> Result<(), String> {
    Err("not supported on this platform".into())
}

// -------------------------------------------------------------------- Docker

pub fn docker_starter() -> Option<&'static str> {
    Command::new("systemctl")
        .args(["--user", "cat", "docker-desktop"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
        .then_some("Docker Desktop")
}

pub fn start_docker() -> std::io::Result<()> {
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

pub fn docker_hint() -> &'static str {
    "start the Docker service (`sudo systemctl start docker`)"
}

// ------------------------------------------------------------ self uninstall

pub fn install_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("bin"))
}

pub fn devstack_pids() -> Vec<u32> {
    // SAFETY: getuid cannot fail.
    let uid = unsafe { libc::getuid() };
    let out = Command::new("ps")
        .args(["-U", &uid.to_string(), "-o", "pid=,comm="])
        .output();
    let Ok(out) = out else { return Vec::new() };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let (pid, comm) = l.trim().split_once(char::is_whitespace)?;
            let name = comm.trim().rsplit('/').next()?;
            (name == "devstack").then(|| pid.parse().ok())?
        })
        .collect()
}

pub fn remove_from_path(_dir: &Path) -> Result<(), String> {
    Ok(())
}

pub const BINARY_GOES_LATER: bool = false;

pub fn remove_binary(exe: &Path) -> Result<(), String> {
    std::fs::remove_file(exe).map_err(|e| format!("remove {}: {e}", exe.display()))
}
