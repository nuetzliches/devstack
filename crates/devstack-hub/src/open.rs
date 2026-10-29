//! Opening a stack's tree in the user's own programs (ADR-0046):
//! `GET /hub/openers`, `POST /hub/open`, and an agent started with a prompt
//! the hub wrote (`POST /hub/agent`, amendment 2026-10-22).
//!
//! A fixed list. Each program is found the way it makes itself known: an
//! editor or agent by the URL scheme it registered (`vscode`, `claude`, ...),
//! started by a link of that scheme, as the shell opens it; a terminal and
//! Orca by their program on `PATH`; GitHub Desktop by its install folder. The page never names a program or a
//! command line, only one of the ids found here. How each is found and
//! started on this operating system is `devstack_desktop`'s.

use devstack_desktop::LocalKind;
use serde::Serialize;
use std::path::{Path, PathBuf};

/// What a program is for, to group the menu, in menu order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Files,
    Terminal,
    Editor,
    Git,
    Agent,
}

/// How an agent takes a prompt it is started with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptMode {
    /// Shown in its input; the user sends it.
    Prefill,
    /// Sent at once: the agent starts working.
    Send,
}

/// One entry of `GET /hub/openers`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Opener {
    pub id: &'static str,
    pub label: &'static str,
    pub kind: Kind,
    /// Whether, and how, it can be started with a prompt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<PromptMode>,
    /// The agent of the agent table it is (ADR-0077): the dialog shows its
    /// skills switch beside it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<&'static str>,
}

/// A program found, and how it is started.
#[derive(Debug, Clone)]
pub struct Found {
    pub opener: Opener,
    how: How,
}

#[derive(Debug, Clone)]
enum How {
    /// The file manager or a terminal of this desktop, in the folder.
    Local(devstack_desktop::Local),
    /// A link of the program's scheme (`links`).
    Link(&'static str),
    /// Orca's CLI.
    Orca(PathBuf),
    /// GitHub Desktop: `--cli-open=<path>`, what its own `github` command
    /// runs.
    GitHubDesktop(PathBuf),
}

/// The editors found by their registered scheme, in menu order.
const EDITORS: &[(&str, &str, Kind)] = &[
    ("vscode", "VS Code", Kind::Editor),
    ("vscode-insiders", "VS Code Insiders", Kind::Editor),
    ("cursor", "Cursor", Kind::Editor),
    ("windsurf", "Windsurf", Kind::Editor),
    ("zed", "Zed", Kind::Editor),
];

/// The programs found by their registered scheme, in menu order: the
/// editors, then the agents of the agent table that say how they are
/// opened (`[agent.open]`, ADR-0074).
fn links() -> Vec<(&'static str, &'static str, Kind, Option<&'static str>)> {
    let mut out: Vec<_> = EDITORS.iter().map(|&(s, l, k)| (s, l, k, None)).collect();
    for a in devstack_skills::agents::all() {
        for l in a.open.iter().flat_map(|o| &o.links) {
            out.push((
                l.scheme.as_str(),
                l.label.as_str(),
                Kind::Agent,
                Some(a.id.as_str()),
            ));
        }
    }
    out
}

/// What this machine can open a folder with, in menu order: files,
/// terminals, editors, Git clients, agents. Nothing without a desktop.
pub fn found() -> Vec<Found> {
    let mut out: Vec<Found> = devstack_desktop::files_and_terminals()
        .into_iter()
        .map(|l| Found {
            opener: Opener {
                id: l.id,
                label: l.label,
                kind: match l.kind {
                    LocalKind::Files => Kind::Files,
                    LocalKind::Terminal => Kind::Terminal,
                },
                prompt: None,
                agent: None,
            },
            how: How::Local(l),
        })
        .collect();
    if out.is_empty() {
        return out;
    }
    let links = links();
    let schemes: Vec<&str> = links.iter().map(|l| l.0).collect();
    let registered = devstack_desktop::registered_schemes(&schemes);
    for &(scheme, label, kind, agent) in &links {
        if registered.contains(&scheme) {
            out.push(Found {
                opener: Opener {
                    id: scheme,
                    label,
                    kind,
                    prompt: (kind == Kind::Agent).then_some(PromptMode::Prefill),
                    agent,
                },
                how: How::Link(scheme),
            });
        }
    }
    if let Some(exe) = devstack_desktop::github_desktop() {
        out.push(Found {
            opener: Opener {
                id: "github-desktop",
                label: "GitHub Desktop",
                kind: Kind::Git,
                prompt: None,
                agent: None,
            },
            how: How::GitHubDesktop(exe),
        });
    }
    if let Some(orca) = devstack_desktop::orca() {
        let a = devstack_skills::agents::Agent::by_program("orca");
        out.push(Found {
            opener: Opener {
                id: "orca",
                label: a.map_or("Orca", |a| a.name.as_str()),
                kind: Kind::Agent,
                prompt: Some(PromptMode::Send),
                agent: a.map(|a| a.id.as_str()),
            },
            how: How::Orca(orca),
        });
    }
    // Stable: within a kind, the order above.
    out.sort_by_key(|f| f.opener.kind);
    out
}

/// Why opening failed: the program is not (or no longer) here, or it
/// refused.
#[derive(Debug)]
pub enum OpenError {
    Unknown,
    /// Found, but it is not started with a prompt.
    NoPrompt,
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
    // Windows: the hub is in the background, the program would open behind
    // the browser; held until it is started or handed the link.
    let _front = devstack_desktop::Front::take();
    match &f.how {
        How::Local(l) => l.open(tree, &path),
        How::Link(scheme) => devstack_desktop::shell_open(&link(scheme, &path)),
        How::Orca(exe) => orca(exe, &path, &display(repo), stack_file),
        How::GitHubDesktop(exe) => devstack_desktop::github_desktop_open(exe, &path),
    }
    .map_err(OpenError::Failed)
}

/// Start the agent `id` in `tree` with `prompt` (written by the hub, never by
/// the page): a link that fills its input, or Orca's agent, which starts on
/// it at once.
pub fn agent(
    id: &str,
    tree: &Path,
    repo: &Path,
    stack_file: &Path,
    prompt: &str,
) -> Result<(), OpenError> {
    let f = found()
        .into_iter()
        .find(|f| f.opener.id == id)
        .ok_or(OpenError::Unknown)?;
    if f.opener.prompt.is_none() {
        return Err(OpenError::NoPrompt);
    }
    let path = display(tree);
    let _front = devstack_desktop::Front::take();
    match &f.how {
        How::Link(scheme) => devstack_desktop::shell_open(&prompt_link(scheme, &path, prompt)),
        How::Orca(exe) => orca(exe, &path, &display(repo), stack_file).and_then(|()| {
            let tree = format!("path:{path}");
            orca_run(
                exe,
                &[
                    "agent",
                    "launch",
                    "--agent",
                    "claude",
                    "--worktree",
                    &tree,
                    "--prompt",
                    prompt,
                    "--focus",
                ],
            )
            .map_err(|e| e.1)
        }),
        _ => return Err(OpenError::NoPrompt),
    }
    .map_err(OpenError::Failed)
}

/// The agent link for `path`, its input filled with `prompt`.
fn prompt_link(scheme: &str, path: &str, prompt: &str) -> String {
    format!("{}&q={}", link(scheme, path), encode(prompt, b""))
}

/// A path as the programs take it: the platform's separators, without the
/// `\\?\` prefix.
pub(crate) fn display(p: &Path) -> String {
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
        // Zed takes what follows `zed://file` as the path, slash and all:
        // `zed://file/D:/src` would be `/D:/src`, an empty window.
        "zed" => format!("zed://file{}", encode(&path.replace('\\', "/"), b"/:")),
        // The editors' `file` links (VS Code and its forks):
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
    let run = |args: &[&str]| orca_run(exe, args);
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

/// Run Orca's CLI. Err: Orca's error code (`selector_not_found`) and what to
/// say.
fn orca_run(exe: &Path, args: &[&str]) -> Result<(), (String, String)> {
    let mut cmd = std::process::Command::new(exe);
    cmd.args(args)
        .arg("--json")
        .stdin(std::process::Stdio::null());
    devstack_desktop::no_window(&mut cmd);
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
    fn zed_links_carry_the_path_as_it_is() {
        assert_eq!(
            link("zed", r"D:\Sources\my app"),
            "zed://fileD:/Sources/my%20app"
        );
        assert_eq!(link("zed", "/home/me/app"), "zed://file/home/me/app");
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
    fn a_prompt_goes_into_the_link_as_one_value() {
        assert_eq!(
            prompt_link("claude-cli", r"D:\src\app", "Fix it & go\n1. `a`"),
            "claude-cli://open?cwd=D%3A%5Csrc%5Capp&q=Fix%20it%20%26%20go%0A1.%20%60a%60"
        );
        assert!(prompt_link("claude", "/a", "x").starts_with("claude://code/new?folder=%2Fa&q=x"));
    }

    #[test]
    fn a_link_cannot_be_given_more_parameters_by_a_path() {
        let l = link("vscode", "/x?windowId=_blank#y");
        assert_eq!(l, "vscode://file/x%3FwindowId%3D_blank%23y");
    }

    #[test]
    fn git_clients_come_after_the_editors_before_the_agents() {
        assert!(Kind::Editor < Kind::Git && Kind::Git < Kind::Agent);
        assert_eq!(serde_json::to_value(Kind::Git).unwrap(), "git");
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
