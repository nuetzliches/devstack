//! Whether a lane is done (#606): its work is on the base branch, merged or
//! squashed, and nothing keeps its worktree (uncommitted changes, a process
//! in it). devstack only says so, with the commands that remove it in their
//! order; removing worktrees and branches stays git's and the user's.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Where a lane's work stands against the base branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OnBase {
    /// Its HEAD is an ancestor of the base.
    Merged,
    /// Every file it changed since the merge base has the same content in a
    /// later commit on the base: the last of those commits (short).
    Squashed(String),
    /// It has commits whose changes the base does not hold.
    Open,
}

/// What `devstack lane list` says of one lane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Done {
    /// On the base, nothing keeps it: it can go.
    Finished(OnBase),
    /// On the base, with uncommitted or untracked changes.
    Dirty(OnBase),
    /// On the base, but the lane runs or a process is in its worktree.
    Busy(OnBase, String),
    Open,
    /// No base branch, or git could not say.
    Unknown,
}

impl Done {
    /// The `DONE` column: `merged`, `squashed <sha>`, `open`, `dirty`, `busy`.
    pub fn label(&self) -> String {
        match self {
            Done::Finished(OnBase::Merged) => "merged".into(),
            Done::Finished(OnBase::Squashed(c)) => format!("squashed {c}"),
            Done::Finished(OnBase::Open) | Done::Open => "open".into(),
            Done::Dirty(_) => "dirty".into(),
            Done::Busy(..) => "busy".into(),
            Done::Unknown => "?".into(),
        }
    }

    /// One line on what it means and what to do, for a lane on the base.
    pub fn hint(&self, lane: &str, path: &str, branch: Option<&str>) -> Option<String> {
        let how = |b: &OnBase| match b {
            OnBase::Merged => "merged into the base".to_string(),
            OnBase::Squashed(c) => format!("squashed into the base ({c})"),
            OnBase::Open => "open".to_string(),
        };
        match self {
            Done::Finished(b) => {
                let mut steps = vec![format!("git worktree remove \"{path}\"")];
                if let Some(br) = branch {
                    // A squashed branch is not merged for git: `-d` refuses it.
                    let flag = if matches!(b, OnBase::Squashed(_)) {
                        "-D"
                    } else {
                        "-d"
                    };
                    steps.push(format!("git branch {flag} {br}"));
                }
                steps.push("devstack lane prune".into());
                Some(format!(
                    "{lane}: done, {}; to remove it: {}",
                    how(b),
                    steps.join(", then ")
                ))
            }
            Done::Dirty(b) => Some(format!(
                "{lane}: {}, but its worktree has uncommitted or untracked changes",
                how(b)
            )),
            Done::Busy(b, who) if who == RUNS => Some(format!(
                "{lane}: {}, but the lane runs: `devstack lane stop {lane}` first",
                how(b)
            )),
            Done::Busy(b, who) => Some(format!("{lane}: {}, but {who}", how(b))),
            _ => None,
        }
    }

    /// As a word for the MCP's `lanes`: the label, and why for dirty or busy.
    pub fn text(&self) -> String {
        match self {
            Done::Busy(_, who) => format!("busy ({who})"),
            Done::Dirty(_) => "dirty (uncommitted changes)".into(),
            d => d.label(),
        }
    }
}

/// Run git in `dir`; its output when it succeeded.
fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let o = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "core.quotePath=false"])
        .args(args)
        .output()
        .ok()?;
    o.status
        .success()
        .then(|| String::from_utf8_lossy(&o.stdout).into_owned())
}

/// The base branch to compare with, as a ref: the remote's default branch
/// (`origin/HEAD`), else `origin/main`, else a local `main` or `master`.
/// Remote refs as last fetched: no fetch here.
pub fn base(repo: &Path) -> Option<String> {
    if let Some(r) = git(repo, &["symbolic-ref", "-q", "refs/remotes/origin/HEAD"]) {
        let r = r.trim();
        if !r.is_empty() {
            return Some(r.to_string());
        }
    }
    [
        "refs/remotes/origin/main",
        "refs/heads/main",
        "refs/heads/master",
    ]
    .into_iter()
    .find(|r| git(repo, &["rev-parse", "--verify", "-q", r]).is_some())
    .map(str::to_string)
}

/// Where the worktree's HEAD stands against `base`.
pub fn on_base(wt: &Path, base: &str) -> Option<OnBase> {
    if git(wt, &["merge-base", "--is-ancestor", "HEAD", base]).is_some() {
        return Some(OnBase::Merged);
    }
    let mb = git(wt, &["merge-base", "HEAD", base])?;
    let mb = mb.trim();
    // What the lane's commits leave, per path: the new blob, `None` deleted.
    let raw = git(
        wt,
        &["diff", "--raw", "--no-abbrev", "--no-renames", mb, "HEAD"],
    )?;
    let mut want: BTreeMap<String, Option<String>> = raw_entries(&raw)
        .into_iter()
        .map(|(path, blob, deleted)| (path, (!deleted).then_some(blob)))
        .collect();
    if want.is_empty() {
        // Commits that undo each other: nothing the base would lack.
        return Some(OnBase::Merged);
    }
    // The base's commits since, oldest first, with what each wrote.
    let range = format!("{mb}..{base}");
    let log = git(
        wt,
        &[
            "log",
            "--reverse",
            "--format=%x01%H",
            "--raw",
            "--no-abbrev",
            "--no-renames",
            &range,
        ],
    )?;
    let mut commit = "";
    let mut last = String::new();
    for line in log.lines() {
        if let Some(c) = line.strip_prefix('\u{1}') {
            commit = c.trim();
            continue;
        }
        for (path, blob, deleted) in raw_entries(line) {
            let matched = match want.get(&path) {
                Some(Some(b)) => !deleted && *b == blob,
                Some(None) => deleted,
                None => false,
            };
            if matched {
                want.remove(&path);
                last = commit.to_string();
            }
        }
        if want.is_empty() {
            return Some(OnBase::Squashed(last.chars().take(7).collect()));
        }
    }
    Some(OnBase::Open)
}

/// `:100644 100644 <old> <new> M\t<path>` lines: path, new blob, deleted.
fn raw_entries(raw: &str) -> Vec<(String, String, bool)> {
    raw.lines()
        .filter_map(|l| {
            let (meta, path) = l.strip_prefix(':')?.split_once('\t')?;
            let f: Vec<&str> = meta.split_whitespace().collect();
            let (blob, status) = (f.get(3)?, f.get(4)?);
            Some((path.to_string(), blob.to_string(), status.starts_with('D')))
        })
        .collect()
}

/// Uncommitted or untracked changes (not ignored ones) in the worktree.
pub fn dirty(wt: &Path) -> bool {
    git(wt, &["status", "--porcelain"]).is_none_or(|s| !s.trim().is_empty())
}

/// The processes of this machine, with the folders their working directory,
/// program and arguments name; without this process and the ones that
/// started it (the shell that asks is not what keeps a worktree).
pub struct Procs(Vec<(String, Vec<String>)>);

impl Procs {
    pub fn read() -> Procs {
        use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
        // Without the process table nothing reads as busy: the rest still says.
        let Ok(mut sys) = System::new() else {
            return Procs(Vec::new());
        };
        sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing()
                .with_cwd(UpdateKind::Always)
                .with_cmd(UpdateKind::Always)
                .with_exe(UpdateKind::Always),
        );
        let mut askers = Vec::new();
        let mut cur = Some(Pid::from_u32(std::process::id()));
        while let Some(p) = cur.filter(|p| !askers.contains(p)) {
            askers.push(p);
            cur = sys.process(p).and_then(|x| x.parent());
        }
        let list = sys
            .processes()
            .iter()
            .filter(|(pid, _)| !askers.contains(pid))
            .map(|(pid, p)| {
                let mut paths: Vec<String> = Vec::new();
                paths.extend(p.cwd().map(norm));
                paths.extend(p.exe().map(norm));
                paths.extend(p.cmd().iter().map(|a| norm(Path::new(a))));
                (format!("{} (pid {pid})", p.name().to_string_lossy()), paths)
            })
            .collect();
        Procs(list)
    }

    /// The first process with a folder in `wt`: `vite.exe (pid 4242)`.
    pub fn in_worktree(&self, wt: &Path) -> Option<&str> {
        let wt = norm(wt);
        self.0
            .iter()
            .find(|(_, paths)| paths.iter().any(|p| inside(p, &wt)))
            .map(|(who, _)| who.as_str())
    }
}

/// A path to compare: `/` separators, no `\\?\`, no trailing `/`, and on
/// Windows in lower case (its paths do not tell case apart).
fn norm(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    let s = s
        .strip_prefix("//?/")
        .unwrap_or(&s)
        .trim_end_matches('/')
        .to_string();
    if cfg!(windows) { s.to_lowercase() } else { s }
}

fn inside(p: &str, wt: &str) -> bool {
    !wt.is_empty() && (p == wt || p.strip_prefix(wt).is_some_and(|r| r.starts_with('/')))
}

/// Why a started lane is busy.
const RUNS: &str = "the lane runs";

/// What `devstack lane list` says of the lane in `wt`: git first, and only
/// for a lane on the base whether something keeps it.
pub fn of(wt: &Path, base: Option<&str>, active: bool, procs: &Procs) -> Done {
    let Some(on) = base.and_then(|b| on_base(wt, b)) else {
        return Done::Unknown;
    };
    if on == OnBase::Open {
        return Done::Open;
    }
    if active {
        return Done::Busy(on, RUNS.into());
    }
    if dirty(wt) {
        return Done::Dirty(on);
    }
    if let Some(who) = procs.in_worktree(wt) {
        return Done::Busy(on, format!("{who} runs in its worktree"));
    }
    Done::Finished(on)
}

/// The worktree of a lane as the daemon names it.
pub fn path_of(l: &serde_json::Value) -> Option<PathBuf> {
    l["path"]
        .as_str()
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(dir: &Path, args: &[&str]) -> String {
        let o = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.com",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(o.status.success(), "git {args:?}: {o:?}");
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }

    fn commit(dir: &Path, file: &str, text: &str, msg: &str) {
        std::fs::write(dir.join(file), text).unwrap();
        run(dir, &["add", "-A"]);
        run(dir, &["commit", "-q", "-m", msg]);
    }

    /// A repository on `main` and a worktree on branch `feat` beside it.
    fn repo() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let main = d.path().join("main");
        std::fs::create_dir(&main).unwrap();
        run(&main, &["init", "-q", "-b", "main"]);
        commit(&main, "a.txt", "a\n", "a");
        let wt = d.path().join("wt");
        run(
            &main,
            &["worktree", "add", "-q", "-b", "feat", wt.to_str().unwrap()],
        );
        (d, main, wt)
    }

    #[test]
    fn the_base_is_the_remotes_default_else_main() {
        let (_d, main, _wt) = repo();
        assert_eq!(base(&main).as_deref(), Some("refs/heads/main"));
        let head = run(&main, &["rev-parse", "HEAD"]);
        run(&main, &["update-ref", "refs/remotes/origin/trunk", &head]);
        run(
            &main,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/trunk",
            ],
        );
        assert_eq!(base(&main).as_deref(), Some("refs/remotes/origin/trunk"));
    }

    #[test]
    fn a_branch_whose_head_is_on_the_base_is_merged() {
        let (_d, main, wt) = repo();
        commit(&wt, "b.txt", "b\n", "b");
        assert_eq!(on_base(&wt, "main"), Some(OnBase::Open));
        run(&main, &["merge", "-q", "--ff-only", "feat"]);
        assert_eq!(on_base(&wt, "main"), Some(OnBase::Merged));
        assert_eq!(
            of(&wt, Some("main"), false, &Procs(Vec::new())),
            Done::Finished(OnBase::Merged)
        );
    }

    /// The case `git branch --merged` and `git cherry` miss: the branch's
    /// commits came to the base as one squash commit, with other work after.
    #[test]
    fn a_squashed_branch_is_found_by_the_content_of_its_files() {
        let (_d, main, wt) = repo();
        commit(&wt, "b.txt", "b1\n", "b1");
        commit(&wt, "b.txt", "b2\n", "b2");
        commit(&wt, "a.txt", "a, changed\n", "a");
        std::fs::remove_file(wt.join("a.txt")).unwrap();
        commit(&wt, "c.txt", "c\n", "a gone, c");
        commit(&main, "other.txt", "o\n", "other work first");
        // The squash commit, then a later change to one of its files.
        std::fs::write(main.join("b.txt"), "b2\n").unwrap();
        std::fs::write(main.join("c.txt"), "c\n").unwrap();
        std::fs::remove_file(main.join("a.txt")).unwrap();
        run(&main, &["add", "-A"]);
        run(&main, &["commit", "-q", "-m", "feat (#1)"]);
        let squash = run(&main, &["rev-parse", "--short=7", "HEAD"]);
        commit(&main, "b.txt", "b3\n", "later");
        assert_eq!(on_base(&wt, "main"), Some(OnBase::Squashed(squash)));
    }

    #[test]
    fn a_squash_of_only_part_of_the_work_leaves_it_open() {
        let (_d, main, wt) = repo();
        commit(&wt, "b.txt", "b\n", "b");
        commit(&wt, "c.txt", "c\n", "c");
        commit(&main, "b.txt", "b\n", "only b");
        assert_eq!(on_base(&wt, "main"), Some(OnBase::Open));
        // A file with other content on the base is not the lane's either.
        commit(&main, "c.txt", "c, but else\n", "c else");
        assert_eq!(on_base(&wt, "main"), Some(OnBase::Open));
    }

    #[test]
    fn a_lane_on_the_base_is_kept_by_changes_a_run_or_a_process() {
        let (_d, _main, wt) = repo();
        let none = Procs(Vec::new());
        assert_eq!(
            of(&wt, Some("main"), false, &none),
            Done::Finished(OnBase::Merged)
        );
        assert_eq!(
            of(&wt, Some("main"), true, &none),
            Done::Busy(OnBase::Merged, "the lane runs".into())
        );
        let vite = Procs(vec![(
            "node (pid 7)".into(),
            vec![
                norm(Path::new("/usr/bin/node")),
                norm(&wt.join("ui/node_modules/.bin/vite")),
            ],
        )]);
        assert_eq!(
            of(&wt, Some("main"), false, &vite),
            Done::Busy(OnBase::Merged, "node (pid 7) runs in its worktree".into())
        );
        std::fs::write(wt.join("new.txt"), "x").unwrap();
        assert_eq!(
            of(&wt, Some("main"), false, &none),
            Done::Dirty(OnBase::Merged)
        );
        // An open lane is open, whatever else.
        commit(&wt, "new.txt", "x", "x");
        assert_eq!(of(&wt, Some("main"), true, &vite), Done::Open);
        assert_eq!(of(&wt, None, false, &none), Done::Unknown);
    }

    #[test]
    fn a_folder_is_inside_only_by_whole_components() {
        assert!(inside("/w/lane", "/w/lane"));
        assert!(inside("/w/lane/ui", "/w/lane"));
        assert!(!inside("/w/lane-2/ui", "/w/lane"));
        assert!(!inside("/w", ""));
        assert_eq!(
            norm(Path::new(r"\\?\C:\W\Lane\")),
            if cfg!(windows) {
                "c:/w/lane"
            } else {
                "C:/W/Lane"
            }
        );
    }

    #[test]
    fn the_hint_names_the_commands_in_their_order() {
        let d = Done::Finished(OnBase::Squashed("1a2b3c4".into()));
        assert_eq!(
            d.hint("fix-x", "/w/fix-x", Some("fix/x")).unwrap(),
            "fix-x: done, squashed into the base (1a2b3c4); to remove it: \
             git worktree remove \"/w/fix-x\", then git branch -D fix/x, then devstack lane prune"
        );
        let m = Done::Finished(OnBase::Merged)
            .hint("x", "/w/x", Some("x"))
            .unwrap();
        assert!(
            m.ends_with("git branch -d x, then devstack lane prune"),
            "{m}"
        );
        let runs = Done::Busy(OnBase::Merged, RUNS.into())
            .hint("x", "/w/x", None)
            .unwrap();
        assert!(runs.ends_with("`devstack lane stop x` first"), "{runs}");
        assert_eq!(Done::Open.hint("x", "/w/x", None), None);
    }
}
