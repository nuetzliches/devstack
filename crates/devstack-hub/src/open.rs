//! Opening a stack's tree in the user's own programs (ADR-0046):
//! `GET /hub/openers`, `POST /hub/open`, and an agent started with a prompt
//! the hub wrote (`POST /hub/agent`, amendment 2026-10-22); each program's
//! own icon (`GET /hub/openers/{id}/icon`, addendum 2026-11-23).
//!
//! A fixed list. Each program is found the way it makes itself known: an
//! editor or agent by the URL scheme it registered (`vscode`, `claude`, ...),
//! started by a link of that scheme, as the shell opens it; a terminal and
//! Orca by their program on `PATH`; GitHub Desktop by its install folder. The page never names a program or a
//! command line, only one of the ids found here. How each is found and
//! started on this operating system is `devstack_desktop`'s.

use devstack_desktop::{IconOf, LocalKind};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

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
fn links() -> Vec<(&'static str, &'static str, Kind)> {
    let mut out = EDITORS.to_vec();
    for a in devstack_skills::agents::all() {
        for l in a.open.iter().flat_map(|o| &o.links) {
            out.push((l.scheme.as_str(), l.label.as_str(), Kind::Agent));
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
    for &(scheme, label, kind) in &links {
        if registered.contains(&scheme) {
            out.push(Found {
                opener: Opener {
                    id: scheme,
                    label,
                    kind,
                    prompt: (kind == Kind::Agent).then_some(PromptMode::Prefill),
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
            },
            how: How::GitHubDesktop(exe),
        });
    }
    if let Some(orca) = devstack_desktop::orca() {
        out.push(Found {
            opener: Opener {
                id: "orca",
                label: "Orca",
                kind: Kind::Agent,
                prompt: Some(PromptMode::Send),
            },
            how: How::Orca(orca),
        });
    }
    // Stable: within a kind, the order above.
    out.sort_by_key(|f| f.opener.kind);
    out
}

/// The icon of the program `id`, as a PNG (`GET /hub/openers/{id}/icon`,
/// addendum 2026-11-23): read from the program installed here, once per hub.
/// `None` for an id not found here, or a program the system shows no icon
/// for.
pub fn icon(id: &str) -> Option<Arc<Vec<u8>>> {
    type Png = Option<Arc<Vec<u8>>>;
    static READ: OnceLock<Mutex<HashMap<String, Png>>> = OnceLock::new();
    let read = READ.get_or_init(Default::default);
    if let Some(png) = read.lock().unwrap().get(id) {
        return png.clone();
    }
    // Not kept: a program installed later is found by a later page.
    let f = found().into_iter().find(|f| f.opener.id == id)?;
    let png = devstack_desktop::icon(match &f.how {
        How::Local(l) => IconOf::Local(l),
        How::Link(scheme) => IconOf::Scheme(scheme),
        How::Orca(cli) => IconOf::Orca(cli),
        How::GitHubDesktop(exe) => IconOf::File(exe),
    })
    .map(Arc::new);
    read.lock().unwrap().insert(id.to_string(), png.clone());
    png
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

/// Start the Git client `id` to clone a repository that is not on this
/// machine yet (ADR-0046, amendment 2026-11-24): with `url` (checked by
/// [`clone_url`]) its clone dialog, the address filled in; without, the
/// program alone. Where the clone goes, the user chooses there.
pub fn clone(id: &str, url: Option<&str>) -> Result<(), OpenError> {
    let f = found()
        .into_iter()
        .find(|f| f.opener.id == id && f.opener.kind == Kind::Git)
        .ok_or(OpenError::Unknown)?;
    let _front = devstack_desktop::Front::take();
    match (&f.how, url) {
        // Its own link: `openRepo/<url>` opens the clone dialog.
        (How::GitHubDesktop(_), Some(url)) => {
            devstack_desktop::shell_open(&format!("x-github-client://openRepo/{url}"))
        }
        (How::GitHubDesktop(exe), None) => devstack_desktop::github_desktop_start(exe),
        _ => return Err(OpenError::Unknown),
    }
    .map_err(OpenError::Failed)
}

/// A repository's address as it may be handed to a Git client: `https://`, a
/// host, and only the characters of a path. No query or fragment, which the
/// client's link would read as its own parameters (`?branch=`).
pub fn clone_url(s: &str) -> Option<&str> {
    let s = s.trim();
    let rest = s.strip_prefix("https://")?;
    let host = rest.split('/').next()?;
    let allowed = |c: char| c.is_ascii_alphanumeric() || "-._~/:@%+".contains(c);
    // Encoded, too: a client that decodes before it parses reads them
    // (`%3F`, `%23`, and `%25` for a second round).
    let lower = rest.to_ascii_lowercase();
    let encoded = ["%3f", "%23", "%25"].iter().any(|e| lower.contains(e));
    (!host.is_empty() && s.len() <= 2048 && rest.chars().all(allowed) && !encoded).then_some(s)
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
    fn a_clone_url_is_https_and_a_plain_path() {
        assert_eq!(
            clone_url(" https://github.com/acme/shop.git "),
            Some("https://github.com/acme/shop.git")
        );
        assert_eq!(
            clone_url("https://git.example.com:8443/team/shop"),
            Some("https://git.example.com:8443/team/shop")
        );
        assert_eq!(clone_url("http://github.com/acme/shop"), None);
        assert_eq!(clone_url("git@github.com:acme/shop.git"), None);
        assert_eq!(clone_url("https:///acme/shop"), None);
        assert_eq!(clone_url("https://github.com/acme/shop?branch=x"), None);
        assert_eq!(clone_url("https://github.com/acme/shop#x"), None);
        assert_eq!(clone_url("https://github.com/acme/my shop"), None);
        assert_eq!(clone_url("https://github.com/a\"b"), None);
        // Security review before 0.54.0, S2: a query or fragment, encoded.
        assert_eq!(clone_url("https://github.com/acme/shop%3Fbranch=x"), None);
        assert_eq!(clone_url("https://github.com/acme/shop%3fbranch=x"), None);
        assert_eq!(clone_url("https://github.com/acme/shop%23x"), None);
        assert_eq!(clone_url("https://github.com/acme/shop%253Fx"), None);
        // An encoded space stays (Azure DevOps project names).
        assert_eq!(
            clone_url("https://dev.azure.com/org/My%20Project/_git/shop"),
            Some("https://dev.azure.com/org/My%20Project/_git/shop")
        );
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
