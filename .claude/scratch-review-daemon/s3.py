import sys; sys.path.insert(0,'D:/Sources/devstack/.claude/scratch-review-daemon')
from rep import apply
P='D:/Sources/devstack/.claude/worktrees/review-daemon/crates/devstack-daemon/src/supervisor.rs'
apply(P,[
# --- prepare: cancellable
("""    /// Run a `prepare` to completion; its output goes to a log of its own.
    fn run_prepare(&self, name: &str, job: PrepareJob) -> Result<(), String> {""",
"""    /// Run a `prepare` to completion; its output goes to a log of its own.
    /// `cancel`: a stop came in, the prepare is killed.
    fn run_prepare(
        &self,
        name: &str,
        job: PrepareJob,
        cancel: &dyn Fn() -> bool,
    ) -> Result<(), String> {"""),
("""        let code = loop {
            if let Some(code) = tree.try_exit_code() {
                break code;
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        if code != 0 {
            return Err(format!(
                "prepare `{}` exited with code {code}\\n{}",""",
"""        let code = loop {
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
                "prepare `{}` exited with code {code}\\n{}","""),
("""    /// A lane's copy, current and prepared for `svc`, before it starts from it.
    fn copy_ready(&self, name: &str, svc: &Service) -> Result<(), String> {
        let job = {
            let mut snap = self.snapshot.lock().unwrap();
            snap.sync().map_err(|e| e.to_string())?;
            self.prepare_job(name, svc, &snap)?
        };
        match job {
            Some(job) => self.run_prepare(name, job),
            None => Ok(()),
        }
    }""",
"""    /// A lane's copy, current and prepared for `svc`, before it starts from it.
    fn copy_ready(
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
        match job {
            Some(job) => self.run_prepare(name, job, cancel),
            None => Ok(()),
        }
    }

    /// Before a command runs from it (ADR-0024 point 2): a lane's copy,
    /// current. The main tree runs commands in place.
    pub(crate) fn sync_copy(&self) -> Result<(), String> {
        if !self.runs_from_copy() {
            return Ok(());
        }
        self.snapshot
            .lock()
            .unwrap()
            .sync()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }"""),
# --- docs: up / owns_main
("""    /// Infra, then every autostart service for this platform, in order. Stops at
    /// the first service that does not become ready.
    /// This lane's instances hold the main slot: workers on (ADR-0008).
    pub fn owns_main(&self) -> bool {""",
"""    /// This lane's instances hold the main slot: workers on (ADR-0008).
    pub fn owns_main(&self) -> bool {"""),
("""    pub fn up(self: &Arc<Self>) -> Result<(), String> {""",
"""    /// Infra, then every autostart service for this platform, in order; a
    /// service that fails holds back only what comes `after` it.
    pub fn up(self: &Arc<Self>) -> Result<(), String> {"""),
# --- down returns failures
("""    /// Stop every service, last started first. Infra stays up (ADR-0004: it is
    /// long-lived) unless `infra` is set.
    pub fn down(&self, infra: bool) {
        for name in self.service_order().into_iter().rev() {
            let _ = self.stop(&name);
        }
        // A lane's infrastructure is its own (ADR-0030); `shared` services
        // are not in its groups. On a node the agent runs it (ADR-0025).
        if infra && self.external_infra().is_none() {
            for g in self.group_order().into_iter().rev() {
                let _ = self.docker.down(&self.compose_group(&g));
            }
            self.refresh_infra();
        }
    }

    /// Build if needed, then start and wait until ready (jobs: until done).
    fn is_hot""",
"""    /// Stop every service, last started first. Infra stays up (ADR-0004: it is
    /// long-lived) unless `infra` is set. Returns what did not stop.
    pub fn down(&self, infra: bool) -> Vec<String> {
        let mut failed = Vec::new();
        for name in self.service_order().into_iter().rev() {
            if let Err(e) = self.stop(&name) {
                failed.push(format!("{name}: {e}"));
            }
        }
        // A lane's infrastructure is its own (ADR-0030); `shared` services
        // are not in its groups. On a node the agent runs it (ADR-0025).
        if infra && self.external_infra().is_none() {
            for g in self.group_order().into_iter().rev() {
                if let Err(e) = self.docker.down(&self.compose_group(&g)) {
                    failed.push(format!("compose group `{g}`: {e}"));
                }
            }
            self.refresh_infra();
        }
        failed
    }

    fn is_hot"""),
("""        self.absorb(name);
        self.held.lock().unwrap().remove(name);
        self.set_candidate(name, None);""",
"""        self.absorb(name);
        self.release_held(name, None);
        self.set_candidate(name, None);"""),
# --- pins: in flight
("""            pinned.extend(slot.view.build_id.clone());
            pinned.extend(slot.previous.clone());""",
"""            pinned.extend(slot.view.build_id.clone());
            pinned.extend(slot.previous.clone());
            pinned.extend(slot.in_flight.clone());"""),
("""        if let Some((id, _)) = self.held.lock().unwrap().get(name) {
            pinned.insert(id.clone());
        }""",
"""        if let Some((id, _)) = self
            .held
            .lock()
            .unwrap()
            .get(name)
            .and_then(|h| h.artifact.as_ref())
        {
            pinned.insert(id.clone());
        }"""),
# --- relaunch keeps its build
("""    /// Start a running service again from its current build, so a changed
    /// worker switch takes effect; nothing is built that is not cached.
    pub fn relaunch(self: &Arc<Self>, name: &str) -> Result<(), String> {
        self.stop(name)?;
        self.start(name)
    }""",
"""    /// Start a running service again from its current build, so a changed
    /// worker switch takes effect; nothing is built (a change in the tree,
    /// held or not, does not come in this way).
    pub fn relaunch(self: &Arc<Self>, name: &str) -> Result<(), String> {
        let current = self.slots.lock().unwrap()[name].view.build_id.clone();
        let artifact = current
            .map(|id| (self.layout.artifact(name, &id), id))
            .filter(|(dir, _)| dir.is_dir())
            .map(|(dir, id)| (id, dir));
        self.stop(name)?;
        self.start_with(name, artifact)
    }

    /// A start of `name` that would take its main port while a lane serves
    /// the main ports, or while they move to one (ADR-0008: the workers
    /// would run in both trees): why not.
    fn main_ports_elsewhere(&self, name: &str, svc: &Service) -> Option<String> {
        if !svc.proxied() && !self.forwards.lock().unwrap().contains_key(name) {
            return None;
        }
        if let Some(lane) = self.served_by() {
            return Some(format!(
                "lane `{lane}` serves the main ports: `devstack promote --lane main` first"
            ));
        }
        self.handing_over
            .load(Ordering::SeqCst)
            .then(|| "the main ports are moving to a lane (`devstack promote --lane`)".into())
    }

    /// Main lane: `promote --lane` begins (`true`) or ends moving the main
    /// ports to a lane.
    pub(crate) fn set_handing_over(&self, on: bool) {
        self.handing_over.store(on, Ordering::SeqCst);
    }

    fn stop_epoch(&self, name: &str) -> u64 {
        self.stops[name].load(Ordering::SeqCst)
    }

    /// A `stop` of `name` came in since `epoch`.
    fn stopped_since(&self, name: &str, epoch: u64) -> bool {
        self.stop_epoch(name) != epoch
    }"""),
# --- start / start_with
("""    pub fn start(self: &Arc<Self>, name: &str) -> Result<(), String> {
        let svc = self.effective(name, self.service(name)?);
        if let Some(lane) = self
            .served_by()
            .filter(|_| svc.proxied() || self.forwards.lock().unwrap().contains_key(name))
        {
            return Err(format!(
                "lane `{lane}` serves the main ports: `devstack promote --lane main` first"
            ));
        }
        if !on_this_platform(&svc) {""",
"""    /// Build if needed, then start and wait until ready (jobs: until done).
    pub fn start(self: &Arc<Self>, name: &str) -> Result<(), String> {
        self.start_with(name, None)
    }

    /// [`Self::start`] from `prebuilt`, an artifact built for it already (a
    /// restart, a promoted candidate): nothing is built again.
    pub(crate) fn start_with(
        self: &Arc<Self>,
        name: &str,
        prebuilt: Option<(String, PathBuf)>,
    ) -> Result<(), String> {
        let svc = self.effective(name, self.service(name)?);
        let requested = now_ms();
        let epoch = self.stop_epoch(name);
        if let Some(e) = self.main_ports_elsewhere(name, &svc) {
            return Err(e);
        }
        if !on_this_platform(&svc) {"""),
("""        let _op = self.op_locks[name].lock().unwrap();
        if self.slots.lock().unwrap()[name].view.status.is_active() {
            return Ok(());
        }
        let result = self.start_locked(name, &svc);
        if let Err(e) = &result {
            self.set_status(name, Status::Failed { reason: e.clone() });
        }
        result
    }""",
"""        let _op = self.op_locks[name].lock().unwrap();
        {
            let slots = self.slots.lock().unwrap();
            let v = &slots[name].view;
            if v.status.is_active() {
                return Ok(());
            }
            // Two services after the same job: the other one ran it while
            // this one waited for its op lock.
            if svc.kind == Kind::Job
                && v.status == (Status::Completed { exit_code: 0 })
                && v.since >= requested
            {
                return Ok(());
            }
        }
        let result = self.start_locked(name, &svc, prebuilt, epoch);
        self.slots.lock().unwrap().get_mut(name).unwrap().in_flight = None;
        if let Err(e) = &result {
            self.set_status(name, Status::Failed { reason: e.clone() });
        }
        result
    }

    /// The artifact of the first start of `name` after a daemon start: the
    /// last one promoted to main, if it is still there (ADR-0016). What the
    /// tree holds now comes as a candidate, like any change.
    fn restorable(&self, name: &str) -> Option<(String, PathBuf)> {
        if self.force_build.lock().unwrap().contains(name) {
            return None;
        }
        let id = self.persisted.lock().unwrap().main.get(name).cloned()?;
        self.builder.artifact(name, &id)?;
        Some((id.clone(), self.layout.artifact(name, &id)))
    }

    /// After a restore: what changed in the tree while no daemon watched it
    /// becomes a candidate, as if the watcher had seen it (a promotion gate
    /// sees those paths too).
    fn follow_restore(self: &Arc<Self>, name: &str, svc: &Service) {
        let Some(before) = self.opened_manifest.lock().unwrap().clone() else {
            return;
        };
        let Some(watch) = devstack_build::watch_set(&svc.watch).ok().flatten() else {
            return; // without `watch`, edits make no candidates
        };
        let (me, name) = (Arc::clone(self), name.to_string());
        let _ = std::thread::Builder::new()
            .name(format!("restored-{name}"))
            .spawn(move || {
                let changed: Vec<String> = {
                    let mut snap = me.snapshot.lock().unwrap();
                    if snap.sync().is_err() {
                        return;
                    }
                    let now = &snap.manifest.files;
                    let mut paths: BTreeSet<&String> = now
                        .iter()
                        .filter(|(p, e)| before.files.get(*p) != Some(*e))
                        .map(|(p, _)| p)
                        .collect();
                    paths.extend(before.files.keys().filter(|p| !now.contains_key(*p)));
                    paths
                        .into_iter()
                        .filter(|p| {
                            !crate::watch::ignored(p)
                                && (watch.is_match(p.as_str())
                                    || devstack_build::is_build_file(p))
                        })
                        .cloned()
                        .collect()
                };
                if !changed.is_empty() {
                    crate::diag::info(
                        "restored_tree_changed",
                        serde_json::json!({ "lane": me.lane, "service": name, "paths": changed }),
                    );
                }
                for p in changed {
                    me.changed(&name, &p);
                }
            });
    }

    /// The promotion gate of `name` opens (ADR-0013): its held candidate
    /// went into main, or a build of the tree as it is now did. `only`:
    /// just when the held build is that one.
    fn release_held(&self, name: &str, only: Option<&str>) {
        let mut held = self.held.lock().unwrap();
        let matches = held.get(name).is_some_and(|h| {
            only.is_none_or(|id| h.artifact.as_ref().is_some_and(|(a, _)| a == id))
        });
        if matches {
            held.remove(name);
            self.save_held(&held);
        }
    }

    /// Hold `artifact` of `name` for `promote` (`None`: its newest build
    /// failed, the gate stays).
    fn hold(&self, name: &str, artifact: Option<(String, PathBuf)>, reason: String) {
        let mut held = self.held.lock().unwrap();
        held.insert(name.to_string(), Held { artifact, reason });
        self.save_held(&held);
    }

    fn save_held(&self, held: &BTreeMap<String, Held>) {
        let mut pers = self.persisted.lock().unwrap();
        pers.held = held
            .iter()
            .map(|(n, h)| {
                (
                    n.clone(),
                    crate::state::HeldRecord {
                        build_id: h
                            .artifact
                            .as_ref()
                            .map(|(id, _)| id.clone())
                            .unwrap_or_default(),
                        reason: h.reason.clone(),
                    },
                )
            })
            .collect();
        pers.save(&self.state_dir);
    }"""),
# --- start_locked
("""    fn start_locked(self: &Arc<Self>, name: &str, svc: &Service) -> Result<(), String> {
        // 1. Build into an artifact (services and jobs with `build`).
        let artifact = if svc.build.is_some() {
            Some(self.build_locked(name, svc, &|| false)?)
        } else {
            None
        };
""",
"""    fn start_locked(
        self: &Arc<Self>,
        name: &str,
        svc: &Service,
        prebuilt: Option<(String, PathBuf)>,
        epoch: u64,
    ) -> Result<(), String> {
        let cancel = || self.stopped_since(name, epoch);
        let first = self.started.lock().unwrap().insert(name.to_string());
        // 1. The artifact (services and jobs with `build`): the one given; at
        //    the first start after a daemon start the last promoted one
        //    (ADR-0016); else built from the tree.
        let (mut restored, mut built) = (false, false);
        let artifact = match (svc.build.is_some(), prebuilt) {
            (false, _) => None,
            (true, Some(a)) => Some(a),
            (true, None) => match first.then(|| self.restorable(name)).flatten() {
                Some(a) => {
                    restored = true;
                    crate::diag::info(
                        "restored",
                        serde_json::json!({
                            "lane": self.lane,
                            "service": name,
                            "build_id": a.0,
                            "why": "the first start after a daemon start runs the build last promoted to main (ADR-0016); a change in the tree comes as a candidate",
                        }),
                    );
                    Some(a)
                }
                None => {
                    built = true;
                    Some(self.build_locked(name, svc, &cancel)?)
                }
            },
        };
        if let Some((id, _)) = &artifact {
            self.slots.lock().unwrap().get_mut(name).unwrap().in_flight = Some(id.clone());
        }
"""),
("""        let mut inst = match self.launch(name, svc, artifact.as_ref(), port, true) {
            Ok(i) => i,
            Err(e) => {
                refuse(&e);
                return Err(e);
            }
        };""",
"""        let mut inst = match self.launch(name, svc, artifact.as_ref(), port, true, &cancel) {
            Ok(i) => i,
            Err(e) => {
                refuse(&e);
                return Err(e);
            }
        };"""),
("""        self.set_status(name, Status::Starting);
        let ready = match self.wait_instance(svc, &mut inst) {
            Ok(s) => s,
            Err(e) => {
                inst.tree.kill();
                self.forget_record(&inst.record);
                refuse(&e);
                return Err(e);
            }
        };

        // 4. In service: behind the proxy, switch it on; record it; watch it.
        if let Some(p) = &proxy {
            p.set_upstream(Some(Upstream {""",
"""        self.set_status(name, Status::Starting);
        let ready = match self.wait_instance(svc, &mut inst, &cancel) {
            Ok(s) => s,
            Err(e) => {
                inst.tree.kill();
                self.forget_record(&inst.record);
                refuse(&e);
                return Err(e);
            }
        };

        // 4. In service: behind the proxy, switch it on; record it; watch it.
        //    Unless a lane took the main ports meanwhile (`promote --lane`
        //    while this one built): this instance runs with workers.
        if proxy.is_some()
            && let Some(e) = self.main_ports_elsewhere(name, svc)
        {
            inst.tree.kill();
            self.forget_record(&inst.record);
            return Err(e);
        }
        if let Some(p) = &proxy {
            p.set_upstream(Some(Upstream {"""),
("""        if let Some((id, _)) = &artifact {
            let mut pers = self.persisted.lock().unwrap();
            pers.main.insert(name.into(), id.clone());
            pers.save(&self.state_dir);
        }
        if svc.kind == Kind::Job {
            self.forget_record(&inst.record);
            self.set_status(name, ready);
            return Ok(());
        }
        let generation = {
            let mut slots = self.slots.lock().unwrap();
            let slot = slots.get_mut(name).unwrap();
            slot.generation += 1;
            slot.main = Some(inst);
            slot.generation
        };
        self.set_status(name, ready);
        self.spawn_monitor(name.to_string(), svc.clone(), generation);
        Ok(())
    }""",
"""        if let Some((id, _)) = &artifact {
            let mut pers = self.persisted.lock().unwrap();
            pers.main.insert(name.into(), id.clone());
            pers.save(&self.state_dir);
        }
        // The tree as it is now is main: a held candidate is older than it.
        if built {
            self.release_held(name, None);
        }
        if svc.kind == Kind::Job {
            self.forget_record(&inst.record);
            self.set_status(name, ready);
            return Ok(());
        }
        let generation = {
            let mut slots = self.slots.lock().unwrap();
            let slot = slots.get_mut(name).unwrap();
            slot.generation += 1;
            slot.main = Some(inst);
            slot.generation
        };
        self.set_status(name, ready);
        self.spawn_monitor(name.to_string(), svc.clone(), generation);
        if restored {
            self.follow_restore(name, svc);
        }
        Ok(())
    }"""),
# --- launch
("""        port: Option<u16>,
        is_main: bool,
    ) -> Result<Instance, String> {
        if artifact.is_none() && self.runs_from_copy() {
            self.copy_ready(name, svc)?;
        }""",
"""        port: Option<u16>,
        is_main: bool,
        cancel: &dyn Fn() -> bool,
    ) -> Result<Instance, String> {
        if artifact.is_none() && self.runs_from_copy() {
            self.copy_ready(name, svc, cancel)?;
        }"""),
])
print("ok")
