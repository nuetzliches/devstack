//! macOS, a developer platform in preview (ADR-0054 point 3), where it needs
//! no app bundle: the hub at logon through a LaunchAgent of devstack's own
//! (`~/Library/LaunchAgents/it.nuetzliche.devstack.hub.plist`, loaded with
//! `launchctl bootstrap gui/<uid>`), the folder dialog through `osascript`
//! (`choose folder`; no AppKit in the hub, whose worker threads ask for it),
//! `open` and `open -a` for the dashboard, a browser and the user's programs
//! (ADR-0046), `open -g -a Docker` (or OrbStack) for the engine.
//!
//! Not yet, and said so (point 5): desktop notifications and the
//! `devstack://` link need an app bundle, and a bundle needs signing to
//! behave; a Mac as a pool node.

use crate::mac::{AGENT_LABEL, agent_path, applescript_string, cancelled, chosen_folder};
use crate::{Local, LocalKind, Platform, on_path};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub(crate) const PLATFORM: Platform = Platform::Macos;

const NOT_YET: &str = "not on macOS yet";

pub const LINK_UNAVAILABLE: Option<&str> = Some(NOT_YET);

pub const PREVIEW: Option<crate::Preview> = Some(crate::Preview {
    name: "macOS preview",
    not_yet: &[
        "desktop notifications",
        "the devstack:// link (the dashboard cannot start a stopped hub; `devstack hub start` does)",
        "a Mac as a pool node",
    ],
});

pub fn no_window(_cmd: &mut Command) {}

/// Nothing to bring to the front: `open` brings what it starts forward,
/// and the folder dialog brings itself (`activate`).
pub struct Front;

impl Front {
    pub fn take() -> Option<Self> {
        None
    }
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// `name.app` in `/Applications` or `~/Applications`.
fn app(name: &str) -> Option<PathBuf> {
    [PathBuf::from("/Applications"), home().join("Applications")]
        .into_iter()
        .map(|d| d.join(format!("{name}.app")))
        .find(|p| p.is_dir())
}

/// Run `cmd` to its end: `Err` with what it said when it failed.
fn run(mut cmd: Command, what: &str) -> Result<(), String> {
    let out = cmd
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{what}: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let said = String::from_utf8_lossy(&out.stderr).trim().to_string();
    Err(if said.is_empty() {
        format!("{what} ended with {}", out.status)
    } else {
        format!("{what}: {said}")
    })
}

// --------------------------------------------- the hub at logon: a LaunchAgent

fn agent_file() -> PathBuf {
    home()
        .join("Library/LaunchAgents")
        .join(format!("{AGENT_LABEL}.plist"))
}

/// The user's GUI domain of launchd, `gui/<uid>`.
fn domain() -> String {
    // SAFETY: getuid cannot fail.
    format!("gui/{}", unsafe { libc::getuid() })
}

fn launchctl(args: &[&str]) -> Result<(), String> {
    let mut cmd = Command::new("launchctl");
    cmd.args(args).stdout(Stdio::null());
    run(cmd, &format!("launchctl {}", args[0]))
}

/// Write the LaunchAgent and load it, which runs `hub start` at once (the
/// hub then runs, as it would after the next logon). A session without a
/// GUI domain (SSH) cannot load it: the file is there, and launchd loads it
/// at the next logon.
pub fn install_autostart(exe: &Path) -> Result<(), String> {
    let file = agent_file();
    let exe = exe.to_string_lossy();
    let path = agent_path(std::env::var("PATH").ok().as_deref());
    let plist = crate::mac::launch_agent(AGENT_LABEL, &[&exe, "hub", "start"], &path);
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(&file, plist).map_err(|e| format!("{}: {e}", file.display()))?;
    // A loaded agent keeps the definition it was loaded with: out, then in.
    let _ = launchctl(&["bootout", &format!("{}/{AGENT_LABEL}", domain())]);
    let _ = launchctl(&["bootstrap", &domain(), &file.to_string_lossy()]);
    Ok(())
}

pub fn remove_autostart() -> Result<(), String> {
    let _ = launchctl(&["bootout", &format!("{}/{AGENT_LABEL}", domain())]);
    match std::fs::remove_file(agent_file()) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(format!("{}: {e}", agent_file().display()))
        }
        _ => Ok(()),
    }
}

pub fn has_autostart() -> bool {
    agent_file().is_file()
}

/// Where the LaunchAgent is, and whether launchd has it loaded.
pub fn autostart_note() -> Option<String> {
    let file = agent_file();
    if !file.is_file() {
        return None;
    }
    let loaded = Command::new("launchctl")
        .args(["print", &format!("{}/{AGENT_LABEL}", domain())])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    Some(format!(
        "LaunchAgent {}, {}",
        file.display(),
        if loaded {
            "loaded"
        } else {
            "not loaded: launchd loads it at the next logon"
        }
    ))
}

// -------------------------------------------- the devstack:// link: not yet

pub fn install_link(_exe: &Path) -> Result<(), String> {
    Ok(())
}

pub fn remove_link() -> Result<(), String> {
    Ok(())
}

pub fn has_link() -> bool {
    false
}

// ------------------------------------------------- notifications: not yet

pub fn notify_sink(_port: u16, _hooks: crate::notify::Hooks) -> Box<dyn crate::notify::Sink> {
    Box::new(crate::notify::Off(NOT_YET))
}

pub fn notify_unregister() {}

// ------------------------------------------------------------- folder dialog

/// `choose folder` in osascript, brought to the front first. Cancel is
/// `None`, as elsewhere; no dialog (an SSH session) is `Err`.
pub fn pick_folder() -> Result<Option<String>, String> {
    let choose = format!(
        "POSIX path of (choose folder with prompt {})",
        applescript_string("Choose the repository folder (devstack)")
    );
    let out = Command::new("osascript")
        .args(["-e", "activate", "-e", &choose])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("no folder dialog (osascript: {e})"))?;
    if out.status.success() {
        return Ok(chosen_folder(&String::from_utf8_lossy(&out.stdout)));
    }
    let said = String::from_utf8_lossy(&out.stderr);
    if cancelled(&said) {
        return Ok(None);
    }
    Err(format!("no folder dialog (osascript: {})", said.trim()))
}

// ------------------------------------------------------------------ browsers

pub const URL_OPENER: &str = "open";

pub fn open_browser(url: &str) -> std::io::Result<()> {
    let mut child = Command::new("open").arg(url).stdin(Stdio::null()).spawn()?;
    std::thread::spawn(move || child.wait());
    Ok(())
}

/// An application by its name or bundle (`open -a "Google Chrome"`), or a
/// program by its path.
pub fn start_browser(program: &str, url: &str) -> std::io::Result<bool> {
    let is_program = program.contains('/') && !program.trim_end_matches('/').ends_with(".app");
    if is_program {
        return Command::new(program).arg(url).status().map(|_| true);
    }
    Ok(Command::new("open")
        .args(["-a", program, url])
        .status()?
        .success())
}

// -------------------------------------------- the user's programs (ADR-0046)

#[derive(Debug, Clone)]
pub enum Launch {
    /// The folder, opened by `open` (Finder).
    Open,
    /// An application, `open -a <name> <folder>`.
    App(&'static str),
}

pub fn files_and_terminals() -> Vec<Local> {
    vec![
        Local {
            id: "files",
            label: "Finder",
            kind: LocalKind::Files,
            launch: Launch::Open,
        },
        Local {
            id: "terminal",
            label: "Terminal",
            kind: LocalKind::Terminal,
            launch: Launch::App("Terminal"),
        },
    ]
}

pub fn launch(how: &Launch, _dir: &Path, path: &str) -> Result<(), String> {
    match how {
        Launch::Open => shell_open(path),
        Launch::App(name) => {
            let mut cmd = Command::new("open");
            cmd.args(["-a", name, path]);
            run(cmd, &format!("open -a {name}"))
        }
    }
}

/// Asks LaunchServices which of `schemes` an application opens, all in one
/// osascript (JavaScript for Automation), one line per scheme found.
const SCHEMES_JXA: &str = "ObjC.import('AppKit');
function run(argv) {
  const ws = $.NSWorkspace.sharedWorkspace;
  return argv.filter(function (s) {
    try {
      const app = ws.URLForApplicationToOpenURL($.NSURL.URLWithString(s + '://'));
      if (!app || app.isNil()) return false;
      return !!ObjC.unwrap(app.path);
    } catch (e) {
      return false;
    }
  }).join('\\n');
}";

pub fn registered_schemes<'a>(schemes: &[&'a str]) -> Vec<&'a str> {
    let Ok(out) = Command::new("osascript")
        .args(["-l", "JavaScript", "-e", SCHEMES_JXA])
        .args(schemes)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&out.stdout);
    schemes
        .iter()
        .copied()
        .filter(|s| text.lines().any(|l| l.trim() == *s))
        .collect()
}

/// The application bundle.
pub fn github_desktop() -> Option<PathBuf> {
    app("GitHub Desktop")
}

/// What its own `github` command does on a Mac: a new instance with
/// `--cli-open=<path>`, which hands the folder to the running one.
pub fn github_desktop_open(bundle: &Path, path: &str) -> Result<(), String> {
    let mut cmd = Command::new("open");
    cmd.arg("-n")
        .arg(bundle)
        .arg("--args")
        .arg(format!("--cli-open={path}"));
    run(cmd, "open GitHub Desktop")
}

pub fn orca() -> Option<PathBuf> {
    on_path("orca").or_else(|| {
        let p = app("Orca")?.join("Contents/Resources/bin/orca");
        p.is_file().then_some(p)
    })
}

/// `open <target>`: a folder in Finder, a link in the application its
/// scheme names.
pub fn shell_open(target: &str) -> Result<(), String> {
    let mut cmd = Command::new("open");
    cmd.arg(target);
    run(cmd, "open")
}

// -------------------------------------------------------------------- Docker
//
// Docker Desktop, else OrbStack, started in the background (`open -g`);
// Colima is started by the user (`colima start`).

pub fn docker_starter() -> Option<&'static str> {
    if app("Docker").is_some() {
        Some("Docker Desktop")
    } else if app("OrbStack").is_some() {
        Some("OrbStack")
    } else {
        None
    }
}

pub fn start_docker() -> std::io::Result<()> {
    let name = match docker_starter() {
        Some("OrbStack") => "OrbStack",
        Some(_) => "Docker",
        None => {
            return Err(std::io::Error::other(
                "neither Docker Desktop nor OrbStack is installed",
            ));
        }
    };
    let s = Command::new("open")
        .args(["-g", "-a", name])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if s.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "`open -g -a {name}` answered {s}"
        )))
    }
}

pub fn docker_hint() -> &'static str {
    "start Docker Desktop, OrbStack or Colima (`colima start`)"
}

pub fn docker_install_hint() -> &'static str {
    "install Docker Desktop, OrbStack or Colima"
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
    // macOS names the program by its path: `/opt/homebrew/bin/devstack`.
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
    // install.sh does not edit a shell profile; Homebrew's directory is
    // Homebrew's.
    Ok(())
}

pub const BINARY_GOES_LATER: bool = false;

pub fn remove_binary(exe: &Path) -> Result<(), String> {
    std::fs::remove_file(exe).map_err(|e| format!("remove {}: {e}", exe.display()))
}
