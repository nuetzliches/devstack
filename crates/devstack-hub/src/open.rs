//! Opening a stack's tree in the user's own programs (ADR-0046):
//! `GET /hub/openers`, `POST /hub/open`.
//!
//! A fixed list. Each program is found the way it makes itself known: an
//! editor or agent by the URL scheme it registered (`vscode`, `claude`, ...),
//! started by a link of that scheme, as the shell opens it; a terminal and
//! Orca by their program on `PATH`. The page never names a program or a
//! command line, only one of the ids found here.

use serde::Serialize;
use std::path::{Path, PathBuf};

/// What a program is for, to group the menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Files,
    Terminal,
    Editor,
    Agent,
}

/// One entry of `GET /hub/openers`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Opener {
    pub id: &'static str,
    pub label: &'static str,
    pub kind: Kind,
}

/// A program found, and how it is started.
#[derive(Debug, Clone)]
pub struct Found {
    pub opener: Opener,
    how: How,
}

#[derive(Debug, Clone)]
enum How {
    /// The folder, opened by the shell (Explorer, the file manager).
    Folder,
    /// A link of the program's scheme (`LINKS`).
    Link(&'static str),
    /// Windows Terminal: `wt -d <path>`.
    #[cfg_attr(not(windows), allow(dead_code))]
    Wt(PathBuf),
    /// A shell in a console of its own, in the folder (PowerShell).
    #[cfg_attr(not(windows), allow(dead_code))]
    Console(PathBuf),
    /// A terminal emulator in the folder (Linux).
    #[cfg_attr(windows, allow(dead_code))]
    Terminal(PathBuf),
    /// Orca's CLI.
    Orca(PathBuf),
}

/// The programs found by their registered scheme, in menu order.
const LINKS: &[(&str, &str, Kind)] = &[
    ("vscode", "VS Code", Kind::Editor),
    ("vscode-insiders", "VS Code Insiders", Kind::Editor),
    ("cursor", "Cursor", Kind::Editor),
    ("windsurf", "Windsurf", Kind::Editor),
    ("zed", "Zed", Kind::Editor),
    ("claude", "Claude Desktop", Kind::Agent),
    ("claude-cli", "Claude Code", Kind::Agent),
];

/// What this machine can open a folder with, in menu order: files,
/// terminals, editors, agents. Nothing without a desktop.
pub fn found() -> Vec<Found> {
    let mut out = imp::files_and_terminals();
    if out.is_empty() {
        return out;
    }
    for &(scheme, label, kind) in LINKS {
        if imp::registered(scheme) {
            out.push(Found {
                opener: Opener {
                    id: scheme,
                    label,
                    kind,
                },
                how: How::Link(scheme),
            });
        }
    }
    if let Some(orca) = imp::orca() {
        out.push(Found {
            opener: Opener {
                id: "orca",
                label: "Orca",
                kind: Kind::Agent,
            },
            how: How::Orca(orca),
        });
    }
    out
}

/// Why opening failed: the program is not (or no longer) here, or it
/// refused.
#[derive(Debug)]
pub enum OpenError {
    Unknown,
    Failed(String),
}

/// Open `tree` (a worktree of a stack, checked by the caller) with the
/// program `id`; `stack_file` is the stack file's path inside `tree` (Orca
/// shows it).
pub fn open(id: &str, tree: &Path, repo: &Path, stack_file: &Path) -> Result<(), OpenError> {
    let f = found()
        .into_iter()
        .find(|f| f.opener.id == id)
        .ok_or(OpenError::Unknown)?;
    let path = display(tree);
    match &f.how {
        How::Folder => imp::shell_open(&path),
        How::Link(scheme) => imp::shell_open(&link(scheme, &path)),
        How::Wt(exe) => imp::wt(exe, &path),
        How::Console(exe) => imp::console(exe, tree),
        How::Terminal(exe) => imp::terminal(exe, tree),
        How::Orca(exe) => orca(exe, &path, &display(repo), stack_file),
    }
    .map_err(OpenError::Failed)
}

/// A path as the programs take it: the platform's separators, without the
/// `\\?\` prefix.
fn display(p: &Path) -> String {
    let s = p.to_string_lossy();
    let s = s.strip_prefix(r"\\?\").unwrap_or(&s);
    if cfg!(windows) {
        s.replace('/', "\\")
    } else {
        s.to_string()
    }
}

/// The link that opens `path` in the program of `scheme`.
fn link(scheme: &str, path: &str) -> String {
    match scheme {
        "claude" => format!("claude://code/new?folder={}", encode(path, b"")),
        "claude-cli" => format!("claude-cli://open?cwd={}", encode(path, b"")),
        // The editors' `file` links (VS Code and its forks, Zed):
        // `vscode://file/C:/src/app`, `vscode://file/home/me/app`.
        _ => {
            let p = path.replace('\\', "/");
            let p = p.trim_start_matches('/');
            format!("{scheme}://file/{}", encode(p, b"/:"))
        }
    }
}

/// Percent-encode all but the unreserved characters and `keep`.
fn encode(s: &str, keep: &[u8]) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) || keep.contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// Orca: start it if it does not run, bring its window to the tree with the
/// stack file open; a repository Orca does not know yet is added first.
fn orca(exe: &Path, path: &str, repo: &str, stack_file: &Path) -> Result<(), String> {
    // Err: Orca's error code (`selector_not_found`) and what to say.
    let run = |args: &[&str]| -> Result<(), (String, String)> {
        let mut cmd = std::process::Command::new(exe);
        cmd.args(args)
            .arg("--json")
            .stdin(std::process::Stdio::null());
        crate::no_window(&mut cmd);
        let out = cmd
            .output()
            .map_err(|e| (String::new(), format!("run orca: {e}")))?;
        if out.status.success() {
            return Ok(());
        }
        let answer = serde_json::from_slice::<serde_json::Value>(&out.stdout).ok();
        let at = |p: &str| {
            answer
                .as_ref()
                .and_then(|v| v.pointer(p))
                .and_then(|m| m.as_str())
                .map(String::from)
        };
        let code = at("/error/code").unwrap_or_default();
        let why = at("/error/data/nextSteps/0")
            .or_else(|| at("/error/message"))
            .unwrap_or_else(|| String::from_utf8_lossy(&out.stderr).trim().to_string());
        Err((code, format!("orca {}: {why}", args[0])))
    };
    run(&["open"]).map_err(|e| e.1)?;
    let tree = format!("path:{path}");
    let file = stack_file.to_string_lossy();
    let show = || {
        if stack_file.is_file() {
            run(&["file", "open", &file, "--worktree", &tree, "--focus"])
        } else {
            run(&["terminal", "create", "--worktree", &tree, "--focus"])
        }
    };
    match show() {
        Err((code, _)) if code == "selector_not_found" => {
            run(&["repo", "add", "--path", repo]).map_err(|e| e.1)?;
            show().map_err(|e| e.1)
        }
        r => r.map_err(|e| e.1),
    }
}

/// Where `name` is on `PATH`, as a program (`.exe` added on Windows).
fn on_path(name: &str) -> Option<PathBuf> {
    let file = if cfg!(windows) && !name.ends_with(".exe") {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(&file))
        // An app execution alias (`wt.exe`) is a reparse point that
        // `metadata` cannot follow; it is there all the same.
        .find(|p| std::fs::symlink_metadata(p).is_ok_and(|m| !m.is_dir()))
}

#[cfg(windows)]
mod imp {
    use super::{Found, How, Kind, Opener, on_path};
    use std::path::{Path, PathBuf};

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn files_and_terminals() -> Vec<Found> {
        let mut out = vec![Found {
            opener: Opener {
                id: "files",
                label: "Explorer",
                kind: Kind::Files,
            },
            how: How::Folder,
        }];
        if let Some(wt) = on_path("wt") {
            out.push(Found {
                opener: Opener {
                    id: "wt",
                    label: "Windows Terminal",
                    kind: Kind::Terminal,
                },
                how: How::Wt(wt),
            });
        }
        let ps = on_path("pwsh")
            .or_else(|| on_path("powershell"))
            .or_else(|| {
                let root = std::env::var_os("SystemRoot")?;
                let p = PathBuf::from(root).join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
                p.is_file().then_some(p)
            });
        if let Some(ps) = ps {
            out.push(Found {
                opener: Opener {
                    id: "powershell",
                    label: "PowerShell",
                    kind: Kind::Terminal,
                },
                how: How::Console(ps),
            });
        }
        out
    }

    /// `HKCR\<scheme>` with a `URL Protocol` value: what the shell opens
    /// a link of that scheme with, packaged apps included.
    pub fn registered(scheme: &str) -> bool {
        use windows_sys::Win32::Foundation::ERROR_SUCCESS;
        use windows_sys::Win32::System::Registry::{
            HKEY, HKEY_CLASSES_ROOT, KEY_READ, RegCloseKey, RegOpenKeyExW, RegQueryValueExW,
        };
        let key_name = wide(scheme);
        let value = wide("URL Protocol");
        let mut key: HKEY = std::ptr::null_mut();
        // SAFETY: plain registry calls on our own buffers; the key is closed.
        unsafe {
            if RegOpenKeyExW(HKEY_CLASSES_ROOT, key_name.as_ptr(), 0, KEY_READ, &mut key)
                != ERROR_SUCCESS
            {
                return false;
            }
            let found = RegQueryValueExW(
                key,
                value.as_ptr(),
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            ) == ERROR_SUCCESS;
            RegCloseKey(key);
            found
        }
    }

    pub fn orca() -> Option<PathBuf> {
        on_path("orca").or_else(|| {
            let p = PathBuf::from(std::env::var_os("LOCALAPPDATA")?)
                .join(r"Programs\orca\resources\bin\orca.exe");
            p.is_file().then_some(p)
        })
    }

    /// What double-clicking `target` does: a folder opens in Explorer, a
    /// link in the program its scheme names.
    pub fn shell_open(target: &str) -> Result<(), String> {
        use windows_sys::Win32::System::Com::{
            COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
        };
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        let verb = wide("open");
        let file = wide(target);
        // SAFETY: COM for this thread (the shell's handlers may need it),
        // released again; ShellExecuteW on our own buffers.
        let code = unsafe {
            let com = CoInitializeEx(
                std::ptr::null(),
                (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
            );
            let h = ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                file.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            );
            if com >= 0 {
                CoUninitialize();
            }
            h as isize
        };
        // Above 32 is success; below, one of the shell's error codes.
        if code > 32 {
            Ok(())
        } else {
            Err(format!("the shell could not open it (error {code})"))
        }
    }

    pub fn wt(exe: &Path, path: &str) -> Result<(), String> {
        // `;` separates Windows Terminal's commands, also inside a path.
        std::process::Command::new(exe)
            .arg("-d")
            .arg(path.replace(';', r"\;"))
            .spawn()
            .map(drop)
            .map_err(|e| format!("start Windows Terminal: {e}"))
    }

    pub fn console(exe: &Path, dir: &Path) -> Result<(), String> {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
        std::process::Command::new(exe)
            .arg("-NoExit")
            .current_dir(dir)
            .creation_flags(CREATE_NEW_CONSOLE)
            .spawn()
            .map(drop)
            .map_err(|e| format!("start {}: {e}", exe.display()))
    }

    pub fn terminal(_exe: &Path, _dir: &Path) -> Result<(), String> {
        Err("no terminal emulator on Windows".into())
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::{Found, How, Kind, Opener, on_path};
    use std::path::{Path, PathBuf};

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

    fn desktop() -> bool {
        std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some()
    }

    pub fn files_and_terminals() -> Vec<Found> {
        if !desktop() {
            return Vec::new();
        }
        let mut out = Vec::new();
        if on_path("xdg-open").is_some() {
            out.push(Found {
                opener: Opener {
                    id: "files",
                    label: "Files",
                    kind: Kind::Files,
                },
                how: How::Folder,
            });
        }
        let term = std::env::var("TERMINAL")
            .ok()
            .and_then(|t| on_path(&t))
            .or_else(|| TERMINALS.iter().find_map(|t| on_path(t)));
        if let Some(t) = term {
            out.push(Found {
                opener: Opener {
                    id: "terminal",
                    label: "Terminal",
                    kind: Kind::Terminal,
                },
                how: How::Terminal(t),
            });
        }
        out
    }

    /// `xdg-mime` names a handler for `x-scheme-handler/<scheme>`.
    pub fn registered(scheme: &str) -> bool {
        std::process::Command::new("xdg-mime")
            .args(["query", "default", &format!("x-scheme-handler/{scheme}")])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .is_ok_and(|o| o.status.success() && !o.stdout.trim_ascii().is_empty())
    }

    pub fn orca() -> Option<PathBuf> {
        on_path("orca")
    }

    /// Start `cmd` and let it go; a thread waits for it, so it leaves no
    /// zombie behind.
    fn detach(mut cmd: std::process::Command, what: &str) -> Result<(), String> {
        let mut child = cmd
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("start {what}: {e}"))?;
        std::thread::spawn(move || child.wait());
        Ok(())
    }

    pub fn shell_open(target: &str) -> Result<(), String> {
        let mut cmd = std::process::Command::new("xdg-open");
        cmd.arg(target);
        detach(cmd, "xdg-open")
    }

    pub fn wt(_exe: &Path, _path: &str) -> Result<(), String> {
        Err("no Windows Terminal on Linux".into())
    }

    pub fn console(_exe: &Path, _dir: &Path) -> Result<(), String> {
        Err("no console on Linux".into())
    }

    /// The terminal in `dir`: most take the working directory they are
    /// started in; those that hand the window to a server need it said.
    pub fn terminal(exe: &Path, dir: &Path) -> Result<(), String> {
        let name = exe.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let mut cmd = std::process::Command::new(exe);
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
}

/// Nothing to open with until #66 (macOS).
#[cfg(not(any(windows, target_os = "linux")))]
mod imp {
    use super::Found;
    use std::path::{Path, PathBuf};

    pub fn files_and_terminals() -> Vec<Found> {
        Vec::new()
    }
    pub fn registered(_scheme: &str) -> bool {
        false
    }
    pub fn orca() -> Option<PathBuf> {
        None
    }
    pub fn shell_open(_target: &str) -> Result<(), String> {
        Err("not supported on this platform".into())
    }
    pub fn wt(_exe: &Path, _path: &str) -> Result<(), String> {
        Err("not supported on this platform".into())
    }
    pub fn console(_exe: &Path, _dir: &Path) -> Result<(), String> {
        Err("not supported on this platform".into())
    }
    pub fn terminal(_exe: &Path, _dir: &Path) -> Result<(), String> {
        Err("not supported on this platform".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_links_carry_the_path_with_forward_slashes() {
        assert_eq!(
            link("vscode", r"D:\Sources\my app"),
            "vscode://file/D:/Sources/my%20app"
        );
        assert_eq!(link("cursor", "/home/me/app"), "cursor://file/home/me/app");
    }

    #[test]
    fn agent_links_carry_the_path_as_a_query_value() {
        assert_eq!(
            link("claude", r"D:\src\app"),
            "claude://code/new?folder=D%3A%5Csrc%5Capp"
        );
        assert_eq!(
            link("claude-cli", "/home/me/a&b"),
            "claude-cli://open?cwd=%2Fhome%2Fme%2Fa%26b"
        );
    }

    #[test]
    fn a_link_cannot_be_given_more_parameters_by_a_path() {
        let l = link("vscode", "/x?windowId=_blank#y");
        assert_eq!(l, "vscode://file/x%3FwindowId%3D_blank%23y");
    }

    #[test]
    fn display_drops_the_verbatim_prefix() {
        if cfg!(windows) {
            assert_eq!(display(Path::new(r"\\?\D:\src\app")), r"D:\src\app");
            assert_eq!(display(Path::new("D:/src/app")), r"D:\src\app");
        } else {
            assert_eq!(display(Path::new("/home/me/app")), "/home/me/app");
        }
    }
}
