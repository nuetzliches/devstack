//! When each stack's repository last got a commit, for the dashboard's
//! "Last edited first" order.
//!
//! `scan` runs every second and must not wait for git, so a thread of its own
//! asks git once a minute and `scan` only reads what it found.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The newest commit (Unix seconds) by repository folder; a folder without
/// one (not a repository, no commit yet) is not in the map.
pub(crate) type Commits = Arc<Mutex<HashMap<PathBuf, u64>>>;

const EVERY: Duration = Duration::from_secs(60);

/// Start the thread that keeps the map current; it ends with the process.
pub(crate) fn spawn() -> Commits {
    let commits: Commits = Arc::default();
    let shared = commits.clone();
    std::thread::spawn(move || {
        loop {
            let repos: Vec<PathBuf> = crate::registry::list()
                .into_iter()
                .map(|e| e.repo)
                .collect();
            let mut found = HashMap::new();
            for repo in repos {
                if let Some(t) = newest(&repo) {
                    found.insert(repo, t);
                }
            }
            *shared.lock().unwrap() = found;
            std::thread::sleep(EVERY);
        }
    });
    commits
}

/// The newest commit over every ref of `repo`: its branches, so its
/// worktrees' too, and the remote ones it knows.
pub(crate) fn newest(repo: &Path) -> Option<u64> {
    crate::branches::newest_commit(repo)
}

/// `git log -1 --all --format=%ct` printed as seconds.
pub(crate) fn parse(out: &str) -> Option<u64> {
    out.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_git_s_answer() {
        assert_eq!(parse("1762876800\n"), Some(1_762_876_800));
    }

    #[test]
    fn no_commit_is_none() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("fatal: bad default revision"), None);
    }
}
