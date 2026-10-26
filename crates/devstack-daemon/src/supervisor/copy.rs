//! A lane's copy of its worktree, and `prepare`.

use super::{COPY_QUIET, KEEP_BUILD_LOGS, STOPPED, Supervisor, tail};
use crate::state::{NoticeLevel, now_ms};
use crate::store::Store;
use devstack_build::Snapshot;
use devstack_config::model::{Prepare, Service};
use devstack_proc::Spec;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

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
        let key = prepare_key(
            p,
            svc,
            snap.manifest
                .files
                .iter()
                .map(|(r, e)| (r.as_str(), e.hash.clone())),
        )?;
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
            store: self.store_use(name, p, snap),
            before_run: None,
        }))
    }

    /// The store of what `prepare` makes (ADR-0062): in the main tree's
    /// state, shared by its builds and its lanes.
    pub(crate) fn store(&self) -> Store {
        let devstack = self
            .layout
            .artifacts_root
            .parent()
            .unwrap_or(&self.state_dir);
        Store::new(devstack.join("store"))
    }

    /// Whether `prepare` of `name` goes through the store: it names
    /// `outputs`, none of them is tracked by git (a copy from the store
    /// would overwrite the source), and the lane is on this machine (a
    /// node's store is the node's).
    fn store_use(&self, name: &str, p: &Prepare, snap: &Snapshot) -> Option<StoreUse> {
        if p.outputs.is_empty() || crate::remote_lane() {
            return None;
        }
        let id = format!("prepare.store.{name}");
        let tracked = p.outputs.iter().find(|o| {
            let dir = format!("{}/", o.replace('\\', "/").trim_matches('/'));
            let dir = dir.trim_start_matches("./");
            snap.manifest.files.keys().any(|f| f.starts_with(dir))
        });
        if let Some(o) = tracked {
            self.set_notice(
                &id,
                NoticeLevel::Warn,
                format!(
                    "`prepare` of {name} runs in every copy: its output `{o}` holds files git tracks, which a copy from the store would overwrite."
                ),
            );
            return None;
        }
        Some(StoreUse {
            root: snap.dir.clone(),
            outputs: p.outputs.clone(),
            version: p.version.clone(),
        })
    }

    /// The store's key of `job`: its inputs, this machine's platform and
    /// the output of `version` (ADR-0062 point 2). `None`: `version` failed,
    /// the store is not used this time.
    fn store_key(
        &self,
        name: &str,
        job: &PrepareJob,
        cancel: &dyn Fn() -> bool,
    ) -> Result<Option<String>, String> {
        let Some(s) = &job.store else {
            return Ok(None);
        };
        let mut version = None;
        if let Some(cmd) = &s.version {
            match self.run_version(name, cmd, &job.cwd, cancel) {
                Ok(out) => version = Some(out),
                Err(e) if cancel() => return Err(e),
                Err(e) => {
                    crate::diag::warn(
                        "prepare_store_version_failed",
                        serde_json::json!({ "lane": self.lane, "service": name, "version": cmd, "error": e }),
                    );
                    self.set_notice(
                        &format!("prepare.store.{name}"),
                        NoticeLevel::Warn,
                        format!(
                            "`prepare` of {name} ran without the store: its `version` (`{cmd}`) failed: {e}"
                        ),
                    );
                    return Ok(None);
                }
            }
        }
        Ok(Some(store_key_of(&job.key, version.as_deref())))
    }

    /// `prepare.version` of `name`, run in `cwd` like `prepare`; its output.
    fn run_version(
        &self,
        name: &str,
        cmd: &str,
        cwd: &Path,
        cancel: &dyn Fn() -> bool,
    ) -> Result<String, String> {
        let out = self.log_dir(name).join("prepare-version.out");
        let file = std::fs::File::create(&out).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(cwd).map_err(|e| e.to_string())?;
        let spec = Spec {
            name: format!("{name}-prepare-version"),
            command: cmd.to_string(),
            cwd: cwd.to_path_buf(),
            env: BTreeMap::new(),
            log: file,
        };
        let (mut tree, _) =
            devstack_proc::spawn(spec, &self.proc_opts).map_err(|e| e.to_string())?;
        let t0 = Instant::now();
        let code = loop {
            if let Some(code) = tree.try_exit_code() {
                break code;
            }
            if cancel() {
                tree.kill();
                return Err(format!("`{cmd}`: {STOPPED}"));
            }
            if t0.elapsed() > VERSION_TIMEOUT {
                tree.kill();
                return Err(format!("`{cmd}` did not end within {VERSION_TIMEOUT:?}"));
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        let text = std::fs::read_to_string(&out).unwrap_or_default();
        if code != 0 {
            return Err(format!("`{cmd}` exited with code {code}: {}", text.trim()));
        }
        Ok(text)
    }

    /// A hit of `key`: its entry copied into the copy, unless the last copy
    /// out of it took longer than its `prepare` (ADR-0062 point 6). `false`:
    /// `prepare` runs.
    fn prepare_from_store(
        &self,
        name: &str,
        store: &Store,
        key: &str,
        root: &Path,
        log: &Path,
    ) -> bool {
        let miss = |why: &str| {
            crate::diag::info(
                "prepare_store_miss",
                serde_json::json!({ "lane": self.lane, "service": name, "key": key, "why": why }),
            );
            false
        };
        let Some(meta) = store.get(name, key) else {
            return miss("no entry for these inputs on this machine");
        };
        if store
            .copy_ms(name, key)
            .is_some_and(|c| c > meta.prepare_ms)
        {
            return miss("copying the entry took longer than running prepare");
        }
        match store.copy_into(name, key, root) {
            Ok(stats) => {
                let _ = std::fs::write(
                    log,
                    format!(
                        "prepare: from store ({key}): {} files, {} MiB in {:.1} s; prepare took {:.1} s when it made them\n",
                        stats.files,
                        stats.bytes >> 20,
                        stats.elapsed.as_secs_f64(),
                        meta.prepare_ms as f64 / 1000.0
                    ),
                );
                crate::diag::info(
                    "prepare_store_hit",
                    serde_json::json!({ "lane": self.lane, "service": name, "key": key,
                        "files": stats.files, "bytes": stats.bytes,
                        "ms": stats.elapsed.as_millis() as u64, "prepare_ms": meta.prepare_ms }),
                );
                true
            }
            Err(e) => {
                crate::diag::warn(
                    "prepare_store_copy_failed",
                    serde_json::json!({ "lane": self.lane, "service": name, "key": key, "error": e.to_string() }),
                );
                false
            }
        }
    }

    /// After a `prepare` that succeeded: its outputs into the store.
    fn store_put(&self, name: &str, store: &Store, key: &str, s: &StoreUse, took: Duration) {
        let id = format!("prepare.store.{name}");
        let t0 = Instant::now();
        match store.put(name, key, &s.root, &s.outputs, took) {
            Ok(meta) => {
                self.clear_notice(&id);
                crate::diag::info(
                    "prepare_store_put",
                    serde_json::json!({ "lane": self.lane, "service": name, "key": key,
                        "files": meta.files, "bytes": meta.bytes,
                        "ms": t0.elapsed().as_millis() as u64, "prepare_ms": meta.prepare_ms }),
                );
            }
            Err(e) => {
                crate::diag::warn(
                    "prepare_store_put_failed",
                    serde_json::json!({ "lane": self.lane, "service": name, "key": key, "error": e.to_string() }),
                );
                self.set_notice(
                    &id,
                    NoticeLevel::Warn,
                    format!(
                        "What `prepare` of {name} made is not in the store, the next copy runs it again: {e}"
                    ),
                );
            }
        }
        sweep_store(store);
    }

    /// Run a `prepare` to completion; its output goes to a log of its own.
    /// `cancel`: a stop came in, the prepare is killed.
    pub(super) fn run_prepare(
        &self,
        name: &str,
        job: PrepareJob,
        cancel: &dyn Fn() -> bool,
    ) -> Result<bool, String> {
        let _one = self.preparing.lock().unwrap();
        let dir = self.log_dir(name);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        crate::diag::prune(&dir, "prepare-", KEEP_BUILD_LOGS);
        let log = dir.join(format!("prepare-{}.log", now_ms()));
        // Through the store (ADR-0062): one copy of a key prepares, the
        // others wait for it and copy what it made.
        let store = self.store();
        let key = self.store_key(name, &job, cancel)?;
        let _key = match &key {
            Some(k) => Some(
                store
                    .lock(name, k, cancel)
                    .ok_or_else(|| format!("prepare `{}`: {STOPPED}", job.run))?,
            ),
            None => None,
        };
        if let (Some(k), Some(s)) = (&key, &job.store)
            && self.prepare_from_store(name, &store, k, &s.root, &log)
        {
            self.clear_notice(&format!("prepare.store.{name}"));
            return job.done().map(|()| true);
        }
        if let Some(f) = &job.before_run {
            f()?;
        }
        let t0 = Instant::now();
        let file = std::fs::File::create(&log).map_err(|e| e.to_string())?;
        crate::diag::info(
            "prepare",
            serde_json::json!({ "lane": self.lane, "service": name, "run": job.run }),
        );
        std::fs::create_dir_all(&job.cwd).map_err(|e| e.to_string())?;
        let spec = Spec {
            name: format!("{name}-prepare"),
            command: job.run.clone(),
            cwd: job.cwd.clone(),
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
        if let (Some(k), Some(s)) = (&key, &job.store) {
            self.store_put(name, &store, k, s, t0.elapsed());
        }
        job.done().map(|()| false)
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
            self.clear_notice("copy");
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
            let id = format!("prepare.{name}.{rel}");
            match link_one(&self.repo, &copy, rel) {
                Linked::Made => {
                    self.clear_notice(&id);
                    crate::diag::info(
                        "prepare_linked",
                        serde_json::json!({ "lane": self.lane, "service": name, "path": rel }),
                    );
                }
                Linked::There(_) => self.clear_notice(&id),
                Linked::Not(why) => {
                    crate::diag::warn(
                        "prepare_not_linked",
                        serde_json::json!({ "lane": self.lane, "service": name, "path": rel, "why": why }),
                    );
                    self.set_notice(
                        &id,
                        NoticeLevel::Warn,
                        format!(
                            "`{rel}`, which `prepare` of {name} installs, is not linked into the worktree: {why}. What runs in the worktree itself (git hooks) does not find it."
                        ),
                    );
                }
            }
        }
    }

    /// The worktree of a lane that runs on a node, linked to a copy on this
    /// machine of what each `prepare` with `link` makes, for its git hooks
    /// (ADR-0063): `<dir>/linked/src`, `dir` the lane's state here. The
    /// copy comes from this machine's store (ADR-0062) when it has the key
    /// of the worktree's inputs; else `prepare` runs once in that copy,
    /// which gets the worktree's source for it, and its outputs go into the
    /// store. `stack` is the worktree's stack file (the main tree's where it
    /// has none). `report` hears every path's state as it changes. Nothing
    /// runs when the key is the one prepared there before and its
    /// directories are there: a look at the inputs.
    pub(crate) fn link_node_lane(
        &self,
        worktree: &Path,
        dir: &Path,
        stack: &devstack_config::Stack,
        report: &dyn Fn(&[NodeLink]),
        cancel: &dyn Fn() -> bool,
    ) {
        let linked = dir.join("linked");
        let mut views: Vec<NodeLink> = Vec::new();
        let snap = match Snapshot::open(worktree, &linked) {
            Ok(s) => s,
            Err(e) => {
                crate::diag::warn(
                    "prepare_node_link_failed",
                    serde_json::json!({ "error": e.to_string() }),
                );
                return;
            }
        };
        let root = snap.dir.clone();
        let files = snap.list().unwrap_or_default();
        let snap = Arc::new(std::sync::Mutex::new(snap));
        for (name, svc) in &stack.services {
            let Some(p) = svc.prepare.as_ref().filter(|p| !p.link.is_empty()) else {
                continue;
            };
            let at = |state: &str, why: Option<String>| {
                p.link
                    .iter()
                    .map(|rel| NodeLink {
                        path: rel.clone(),
                        service: name.clone(),
                        state: state.to_string(),
                        why: why.clone(),
                    })
                    .collect::<Vec<_>>()
            };
            if p.outputs.is_empty() {
                views.extend(at(
                    "not linked",
                    Some(format!(
                        "`prepare` of {name} names no `outputs`: with them this machine keeps what it makes and links the worktree to a copy"
                    )),
                ));
                continue;
            }
            // The key of the worktree's inputs, as a copy of it would have it.
            let globs = devstack_build::watch_set(&p.inputs).ok().flatten();
            let hashed = files
                .iter()
                .filter(|rel| globs.as_ref().is_some_and(|g| g.is_match(rel.as_str())))
                .filter_map(|rel| {
                    let bytes = std::fs::read(worktree.join(rel)).ok()?;
                    Some((rel.as_str(), blake3::hash(&bytes).to_hex().to_string()))
                });
            let key = match prepare_key(p, svc, hashed) {
                Ok(k) => k,
                Err(e) => {
                    views.extend(at("not linked", Some(e)));
                    continue;
                }
            };
            let mark = linked.join("prepared").join(name);
            let there = p.link.iter().all(|l| root.join(l).is_dir());
            let done = there && std::fs::read_to_string(&mark).is_ok_and(|k| k == key);
            let mut state = "linked";
            if !done {
                let mut now = views.clone();
                now.extend(at("preparing", None));
                report(&now);
                let cwd = p.cwd.as_ref().or(svc.cwd.as_ref());
                let sync = snap.clone();
                let job = PrepareJob {
                    key,
                    mark,
                    cwd: cwd.map_or_else(|| root.clone(), |c| root.join(c)),
                    run: p.run.clone(),
                    store: Some(StoreUse {
                        root: root.clone(),
                        outputs: p.outputs.clone(),
                        version: p.version.clone(),
                    }),
                    before_run: Some(Box::new(move || {
                        sync.lock()
                            .unwrap()
                            .sync()
                            .map(|_| ())
                            .map_err(|e| e.to_string())
                    })),
                };
                match self.run_prepare(name, job, cancel) {
                    Ok(true) => state = "linked (store)",
                    Ok(false) => {}
                    Err(e) => {
                        views.extend(at("not linked", Some(e)));
                        continue;
                    }
                }
            }
            for rel in &p.link {
                let (state, why) = match link_one(worktree, &root, rel) {
                    Linked::Made | Linked::There(true) => (state, None),
                    Linked::There(false) => (
                        "not linked",
                        Some(format!("the worktree has a `{rel}` of its own")),
                    ),
                    Linked::Not(why) => ("not linked", Some(why)),
                };
                views.push(NodeLink {
                    path: rel.clone(),
                    service: name.clone(),
                    state: state.to_string(),
                    why,
                });
            }
        }
        report(&views);
    }

    /// The links [`Self::link_node_lane`] made in `worktree`, removed: the
    /// lane stopped, moved or went (ADR-0044 point 3 holds for them too).
    pub(crate) fn unlink_node_lane(
        &self,
        worktree: &Path,
        dir: &Path,
        stack: &devstack_config::Stack,
    ) {
        let copy = dir.join("linked").join("src");
        for rel in stack
            .services
            .values()
            .filter_map(|s| s.prepare.as_ref())
            .flat_map(|p| &p.link)
        {
            let link = worktree.join(rel);
            if links_to(&link, &copy.join(rel)) {
                let r = remove_link(&link);
                crate::diag::info(
                    "prepare_unlinked",
                    serde_json::json!({ "path": rel, "place": "remote",
                        "error": r.err().map(|e| e.to_string()) }),
                );
            }
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
        self.clear_notice("copy");
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
                Ok(stats) => {
                    s.clear_notice("copy");
                    s.log_sync("change", &stats);
                }
                Err(e) => {
                    crate::diag::warn(
                        "copy_not_synced",
                        serde_json::json!({ "lane": s.lane, "error": e.to_string() }),
                    );
                    s.set_notice(
                        "copy",
                        NoticeLevel::Warn,
                        format!(
                            "The lane's copy could not be brought up to date with its worktree: {e}. Its dev servers and tools run the code of before; the next edit tries again."
                        ),
                    );
                }
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
    /// What the store needs, when it is used (ADR-0062).
    store: Option<StoreUse>,
    /// Runs before `prepare` itself does, not before a hit: the copy of a
    /// worktree linked for a lane on a node gets its source only when it
    /// has to prepare (ADR-0063).
    before_run: Option<Box<dyn Fn() -> Result<(), String> + Send>>,
}

impl PrepareJob {
    /// The copy is prepared for `key`: the next start does not prepare it.
    fn done(&self) -> Result<(), String> {
        if let Some(parent) = self.mark.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&self.mark, &self.key).map_err(|e| e.to_string())
    }
}

/// The key of `prepare` `p` of `svc` over the files of a copy (path, blake3
/// of the content): its command, its directory and the files of `inputs`
/// (ADR-0002's way: what changes it, nothing else).
fn prepare_key<'a>(
    p: &Prepare,
    svc: &Service,
    files: impl Iterator<Item = (&'a str, String)>,
) -> Result<String, String> {
    let cwd = p.cwd.as_ref().or(svc.cwd.as_ref());
    let mut h = blake3::Hasher::new();
    h.update(p.run.as_bytes());
    h.update(cwd.map_or("", String::as_str).as_bytes());
    if let Some(g) = devstack_build::watch_set(&p.inputs).map_err(|e| e.to_string())? {
        for (rel, hash) in files.filter(|(r, _)| g.is_match(r)) {
            h.update(rel.as_bytes());
            h.update(hash.as_bytes());
        }
    }
    Ok(h.finalize().to_hex().to_string())
}

/// The store's key of a `prepare` whose key is `key` on this machine, with
/// the output of its `version` (ADR-0062 point 2). Short: Windows paths below
/// it are long already (`node_modules`).
fn store_key_of(key: &str, version: Option<&str>) -> String {
    let mut h = blake3::Hasher::new();
    h.update(key.as_bytes());
    h.update(std::env::consts::OS.as_bytes());
    h.update(std::env::consts::ARCH.as_bytes());
    if let Some(v) = version {
        h.update(v.trim().as_bytes());
    }
    h.finalize().to_hex()[..20].to_string()
}

/// A `prepare` that goes through the store.
struct StoreUse {
    /// The copy's root, where `outputs` are.
    root: PathBuf,
    outputs: Vec<String>,
    version: Option<String>,
}

/// How long `prepare.version` (`node --version`) may take.
const VERSION_TIMEOUT: Duration = Duration::from_secs(60);
/// How long an entry of the store is kept unused (ADR-0062 point 9).
const STORE_KEEP: Duration = Duration::from_secs(14 * 24 * 3600);

/// The store's unused entries removed, at most once a day per process, in
/// the background: the first time the store is written after a daemon
/// starts, then daily.
fn sweep_store(store: &Store) {
    static LAST: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);
    {
        let mut last = LAST.lock().unwrap();
        if last.is_some_and(|t| t.elapsed() < Duration::from_secs(24 * 3600)) {
            return;
        }
        *last = Some(Instant::now());
    }
    let store = Store::new(store.root().to_path_buf());
    std::thread::spawn(move || {
        for s in store.sweep(STORE_KEEP, false) {
            crate::diag::info(
                "prepare_store_swept",
                serde_json::json!({ "service": s.service, "key": s.key, "bytes": s.bytes,
                    "idle_h": s.idle.as_secs() / 3600, "error": s.error }),
            );
        }
    });
}

/// A path of a lane on a node's worktree, linked (or not) to a copy on this
/// machine (ADR-0063): what `devstack status` and the dashboard show.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct NodeLink {
    pub path: String,
    pub service: String,
    /// `linked`, `linked (store)`, `preparing`, `not linked`.
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
}

/// What [`link_one`] did.
enum Linked {
    Made,
    /// Something is there already: a link to the copy (`true`), or the
    /// developer's own (`false`), which stays.
    There(bool),
    Not(String),
}

/// `rel` in `repo` linked to `rel` in `copy` (ADR-0044): only where the
/// worktree has nothing of that name, and only where git ignores the link
/// (`git add -A` would commit it; it is taken away again).
fn link_one(repo: &Path, copy: &Path, rel: &str) -> Linked {
    let (link, target) = (repo.join(rel), copy.join(rel));
    if std::fs::symlink_metadata(&link).is_ok() {
        return Linked::There(links_to(&link, &target));
    }
    if !target.is_dir() {
        return Linked::Not(format!(
            "`prepare` made no directory `{rel}` in the lane's copy"
        ));
    }
    if !link.parent().is_some_and(Path::is_dir) {
        return Linked::Not("its parent directory is not in the worktree".into());
    }
    if let Err(e) = make_link(&target, &link) {
        return Linked::Not(e.to_string());
    }
    if !git_ignores(repo, rel) {
        let _ = remove_link(&link);
        return Linked::Not(format!(
            "git does not ignore `{rel}` as a link: ignore it without a trailing slash (`node_modules`, not `node_modules/`)"
        ));
    }
    Linked::Made
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

    #[test]
    fn a_lane_on_a_node_gets_its_worktree_linked_to_a_copy_from_the_store() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let d = tempfile::tempdir().unwrap();
        let (main, wt, lane) = (
            d.path().join("main"),
            d.path().join("wt"),
            d.path().join("lane"),
        );
        let text = "version = 1\n[stack]\nname = \"t\"\n[services.web]\nkind = \"devserver\"\nrun = \"x\"\nport = 4200\nprepare = { run = \"npm ci\", inputs = [\"package-lock.json\"], outputs = [\"deps\"], link = [\"deps\"] }\n";
        let stack = devstack_config::from_str(text, Path::new("t")).unwrap();
        for repo in [&main, &wt] {
            std::fs::create_dir_all(repo).unwrap();
            let init = std::process::Command::new("git")
                .args(["init", "-q"])
                .current_dir(repo)
                .status()
                .unwrap();
            assert!(init.success());
            std::fs::write(repo.join(".gitignore"), "deps\n.devstack\n").unwrap();
            std::fs::write(repo.join("package-lock.json"), "{\"v\":1}").unwrap();
        }
        let sup = Supervisor::new(
            stack.clone(),
            main.join("devstack.toml"),
            main.clone(),
            main.join(".devstack"),
            devstack_proc::Options::default(),
            rt.handle().clone(),
        )
        .unwrap();
        // What a lane with the same lockfile here made before: in the store.
        let p = stack.services["web"].prepare.as_ref().unwrap();
        let key = prepare_key(
            p,
            &stack.services["web"],
            [(
                "package-lock.json",
                blake3::hash(b"{\"v\":1}").to_hex().to_string(),
            )]
            .into_iter(),
        )
        .unwrap();
        let made = d.path().join("made");
        std::fs::create_dir_all(made.join("deps/eslint")).unwrap();
        std::fs::write(made.join("deps/eslint/index.js"), "lint").unwrap();
        sup.store()
            .put(
                "web",
                &store_key_of(&key, None),
                &made,
                &p.outputs,
                Duration::from_secs(60),
            )
            .unwrap();

        let seen = std::sync::Mutex::new(Vec::new());
        let report = |l: &[NodeLink]| seen.lock().unwrap().push(l.to_vec());
        sup.link_node_lane(&wt, &lane, &stack, &report, &|| false);
        let last = seen.lock().unwrap().last().cloned().unwrap();
        assert_eq!(last.len(), 1);
        assert_eq!(last[0].state, "linked (store)", "{last:?}");
        assert!(
            std::fs::symlink_metadata(wt.join("deps"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read_to_string(wt.join("deps/eslint/index.js")).unwrap(),
            "lint"
        );
        // A copy, not the store's entry.
        assert!(lane.join("linked/src/deps/eslint/index.js").is_file());

        // Nothing changed: linked as it is, nothing copied again.
        sup.link_node_lane(&wt, &lane, &stack, &report, &|| false);
        let last = seen.lock().unwrap().last().cloned().unwrap();
        assert_eq!(last[0].state, "linked", "{last:?}");

        // The lane stops: the link goes, the copy stays for the next start.
        sup.unlink_node_lane(&wt, &lane, &stack);
        assert!(std::fs::symlink_metadata(wt.join("deps")).is_err());
        assert!(lane.join("linked/src/deps/eslint/index.js").is_file());

        // Without `outputs` there is no store and no link, and it says why.
        let plain =
            devstack_config::from_str(&text.replace(", outputs = [\"deps\"]", ""), Path::new("t"))
                .unwrap();
        sup.link_node_lane(&wt, &lane, &plain, &report, &|| false);
        let last = seen.lock().unwrap().last().cloned().unwrap();
        assert_eq!(last[0].state, "not linked");
        assert!(
            last[0].why.as_deref().unwrap().contains("outputs"),
            "{last:?}"
        );
        assert!(std::fs::symlink_metadata(wt.join("deps")).is_err());
    }
}
