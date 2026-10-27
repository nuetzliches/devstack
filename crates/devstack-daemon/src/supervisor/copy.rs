//! A lane's copy of its worktree, and `prepare`.

use super::{COPY_QUIET, KEEP_BUILD_LOGS, STOPPED, Supervisor, tail};
use crate::state::{NoticeLevel, PrepareView, now_ms};
use devstack_build::Snapshot;
use devstack_build::store::Store;
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
        let counted: Vec<PathBuf> = p.link.iter().map(|l| snap.dir.join(l)).collect();
        if linked_there && std::fs::read_to_string(&mark).is_ok_and(|k| k == key) {
            // Emptied through the worktree's link (an `npm ci` there, #311):
            // prepared again, and said so.
            let Some((rel, now, then)) = emptied(&mark, &counted, &p.link) else {
                return Ok(None);
            };
            crate::diag::warn(
                "prepare_copy_emptied",
                serde_json::json!({ "lane": self.lane, "service": name, "path": rel,
                    "entries": now, "prepared_with": then }),
            );
            self.set_notice(
                &format!("prepare.emptied.{name}"),
                NoticeLevel::Warn,
                format!(
                    "`{rel}` of {name} had lost what `prepare` made ({now} of {then} entries): something emptied it, likely a reinstall through the worktree's link. It is prepared again; leave a linked directory alone in the worktree."
                ),
            );
        }
        Ok(Some(PrepareJob {
            key,
            mark,
            cwd: cwd.map_or_else(|| snap.dir.clone(), |c| snap.dir.join(c)),
            run: p.run.clone(),
            store: self.store_use(name, p, snap),
            before_run: None,
            counted,
        }))
    }

    /// Where `name`'s last `prepare` outputs came from, on its view.
    pub(super) fn set_prepare(&self, name: &str, view: PrepareView) {
        if let Some(slot) = self.slots.lock().unwrap().get_mut(name) {
            slot.view.prepare = Some(view);
        }
    }

    /// Of each service, the store key its last `prepare` outputs came from
    /// or went to (ADR-0062).
    pub(crate) fn prepared_keys(&self) -> Vec<(String, String)> {
        self.slots
            .lock()
            .unwrap()
            .iter()
            .filter_map(|(name, slot)| {
                let key = slot.view.prepare.as_ref()?.key.clone()?;
                Some((name.clone(), key))
            })
            .collect()
    }

    /// The store of what `prepare` makes (ADR-0062): in the main tree's
    /// state, shared by its builds and its lanes.
    pub(crate) fn store(&self) -> Store {
        if let Some((root, outbox)) = node_store() {
            return Store::with_outbox(root, outbox);
        }
        let devstack = self
            .layout
            .artifacts_root
            .parent()
            .unwrap_or(&self.state_dir);
        Store::new(devstack.join("store"))
    }

    /// Whether `prepare` of `name` goes through the store: it names
    /// `outputs`, none of them is tracked by git (a copy from the store
    /// would overwrite the source), and, in a lane on a node, the node
    /// gives it the member's store (an older node does not). `Err`: why
    /// not, `None` when it names no `outputs`.
    fn store_use(
        &self,
        name: &str,
        p: &Prepare,
        snap: &Snapshot,
    ) -> Result<StoreUse, Option<String>> {
        if p.outputs.is_empty() {
            return Err(None);
        }
        if crate::remote_lane() && node_store().is_none() {
            return Err(Some("the node gives this lane no store".into()));
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
            return Err(Some(format!("its output `{o}` holds files git tracks")));
        }
        Ok(StoreUse {
            root: snap.dir.clone(),
            outputs: p.outputs.clone(),
            version: p.version.clone(),
        })
    }

    /// The store's key of `job`: its inputs, this machine's platform and
    /// the output of `version` (ADR-0062 point 2). `Ok(Err(why))`: the store
    /// is not used this time (`None`: the `prepare` does not use it).
    fn store_key(
        &self,
        name: &str,
        job: &PrepareJob,
        cancel: &dyn Fn() -> bool,
    ) -> Result<Result<String, Option<String>>, String> {
        let s = match &job.store {
            Ok(s) => s,
            Err(why) => return Ok(Err(why.clone())),
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
                    return Ok(Err(Some(format!("its `version` (`{cmd}`) failed"))));
                }
            }
        }
        Ok(Ok(store_key_of(&job.key, &s.outputs, version.as_deref())))
    }

    /// `prepare.version` of `name`, run in `cwd` like `prepare`; its output.
    fn run_version(
        &self,
        name: &str,
        cmd: &str,
        cwd: &Path,
        cancel: &dyn Fn() -> bool,
    ) -> Result<String, String> {
        // A service that never ran has no log directory yet (#297).
        let dir = self.log_dir(name);
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let out = dir.join("prepare-version.out");
        let file = std::fs::File::create(&out).map_err(|e| format!("{}: {e}", out.display()))?;
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
    /// out of it took longer than its `prepare` (ADR-0062 point 6). `Err`:
    /// why not, `prepare` runs.
    fn prepare_from_store(
        &self,
        name: &str,
        store: &Store,
        key: &str,
        root: &Path,
        log: &Path,
    ) -> Result<PrepareView, String> {
        let miss = |why: &str| {
            crate::diag::info(
                "prepare_store_miss",
                serde_json::json!({ "lane": self.lane, "service": name, "key": key, "why": why }),
            );
            Err(why.to_string())
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
                Ok(PrepareView {
                    source: "store",
                    at: now_ms(),
                    ms: stats.elapsed.as_millis() as u64,
                    key: Some(key.to_string()),
                    prepare_ms: Some(meta.prepare_ms),
                    miss: None,
                    stored: false,
                })
            }
            Err(e) => {
                crate::diag::warn(
                    "prepare_store_copy_failed",
                    serde_json::json!({ "lane": self.lane, "service": name, "key": key, "error": e.to_string() }),
                );
                Err(format!("copying the entry failed: {e}"))
            }
        }
    }

    /// After a `prepare` that succeeded: its outputs into the store. Whether
    /// they are there.
    fn store_put(
        &self,
        name: &str,
        store: &Store,
        key: &str,
        s: &StoreUse,
        took: Duration,
    ) -> bool {
        let id = format!("prepare.store.{name}");
        let t0 = Instant::now();
        let stored = match store.put(name, key, &s.root, &s.outputs, took) {
            Ok(meta) => {
                self.clear_notice(&id);
                crate::diag::info(
                    "prepare_store_put",
                    serde_json::json!({ "lane": self.lane, "service": name, "key": key,
                        "files": meta.files, "bytes": meta.bytes,
                        "ms": t0.elapsed().as_millis() as u64, "prepare_ms": meta.prepare_ms }),
                );
                true
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
                false
            }
        };
        sweep_store(store);
        stored
    }

    /// Run a `prepare` to completion; its output goes to a log of its own.
    /// `cancel`: a stop came in, the prepare is killed. Where its outputs
    /// came from: the store or the run.
    pub(super) fn run_prepare(
        &self,
        name: &str,
        job: PrepareJob,
        cancel: &dyn Fn() -> bool,
    ) -> Result<PrepareView, String> {
        let _one = self.preparing.lock().unwrap();
        let dir = self.log_dir(name);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        crate::diag::prune(&dir, "prepare-", KEEP_BUILD_LOGS);
        let log = dir.join(format!("prepare-{}.log", now_ms()));
        // Through the store (ADR-0062): one copy of a key prepares, the
        // others wait for it and copy what it made.
        let store = self.store();
        let (key, mut miss) = match self.store_key(name, &job, cancel)? {
            Ok(k) => (Some(k), None),
            Err(why) => (None, why),
        };
        let _key = match &key {
            Some(k) => Some(
                store
                    .lock(name, k, cancel)
                    .ok_or_else(|| format!("prepare `{}`: {STOPPED}", job.run))?,
            ),
            None => None,
        };
        if let (Some(k), Ok(s)) = (&key, &job.store) {
            match self.prepare_from_store(name, &store, k, &s.root, &log) {
                Ok(view) => {
                    self.clear_notice(&format!("prepare.store.{name}"));
                    return job.done().map(|()| view);
                }
                Err(why) => miss = Some(why),
            }
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
            crate::diag::warn(
                "prepare_failed",
                serde_json::json!({ "lane": self.lane, "service": name, "run": job.run,
                    "exit_code": code, "cwd": job.cwd, "log": log }),
            );
            return Err(format!(
                "prepare `{}` exited with code {code}\n{}",
                job.run,
                tail(&log, 15)
            ));
        }
        let took = t0.elapsed();
        let stored = match (&key, &job.store) {
            (Some(k), Ok(s)) => self.store_put(name, &store, k, s, took),
            _ => false,
        };
        job.done().map(|()| PrepareView {
            source: "run",
            at: now_ms(),
            ms: took.as_millis() as u64,
            key: key.filter(|_| stored),
            prepare_ms: None,
            miss,
            stored,
        })
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
            let view = self.run_prepare(name, job, cancel)?;
            self.set_prepare(name, view);
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
    /// (ADR-0063): `<dir>/linked/src` or `<dir>/linked/next/src`, `dir` the
    /// lane's state here. The copy comes from this machine's store
    /// (ADR-0062) when it has the key of the worktree's inputs; else
    /// `prepare` runs once in that copy, which gets the worktree's source
    /// for it, and its outputs go into the store. `stack` is the worktree's
    /// stack file (the main tree's where it has none). `report` hears every
    /// path's state as it changes. Nothing runs when the key is the one
    /// prepared there before and its directories are there: a look at the
    /// inputs.
    ///
    /// New inputs are prepared in the copy the link does not point to, and
    /// the link moves once that succeeded (ADR-0063 addendum 2026-10-27):
    /// while it runs, and when it fails, the worktree keeps the last copy
    /// that was whole (`linked (stale)`), never one `npm ci` is emptying.
    pub(crate) fn link_node_lane(
        &self,
        worktree: &Path,
        dir: &Path,
        stack: &devstack_config::Stack,
        report: &dyn Fn(&[NodeLink]),
        cancel: &dyn Fn() -> bool,
    ) {
        let linked = dir.join("linked");
        let copies = node_copies(&linked);
        let mut views: Vec<NodeLink> = Vec::new();
        let files = match Snapshot::open(worktree, &copies[0]).and_then(|s| s.list()) {
            Ok(f) => f,
            Err(e) => {
                crate::diag::warn(
                    "prepare_node_link_failed",
                    serde_json::json!({ "error": e.to_string() }),
                );
                return;
            }
        };
        let roots = copies.clone().map(|c| c.join("src"));
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
            // The copy the worktree is linked to now, and those made for
            // this key already.
            let current = (0..2).find(|&i| {
                p.link
                    .iter()
                    .any(|rel| links_to(&worktree.join(rel), &roots[i].join(rel)))
            });
            let counted =
                |i: usize| -> Vec<PathBuf> { p.link.iter().map(|l| roots[i].join(l)).collect() };
            let ready = |i: usize| {
                let mark = copies[i].join("prepared").join(name);
                p.link.iter().all(|l| roots[i].join(l).is_dir())
                    && std::fs::read_to_string(&mark).is_ok_and(|k| k == key)
                    // Emptied through the worktree's link (#311): not ready.
                    && emptied(&mark, &counted(i), &p.link).is_none()
            };
            let mut state = "linked";
            let to = match current
                .filter(|&i| ready(i))
                .or_else(|| (0..2).find(|&i| ready(i)))
            {
                Some(i) => i,
                None => {
                    // The other copy than the one the worktree has: it
                    // keeps that one until this one is whole.
                    let to = current.map_or(0, |i| 1 - i);
                    let mark = copies[to].join("prepared").join(name);
                    // Half-made, it is no copy of any key.
                    let _ = std::fs::remove_file(&mark);
                    let mut now = views.clone();
                    now.extend(at("preparing", None));
                    report(&now);
                    let snap = match Snapshot::open(worktree, &copies[to]) {
                        Ok(s) => Arc::new(std::sync::Mutex::new(s)),
                        Err(e) => {
                            views.extend(at("not linked", Some(e.to_string())));
                            continue;
                        }
                    };
                    let root = roots[to].clone();
                    let cwd = p.cwd.as_ref().or(svc.cwd.as_ref());
                    let job = PrepareJob {
                        key,
                        mark,
                        cwd: cwd.map_or_else(|| root.clone(), |c| root.join(c)),
                        run: p.run.clone(),
                        store: Ok(StoreUse {
                            root: root.clone(),
                            outputs: p.outputs.clone(),
                            version: p.version.clone(),
                        }),
                        before_run: Some(Box::new(move || {
                            snap.lock()
                                .unwrap()
                                .sync()
                                .map(|_| ())
                                .map_err(|e| e.to_string())
                        })),
                        counted: counted(to),
                    };
                    match self.run_prepare(name, job, cancel) {
                        Ok(v) if v.source == "store" => state = "linked (store)",
                        Ok(_) => {}
                        Err(e) if current.is_some() => {
                            views.extend(at(
                                "linked (stale)",
                                Some(format!(
                                    "the worktree keeps the copy made for its inputs before: {e}"
                                )),
                            ));
                            continue;
                        }
                        Err(e) => {
                            views.extend(at("not linked", Some(e)));
                            continue;
                        }
                    }
                    to
                }
            };
            for rel in &p.link {
                // A link of ours to the other copy moves to this one.
                let link = worktree.join(rel);
                if (0..2).any(|i| i != to && links_to(&link, &roots[i].join(rel))) {
                    let _ = remove_link(&link);
                }
                let (state, why) = match link_one(worktree, &roots[to], rel) {
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

    /// `prepare` of `name` for a ref checked out at `root` (ADR-0065
    /// points 3, 4): there are the files of its `inputs` at least, and
    /// `complete` checks out the rest. The key is a copy's, over those
    /// files as git checked them out. `Ok(Warmed::Had)` when the store has
    /// it; else the checkout is completed and prepared, and its outputs go
    /// into the store.
    pub(crate) fn warm_service(
        &self,
        name: &str,
        svc: &Service,
        root: &Path,
        complete: Box<dyn Fn() -> Result<(), String> + Send>,
        cancel: &dyn Fn() -> bool,
    ) -> Result<Warmed, String> {
        let Some(p) = svc.prepare.as_ref().filter(|p| !p.outputs.is_empty()) else {
            return Err("its `prepare` names no `outputs`".into());
        };
        let globs = devstack_build::watch_set(&p.inputs).map_err(|e| e.to_string())?;
        let mut files: Vec<(String, String)> = Vec::new();
        let wanted = |rel: &str| globs.as_ref().is_some_and(|g| g.is_match(rel));
        collect_files(root, root, &wanted, &mut files);
        files.sort();
        let hashed = files.iter().map(|(rel, hash)| (rel.as_str(), hash.clone()));
        let key = prepare_key(p, svc, hashed)?;
        let cwd = p.cwd.as_ref().or(svc.cwd.as_ref());
        let job = PrepareJob {
            key,
            mark: root.parent().unwrap_or(root).join("prepared").join(name),
            cwd: cwd.map_or_else(|| root.to_path_buf(), |c| root.join(c)),
            run: p.run.clone(),
            store: Ok(StoreUse {
                root: root.to_path_buf(),
                outputs: p.outputs.clone(),
                version: p.version.clone(),
            }),
            before_run: Some(complete),
            counted: Vec::new(),
        };
        let key = match self.store_key(name, &job, cancel)? {
            Ok(k) => k,
            Err(why) => return Err(why.unwrap_or_else(|| "it does not use the store".into())),
        };
        if self.store().get(name, &key).is_some() {
            return Ok(Warmed::Had(key));
        }
        let t0 = Instant::now();
        self.run_prepare(name, job, cancel)?;
        Ok(Warmed::Made(key, t0.elapsed()))
    }

    /// In a warm lease's container (ADR-0065 point 5): `prepare` of every
    /// service whose `prepare` names `outputs`, in the lane's copy, from the
    /// member's store or run into it; what each did, for the requester.
    pub(crate) fn warm_lane(&self, cancel: &dyn Fn() -> bool) -> Vec<serde_json::Value> {
        let mut out = Vec::new();
        for (name, svc) in &self.stack.services {
            if svc.prepare.as_ref().is_none_or(|p| p.outputs.is_empty()) {
                continue;
            }
            let t0 = Instant::now();
            let job = {
                let mut snap = self.snapshot.lock().unwrap();
                snap.sync()
                    .map_err(|e| e.to_string())
                    .and_then(|_| self.prepare_job(name, svc, &snap))
            };
            let done = match job {
                Ok(None) => Ok(("prepared before", None, None)),
                Ok(Some(job)) => self.run_prepare(name, job, cancel).map(|v| {
                    let how = if v.source == "store" {
                        "from the store"
                    } else {
                        "prepared"
                    };
                    let (key, miss) = (v.key.clone(), v.miss.clone());
                    self.set_prepare(name, v);
                    (how, key, miss)
                }),
                Err(e) => Err(e),
            };
            out.push(match done {
                Ok((how, key, miss)) => serde_json::json!({ "service": name, "done": how,
                    "key": key, "miss": miss, "ms": t0.elapsed().as_millis() as u64 }),
                Err(e) => serde_json::json!({ "service": name, "error": e }),
            });
        }
        out
    }

    /// The links [`Self::link_node_lane`] made in `worktree`, removed: the
    /// lane stopped, moved or went (ADR-0044 point 3 holds for them too).
    pub(crate) fn unlink_node_lane(
        &self,
        worktree: &Path,
        dir: &Path,
        stack: &devstack_config::Stack,
    ) {
        let roots = node_copies(&dir.join("linked")).map(|c| c.join("src"));
        for rel in stack
            .services
            .values()
            .filter_map(|s| s.prepare.as_ref())
            .flat_map(|p| &p.link)
        {
            let link = worktree.join(rel);
            if roots.iter().any(|r| links_to(&link, &r.join(rel))) {
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
    /// `Err`: why not (`None`: it names no `outputs`).
    store: Result<StoreUse, Option<String>>,
    /// Runs before `prepare` itself does, not before a hit: the copy of a
    /// worktree linked for a lane on a node gets its source only when it
    /// has to prepare (ADR-0063).
    before_run: Option<Box<dyn Fn() -> Result<(), String> + Send>>,
    /// The directories whose entries are counted beside the mark (#311):
    /// those linked into the worktree, where a reinstall can empty them.
    counted: Vec<PathBuf>,
}

impl PrepareJob {
    /// The copy is prepared for `key`: the next start does not prepare it.
    fn done(&self) -> Result<(), String> {
        if let Some(parent) = self.mark.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&self.mark, &self.key).map_err(|e| e.to_string())?;
        let counts: String = self
            .counted
            .iter()
            .map(|d| format!("{}\n", entries(d)))
            .collect();
        std::fs::write(entries_file(&self.mark), counts).map_err(|e| e.to_string())
    }
}

/// Beside a mark: the entries each counted directory had once prepared.
fn entries_file(mark: &Path) -> PathBuf {
    let mut s = mark.as_os_str().to_owned();
    s.push(".entries");
    s.into()
}

/// The entries right in `dir` (a `node_modules`' packages and scopes).
fn entries(dir: &Path) -> usize {
    std::fs::read_dir(dir).map_or(0, |r| r.count())
}

/// The first of `dirs` (named `rels`) that has lost more than a tenth of
/// the entries it had when it was prepared, with now and then (#311); none
/// when nothing was counted (a mark from before).
fn emptied(mark: &Path, dirs: &[PathBuf], rels: &[String]) -> Option<(String, usize, usize)> {
    let text = std::fs::read_to_string(entries_file(mark)).ok()?;
    dirs.iter()
        .zip(rels)
        .zip(text.lines())
        .find_map(|((dir, rel), then)| {
            let then: usize = then.trim().parse().ok()?;
            let now = entries(dir);
            (now * 10 < then * 9).then(|| (rel.clone(), now, then))
        })
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
fn store_key_of(key: &str, outputs: &[String], version: Option<&str>) -> String {
    let mut h = blake3::Hasher::new();
    h.update(key.as_bytes());
    // What it keeps: two services whose `prepare` is the same and keeps the
    // same directories share an entry (#297).
    for o in outputs {
        h.update(b"\0");
        h.update(o.as_bytes());
    }
    h.update(std::env::consts::OS.as_bytes());
    h.update(std::env::consts::ARCH.as_bytes());
    if let Some(v) = version {
        h.update(v.trim().as_bytes());
    }
    // In a lane on a node, what its toolchain is (ADR-0062 point 2).
    if let Some(image) = std::env::var("DEVSTACK_LANE_IMAGE")
        .ok()
        .filter(|i| !i.is_empty())
    {
        h.update(image.as_bytes());
    }
    h.finalize().to_hex()[..20].to_string()
}

/// In a lane on a node: the member's store the node mounts read-only
/// (`DEVSTACK_STORE`), and the outbox in the lane's slot its agent takes new
/// entries from (`DEVSTACK_STORE_OUT`), ADR-0062 points 3, 4.
fn node_store() -> Option<(PathBuf, PathBuf)> {
    if !crate::remote_lane() {
        return None;
    }
    let var = |k: &str| {
        std::env::var_os(k)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    Some((var("DEVSTACK_STORE")?, var("DEVSTACK_STORE_OUT")?))
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
    // A node's store is its agent's to sweep.
    if !store.owned() {
        return;
    }
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

/// The store of what `prepare` makes (ADR-0062) as `GET /v1/store` shows it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct StoreView {
    /// This daemon writes and sweeps it; `false`: a node's, read-only here.
    pub owned: bool,
    /// The largest first.
    pub entries: Vec<StoreEntryView>,
    /// What all entries take.
    pub bytes: u64,
}

/// An entry of the store.
#[derive(Debug, Clone, serde::Serialize)]
pub struct StoreEntryView {
    pub service: String,
    pub key: String,
    /// The directories it holds, relative to the repository.
    pub outputs: Vec<String>,
    pub bytes: u64,
    pub files: u64,
    /// Unix ms.
    pub created: u128,
    /// Unix ms of its last use (a copy out of it), else of its making.
    pub last_used: u128,
    /// How long the `prepare` that made it took.
    pub prepare_ms: u64,
    /// How long the last copy out of it took.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub copy_ms: Option<u64>,
    /// The trees whose service's last `prepare` outputs came from it or
    /// went into it, since their daemon started: lane names, `""` the main
    /// working tree (its builds).
    pub used_by: Vec<String>,
}

/// `store`'s entries; `used`: (tree, service, key) of the last `prepare`
/// of each service of each tree ([`Supervisor::prepared_keys`]).
pub fn store_view(store: &Store, used: &[(String, String, String)]) -> StoreView {
    let now = now_ms();
    let mut entries: Vec<StoreEntryView> = store
        .entries()
        .into_iter()
        .map(|(m, idle)| StoreEntryView {
            copy_ms: store.copy_ms(&m.service, &m.key),
            used_by: used
                .iter()
                .filter(|(_, s, k)| *s == m.service && *k == m.key)
                .map(|(t, _, _)| t.clone())
                .collect(),
            last_used: now.saturating_sub(idle.as_millis()),
            service: m.service,
            key: m.key,
            outputs: m.outputs,
            bytes: m.bytes,
            files: m.files,
            created: m.created,
            prepare_ms: m.prepare_ms,
        })
        .collect();
    entries.sort_by(|a, b| {
        b.bytes
            .cmp(&a.bytes)
            .then_with(|| (&a.service, &a.key).cmp(&(&b.service, &b.key)))
    });
    StoreView {
        owned: store.owned(),
        bytes: entries.iter().map(|e| e.bytes).sum(),
        entries,
    }
}

/// Why `DELETE /v1/store/{service}/{key}` did not remove an entry.
#[derive(Debug, PartialEq, Eq)]
pub enum StoreRemoveError {
    /// No such entry (or a name that cannot be one).
    NotFound(String),
    /// Not this daemon's to change, or a `prepare` of the key under way.
    Refused(String),
    Failed(String),
}

/// A segment of a store path as an entry's service or key may be: no
/// separator, nothing hidden or relative.
fn store_name_ok(s: &str) -> bool {
    !s.is_empty() && !s.starts_with('.') && !s.contains(['/', '\\', ':']) && !s.contains("..")
}

/// The entry `service`/`key` removed from `store`, unless a `prepare` of
/// the key in this daemon copies out of it or puts it right now.
pub fn store_remove(store: &Store, service: &str, key: &str) -> Result<(), StoreRemoveError> {
    if !store_name_ok(service) || !store_name_ok(key) {
        return Err(StoreRemoveError::NotFound(format!(
            "no entry {service}/{key}"
        )));
    }
    if !store.owned() {
        return Err(StoreRemoveError::Refused(
            "the node's store is its agent's to change".into(),
        ));
    }
    if store.get(service, key).is_none() {
        return Err(StoreRemoveError::NotFound(format!(
            "no entry {service}/{key} in the store"
        )));
    }
    let t0 = Instant::now();
    let Some(_guard) = store.lock(service, key, &|| t0.elapsed() > STORE_REMOVE_WAIT) else {
        return Err(StoreRemoveError::Refused(format!(
            "a `prepare` of {service} uses the entry {key} right now"
        )));
    };
    store
        .remove(service, key)
        .map_err(|e| StoreRemoveError::Failed(e.to_string()))?;
    crate::diag::info(
        "prepare_store_removed",
        serde_json::json!({ "service": service, "key": key }),
    );
    Ok(())
}

/// How long a removal waits for a `prepare` of the key to end.
const STORE_REMOVE_WAIT: Duration = Duration::from_secs(5);

/// A path of a lane on a node's worktree, linked (or not) to a copy on this
/// machine (ADR-0063): what `devstack status` and the dashboard show.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct NodeLink {
    pub path: String,
    pub service: String,
    /// `linked`, `linked (store)`, `preparing`, `linked (stale)`, `not
    /// linked`.
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
}

/// What [`Supervisor::warm_service`] did, with the store's key.
pub enum Warmed {
    /// The store had it.
    Had(String),
    /// Prepared, in this long.
    Made(String, Duration),
}

/// The files below `dir` that `keep` wants, as paths below `root` with `/`
/// and the blake3 of their content: what a copy's manifest says of them.
fn collect_files(
    root: &Path,
    dir: &Path,
    keep: &dyn Fn(&str) -> bool,
    out: &mut Vec<(String, String)>,
) {
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = e.path();
        if e.file_type().is_ok_and(|t| t.is_dir()) {
            collect_files(root, &path, keep, out);
            continue;
        }
        let Ok(rel) = path.strip_prefix(root) else {
            continue;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        if keep(&rel)
            && let Ok(bytes) = std::fs::read(&path)
        {
            out.push((rel, blake3::hash(&bytes).to_hex().to_string()));
        }
    }
}

/// What [`link_one`] did.
enum Linked {
    Made,
    /// Something is there already: a link to the copy (`true`), or the
    /// developer's own (`false`), which stays.
    There(bool),
    Not(String),
}

/// The two copies a lane on a node's worktree is linked to in turn
/// (ADR-0063 addendum 2026-10-27), each a snapshot with its `src` and its
/// `prepared` marks: `linked` itself, the one there was before, and
/// `linked/next`.
fn node_copies(linked: &Path) -> [PathBuf; 2] {
    [linked.to_path_buf(), linked.join("next")]
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
    fn a_copy_emptied_through_its_link_is_noticed() {
        let d = tempfile::tempdir().unwrap();
        let deps = d.path().join("copy/node_modules");
        for i in 0..10 {
            std::fs::create_dir_all(deps.join(format!("pkg{i}"))).unwrap();
        }
        let mark = d.path().join("prepared/web");
        let rels = ["web/node_modules".to_string()];
        // A mark from before counted nothing: no check.
        assert_eq!(emptied(&mark, std::slice::from_ref(&deps), &rels), None);
        PrepareJob {
            key: "k".into(),
            mark: mark.clone(),
            cwd: d.path().into(),
            run: "x".into(),
            store: Err(None),
            before_run: None,
            counted: vec![deps.clone()],
        }
        .done()
        .unwrap();
        assert_eq!(emptied(&mark, std::slice::from_ref(&deps), &rels), None);
        // One package less is a tool's doing; most of them gone is not.
        std::fs::remove_dir(deps.join("pkg0")).unwrap();
        assert_eq!(emptied(&mark, std::slice::from_ref(&deps), &rels), None);
        for i in 1..8 {
            std::fs::remove_dir(deps.join(format!("pkg{i}"))).unwrap();
        }
        assert_eq!(
            emptied(&mark, std::slice::from_ref(&deps), &rels),
            Some(("web/node_modules".into(), 2, 10))
        );
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
                &store_key_of(&key, &p.outputs, None),
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

        // New inputs, and their prepare fails: the worktree keeps the copy
        // it had, whole, and says so (#305).
        std::fs::write(wt.join("package-lock.json"), "{\"v\":2}").unwrap();
        let failing = devstack_config::from_str(
            &text.replace("run = \"npm ci\"", "run = \"exit 3\""),
            Path::new("t"),
        )
        .unwrap();
        sup.link_node_lane(&wt, &lane, &failing, &report, &|| false);
        let last = seen.lock().unwrap().last().cloned().unwrap();
        assert_eq!(last[0].state, "linked (stale)", "{last:?}");
        // Elsewhere a unit test cannot spawn (no shim): a failure all the same.
        let why = last[0].why.as_deref().unwrap();
        assert!(
            why.contains("keeps the copy made for its inputs before")
                && (!cfg!(windows) || why.contains("exited with code 3")),
            "{last:?}"
        );
        assert_eq!(
            std::fs::read_to_string(wt.join("deps/eslint/index.js")).unwrap(),
            "lint"
        );
        // Once the store has them, the other copy gets them and the link
        // moves there; the first copy stays as it was.
        let p2 = failing.services["web"].prepare.as_ref().unwrap();
        let key2 = prepare_key(
            p2,
            &failing.services["web"],
            [(
                "package-lock.json",
                blake3::hash(b"{\"v\":2}").to_hex().to_string(),
            )]
            .into_iter(),
        )
        .unwrap();
        std::fs::write(made.join("deps/eslint/index.js"), "lint 2").unwrap();
        sup.store()
            .put(
                "web",
                &store_key_of(&key2, &p2.outputs, None),
                &made,
                &p2.outputs,
                Duration::from_secs(60),
            )
            .unwrap();
        sup.link_node_lane(&wt, &lane, &failing, &report, &|| false);
        let last = seen.lock().unwrap().last().cloned().unwrap();
        assert_eq!(last[0].state, "linked (store)", "{last:?}");
        assert_eq!(
            std::fs::read_to_string(wt.join("deps/eslint/index.js")).unwrap(),
            "lint 2"
        );
        assert!(lane.join("linked/next/src/deps/eslint/index.js").is_file());
        assert_eq!(
            std::fs::read_to_string(lane.join("linked/src/deps/eslint/index.js")).unwrap(),
            "lint"
        );
        // Back to the first inputs: that copy is still theirs, nothing runs.
        std::fs::write(wt.join("package-lock.json"), "{\"v\":1}").unwrap();
        sup.link_node_lane(&wt, &lane, &stack, &report, &|| false);
        let last = seen.lock().unwrap().last().cloned().unwrap();
        assert_eq!(last[0].state, "linked", "{last:?}");
        assert_eq!(
            std::fs::read_to_string(wt.join("deps/eslint/index.js")).unwrap(),
            "lint"
        );

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

    /// A repository with one commit of `files`, and its main tree's
    /// supervisor for `stack`.
    fn warm_repo(
        d: &Path,
        stack: &str,
        files: &[(&str, &str)],
        rt: &tokio::runtime::Runtime,
    ) -> Arc<Supervisor> {
        let git = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .current_dir(d)
                .status()
                .unwrap();
            assert!(ok.success(), "git {args:?}");
        };
        git(&["init", "-q"]);
        git(&["config", "core.autocrlf", "false"]);
        std::fs::write(d.join(".gitignore"), ".devstack\ndeps\n").unwrap();
        for (rel, text) in files {
            std::fs::write(d.join(rel), text).unwrap();
        }
        git(&["add", "-A"]);
        git(&["commit", "-qm", "init"]);
        let stack = devstack_config::from_str(stack, Path::new("t")).unwrap();
        Supervisor::new(
            stack,
            d.join("devstack.toml"),
            d.to_path_buf(),
            d.join(".devstack"),
            devstack_proc::Options::default(),
            rt.handle().clone(),
        )
        .unwrap()
    }

    const WARM_STACK: &str = "version = 1\n[stack]\nname = \"t\"\n[prepare]\nwarm = [\"HEAD\"]\n[services.web]\nkind = \"devserver\"\nrun = \"x\"\nport = 4200\nprepare = { run = \"mkdir deps && echo made> deps\\\\made.txt\", inputs = [\"package-lock.json\"], outputs = [\"deps\"] }\n";

    #[test]
    fn a_ref_whose_key_the_store_has_is_warm_after_its_inputs_are_read() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let d = tempfile::tempdir().unwrap();
        let sup = warm_repo(
            d.path(),
            WARM_STACK,
            &[("package-lock.json", "{\"v\":1}"), ("app.js", "1")],
            &rt,
        );
        let svc = &sup.stack.services["web"];
        let p = svc.prepare.as_ref().unwrap();
        let key = prepare_key(
            p,
            svc,
            [(
                "package-lock.json",
                blake3::hash(b"{\"v\":1}").to_hex().to_string(),
            )]
            .into_iter(),
        )
        .unwrap();
        let made = d.path().join("made");
        std::fs::create_dir_all(made.join("deps")).unwrap();
        std::fs::write(made.join("deps/a.txt"), "a").unwrap();
        sup.store()
            .put(
                "web",
                &store_key_of(&key, &p.outputs, None),
                &made,
                &p.outputs,
                Duration::ZERO,
            )
            .unwrap();
        let co = crate::warm::warm_here(&sup, "HEAD", "HEAD", &|| false).unwrap();
        // Only the inputs were checked out: the store had the key.
        assert!(co.src.join("package-lock.json").is_file());
        assert!(!co.src.join("app.js").exists());
        assert_eq!(sup.store().entries().len(), 1);
        assert!(sup.notices().is_empty(), "{:?}", sup.notices());
        let dir = co.src.parent().unwrap().to_path_buf();
        drop(co);
        assert!(!dir.exists(), "the checkout goes");
    }

    // #297: a second service with the same `prepare` and `version`, which
    // never ran (no log directory), is warm from the first one's entry.
    #[cfg(windows)]
    #[test]
    fn two_services_with_the_same_prepare_warm_from_one_entry() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let d = tempfile::tempdir().unwrap();
        let prep = "prepare = { run = \"mkdir deps && echo made> deps\\\\made.txt\", inputs = [\"package-lock.json\"], outputs = [\"deps\"], version = \"git --version\" }";
        let stack = format!(
            "version = 1\n[stack]\nname = \"t\"\n[prepare]\nwarm = [\"HEAD\"]\n[services.web]\nkind = \"devserver\"\nrun = \"x\"\nport = 4200\n{prep}\n[services.e2e]\nkind = \"tool\"\nautostart = false\nrun = \"x\"\n{prep}\n"
        );
        let sup = warm_repo(
            d.path(),
            &stack,
            &[("package-lock.json", "{\"v\":3}"), ("app.js", "1")],
            &rt,
        );
        let co = crate::warm::warm_here(&sup, "HEAD", "HEAD", &|| false).unwrap();
        drop(co);
        assert!(sup.notices().is_empty(), "{:?}", sup.notices());
        let entries = sup.store().entries();
        assert_eq!(entries.len(), 1, "one entry for both: {entries:?}");
        let log = std::fs::read_to_string(sup.state_dir.join("daemon.log")).unwrap_or_default();
        assert!(!log.contains("prepare_warm_failed"), "{log}");
    }

    #[cfg(windows)]
    #[test]
    fn a_ref_whose_key_the_store_lacks_is_prepared_into_it() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let d = tempfile::tempdir().unwrap();
        let sup = warm_repo(
            d.path(),
            WARM_STACK,
            &[("package-lock.json", "{\"v\":2}"), ("app.js", "1")],
            &rt,
        );
        let co = crate::warm::warm_here(&sup, "HEAD", "HEAD", &|| false).unwrap();
        assert!(co.src.join("app.js").is_file(), "the whole ref, to prepare");
        let entries = sup.store().entries();
        assert_eq!(entries.len(), 1, "{:?}", sup.notices());
        assert_eq!(entries[0].0.outputs, ["deps"]);
        // A second look: the store has it now.
        drop(co);
        let co = crate::warm::warm_here(&sup, "HEAD", "HEAD", &|| false).unwrap();
        assert!(!co.src.join("app.js").exists());
    }

    #[test]
    fn the_store_lists_its_entries_with_who_used_them_and_removes_one() {
        let d = tempfile::tempdir().unwrap();
        let made = d.path().join("made");
        std::fs::create_dir_all(made.join("deps")).unwrap();
        std::fs::write(made.join("deps/index.js"), "made").unwrap();
        let store = Store::new(d.path().join("store"));
        store
            .put("web", "k1", &made, &["deps".into()], Duration::from_secs(3))
            .unwrap();
        let used = vec![("fix-login".to_string(), "web".to_string(), "k1".to_string())];
        let v = store_view(&store, &used);
        assert!(v.owned);
        assert_eq!(v.entries.len(), 1);
        let e = &v.entries[0];
        assert_eq!((e.service.as_str(), e.key.as_str()), ("web", "k1"));
        assert_eq!((e.files, e.prepare_ms), (1, 3000));
        assert_eq!(e.used_by, vec!["fix-login".to_string()]);
        assert_eq!(v.bytes, e.bytes);
        assert!(matches!(
            store_remove(&store, "web", "../web"),
            Err(StoreRemoveError::NotFound(_))
        ));
        store_remove(&store, "web", "k1").unwrap();
        assert!(store_view(&store, &used).entries.is_empty());
        assert!(matches!(
            store_remove(&store, "web", "k1"),
            Err(StoreRemoveError::NotFound(_))
        ));
        // A node's store is its agent's.
        let lane = Store::with_outbox(d.path().join("store"), d.path().join("out"));
        assert!(!store_view(&lane, &[]).owned);
        assert!(matches!(
            store_remove(&lane, "web", "k1"),
            Err(StoreRemoveError::Refused(_))
        ));
    }

    // `prepare` runs as a process: without devstack's shim, on Windows only.
    #[cfg(windows)]
    #[test]
    fn a_service_says_where_its_prepare_came_from_and_the_store_lists_and_removes_entries() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let d = tempfile::tempdir().unwrap();
        let main = d.path().join("main");
        std::fs::create_dir_all(&main).unwrap();
        let text = "version = 1\n[stack]\nname = \"t\"\n[services.web]\nkind = \"devserver\"\nrun = \"x\"\nport = 4200\nprepare = { run = \"git --version\", inputs = [\"package-lock.json\"], outputs = [\"deps\"] }\n";
        let stack = devstack_config::from_str(text, Path::new("t")).unwrap();
        let sup = Supervisor::new(
            stack,
            main.join("devstack.toml"),
            main.clone(),
            main.join(".devstack"),
            devstack_proc::Options::default(),
            rt.handle().clone(),
        )
        .unwrap();
        let copy = d.path().join("copy");
        std::fs::create_dir_all(copy.join("deps")).unwrap();
        std::fs::write(copy.join("deps/index.js"), "made").unwrap();
        let job = || PrepareJob {
            key: "inputs".into(),
            mark: d.path().join("mark"),
            cwd: copy.clone(),
            run: "git --version".into(),
            counted: Vec::new(),
            store: Ok(StoreUse {
                root: copy.clone(),
                outputs: vec!["deps".into()],
                version: None,
            }),
            before_run: None,
        };
        let key = store_key_of("inputs", &["deps".to_string()], None);

        // The first copy runs it: the store has no entry yet, and gets one.
        let ran = sup.run_prepare("web", job(), &|| false).unwrap();
        assert_eq!(ran.source, "run");
        assert!(ran.stored);
        assert_eq!(ran.key.as_deref(), Some(key.as_str()));
        assert!(ran.miss.as_deref().unwrap().contains("no entry"), "{ran:?}");

        // The next one copies it out of the store.
        let hit = sup.run_prepare("web", job(), &|| false).unwrap();
        assert_eq!(hit.source, "store", "{hit:?}");
        assert_eq!(hit.key.as_deref(), Some(key.as_str()));
        assert!(hit.prepare_ms.is_some() && hit.miss.is_none());
        sup.set_prepare("web", hit.clone());
        assert_eq!(sup.view().services[0].prepare.as_ref(), Some(&hit));
        assert_eq!(sup.prepared_keys(), vec![("web".to_string(), key.clone())]);

        // Without the store: why, and nothing stored.
        let mut off = job();
        off.store = Err(Some("its `version` (`node --version`) failed".into()));
        let plain = sup.run_prepare("web", off, &|| false).unwrap();
        assert_eq!(plain.source, "run");
        assert!(!plain.stored && plain.key.is_none());
        assert!(plain.miss.as_deref().unwrap().contains("version"));

        // The store as the dashboard lists it, with who used an entry.
        let store = sup.store();
        let used = vec![("fix-login".to_string(), "web".to_string(), key.clone())];
        let v = store_view(&store, &used);
        assert!(v.owned);
        assert_eq!(v.entries.len(), 1);
        let e = &v.entries[0];
        assert_eq!((e.service.as_str(), e.key.as_str()), ("web", key.as_str()));
        assert_eq!(e.outputs, vec!["deps".to_string()]);
        assert_eq!(e.files, 1);
        assert_eq!(v.bytes, e.bytes);
        assert!(e.copy_ms.is_some());
        assert_eq!(e.used_by, vec!["fix-login".to_string()]);

        // Removed: gone from the list; a name that is no entry is refused.
        assert!(matches!(
            store_remove(&store, "web", "../web"),
            Err(StoreRemoveError::NotFound(_))
        ));
        store_remove(&store, "web", &key).unwrap();
        assert!(store_view(&store, &used).entries.is_empty());
        assert!(matches!(
            store_remove(&store, "web", &key),
            Err(StoreRemoveError::NotFound(_))
        ));
    }
}
