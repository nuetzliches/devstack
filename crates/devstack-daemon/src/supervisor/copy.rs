//! A lane's copy of its worktree, and `prepare`.

use super::{COPY_QUIET, KEEP_BUILD_LOGS, STOPPED, Supervisor, tail};
use crate::state::now_ms;
use devstack_build::Snapshot;
use devstack_config::model::Service;
use devstack_proc::Spec;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

impl Supervisor {
    /// A lane runs from a copy of its worktree, never from the worktree itself
    /// (ADR-0024): git and the tools that manage worktrees can remove one at
    /// any time, and a process inside it would hold it open (Windows).
    pub(super) fn runs_from_copy(&self) -> bool {
        // A lane on a node runs from a copy of its mirror too: `prepare`
        // installs there what the mirror lacks (ignored files, node_modules).
        !self.lane.is_empty() || crate::remote_lane()
    }

    /// Where processes without an artifact run and what `{repo}` means for
    /// them: the main tree itself, or a lane's snapshot.
    pub fn run_root(&self) -> PathBuf {
        if self.runs_from_copy() {
            self.layout.lane("main").join("src")
        } else {
            self.repo.clone()
        }
    }

    /// What `prepare` of `svc` would run in `snap`; `None` when it ran for the
    /// same command and inputs already.
    pub(super) fn prepare_job(
        &self,
        name: &str,
        svc: &Service,
        snap: &Snapshot,
    ) -> Result<Option<PrepareJob>, String> {
        let Some(p) = &svc.prepare else {
            return Ok(None);
        };
        let cwd = p.cwd.as_ref().or(svc.cwd.as_ref());
        let mut h = blake3::Hasher::new();
        h.update(p.run.as_bytes());
        h.update(cwd.map_or("", String::as_str).as_bytes());
        if let Some(g) = devstack_build::watch_set(&p.inputs).map_err(|e| e.to_string())? {
            for (rel, e) in snap
                .manifest
                .files
                .iter()
                .filter(|(r, _)| g.is_match(r.as_str()))
            {
                h.update(rel.as_bytes());
                h.update(e.hash.as_bytes());
            }
        }
        let key = h.finalize().to_hex().to_string();
        let mark = self.layout.lane("main").join("prepared").join(name);
        // A linked directory that is gone (a `git worktree remove` followed
        // the link into it, ADR-0044) is prepared again.
        let linked_there =
            !self.links_prepared() || p.link.iter().all(|l| snap.dir.join(l).is_dir());
        if linked_there && std::fs::read_to_string(&mark).is_ok_and(|k| k == key) {
            return Ok(None);
        }
        Ok(Some(PrepareJob {
            key,
            mark,
            cwd: cwd.map_or_else(|| snap.dir.clone(), |c| snap.dir.join(c)),
            run: p.run.clone(),
        }))
    }

    /// Run a `prepare` to completion; its output goes to a log of its own.
    /// `cancel`: a stop came in, the prepare is killed.
    pub(super) fn run_prepare(
        &self,
        name: &str,
        job: PrepareJob,
        cancel: &dyn Fn() -> bool,
    ) -> Result<(), String> {
        let _one = self.preparing.lock().unwrap();
        let dir = self.log_dir(name);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        crate::diag::prune(&dir, "prepare-", KEEP_BUILD_LOGS);
        let log = dir.join(format!("prepare-{}.log", now_ms()));
        let file = std::fs::File::create(&log).map_err(|e| e.to_string())?;
        crate::diag::info(
            "prepare",
            serde_json::json!({ "lane": self.lane, "service": name, "run": job.run }),
        );
        std::fs::create_dir_all(&job.cwd).map_err(|e| e.to_string())?;
        let spec = Spec {
            name: format!("{name}-prepare"),
            command: job.run.clone(),
            cwd: job.cwd,
            env: BTreeMap::new(),
            log: file,
        };
        let (mut tree, _) =
            devstack_proc::spawn(spec, &self.proc_opts).map_err(|e| e.to_string())?;
        let code = loop {
            if let Some(code) = tree.try_exit_code() {
                break code;
            }
            if cancel() {
                tree.kill();
                return Err(format!("prepare `{}`: {STOPPED}", job.run));
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        if code != 0 {
            return Err(format!(
                "prepare `{}` exited with code {code}\n{}",
                job.run,
                tail(&log, 15)
            ));
        }
        if let Some(parent) = job.mark.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&job.mark, job.key).map_err(|e| e.to_string())
    }

    /// A lane's copy, current and prepared for `svc`, before it starts from it.
    pub(super) fn copy_ready(
        &self,
        name: &str,
        svc: &Service,
        cancel: &dyn Fn() -> bool,
    ) -> Result<(), String> {
        let job = {
            let mut snap = self.snapshot.lock().unwrap();
            snap.sync().map_err(|e| e.to_string())?;
            self.prepare_job(name, svc, &snap)?
        };
        if let Some(job) = job {
            self.run_prepare(name, job, cancel)?;
        }
        self.link_prepared(name, svc);
        Ok(())
    }

    /// Whether what `prepare` installs is linked into the worktree
    /// (ADR-0044): in a lane on this machine. The main tree runs in place
    /// with its own; a lane on a node has no worktree there.
    fn links_prepared(&self) -> bool {
        !self.lane.is_empty() && !crate::remote_lane()
    }

    /// After `prepare` of `svc`: each of its `link` paths the worktree does
    /// not have, linked to the prepared directory in the copy (a junction on
    /// Windows, which needs no privilege), so the worktree's git hooks find
    /// it (ADR-0044). What the worktree has already, a developer's own
    /// install or a link of before, stays as it is. A link git does not
    /// ignore is taken away again: `git add -A` would commit it.
    pub(super) fn link_prepared(&self, name: &str, svc: &Service) {
        let Some(p) = &svc.prepare else {
            return;
        };
        if !self.links_prepared() {
            return;
        }
        let copy = self.run_root();
        for rel in &p.link {
            let (link, target) = (self.repo.join(rel), copy.join(rel));
            if std::fs::symlink_metadata(&link).is_ok() {
                continue;
            }
            let not_linked = |why: String| {
                crate::diag::warn(
                    "prepare_not_linked",
                    serde_json::json!({ "lane": self.lane, "service": name, "path": rel, "why": why }),
                );
            };
            if !target.is_dir() {
                not_linked(format!(
                    "`prepare` made no directory `{rel}` in the lane's copy"
                ));
                continue;
            }
            if !link.parent().is_some_and(Path::is_dir) {
                not_linked("its parent directory is not in the worktree".into());
                continue;
            }
            if let Err(e) = make_link(&target, &link) {
                not_linked(e.to_string());
                continue;
            }
            if !git_ignores(&self.repo, rel) {
                let _ = remove_link(&link);
                not_linked(format!(
                    "git does not ignore `{rel}` as a link: ignore it without a trailing slash (`node_modules`, not `node_modules/`)"
                ));
                continue;
            }
            crate::diag::info(
                "prepare_linked",
                serde_json::json!({ "lane": self.lane, "service": name, "path": rel }),
            );
        }
    }

    /// The links [`Self::link_prepared`] made, removed: a lane that stops
    /// leaves no link in its worktree for `git worktree remove` to follow
    /// into its copy (ADR-0044). Only links to the copy; anything else at
    /// those paths is the developer's.
    pub(crate) fn unlink_prepared(&self) {
        if !self.links_prepared() {
            return;
        }
        let copy = self.run_root();
        for rel in self
            .stack
            .services
            .values()
            .filter_map(|s| s.prepare.as_ref())
            .flat_map(|p| &p.link)
        {
            let link = self.repo.join(rel);
            if !links_to(&link, &copy.join(rel)) {
                continue;
            }
            let r = remove_link(&link);
            crate::diag::info(
                "prepare_unlinked",
                serde_json::json!({ "lane": self.lane, "path": rel,
                    "error": r.err().map(|e| e.to_string()) }),
            );
        }
    }

    /// Before a command runs from it (ADR-0024 point 2): a lane's copy,
    /// current. The main tree runs commands in place.
    pub(crate) fn sync_copy(&self) -> Result<(), String> {
        if !self.runs_from_copy() {
            return Ok(());
        }
        let stats = self
            .snapshot
            .lock()
            .unwrap()
            .sync()
            .map_err(|e| e.to_string())?;
        self.log_sync("command", &stats);
        Ok(())
    }

    /// What a sync of the lane's copy changed under what runs from it: a
    /// dev server that ends right after one says why (#103).
    pub(super) fn log_sync(&self, why: &str, s: &devstack_build::snapshot::SyncStats) {
        if s.copied + s.removed == 0 {
            return;
        }
        crate::diag::info(
            "copy_synced",
            serde_json::json!({
                "lane": self.lane,
                "why": why,
                "copied": s.copied,
                "removed": s.removed,
                "files": s.files,
                "ms": s.elapsed.as_millis() as u64,
            }),
        );
    }

    /// Something changed in a lane's worktree: bring the copy its dev servers
    /// and tools run from up to date, after a short quiet time, so their own
    /// watchers (hot module replacement) see the change.
    pub fn copy_changed(self: &Arc<Self>) {
        if !self.runs_from_copy() {
            return;
        }
        self.debounce_for("copy".into(), COPY_QUIET, |s| {
            let from_copy = s
                .slots
                .lock()
                .unwrap()
                .values()
                .any(|slot| slot.main.as_ref().is_some_and(|i| i.build_id.is_none()));
            if !from_copy {
                return;
            }
            let synced = s.snapshot.lock().unwrap().sync();
            match synced {
                Ok(stats) => s.log_sync("change", &stats),
                Err(e) => crate::diag::warn(
                    "copy_not_synced",
                    serde_json::json!({ "lane": s.lane, "error": e.to_string() }),
                ),
            }
        });
    }
}

/// A `prepare` to run ([`Supervisor::prepare_job`]).
pub(super) struct PrepareJob {
    key: String,
    /// Holds `key` once it succeeded.
    mark: PathBuf,
    cwd: PathBuf,
    run: String,
}

/// A directory link at `link` to `target`: a junction on Windows (no
/// privilege needed, unlike a symbolic link), a symbolic link elsewhere.
fn make_link(target: &Path, link: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    return junction::create(target, link);
    #[cfg(not(windows))]
    return std::os::unix::fs::symlink(target, link);
}

/// Removes the link itself, never what it points to.
fn remove_link(link: &Path) -> std::io::Result<()> {
    // A junction is a directory to Windows; removing it leaves its target.
    if cfg!(windows) {
        std::fs::remove_dir(link)
    } else {
        std::fs::remove_file(link)
    }
}

/// Is `link` a link (a junction on Windows) to `target`?
fn links_to(link: &Path, target: &Path) -> bool {
    let is_link = std::fs::symlink_metadata(link).is_ok_and(|m| m.file_type().is_symlink());
    let norm = |p: &Path| {
        let s = p.to_string_lossy().replace('\\', "/");
        let s = s
            .strip_prefix("//?/")
            .or_else(|| s.strip_prefix("/??/"))
            .unwrap_or(&s)
            .trim_end_matches('/')
            .to_string();
        if cfg!(windows) { s.to_lowercase() } else { s }
    };
    is_link && std::fs::read_link(link).is_ok_and(|t| norm(&t) == norm(target))
}

/// Does git ignore `rel` in `repo` as it is now? Unknown counts as no.
fn git_ignores(repo: &Path, rel: &str) -> bool {
    std::process::Command::new("git")
        .args(["ls-files", "-o", "--exclude-standard", "-z", "--", rel])
        .current_dir(repo)
        .output()
        .is_ok_and(|o| o.status.success() && o.stdout.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_link_points_at_its_target_and_goes_without_it() {
        let d = tempfile::tempdir().unwrap();
        let target = d.path().join("copy").join("node_modules");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("x.js"), "1").unwrap();
        let link = d.path().join("node_modules");
        make_link(&target, &link).unwrap();
        assert!(link.join("x.js").is_file());
        assert!(links_to(&link, &target));
        assert!(!links_to(&link, &d.path().join("elsewhere")));
        assert!(!links_to(&target, &target), "a directory is no link");
        remove_link(&link).unwrap();
        assert!(std::fs::symlink_metadata(&link).is_err());
        assert!(target.join("x.js").is_file(), "the target stays");
    }

    #[test]
    fn a_link_git_does_not_ignore_is_seen() {
        let d = tempfile::tempdir().unwrap();
        let (repo, target) = (d.path().join("repo"), d.path().join("copy"));
        std::fs::create_dir_all(repo.join("web")).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("x.js"), "1").unwrap();
        let init = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repo)
            .status()
            .unwrap();
        assert!(init.success());
        make_link(&target, &repo.join("web/node_modules")).unwrap();
        assert!(!git_ignores(&repo, "web/node_modules"));
        std::fs::write(repo.join(".gitignore"), "node_modules\n").unwrap();
        assert!(git_ignores(&repo, "web/node_modules"));
    }
}
