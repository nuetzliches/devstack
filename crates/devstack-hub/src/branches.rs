//! A stack's branches, and lanes made from them (ADR-0047):
//!
//! - `GET /hub/stacks/{id}/branches`: its local branches and the
//!   remote-tracking ones no local branch has, each with the worktree that
//!   has it checked out; `fetched`: when it last fetched.
//! - `POST /hub/stacks/{id}/worktrees {branch, new?}`: a worktree (a lane)
//!   on `branch`, or on a new branch `new` from it: `{path}`.
//! - `POST /hub/stacks/{id}/worktrees/remove {path}`: `git worktree remove`,
//!   without `--force`.
//! - `POST /hub/stacks/{id}/fetch`: `git fetch --all --prune`.
//!
//! The page names a branch the hub lists and a worktree git lists; the
//! folder of a new one is the hub's. git never asks: no prompt, no window,
//! and a time limit.

use serde::Serialize;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long git may take to list or change worktrees.
const LOCAL: Duration = Duration::from_secs(30);
/// How long a fetch may take.
const FETCH: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Branch {
    /// `feat/login`, or `origin/feat/login` for a remote-tracking one.
    pub name: String,
    /// The remote of a remote-tracking branch; absent for a local one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
    /// What a local branch tracks (`origin/feat/login`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream: Option<String>,
    /// The worktree that has it checked out.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree: Option<PathBuf>,
    /// That worktree is the main working tree.
    pub main: bool,
    /// Its last commit, Unix seconds.
    pub date: i64,
}

#[derive(Debug, Serialize)]
pub struct Branches {
    pub branches: Vec<Branch>,
    /// When the repository last fetched (Unix seconds); absent: never.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fetched: Option<i64>,
}

/// Why a request was refused: the page's mistake, or git's answer.
#[derive(Debug)]
pub enum Error {
    Bad(String),
    Git(String),
}

/// `git` in `repo`, without a prompt or a window, ended after `limit`:
/// its output, or what it said on failure.
fn git(repo: &Path, args: &[&str], limit: Duration) -> Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.args(args)
        .current_dir(repo)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    crate::no_window(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| format!("run git: {e}"))?;
    // Read both pipes while it runs: a full pipe would stop it.
    let drain = |p: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut v = Vec::new();
            if let Some(mut p) = p {
                let _ = p.read_to_end(&mut v);
            }
            v
        })
    };
    let out = drain(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let err = drain(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let deadline = Instant::now() + limit;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(25)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "git {} did not finish within {} s",
                    args[0],
                    limit.as_secs()
                ));
            }
            Err(e) => return Err(format!("git {}: {e}", args[0])),
        }
    };
    let out = out.join().unwrap_or_default();
    let err = err.join().unwrap_or_default();
    if status.success() {
        Ok(String::from_utf8_lossy(&out).into_owned())
    } else {
        Err(String::from_utf8_lossy(&err).trim().to_string())
    }
}

/// The repository's git directory shared by its worktrees.
fn common_dir(repo: &Path) -> Result<PathBuf, String> {
    let d = git(repo, &["rev-parse", "--git-common-dir"], LOCAL)?;
    Ok(repo.join(d.trim()))
}

/// The branches of `repo`, local ones first, then each group by its last
/// commit, newest first.
pub fn list(repo: &Path) -> Result<Branches, Error> {
    let refs = git(
        repo,
        &[
            "for-each-ref",
            "--format=%(refname)%00%(upstream:short)%00%(committerdate:unix)",
            "refs/heads",
            "refs/remotes",
        ],
        LOCAL,
    )
    .map_err(Error::Git)?;
    let trees = devstack_daemon::lanes::worktree_branches(repo).unwrap_or_default();
    let mut branches = parse(&refs, &trees, repo);
    branches.sort_by(|a, b| {
        (a.remote.is_some(), -a.date, &a.name).cmp(&(b.remote.is_some(), -b.date, &b.name))
    });
    let fetched = common_dir(repo)
        .ok()
        .and_then(|d| std::fs::metadata(d.join("FETCH_HEAD")).ok())
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64);
    Ok(Branches { branches, fetched })
}

/// `for-each-ref` lines (`<ref>\0<upstream>\0<date>`) as branches: a
/// remote's `HEAD` and a remote-tracking branch that a local one tracks or
/// is named after are left out (the local one stands for it).
fn parse(refs: &str, trees: &[(PathBuf, Option<String>)], repo: &Path) -> Vec<Branch> {
    let mut local = Vec::new();
    let mut remote = Vec::new();
    for line in refs.lines() {
        let mut f = line.split('\0');
        let (Some(r), Some(up), Some(date)) = (f.next(), f.next(), f.next()) else {
            continue;
        };
        let date = date.trim().parse().unwrap_or(0);
        if let Some(name) = r.strip_prefix("refs/heads/") {
            let tree = trees
                .iter()
                .find(|(_, b)| b.as_deref() == Some(name))
                .map(|(p, _)| p.clone());
            local.push(Branch {
                name: name.to_string(),
                remote: None,
                upstream: (!up.is_empty()).then(|| up.to_string()),
                main: tree
                    .as_deref()
                    .is_some_and(|t| devstack_daemon::lanes::same_tree(t, repo)),
                worktree: tree,
                date,
            });
        } else if let Some(name) = r.strip_prefix("refs/remotes/") {
            let Some((rem, short)) = name.split_once('/') else {
                continue;
            };
            if short == "HEAD" {
                continue;
            }
            remote.push(Branch {
                name: name.to_string(),
                remote: Some(rem.to_string()),
                upstream: None,
                worktree: None,
                main: false,
                date,
            });
        }
    }
    remote.retain(|r| {
        let short = &r.name[r.remote.as_deref().unwrap_or("").len() + 1..];
        !local
            .iter()
            .any(|l| l.name == short || l.upstream.as_deref() == Some(r.name.as_str()))
    });
    local.extend(remote);
    local
}

/// Where a new worktree for `name` goes (ADR-0047 point 3): beside the
/// repository's most recently added worktree, else in `<repo>.worktrees`
/// beside the repository; named as its lane will be, a taken name with
/// `-2`, `-3`.
fn folder(repo: &Path, name: &str) -> Result<PathBuf, String> {
    let latest = std::fs::read_dir(common_dir(repo)?.join("worktrees"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let at = e.metadata().ok()?;
            let at = at.created().or_else(|_| at.modified()).ok()?;
            let gitdir = std::fs::read_to_string(e.path().join("gitdir")).ok()?;
            // `<worktree>/.git`: the worktree's parent is where they go.
            let dir = Path::new(gitdir.trim()).parent()?.parent()?.to_path_buf();
            dir.is_dir().then_some((at, dir))
        })
        .max_by_key(|(at, _)| *at)
        .map(|(_, d)| d);
    let parent = match latest {
        Some(d) => d,
        None => {
            let base = repo
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| format!("{} has no folder name", repo.display()))?;
            repo.parent()
                .ok_or_else(|| format!("{} has no parent folder", repo.display()))?
                .join(format!("{base}.worktrees"))
        }
    };
    let base = devstack_daemon::lanes::lane_name(name);
    let base = if base.is_empty() || base == "main" {
        format!("lane-{base}").trim_end_matches('-').to_string()
    } else {
        base
    };
    (1..)
        .map(|i| {
            parent.join(if i == 1 {
                base.clone()
            } else {
                format!("{base}-{i}")
            })
        })
        .find(|p| !p.exists())
        .ok_or_else(|| "no free folder name".to_string())
}

/// A worktree on `branch` (one [`list`] gives), or on a new branch `new`
/// made from it: its folder.
pub fn add(repo: &Path, branch: &str, new: Option<&str>) -> Result<PathBuf, Error> {
    let all = list(repo)?.branches;
    let Some(b) = all.iter().find(|b| b.name == branch) else {
        return Err(Error::Bad(format!(
            "no branch `{branch}` in this repository"
        )));
    };
    if let Some(n) = new {
        // git's own rules for a branch name; `-x` is no name but an option.
        let ok =
            !n.starts_with('-') && git(repo, &["check-ref-format", "--branch", n], LOCAL).is_ok();
        if !ok {
            return Err(Error::Bad(format!("`{n}` is no valid branch name")));
        }
        if all.iter().any(|b| b.remote.is_none() && b.name == n) {
            return Err(Error::Bad(format!("a branch `{n}` exists already")));
        }
    } else if let Some(t) = &b.worktree {
        return Err(Error::Bad(format!(
            "`{branch}` is checked out in {} already",
            t.display()
        )));
    }
    // A remote-tracking branch gets a local one of its name that tracks it.
    let local = match (&b.remote, new) {
        (_, Some(n)) => Some(n.to_string()),
        (Some(r), None) => Some(b.name[r.len() + 1..].to_string()),
        (None, None) => None,
    };
    let dir = folder(repo, local.as_deref().unwrap_or(branch)).map_err(Error::Git)?;
    if let Some(p) = dir.parent() {
        std::fs::create_dir_all(p).map_err(|e| Error::Git(format!("{}: {e}", p.display())))?;
    }
    let path = dir.to_string_lossy().into_owned();
    let mut args = vec!["worktree", "add"];
    if let Some(l) = &local {
        if b.remote.is_some() && new.is_none() {
            args.push("--track");
        }
        args.extend(["-b", l.as_str()]);
    }
    args.extend(["--", path.as_str(), branch]);
    git(repo, &args, LOCAL).map_err(Error::Git)?;
    Ok(dir)
}

/// `git worktree remove` of a linked worktree of `repo` (not the main
/// working tree); git refuses one with changes.
pub fn remove(repo: &Path, path: &Path) -> Result<(), Error> {
    let Some(tree) = devstack_daemon::lanes::worktree_of(repo, path) else {
        return Err(Error::Bad("not a worktree of this stack".into()));
    };
    if devstack_daemon::lanes::same_tree(&tree, repo) {
        return Err(Error::Bad("the main working tree stays".into()));
    }
    let t = tree.to_string_lossy();
    git(repo, &["worktree", "remove", "--", &t], LOCAL).map_err(Error::Git)?;
    Ok(())
}

/// `git fetch --all --prune`, without a prompt.
pub fn fetch(repo: &Path) -> Result<(), Error> {
    git(repo, &["fetch", "--all", "--prune"], FETCH).map_err(Error::Git)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_ok(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A repository `app` with a commit on `main` and a branch `feat/login`.
    fn repo() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("app");
        std::fs::create_dir(&repo).unwrap();
        git_ok(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("a.txt"), "a").unwrap();
        git_ok(&repo, &["add", "."]);
        git_ok(&repo, &["commit", "-q", "-m", "a"]);
        git_ok(&repo, &["branch", "feat/login"]);
        (tmp, repo)
    }

    #[test]
    fn a_remote_branch_a_local_one_stands_for_is_left_out() {
        let refs = "refs/heads/main\0origin/main\x0010\n\
                    refs/heads/fix\0\x0020\n\
                    refs/remotes/origin/HEAD\0\x0010\n\
                    refs/remotes/origin/main\0\x0010\n\
                    refs/remotes/origin/fix\0\x0020\n\
                    refs/remotes/origin/feat/x\0\x0030\n";
        let repo = Path::new("/r");
        let trees = vec![(PathBuf::from("/r"), Some("main".to_string()))];
        let b = parse(refs, &trees, repo);
        let names: Vec<&str> = b.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, ["main", "fix", "origin/feat/x"]);
        assert!(b[0].main && b[0].worktree.is_some());
        assert_eq!(b[2].remote.as_deref(), Some("origin"));
    }

    #[test]
    fn a_lane_goes_beside_the_repository_named_by_its_branch() {
        let (tmp, repo) = repo();
        let dir = add(&repo, "feat/login", None).unwrap();
        assert_eq!(dir, tmp.path().join("app.worktrees").join("feat-login"));
        assert!(dir.join("a.txt").is_file());
        let b = list(&repo).unwrap().branches;
        let feat = b.iter().find(|b| b.name == "feat/login").unwrap();
        assert!(feat.worktree.is_some() && !feat.main);
        // Checked out already: refused, not a second worktree.
        assert!(matches!(add(&repo, "feat/login", None), Err(Error::Bad(_))));

        // A new branch from it goes beside the worktree just made.
        let next = add(&repo, "feat/login", Some("feat/next")).unwrap();
        assert_eq!(next, tmp.path().join("app.worktrees").join("feat-next"));

        remove(&repo, &next).unwrap();
        assert!(!next.exists());
        assert!(matches!(remove(&repo, &repo), Err(Error::Bad(_))));
    }

    #[test]
    fn a_remote_branch_gets_a_local_one_that_tracks_it() {
        let (tmp, origin) = repo();
        let clone = tmp.path().join("clone");
        git_ok(
            tmp.path(),
            &[
                "clone",
                "-q",
                origin.to_str().unwrap(),
                clone.to_str().unwrap(),
            ],
        );
        let b = list(&clone).unwrap().branches;
        assert!(b.iter().any(|b| b.name == "origin/feat/login"));
        // `main` is local, so `origin/main` is not listed of its own.
        assert!(!b.iter().any(|b| b.name == "origin/main"));
        let dir = add(&clone, "origin/feat/login", None).unwrap();
        assert!(dir.ends_with("feat-login"));
        let b = list(&clone).unwrap().branches;
        let feat = b.iter().find(|b| b.name == "feat/login").unwrap();
        assert_eq!(feat.upstream.as_deref(), Some("origin/feat/login"));
        assert!(feat.worktree.is_some());
        assert!(!b.iter().any(|b| b.name == "origin/feat/login"));
    }

    #[test]
    fn a_name_the_page_makes_up_is_refused() {
        let (_tmp, repo) = repo();
        assert!(matches!(add(&repo, "nope", None), Err(Error::Bad(_))));
        assert!(matches!(add(&repo, "main", Some("-f")), Err(Error::Bad(_))));
        assert!(matches!(
            add(&repo, "main", Some("a..b")),
            Err(Error::Bad(_))
        ));
        assert!(matches!(
            add(&repo, "main", Some("feat/login")),
            Err(Error::Bad(_))
        ));
    }

    #[test]
    fn worktrees_follow_the_ones_the_repository_has() {
        let (tmp, repo) = repo();
        let elsewhere = tmp.path().join("trees");
        std::fs::create_dir(&elsewhere).unwrap();
        git_ok(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                elsewhere.join("x").to_str().unwrap(),
                "-b",
                "x",
            ],
        );
        let dir = add(&repo, "feat/login", None).unwrap();
        assert_eq!(
            std::fs::canonicalize(dir.parent().unwrap()).unwrap(),
            std::fs::canonicalize(&elsewhere).unwrap()
        );
    }
}
