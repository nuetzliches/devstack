//! Linux: XDG files in the user's profile for the hub at logon and the
//! `devstack://` link, notifications over the session bus, the desktop's
//! folder dialog through the XDG portal (no GTK linked in), `xdg-open`,
//! Docker Desktop's systemd user unit.

use crate::{Local, LocalKind, Platform, on_path};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

mod notify;

pub(crate) const PLATFORM: Platform = Platform::Linux;

pub const PREVIEW: Option<crate::Preview> = None;

pub const LINK_UNAVAILABLE: Option<&str> = None;

pub fn autostart_note() -> Option<String> {
    None
}

pub fn no_window(_cmd: &mut Command) {}

/// Nothing to bring to the front: the desktop lets the dialog and the
/// programs the hub starts come forward on their own.
pub struct Front;

impl Front {
    pub fn take() -> Option<Self> {
        None
    }
}

// ------------------------------------------- autostart and the devstack:// link
//
// `~/.config/autostart/devstack-hub.desktop` and
// `~/.local/share/applications/devstack-url.desktop` + `xdg-mime`.

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
    // Without xdg-utils (a server, a container) the file is there but no
    // browser asks for it; that is all we can do.
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
    // `xdg-mime default` wrote the association into mimeapps.list; it
    // would point at a file that is gone.
    for list in [
        config_dir().join("mimeapps.list"),
        data_dir().join("applications/mimeapps.list"),
    ] {
        if let Ok(text) = std::fs::read_to_string(&list) {
            let kept = without_handler(&text);
            if kept != text {
                let _ = std::fs::write(&list, kept);
            }
        }
    }
    Ok(())
}

pub fn has_link() -> bool {
    link_file().is_file()
}

/// `mimeapps.list` without devstack's `x-scheme-handler/devstack` entries
/// (another handler the user chose for the scheme stays).
fn without_handler(text: &str) -> String {
    let mut out: String = text
        .lines()
        .filter(|l| {
            let l = l.trim();
            !(l.starts_with("x-scheme-handler/devstack=") && l.contains("devstack-url.desktop"))
        })
        .collect::<Vec<_>>()
        .join("\n");
    if text.ends_with('\n') {
        out.push('\n');
    }
    out
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

pub fn notify_sink(_port: u16, hooks: crate::notify::Hooks) -> Box<dyn crate::notify::Sink> {
    Box::new(notify::Bus::new(hooks))
}

pub fn notify_unregister() {}

// ------------------------------------------------------------- folder dialog

pub fn pick_folder() -> Result<Option<String>, String> {
    // The portal answers on the session bus; without a desktop session
    // there is none, and rfd returns None just like a cancel.
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
    let mut cmd = Command::new("xdg-open");
    cmd.arg(url);
    crate::activation::apply(&mut cmd);
    cmd.spawn().map(drop)
}

pub fn start_browser(program: &str, url: &str) -> std::io::Result<bool> {
    let mut cmd = Command::new(program);
    cmd.arg(url);
    crate::activation::apply(&mut cmd);
    cmd.status().map(|_| true)
}

/// The launcher of the installed web app `id` (ADR-0061): a `.desktop` file
/// in the applications folder, then the desktop folder, whose `Exec` has
/// `--app-id=<id>`.
pub fn dashboard_app(id: &str) -> Option<crate::webapp::WebApp> {
    use crate::webapp;
    let home = home();
    let desktop = std::fs::read_to_string(config_dir().join("user-dirs.dirs"))
        .ok()
        .and_then(|t| webapp::desktop_dir(&t, &home))
        .unwrap_or_else(|| home.join("Desktop"));
    for dir in [data_dir().join("applications"), desktop] {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "desktop"))
            .collect();
        files.sort();
        for file in files {
            let Some(mut app) = std::fs::read_to_string(&file)
                .ok()
                .and_then(|t| webapp::desktop_entry_app(&t))
                .filter(|a| a.id == id)
            else {
                continue;
            };
            let name = file.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let program = webapp::snap_command(&app.program)
                .filter(|p| p.exists())
                .or_else(|| app.program.exists().then(|| app.program.clone()))
                .or_else(|| {
                    webapp::programs_for_launcher(name)
                        .iter()
                        .find_map(|p| on_path(p))
                });
            if let Some(program) = program {
                app.program = program;
                return Some(app);
            }
        }
    }
    None
}

// -------------------------------------------- the user's programs (ADR-0046)

/// The terminals tried after `$TERMINAL`, in order.
const TERMINALS: &[&str] = &[
    "x-terminal-emulator",
    "gnome-terminal",
    "konsole",
    "xfce4-terminal",
    "kitty",
    "alacritty",
    "wezterm",
    "foot",
    "xterm",
];

#[derive(Debug, Clone)]
pub enum Launch {
    /// The folder, opened by `xdg-open` (the file manager).
    Shell,
    /// A terminal emulator in the folder.
    Terminal(PathBuf),
}

fn desktop() -> bool {
    std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some()
}

pub fn files_and_terminals() -> Vec<Local> {
    if !desktop() {
        return Vec::new();
    }
    let mut out = Vec::new();
    if on_path("xdg-open").is_some() {
        out.push(Local {
            id: "files",
            label: "Files",
            kind: LocalKind::Files,
            launch: Launch::Shell,
        });
    }
    let term = std::env::var("TERMINAL")
        .ok()
        .and_then(|t| on_path(&t))
        .or_else(|| TERMINALS.iter().find_map(|t| on_path(t)));
    if let Some(t) = term {
        out.push(Local {
            id: "terminal",
            label: "Terminal",
            kind: LocalKind::Terminal,
            launch: Launch::Terminal(t),
        });
    }
    out
}

pub fn launch(how: &Launch, dir: &Path, path: &str) -> Result<(), String> {
    match how {
        Launch::Shell => shell_open(path),
        Launch::Terminal(exe) => terminal(exe, dir),
    }
}

/// The terminal in `dir`: most take the working directory they are
/// started in; those that hand the window to a server need it said.
fn terminal(exe: &Path, dir: &Path) -> Result<(), String> {
    let name = exe.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let mut cmd = Command::new(exe);
    match name {
        "gnome-terminal" | "xfce4-terminal" => {
            cmd.arg(format!("--working-directory={}", dir.display()));
        }
        "konsole" => {
            cmd.arg("--workdir").arg(dir);
        }
        "wezterm" => {
            cmd.args(["start", "--cwd"]).arg(dir);
        }
        _ => {}
    }
    cmd.current_dir(dir);
    detach(cmd, name)
}

/// `xdg-mime` names a handler for `x-scheme-handler/<scheme>`.
pub fn registered_schemes<'a>(schemes: &[&'a str]) -> Vec<&'a str> {
    schemes.iter().copied().filter(|s| registered(s)).collect()
}

fn registered(scheme: &str) -> bool {
    Command::new("xdg-mime")
        .args(["query", "default", &format!("x-scheme-handler/{scheme}")])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .is_ok_and(|o| o.status.success() && !o.stdout.trim_ascii().is_empty())
}

pub fn orca() -> Option<PathBuf> {
    on_path("orca")
}

/// GitHub publishes no Linux build.
pub fn github_desktop() -> Option<PathBuf> {
    None
}

pub fn github_desktop_open(_exe: &Path, _path: &str) -> Result<(), String> {
    Err("no GitHub Desktop on Linux".into())
}

/// Start `cmd` and let it go; a thread waits for it, so it leaves no
/// zombie behind.
fn detach(mut cmd: Command, what: &str) -> Result<(), String> {
    crate::activation::apply(&mut cmd);
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("start {what}: {e}"))?;
    std::thread::spawn(move || child.wait());
    Ok(())
}

pub fn shell_open(target: &str) -> Result<(), String> {
    let mut cmd = Command::new("xdg-open");
    cmd.arg(target);
    detach(cmd, "xdg-open")
}

// -------------------------------------------------------------------- Docker
//
// Docker Desktop for Linux runs as a systemd user unit, and devstack starts
// it; Docker Engine is a system service that needs root, so devstack names
// `sudo systemctl start docker`.

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

pub fn docker_install_hint() -> &'static str {
    "install Docker Desktop or Docker Engine"
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
    // install.sh only notes when ~/.local/bin is not on PATH; it never edits a
    // shell profile, so there is nothing to undo.
    Ok(())
}

pub const BINARY_GOES_LATER: bool = false;

pub fn remove_binary(exe: &Path) -> Result<(), String> {
    std::fs::remove_file(exe).map_err(|e| format!("remove {}: {e}", exe.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scheme_handler_leaves_mimeapps_list() {
        let text = "[Default Applications]\nx-scheme-handler/devstack=devstack-url.desktop\ntext/html=firefox.desktop\n[Added Associations]\nx-scheme-handler/devstack=devstack-url.desktop;\n";
        assert_eq!(
            without_handler(text),
            "[Default Applications]\ntext/html=firefox.desktop\n[Added Associations]\n"
        );
        let other = "x-scheme-handler/devstack=mine.desktop\n";
        assert_eq!(without_handler(other), other);
    }
}
