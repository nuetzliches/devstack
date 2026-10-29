//! Every registered stack's worktrees, whether its daemon runs or not
//! (ADR-0078):
//!
//! - The hub looks at `<git common dir>/worktrees` of each stack once a
//!   second (what the daemon of a running stack does too: its entries and
//!   their `HEAD`s, a local folder) and tells dashboards on `/hub/events`
//!   (`worktrees`: a version per stack) when one was added, removed or
//!   checked out another branch; the dashboard reads that stack's lanes
//!   again then.
//! - `GET /hub/stacks/{id}/worktrees`: a stack's worktrees as lanes not
//!   started, for a stack whose daemon does not run.
//! - `POST /hub/stacks/{id}/lanes/start {path, on?}`: start a worktree's
//!   lane as `devstack up` in it does: the daemon comes up, the main working
//!   tree's services stay stopped. `on`: a node of the pool or `pool`;
//!   without it, this machine.

use axum::Json;
use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;
use tokio::sync::watch;

/// How often the folders are looked at.
const EVERY: Duration = Duration::from_secs(1);

fn sender() -> &'static watch::Sender<BTreeMap<String, String>> {
    static TX: OnceLock<watch::Sender<BTreeMap<String, String>>> = OnceLock::new();
    TX.get_or_init(|| watch::channel(BTreeMap::new()).0)
}

/// Each stack's worktrees' version now, and each change.
pub fn subscribe() -> watch::Receiver<BTreeMap<String, String>> {
    sender().subscribe()
}

/// Start looking, once per hub.
pub fn start() {
    static ONCE: OnceLock<()> = OnceLock::new();
    if ONCE.set(()).is_err() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("worktrees".into())
        .spawn(|| {
            // A stack's git directory, asked of git once.
            let mut dirs: HashMap<String, Option<PathBuf>> = HashMap::new();
            loop {
                let stacks = devstack_daemon::registry::list();
                let mut now = BTreeMap::new();
                for e in &stacks {
                    let dir = dirs
                        .entry(e.id.clone())
                        .or_insert_with(|| crate::branches::common_dir(&e.repo).ok())
                        .clone();
                    let Some(dir) = dir else { continue };
                    let print = devstack_daemon::lanes::fingerprint(&dir.join("worktrees"));
                    now.insert(e.id.clone(), version(&print));
                }
                dirs.retain(|id, _| stacks.iter().any(|e| &e.id == id));
                sender().send_if_modified(|v| {
                    if *v == now {
                        false
                    } else {
                        *v = now;
                        true
                    }
                });
                std::thread::sleep(EVERY);
            }
        });
}

fn version(print: &[(std::ffi::OsString, Option<std::time::SystemTime>)]) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    print.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// A worktree as the dashboard's lane list has it, not started.
#[derive(Debug, Serialize)]
pub struct Worktree {
    pub name: String,
    pub path: PathBuf,
    pub branch: Option<String>,
    pub number: u32,
    pub active: bool,
    pub port_offset: u32,
    pub owns_main: bool,
    pub running: u32,
    pub services: u32,
    /// It has no stack file of its own: it runs with the main tree's.
    pub main_stack_file: bool,
    /// Why it cannot start on a node of the pool (ADR-0028).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub not_in_pool: Option<&'static str>,
}

/// The worktrees of the stack in `repo` with its stack file `file`, the
/// main working tree left out.
pub fn of(repo: &Path, file: &Path) -> Vec<Worktree> {
    let rel = file
        .strip_prefix(repo)
        .map(Path::to_path_buf)
        .unwrap_or_else(|_| PathBuf::from(file.file_name().unwrap_or("devstack.toml".as_ref())));
    devstack_daemon::lanes::worktree_branches(repo)
        .unwrap_or_default()
        .into_iter()
        .filter(|(p, _)| !devstack_daemon::lanes::same_tree(p, repo))
        .map(|(path, branch)| {
            let own = path.join(&rel);
            let has_own = own.is_file();
            // As the daemon names it (a second of one name gets `-2` there).
            let base = devstack_daemon::lanes::lane_name(
                &path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            );
            let name = if base.is_empty() || base == "main" {
                format!("lane-{base}").trim_end_matches('-').to_string()
            } else {
                base
            };
            Worktree {
                name,
                not_in_pool: devstack_daemon::lanes::not_in_pool(if has_own { &own } else { file }),
                main_stack_file: !has_own,
                path,
                branch,
                number: 0,
                active: false,
                port_offset: 0,
                owns_main: false,
                running: 0,
                services: 0,
            }
        })
        .collect()
}

/// `GET /hub/stacks/{id}/worktrees`.
pub async fn list(State(s): State<crate::AppState>, UrlPath(id): UrlPath<String>) -> Response {
    let Some(e) = crate::entry(&s, &id) else {
        return crate::err(StatusCode::NOT_FOUND, "no such stack");
    };
    match tokio::task::spawn_blocking(move || of(&e.repo, &e.file)).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => crate::err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[derive(Deserialize)]
pub struct StartBody {
    pub path: String,
    #[serde(default)]
    pub on: Option<String>,
}

/// `POST /hub/stacks/{id}/lanes/start {path, on?}`: `devstack up` in the
/// worktree `path` (one git lists for the stack), `--on local` without
/// `on`. `202` once started; a start that fails before the daemon runs is
/// the stack's `start_error`, as a stack's start.
pub async fn start_lane(
    State(s): State<crate::AppState>,
    UrlPath(id): UrlPath<String>,
    Json(b): Json<StartBody>,
) -> Response {
    let Some(e) = crate::entry(&s, &id) else {
        return crate::err(StatusCode::NOT_FOUND, "no such stack");
    };
    let on = b.on.map(|o| o.trim().to_string()).filter(|o| !o.is_empty());
    if on.as_deref().is_some_and(|o| {
        o.len() > 100
            || !o
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.:".contains(c))
    }) {
        return crate::err(
            StatusCode::BAD_REQUEST,
            "`on`: a node, `<pool>:<node>` or `pool`",
        );
    }
    let raw = PathBuf::from(&b.path);
    let repo = e.repo.clone();
    let tree = tokio::task::spawn_blocking(move || {
        devstack_daemon::lanes::worktree_of(&repo, &raw)
            .filter(|t| !devstack_daemon::lanes::same_tree(t, &repo))
    })
    .await
    .ok()
    .flatten();
    let Some(tree) = tree else {
        return crate::err(
            StatusCode::BAD_REQUEST,
            "not a worktree of this stack's repository",
        );
    };
    let Ok(exe) = crate::devstack_exe() else {
        return crate::err(StatusCode::INTERNAL_SERVER_ERROR, "cannot find devstack");
    };
    s.start_errors.lock().unwrap().remove(&id);
    let log = crate::state_dir().join(format!("up-{id}-lane.log"));
    let Ok((out, out2)) = std::fs::File::create(&log).and_then(|f| Ok((f.try_clone()?, f))) else {
        return crate::err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "cannot write the start's log",
        );
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("up")
        .arg("--on")
        .arg(on.as_deref().unwrap_or("local"))
        .current_dir(&tree)
        .env("NO_COLOR", "1")
        .stdin(std::process::Stdio::null())
        .stdout(out)
        .stderr(out2);
    crate::no_window(&mut cmd);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return crate::err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    let errors = s.start_errors.clone();
    let repo = e.repo.clone();
    std::thread::spawn(move || {
        if child.wait().is_ok_and(|st| st.success()) {
            return;
        }
        // A daemon that runs reports what went wrong with the lane itself.
        if devstack_daemon::read_info(&repo.join(".devstack")).is_some() {
            return;
        }
        let text = std::fs::read_to_string(&log).unwrap_or_default();
        let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
        let tail = lines[lines.len().saturating_sub(12)..].join("\n");
        crate::diag::warn(
            "lane_start_failed",
            serde_json::json!({ "id": id, "output": tail }),
        );
        errors.lock().unwrap().insert(
            id,
            if tail.is_empty() {
                "devstack up ended without a word".into()
            } else {
                tail
            },
        );
    });
    StatusCode::ACCEPTED.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    }

    #[test]
    fn a_stacks_worktrees_are_lanes_not_started_with_their_stack_file() {
        let d = tempfile::tempdir().unwrap();
        let repo = d.path().join("shop");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "t@example.com"]);
        git(&repo, &["config", "user.name", "t"]);
        std::fs::write(
            repo.join("devstack.toml"),
            "version = 1\n[stack]\nname = \"shop\"\n",
        )
        .unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "x"]);
        let common = crate::branches::common_dir(&repo).unwrap();
        let before = devstack_daemon::lanes::fingerprint(&common.join("worktrees"));
        let wt = d.path().join("shop.worktrees").join("Fix Login");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "fix/login",
                wt.to_str().unwrap(),
            ],
        );
        let after = devstack_daemon::lanes::fingerprint(&common.join("worktrees"));
        assert_ne!(
            version(&before),
            version(&after),
            "a new worktree changes the version"
        );

        let list = of(&repo, &repo.join("devstack.toml"));
        assert_eq!(list.len(), 1, "the main working tree is left out");
        assert_eq!(list[0].name, "fix-login");
        assert_eq!(list[0].branch.as_deref(), Some("fix/login"));
        assert!(!list[0].active && !list[0].main_stack_file);
        assert!(
            list[0].not_in_pool.is_some(),
            "no `[lanes] image`: not in the pool"
        );
    }
}
