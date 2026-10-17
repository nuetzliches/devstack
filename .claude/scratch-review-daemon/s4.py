import sys; sys.path.insert(0,'D:/Sources/devstack/.claude/scratch-review-daemon')
from rep import apply
P='D:/Sources/devstack/.claude/worktrees/review-daemon/crates/devstack-daemon/src/supervisor.rs'
apply(P,[
("""        building(None);
        let mut snap = self.snapshot.lock().unwrap();
        snap.sync().map_err(|e| e.to_string())?;
        let mut id = build_id(&snap.manifest, svc).map_err(|e| e.to_string())?;
        // `--rebuild`: the same inputs once more, under `<id>-<n>`; the artifact
        // that exists may be running and is never overwritten (ADR-0002).
        if self.force_build.lock().unwrap().remove(name) {""",
"""        building(None);
        // `--rebuild` once, for every attempt below.
        let forced = self.force_build.lock().unwrap().remove(name);
        // All services of a tree share one snapshot: another one's build may
        // sync it while this one builds. An artifact whose inputs changed
        // under it is not what its id says (ADR-0002); it goes, and the build
        // runs again on the tree as it is now.
        for _ in 0..3 {
            if let Some(a) = self.build_once(name, svc, cancel, &building, forced)? {
                self.slots.lock().unwrap().get_mut(name).unwrap().in_flight = Some(a.0.clone());
                return Ok(a);
            }
        }
        Err("the sources changed under every build attempt (other builds synced the snapshot meanwhile); try again".into())
    }

    /// One build of [`Self::build_locked`]; `None` when the snapshot's
    /// inputs of `name` changed while it built (its artifact is discarded).
    fn build_once(
        &self,
        name: &str,
        svc: &Service,
        cancel: &dyn Fn() -> bool,
        building: &dyn Fn(Option<String>),
        forced: bool,
    ) -> Result<Option<(String, PathBuf)>, String> {
        let mut snap = self.snapshot.lock().unwrap();
        snap.sync().map_err(|e| e.to_string())?;
        let base = build_id(&snap.manifest, svc).map_err(|e| e.to_string())?;
        let mut id = base.clone();
        // `--rebuild`: the same inputs once more, under `<id>-<n>`; the artifact
        // that exists may be running and is never overwritten (ADR-0002).
        if forced {"""),
("""            if let Some(job) = self.prepare_job(name, svc, &snap)? {
                drop(snap);
                self.run_prepare(name, job)?;
                snap = self.snapshot.lock().unwrap();
            }
            let mut running = self""",
"""            if let Some(job) = self.prepare_job(name, svc, &snap)? {
                drop(snap);
                self.run_prepare(name, job, cancel)?;
                snap = self.snapshot.lock().unwrap();
            }
            let started = now_ms();
            let mut running = self"""),
("""            match outcome {
                Outcome::Published { .. } => self.prune_artifacts(name),
                Outcome::Failed { exit_code, log, .. } => {""",
"""            match outcome {
                Outcome::Published { .. } => {
                    let now = build_id(&self.snapshot.lock().unwrap().manifest, svc)
                        .map_err(|e| e.to_string())?;
                    if now != base {
                        crate::diag::warn(
                            "build_discarded",
                            serde_json::json!({
                                "lane": self.lane,
                                "service": name,
                                "build_id": id,
                                "why": "its inputs changed in the snapshot while it built (another build synced it); built again",
                            }),
                        );
                        self.discard_artifact(name, &id, started);
                        return Ok(None);
                    }
                    self.prune_artifacts(name)
                }
                Outcome::Failed { exit_code, log, .. } => {"""),
("""                Outcome::Cancelled { .. } => return Err("build cancelled".into()),
            }
        }
        Ok((id.clone(), self.layout.artifact(name, &id)))
    }""",
"""                Outcome::Cancelled { .. } => return Err("build cancelled".into()),
            }
        }
        Ok(Some((id.clone(), self.layout.artifact(name, &id))))
    }

    /// Remove an artifact this daemon just published (since `since`, Unix
    /// ms), its metadata first: without it the directory is no artifact.
    fn discard_artifact(&self, name: &str, id: &str, since: u128) {
        if self
            .builder
            .artifact(name, id)
            .is_none_or(|m| m.created < since)
        {
            return; // published before this build: not this build's to remove
        }
        let dir = self.layout.artifact(name, id);
        let _ = std::fs::remove_file(dir.join(devstack_build::META_FILE));
        let _ = std::fs::remove_dir_all(&dir);
    }"""),
# --- wait_instance
("""    /// the proxy: ADR-0010 condition 1), else a moment to fail fast. Returns the
    /// status to show.
    fn wait_instance(&self, svc: &Service, inst: &mut Instance) -> Result<Status, String> {
        match svc.kind {
            Kind::Job => loop {
                if let Some(code) = inst.tree.try_exit_code() {
                    if code == 0 {
                        return Ok(Status::Completed { exit_code: 0 });
                    }
                    return Err(format!(
                        "job exited with code {code}\\n{}",
                        tail(&inst.log, 15)
                    ));
                }
                std::thread::sleep(Duration::from_millis(100));
            },""",
"""    /// the proxy: ADR-0010 condition 1), else a moment to fail fast. Returns the
    /// status to show. A job runs for its `timeout` at most; `cancel` (a
    /// stop) ends the wait at once. The caller kills the instance on an error.
    fn wait_instance(
        &self,
        svc: &Service,
        inst: &mut Instance,
        cancel: &dyn Fn() -> bool,
    ) -> Result<Status, String> {
        match svc.kind {
            Kind::Job => {
                let limit = svc.timeout.unwrap_or(JOB_TIMEOUT);
                let t0 = Instant::now();
                loop {
                    if let Some(code) = inst.tree.try_exit_code() {
                        if code == 0 {
                            return Ok(Status::Completed { exit_code: 0 });
                        }
                        return Err(format!(
                            "job exited with code {code}\\n{}",
                            tail(&inst.log, 15)
                        ));
                    }
                    if cancel() {
                        return Err(STOPPED.into());
                    }
                    if t0.elapsed() >= limit {
                        return Err(format!(
                            "job still ran after {} s (its `timeout`), and was stopped\\n{}",
                            limit.as_secs(),
                            tail(&inst.log, 15)
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }"""),
("""                    while t0.elapsed() < h.timeout {
                        if let Some(code) = inst.tree.try_exit_code() {""",
"""                    while t0.elapsed() < h.timeout {
                        if cancel() {
                            return Err(STOPPED.into());
                        }
                        if let Some(code) = inst.tree.try_exit_code() {"""),
# --- monitor generation check
("""                        let inst = {
                            let mut slots = me.slots.lock().unwrap();
                            let slot = slots.get_mut(&name).unwrap();
                            slot.view.pid = None;
                            slot.main.take()
                        };""",
"""                        let inst = {
                            let mut slots = me.slots.lock().unwrap();
                            let slot = slots.get_mut(&name).unwrap();
                            // A switch or a stop came in since the exit was
                            // seen: the slot holds another instance now.
                            if slot.generation != generation {
                                return;
                            }
                            slot.view.pid = None;
                            slot.main.take()
                        };"""),
# --- stop bumps the epoch
("""    pub fn stop(&self, name: &str) -> Result<(), String> {
        self.service(name)?;
        let _op = self.op_locks[name].lock().unwrap();""",
"""    /// A start, build or job wait in progress gives up first and lets go of
    /// the op lock (a hanging migrator, a long build).
    pub fn stop(&self, name: &str) -> Result<(), String> {
        self.service(name)?;
        self.stops[name].fetch_add(1, Ordering::SeqCst);
        let _op = self.op_locks[name].lock().unwrap();"""),
# --- doc misattached: restart / env_of
("""    /// Build first; only a successful build replaces the running instance
    /// (ADR-0003). Behind the proxy the replacement is a candidate that takes
    /// over without dropping a request; otherwise a restart in place.
    /// The environment `name` gets from this supervisor, as it would start""",
"""    /// The environment `name` gets from this supervisor, as it would start"""),
("""    pub fn restart(self: &Arc<Self>, name: &str) -> Result<(), String> {
        if self.is_hot(name) {
            // The watcher builds by itself; a restart starts it afresh.
            self.stop(name)?;
            return self.start(name);
        }
        let svc = self.service(name)?.clone();
        self.absorb(name);
        let mut artifact = None;
        if svc.build.is_some() {
            let _op = self.op_locks[name].lock().unwrap();
            let before = self.slots.lock().unwrap()[name].view.status.clone();
            match self.build_locked(name, &svc, &|| false) {""",
"""    /// Build first; only a successful build replaces the running instance
    /// (ADR-0003). Behind the proxy the replacement is a candidate that takes
    /// over without dropping a request; otherwise a restart in place, from
    /// the artifact just built.
    pub fn restart(self: &Arc<Self>, name: &str) -> Result<(), String> {
        let result = self.restart_inner(name);
        if let Some(s) = self.slots.lock().unwrap().get_mut(name) {
            s.in_flight = None;
        }
        result
    }

    fn restart_inner(self: &Arc<Self>, name: &str) -> Result<(), String> {
        if self.is_hot(name) {
            // The watcher builds by itself; a restart starts it afresh.
            self.stop(name)?;
            return self.start(name);
        }
        let svc = self.service(name)?.clone();
        self.absorb(name);
        let mut artifact = None;
        if svc.build.is_some() {
            let epoch = self.stop_epoch(name);
            let _op = self.op_locks[name].lock().unwrap();
            let before = self.slots.lock().unwrap()[name].view.status.clone();
            match self.build_locked(name, &svc, &|| self.stopped_since(name, epoch)) {"""),
("""        let running = self.slots.lock().unwrap()[name].main.is_some();
        if svc.proxied() && running {
            return self.replace(name, &svc, artifact);
        }
        self.set_candidate(name, None);
        self.stop(name)?;
        self.start(name)
    }""",
"""        let running = self.slots.lock().unwrap()[name].main.is_some();
        if svc.proxied() && running {
            self.replace(name, &svc, artifact)?;
        } else {
            self.set_candidate(name, None);
            self.stop(name)?;
            self.start_with(name, artifact)?;
        }
        // A build of the tree as it is now is main: a held candidate is older.
        self.release_held(name, None);
        Ok(())
    }"""),
# --- replace
("""        artifact: Option<(String, PathBuf)>,
    ) -> Result<(), String> {
        let _op = self.op_locks[name].lock().unwrap();
        let proxy = self.ensure_proxy(name, svc)?;""",
"""        artifact: Option<(String, PathBuf)>,
    ) -> Result<(), String> {
        let epoch = self.stop_epoch(name);
        let cancel = || self.stopped_since(name, epoch);
        let _op = self.op_locks[name].lock().unwrap();
        let proxy = self.ensure_proxy(name, svc)?;"""),
("""        let mut candidate =
            match self.launch(name, svc, artifact.as_ref(), Some(free_port()?), !relay) {
                Ok(i) => i,
                Err(e) => return fail(e),
            };
        if let Err(e) = self.wait_instance(svc, &mut candidate) {
            candidate.tree.kill();
            self.forget_record(&candidate.record);
            return fail(e);
        }
""",
"""        let mut candidate = match self.launch(
            name,
            svc,
            artifact.as_ref(),
            Some(free_port()?),
            !relay,
            &cancel,
        ) {
            Ok(i) => i,
            Err(e) => return fail(e),
        };
        if let Err(e) = self.wait_instance(svc, &mut candidate, &cancel) {
            candidate.tree.kill();
            self.forget_record(&candidate.record);
            return fail(e);
        }
        // A lane took the main ports meanwhile (`promote --lane`): the
        // proxy forwards there now, and stays so.
        if let Some(e) = self.main_ports_elsewhere(name, svc) {
            candidate.tree.kill();
            self.forget_record(&candidate.record);
            return fail(e);
        }
"""),
("""        if relay {
            let mut fin = match self.launch(name, svc, artifact.as_ref(), Some(free_port()?), true)
            {
                Ok(i) => i,
                Err(e) => return self.running_without_workers(name, svc, e),
            };
            if let Err(e) = self.wait_instance(svc, &mut fin) {""",
"""        if relay {
            let mut fin = match self.launch(
                name,
                svc,
                artifact.as_ref(),
                Some(free_port()?),
                true,
                &cancel,
            ) {
                Ok(i) => i,
                Err(e) => return self.running_without_workers(name, svc, e),
            };
            if let Err(e) = self.wait_instance(svc, &mut fin, &cancel) {"""),
])
print("ok")
