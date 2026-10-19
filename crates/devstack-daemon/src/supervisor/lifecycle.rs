//! The slot lifecycle: start, stop, monitor.

use super::{
    Instance, JOB_TIMEOUT, KEEP_BUILD_LOGS, KEEP_RUN_LOGS, STOPPED, Supervisor, free_port,
    on_this_platform, port_in_use, tail,
};
use crate::proxy::{Proxy, Upstream, Waiting};
use crate::state::{EventBody, Status, now_ms};
use devstack_config::model::{Kind, Service};
use devstack_env::Context;
use devstack_proc::Spec;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// How long a service without a health check is given to fail fast before
/// it counts as running: a fixed wait, not a measurement.
pub(super) const SETTLE: Duration = Duration::from_millis(500);
/// A long-running service that exits by itself is started again, after 1,
/// 2, 4, … s (at most a minute), [`MAX_EXITS`] times in a row; an instance
/// that ran this long starts the count again.
pub(super) const STABLE: Duration = Duration::from_secs(300);
const MAX_EXITS: u32 = 5;

impl Supervisor {
    /// Infra, then every autostart service for this platform, in order; a
    /// service that fails holds back only what comes `after` it.
    pub fn up(self: &Arc<Self>) -> Result<(), String> {
        {
            // Every lane has its own infrastructure (ADR-0030).
            self.infra_up()?;
            // Healthy inside Docker is not reachable from here: say so
            // (a service that needs the container repairs it, ensure_after).
            for (g, svc, lost) in self.check_forwards(true) {
                self.emit(EventBody::Infra {
                    group: g,
                    message: format!(
                        "{svc}: {} closes connections from this machine at once (Docker lost the port forward, or nothing listens inside)",
                        lost.join(", ")
                    ),
                });
            }
        }
        // A failed service holds back only what comes `after` it; everything
        // independent of it still starts (one broken build must not keep the
        // rest of the stack down).
        let mut failed: BTreeSet<String> = BTreeSet::new();
        for name in self.service_order() {
            let svc = self.service(&name)?;
            if !svc.autostart || !self.runs_here(svc) {
                continue;
            }
            if let Some(dep) = svc.after.iter().find(|d| failed.contains(*d)) {
                self.set_status(
                    &name,
                    Status::Failed {
                        reason: format!("not started: `{dep}` failed"),
                    },
                );
                failed.insert(name);
                continue;
            }
            if self.start(&name).is_err() {
                failed.insert(name);
            }
        }
        if failed.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "failed: {}",
                failed.into_iter().collect::<Vec<_>>().join(", ")
            ))
        }
    }

    /// Stop every service, last started first. Infra stays up (ADR-0004: it is
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

    pub(super) fn is_hot(&self, name: &str) -> bool {
        self.hot.lock().unwrap().contains(name)
    }

    /// The service as it runs now: in hot-reload mode its `hot_reload`
    /// command from the source tree, without a build (plan §3.4a).
    fn effective(&self, name: &str, svc: &Service) -> Service {
        match (&svc.hot_reload, self.is_hot(name)) {
            (Some(h), true) => {
                let mut s = svc.clone();
                s.build = None;
                s.output = None;
                s.run = h.run.clone();
                if h.cwd.is_some() {
                    s.cwd = h.cwd.clone();
                }
                s.watch.clear();
                s.promote = None;
                s
            }
            _ => svc.clone(),
        }
    }

    /// Switch hot-reload mode on or off (plan §3.4a): on, the running
    /// instance and any candidate make way for the toolchain's watcher from
    /// the source tree on the same port; off, the service is built and
    /// started as usual again.
    pub fn hot_reload(self: &Arc<Self>, name: &str, on: bool) -> Result<(), String> {
        let svc = self.service(name)?.clone();
        if svc.hot_reload.is_none() {
            return Err(format!(
                "`{name}` has no hot-reload mode: add [services.{name}.hot_reload] with the watcher's `run` to the stack file"
            ));
        }
        if self.is_hot(name) == on {
            return Ok(());
        }
        {
            let mut hot = self.hot.lock().unwrap();
            if on {
                hot.insert(name.to_string());
            } else {
                hot.remove(name);
            }
        }
        if let Some(slot) = self.slots.lock().unwrap().get_mut(name) {
            slot.view.hot_reload = on;
            slot.previous = None;
        }
        self.absorb(name);
        self.release_held(name, None);
        self.set_candidate(name, None);
        crate::diag::log(
            "info",
            "hot_reload",
            serde_json::json!({ "service": name, "on": on }),
        );
        self.stop(name)?;
        self.start(name)
    }

    /// Start a running service again from its current build, so a changed
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

    /// `name` exited by itself after running `ran`: start it again from the
    /// same build after a backoff, unless something else touched it
    /// meanwhile (a stop, a start, a switch); `Failed` after [`MAX_EXITS`]
    /// times in a row.
    fn restart_exited(
        self: &Arc<Self>,
        name: &str,
        generation: u64,
        ran: Duration,
        code: i32,
        build: Option<String>,
    ) {
        let n = {
            let mut slots = self.slots.lock().unwrap();
            let Some(slot) = slots.get_mut(name) else {
                return;
            };
            if ran >= STABLE {
                slot.exits = 0;
            }
            slot.exits += 1;
            slot.exits
        };
        if n > MAX_EXITS {
            let reason = format!(
                "exited by itself {MAX_EXITS} times in a row (last with code {code}); not started again (devstack logs {name})"
            );
            crate::diag::warn(
                "service_given_up",
                serde_json::json!({ "lane": self.lane, "service": name, "reason": reason }),
            );
            self.set_status(name, Status::Failed { reason });
            return;
        }
        let backoff = Duration::from_secs(1 << (n - 1).min(6)).min(Duration::from_secs(60));
        let epoch = self.stop_epoch(name);
        std::thread::sleep(backoff);
        let still = {
            let slots = self.slots.lock().unwrap();
            slots.get(name).is_some_and(|s| {
                s.generation == generation && matches!(s.view.status, Status::Exited { .. })
            })
        };
        if !still || self.stopped_since(name, epoch) || self.closed() {
            return;
        }
        crate::diag::info(
            "service_restarting",
            serde_json::json!({
                "lane": self.lane,
                "service": name,
                "attempt": n,
                "after_exit_code": code,
                "backoff_ms": backoff.as_millis() as u64,
            }),
        );
        let artifact = build
            .map(|id| (self.layout.artifact(name, &id), id))
            .filter(|(dir, _)| dir.is_dir())
            .map(|(dir, id)| (id, dir));
        let _ = self.start_with(name, artifact);
    }

    pub(super) fn stop_epoch(&self, name: &str) -> u64 {
        self.stops[name].load(Ordering::SeqCst)
    }

    /// A `stop` of `name` came in since `epoch`.
    pub(super) fn stopped_since(&self, name: &str, epoch: u64) -> bool {
        self.stop_epoch(name) != epoch
    }

    /// Build if needed, then start and wait until ready (jobs: until done).
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
        if !on_this_platform(&svc) {
            return Err(format!("`{name}` does not run on this platform"));
        }
        if !self.runs_here(&svc) {
            return Err(format!(
                "`{name}` runs in the main working tree only (`lanes = false`); its instance there serves this lane too"
            ));
        }
        if let Err(e) = self.ensure_after(&svc) {
            self.set_status(name, Status::Failed { reason: e.clone() });
            return Err(e);
        }
        let _op = self.op_locks[name].lock().unwrap();
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
        {
            let mut slots = self.slots.lock().unwrap();
            let slot = slots.get_mut(name).unwrap();
            slot.in_flight = None;
            if result.is_err() && slot.main.is_none() {
                // The process it started is gone.
                slot.view.pid = None;
                slot.view.instance_port = None;
            }
        }
        if let Err(e) = &result {
            self.set_status(name, Status::Failed { reason: e.clone() });
        }
        result
    }

    /// The proxy on the service's fixed port, bound on first use (ADR-0010).
    pub(super) fn ensure_proxy(&self, name: &str, svc: &Service) -> Result<Proxy, String> {
        let mut proxies = self.proxies.lock().unwrap();
        if let Some(p) = proxies.get(name) {
            return Ok(p.clone());
        }
        let port = svc.port.expect("proxied services have a port");
        if port_in_use(port) {
            return Err(format!("port {port} is already in use by another process"));
        }
        let p =
            Proxy::bind(name, port, &self.rt).map_err(|e| format!("proxy on port {port}: {e}"))?;
        proxies.insert(name.into(), p.clone());
        Ok(p)
    }

    pub(super) fn proxy(&self, name: &str) -> Option<Proxy> {
        self.proxies.lock().unwrap().get(name).cloned()
    }

    fn start_locked(
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

        // 2. The port: behind the proxy an internal one, else the fixed one, free.
        let proxy = if svc.proxied() {
            let p = self.ensure_proxy(name, svc)?;
            let hold = svc
                .health
                .as_ref()
                .map_or(Duration::from_secs(60), |h| h.timeout);
            p.set_waiting(Waiting::Hold(hold));
            Some(p)
        } else {
            if let Some(port) = svc.port
                && port_in_use(port)
            {
                return Err(format!("port {port} is already in use by another process"));
            }
            None
        };
        let port = match &proxy {
            Some(_) => Some(free_port()?),
            None => svc.port,
        };

        // 3. Start and wait until ready (jobs: until done).
        let refuse = |reason: &str| {
            if let Some(p) = &proxy {
                p.set_waiting(Waiting::Refuse(format!(
                    "`{name}` failed to start: {}",
                    reason.lines().next().unwrap_or("")
                )));
            }
        };
        let mut inst = match self.launch(name, svc, artifact.as_ref(), port, true, &cancel) {
            Ok(i) => i,
            Err(e) => {
                refuse(&e);
                return Err(e);
            }
        };
        {
            let mut slots = self.slots.lock().unwrap();
            let v = &mut slots.get_mut(name).unwrap().view;
            v.pid = Some(inst.tree.root_pid());
            v.log = Some(inst.log.clone());
            v.build_id = inst.build_id.clone();
            v.instance_port = inst.port;
        }
        self.set_status(name, Status::Starting);
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
            p.set_upstream(Some(Upstream {
                port: inst.port.expect("internal port"),
                build: inst.build_id.clone(),
            }));
            p.set_pending(None);
        }
        if let Some((id, _)) = &artifact {
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
    }

    /// Start one process of `svc` on `port`; `is_main` decides its worker
    /// switches (ADR-0008). Its cleanup record is persisted (ADR-0016).
    pub(super) fn launch(
        &self,
        name: &str,
        svc: &Service,
        artifact: Option<&(String, PathBuf)>,
        port: Option<u16>,
        is_main: bool,
        cancel: &dyn Fn() -> bool,
    ) -> Result<Instance, String> {
        if artifact.is_none() && self.runs_from_copy() {
            self.copy_ready(name, svc, cancel)?;
        }
        let ctx = self.context(svc, artifact.map(|(_, d)| d.as_path()), port, is_main);
        let env = {
            let infra = self.infra.lock().unwrap();
            devstack_env::compose(name, svc, &ctx, &infra.envs, true).map_err(|e| e.to_string())?
        };
        let lookup = |k: &str| ctx.var(k);
        let command = devstack_config::interpolate::expand(&svc.run, &ctx, &lookup)
            .map_err(|e| format!("run: {e}"))?;
        for d in &svc.ensure_dirs {
            let dir = devstack_config::interpolate::expand(d, &ctx, &lookup)
                .map_err(|e| format!("ensure_dirs: {e}"))?;
            std::fs::create_dir_all(&dir).map_err(|e| format!("create {dir}: {e}"))?;
        }
        let cwd = match (artifact, &svc.cwd) {
            // Built services run from their artifact (ADR-0002); its files are their content root.
            (Some((_, dir)), _) => dir.clone(),
            (None, Some(c)) => self.run_root().join(c),
            (None, None) => self.run_root(),
        };
        let mut env_map: BTreeMap<String, String> =
            env.into_iter().map(|(k, v)| (k, v.value)).collect();
        if let Some((id, _)) = artifact {
            env_map.insert("DEVSTACK_BUILD_ID".into(), id.clone());
        }
        let record = match port {
            Some(p) => format!("{name}@{p}"),
            None => name.to_string(),
        };
        let log = self.log_dir(name).join(crate::logfiles::run_name(
            now_ms(),
            port,
            artifact.map(|(id, _)| id.as_str()),
        ));
        std::fs::create_dir_all(log.parent().unwrap()).map_err(|e| e.to_string())?;
        let file = std::fs::File::create(&log).map_err(|e| e.to_string())?;
        // Every start and every build writes a log; keep the newest (plan §11).
        let dir = log.parent().unwrap();
        crate::diag::prune(dir, "run-", KEEP_RUN_LOGS);
        crate::diag::prune(dir, "build-", KEEP_BUILD_LOGS);
        let spec = Spec {
            name: record.clone(),
            command,
            cwd,
            env: env_map,
            log: file,
        };
        let (tree, rec) = devstack_proc::spawn(spec, &self.proc_opts).map_err(|e| e.to_string())?;
        {
            let mut p = self.persisted.lock().unwrap();
            p.records.retain(|r| r.name != record);
            p.records.push(rec);
            p.save(&self.state_dir);
        }
        Ok(Instance {
            tree,
            port,
            build_id: artifact.map(|(id, _)| id.clone()),
            log,
            record,
            started: Instant::now(),
        })
    }

    pub(super) fn forget_record(&self, record: &str) {
        let mut p = self.persisted.lock().unwrap();
        p.records.retain(|r| r.name != record);
        p.save(&self.state_dir);
    }

    fn context(
        &self,
        svc: &Service,
        artifact: Option<&Path>,
        port: Option<u16>,
        is_main: bool,
    ) -> Context {
        let infra = self.infra.lock().unwrap();
        let mut ctx = Context {
            repo: self.run_root(),
            artifact: artifact.map(Path::to_path_buf),
            port,
            // A lane without the main slot never runs workers (ADR-0012 interim rule).
            // What runs in the main tree only keeps its workers: no lane takes them over.
            is_main: is_main
                && (self.owns_main() || !svc.lanes || self.lane_workers.load(Ordering::SeqCst)),
            // The slot, not the workers: a lane's main instance is no candidate.
            candidate: !is_main,
            lane: self.lane.clone(),
            published: infra.published.clone(),
            pending: infra.pending.clone(),
            ..Default::default()
        };
        // Other services are reached on their fixed port: through the proxy, at
        // whichever instance is current.
        for (n, s) in &self.stack.services {
            if let Some(p) = s.port {
                let tls = s.health.as_ref().is_some_and(|h| h.insecure_tls);
                ctx.service_urls.insert(
                    n.clone(),
                    format!("{}://127.0.0.1:{p}", if tls { "https" } else { "http" }),
                );
            }
        }
        ctx.vars
            .extend(devstack_env::variables(&self.stack, Some(svc), &self.repo));
        ctx
    }

    /// Until the instance is ready: a job until it finished, a service with a
    /// health check until it answers (on the instance's own port, never through
    /// the proxy: ADR-0010 condition 1), else a moment to fail fast. Returns the
    /// status to show. A job runs for its `timeout` at most; `cancel` (a
    /// stop) ends the wait at once. The caller kills the instance on an error.
    pub(super) fn wait_instance(
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
                            "job exited with code {code}\n{}",
                            tail(&inst.log, 15)
                        ));
                    }
                    if cancel() {
                        return Err(STOPPED.into());
                    }
                    if t0.elapsed() >= limit {
                        return Err(format!(
                            "job still ran after {} s (its `timeout`), and was stopped\n{}",
                            limit.as_secs(),
                            tail(&inst.log, 15)
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
            _ => match (&svc.health, inst.port) {
                (Some(h), Some(port)) => {
                    let t0 = Instant::now();
                    let mut last = String::new();
                    while t0.elapsed() < h.timeout {
                        if cancel() {
                            return Err(STOPPED.into());
                        }
                        if let Some(code) = inst.tree.try_exit_code() {
                            return Err(format!(
                                "exited with code {code} before it became healthy\n{}",
                                tail(&inst.log, 15)
                            ));
                        }
                        match self
                            .health
                            .check(port, &h.http, h.insecure_tls, h.insecure_tls)
                        {
                            Ok(()) => return Ok(Status::Healthy),
                            Err(e) => last = e,
                        }
                        std::thread::sleep(Duration::from_millis(300));
                    }
                    Err(format!(
                        "not healthy after {} s: {last}",
                        h.timeout.as_secs()
                    ))
                }
                _ => {
                    // Give it a moment to fail fast, e.g. on a wrong command line.
                    std::thread::sleep(SETTLE);
                    if let Some(code) = inst.tree.try_exit_code() {
                        return Err(format!(
                            "exited with code {code} right after start\n{}",
                            tail(&inst.log, 15)
                        ));
                    }
                    Ok(Status::Running)
                }
            },
        }
    }

    /// Watch the main instance: exit and health changes become events.
    pub(super) fn spawn_monitor(self: &Arc<Self>, name: String, svc: Service, generation: u64) {
        let me = Arc::clone(self);
        let _ = std::thread::Builder::new()
            .name(format!("monitor-{name}"))
            .spawn(move || {
                let mut last_health = Instant::now();
                loop {
                    std::thread::sleep(Duration::from_millis(500));
                    let (exit, port) = {
                        let mut slots = me.slots.lock().unwrap();
                        let Some(slot) = slots.get_mut(&name) else {
                            return;
                        };
                        if slot.generation != generation {
                            return;
                        }
                        let Some(inst) = slot.main.as_mut() else {
                            return;
                        };
                        (inst.tree.try_exit_code(), inst.port)
                    };
                    if let Some(code) = exit {
                        // Let the rest of the tree go too.
                        let inst = {
                            let mut slots = me.slots.lock().unwrap();
                            let slot = slots.get_mut(&name).unwrap();
                            // A switch or a stop came in since the exit was
                            // seen: the slot holds another instance now.
                            if slot.generation != generation {
                                return;
                            }
                            slot.view.pid = None;
                            slot.main.take()
                        };
                        let mut ran = Duration::ZERO;
                        let mut build = None;
                        if let Some(mut i) = inst {
                            // What still ran of its tree says whether the
                            // root (a shell, a wrapper) went, or everything.
                            ran = i.started.elapsed();
                            let left = i.tree.pids().len();
                            build = i.build_id.clone();
                            i.tree.kill();
                            me.forget_record(&i.record);
                            crate::diag::warn(
                                "service_exited",
                                serde_json::json!({
                                    "lane": me.lane,
                                    "service": name,
                                    "exit_code": code,
                                    "ran_s": ran.as_secs(),
                                    "tree_left": left,
                                    "why": "it exited by itself: no stop, restart or switch asked for it",
                                }),
                            );
                        }
                        if let Some(p) = me.proxy(&name) {
                            p.set_upstream(None);
                            p.set_waiting(Waiting::Refuse(format!(
                                "`{name}` exited with code {code} (devstack logs {name})"
                            )));
                        }
                        me.set_status(&name, Status::Exited { exit_code: code });
                        // A tool may be closed on purpose; everything else
                        // that runs for good comes back.
                        if matches!(svc.kind, Kind::Tool | Kind::Job) {
                            return;
                        }
                        me.restart_exited(&name, generation, ran, code, build);
                        return;
                    }
                    if let (Some(h), Some(port)) = (&svc.health, port)
                        && last_health.elapsed() >= Duration::from_secs(5)
                    {
                        last_health = Instant::now();
                        match me
                            .health
                            .check(port, &h.http, h.insecure_tls, h.insecure_tls)
                        {
                            Ok(()) => me.set_status(&name, Status::Healthy),
                            Err(reason) => me.set_status(&name, Status::Unhealthy { reason }),
                        }
                    }
                }
            });
    }

    /// Graceful stop, then kill (ADR-0009). Behind the proxy, requests are
    /// refused with the reason from now on and running ones may finish briefly.
    /// A start, build or job wait in progress gives up first and lets go of
    /// the op lock (a hanging migrator, a long build).
    pub fn stop(&self, name: &str) -> Result<(), String> {
        self.service(name)?;
        self.stops[name].fetch_add(1, Ordering::SeqCst);
        let _op = self.op_locks[name].lock().unwrap();
        let inst = {
            let mut slots = self.slots.lock().unwrap();
            let slot = slots.get_mut(name).unwrap();
            slot.generation += 1;
            slot.view.pid = None;
            slot.view.instance_port = None;
            slot.main.take()
        };
        // While a lane serves the main ports, the main port is the lane's:
        // a stop in the main tree leaves its forward alone (nothing of the
        // main tree runs behind it then, `main_ports_elsewhere`).
        if let Some(p) = self.proxy(name).filter(|_| self.served_by().is_none()) {
            p.set_upstream(None);
            p.set_waiting(Waiting::Refuse(format!(
                "`{name}` is stopped (devstack start {name})"
            )));
            if let Some(port) = inst.as_ref().and_then(|i| i.port) {
                p.drain(port, Duration::from_secs(2));
            }
        }
        if let Some(mut i) = inst {
            i.tree.stop(Duration::from_secs(20));
            self.forget_record(&i.record);
        }
        self.set_status(name, Status::Stopped);
        Ok(())
    }

    /// The environment `name` gets from this supervisor, as it would start
    /// now: in a lane with the lane's ports, its own containers and
    /// `{lane}` (`devstack env` in a worktree). `include_devstack`: also
    /// devstack's own variables (build id, lane, slot).
    pub fn env_of(&self, name: &str, include_devstack: bool) -> Result<devstack_env::Env, String> {
        let svc = self.effective(name, &self.service(name)?.clone());
        let (build, port) = {
            let slots = self.slots.lock().unwrap();
            let v = &slots[name].view;
            (v.build_id.clone(), v.instance_port.or(svc.port))
        };
        let artifact = build
            .map(|id| self.layout.artifact(name, &id))
            .filter(|d| d.is_dir());
        let ctx = self.context(&svc, artifact.as_deref(), port, true);
        let infra = self.infra.lock().unwrap();
        devstack_env::compose(name, &svc, &ctx, &infra.envs, include_devstack)
            .map_err(|e| e.to_string())
    }

    /// `start`, `stop` or `restart` for every member of a group (plan §6).
    /// Services start one after the other in member order (their `after`
    /// pulls dependencies first) and stop in reverse; restarts build at the
    /// same time. `restart` also restarts container members; commands are
    /// left out. Errors name the members that failed, the others still ran.
    pub fn group_action(self: &Arc<Self>, group: &str, action: &str) -> Result<(), String> {
        let g = self
            .stack
            .groups
            .get(group)
            .ok_or_else(|| format!("no group `{group}`"))?;
        let services: Vec<String> = g
            .members
            .iter()
            .filter(|m| self.stack.services.contains_key(m.as_str()))
            .cloned()
            .collect();
        let containers: Vec<(String, String)> = g
            .members
            .iter()
            .filter_map(|m| {
                let rest = m.strip_prefix("compose.")?;
                let (grp, svc) = rest.split_once('.')?;
                Some((grp.to_string(), svc.to_string()))
            })
            .collect();
        let mut failed: Vec<String> = Vec::new();
        match action {
            "start" => {
                for s in &services {
                    if let Err(e) = self.start(s) {
                        failed.push(format!("{s}: {e}"));
                    }
                }
            }
            "stop" => {
                for s in services.iter().rev() {
                    if let Err(e) = self.stop(s) {
                        failed.push(format!("{s}: {e}"));
                    }
                }
            }
            "restart" => {
                let results: Vec<(String, Result<(), String>)> = std::thread::scope(|scope| {
                    let handles: Vec<_> = services
                        .iter()
                        .map(|s| (s.clone(), scope.spawn(|| self.restart(s))))
                        .collect();
                    handles
                        .into_iter()
                        .map(|(s, h)| (s, h.join().unwrap_or_else(|_| Err("panicked".into()))))
                        .collect()
                });
                failed.extend(
                    results
                        .into_iter()
                        .filter_map(|(s, r)| r.err().map(|e| format!("{s}: {e}"))),
                );
                for (grp, svc) in &containers {
                    if let Err(e) = self.docker.restart(&self.compose_group(grp), svc) {
                        failed.push(format!("{svc}: {e}"));
                    }
                }
                if !containers.is_empty() {
                    self.refresh_containers();
                }
            }
            _ => return Err(format!("unknown group action `{action}`")),
        }
        if failed.is_empty() {
            Ok(())
        } else {
            Err(failed.join("\n"))
        }
    }

    /// Start what `svc` comes after and is not ready yet (services, jobs that
    /// have not completed). Compose services are the infra plane's job (`up`).
    fn ensure_after(self: &Arc<Self>, svc: &Service) -> Result<(), String> {
        for dep in &svc.after {
            if let Some(rest) = dep.strip_prefix("compose.")
                && let Some((group, service)) = rest.split_once('.')
            {
                self.ensure_reachable(group, service)?;
                continue;
            }
            let Some(d) = self.stack.services.get(dep) else {
                continue;
            };
            if !self.runs_here(d) {
                continue;
            }
            let status = self.slots.lock().unwrap()[dep].view.status.clone();
            let ready = match d.kind {
                Kind::Job => status == Status::Completed { exit_code: 0 },
                _ => status.is_ready(),
            };
            if !ready {
                self.start(dep)
                    .map_err(|e| format!("`{dep}` (needed first) failed: {e}"))?;
            }
        }
        Ok(())
    }
}
