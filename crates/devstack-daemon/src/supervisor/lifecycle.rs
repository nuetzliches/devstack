//! The slot lifecycle: start, stop, monitor.

use super::{
    Instance, JOB_TIMEOUT, KEEP_BUILD_LOGS, KEEP_RUN_LOGS, STOPPED, StopCmd, Supervisor, free_port,
    on_this_platform, port_in_use, tail,
};
use crate::proxy::{Proxy, Upstream, Waiting};
use crate::state::{EventBody, Status, StopRecord, now_ms};
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

/// How long a service's `stop` command may run (ADR-0059).
pub(super) const STOP_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

/// Run an instance's `stop` command (ADR-0059) to its end, at most
/// [`STOP_COMMAND_TIMEOUT`], its output appended to the instance's `log`.
/// A failure is said there and as a diagnostic; it never holds up the stop.
#[allow(clippy::too_many_arguments)]
pub(super) fn run_stop(
    lane: &str,
    service: &str,
    record: &str,
    command: &str,
    cwd: &Path,
    env: &BTreeMap<String, String>,
    log: &Path,
    opts: &devstack_proc::Options,
) {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .ok();
    let outcome = (|| -> Result<(), String> {
        let out = file
            .as_mut()
            .ok_or_else(|| format!("cannot open {}", log.display()))?;
        let _ = writeln!(out, "\n[devstack] stop: {command}");
        let spec = Spec {
            name: format!("{record}-stop"),
            command: command.to_string(),
            cwd: cwd.to_path_buf(),
            env: env.clone(),
            log: out.try_clone().map_err(|e| e.to_string())?,
        };
        let (mut tree, _) = devstack_proc::spawn(spec, opts).map_err(|e| e.to_string())?;
        let t0 = Instant::now();
        loop {
            if let Some(code) = tree.try_exit_code() {
                // What it left of its own tree goes with it.
                tree.kill();
                return match code {
                    0 => Ok(()),
                    c => Err(format!(
                        "exited with code {}",
                        crate::state::exit_code_text(c)
                    )),
                };
            }
            if t0.elapsed() >= STOP_COMMAND_TIMEOUT {
                tree.kill();
                return Err(format!(
                    "still ran after {} s, and was ended",
                    STOP_COMMAND_TIMEOUT.as_secs()
                ));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    })();
    if let Err(e) = outcome {
        if let Some(out) = file.as_mut() {
            let _ = writeln!(out, "[devstack] stop failed: {e}");
        }
        crate::diag::warn(
            "service_stop_failed",
            serde_json::json!({
                "lane": lane,
                "service": service,
                "error": e,
            }),
        );
    }
}

impl Supervisor {
    /// Infra, then every autostart service for this platform, in order; a
    /// service that fails holds back only what comes `after` it.
    pub fn up(self: &Arc<Self>) -> Result<(), String> {
        let r = self.up_all();
        self.upped.store(true, Ordering::SeqCst);
        r
    }

    /// Wait until an `up` of this tree ran to its end, `within` at most:
    /// `false` when none did.
    pub fn wait_upped(&self, within: Duration) -> bool {
        let until = std::time::Instant::now() + within;
        while !self.upped.load(Ordering::SeqCst) {
            if std::time::Instant::now() >= until {
                return false;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        true
    }

    fn up_all(self: &Arc<Self>) -> Result<(), String> {
        {
            // A profile with `data = "fresh"` (ADR-0041): the tree's own
            // volumes go first, once per start, so its seed jobs fill new
            // ones. On a node its agent does it (ADR-0025).
            self.fresh_data();
            // Every lane has its own infrastructure (ADR-0030). A build needs
            // no container: the services are built while it starts.
            let infra = {
                let me = self.clone();
                std::thread::Builder::new()
                    .name("infra-up".into())
                    .spawn(move || me.infra_up())
                    .map_err(|e| e.to_string())?
            };
            self.prebuild();
            infra
                .join()
                .map_err(|_| "starting the infrastructure failed inside devstack".to_string())??;
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
        // rest of the stack down). Services whose `after` is done start at
        // once, side by side.
        let mut failed: BTreeSet<String> = BTreeSet::new();
        for level in super::levels(&self.service_deps()) {
            let mut go = Vec::new();
            for name in level {
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
                go.push(name);
            }
            let results: Vec<(String, bool)> = std::thread::scope(|s| {
                let running: Vec<_> = go
                    .into_iter()
                    .map(|name| {
                        let me = self.clone();
                        s.spawn(move || {
                            let ok = me.start(&name).is_ok();
                            (name, ok)
                        })
                    })
                    .collect();
                running.into_iter().filter_map(|h| h.join().ok()).collect()
            });
            failed.extend(results.into_iter().filter(|(_, ok)| !ok).map(|(n, _)| n));
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

    /// Build what the start will run, while the infrastructure starts: each
    /// service with `build` that starts with the tree and has no artifact
    /// to restore (ADR-0016). Its start then finds the artifact. One that
    /// fails here (a build that needs a container after all) is built again
    /// by its start, which says why.
    fn prebuild(self: &Arc<Self>) {
        let first: Vec<String> = self.started.lock().unwrap().iter().cloned().collect();
        for name in self.service_order() {
            let Ok(declared) = self.service(&name) else {
                continue;
            };
            if !declared.autostart || !self.runs_here(declared) || first.contains(&name) {
                continue;
            }
            let svc = self.effective(&name, declared);
            if svc.build.is_none()
                || !super::on_this_platform(&svc)
                || self.restorable(&name).is_some()
            {
                continue;
            }
            let epoch = self.stop_epoch(&name);
            let _op = self.op_locks[&name].lock().unwrap();
            if self.slots.lock().unwrap()[&name].view.status.is_active() {
                continue;
            }
            let cancel = || self.stopped_since(&name, epoch);
            let t0 = std::time::Instant::now();
            let r = self.build_locked(&name, &svc, &cancel, epoch);
            crate::diag::info(
                "prebuilt",
                serde_json::json!({ "lane": self.lane, "service": name,
                    "ms": t0.elapsed().as_millis() as u64, "error": r.as_ref().err() }),
            );
            // Built, not started: it waits for the infrastructure.
            self.set_status(&name, Status::Stopped);
        }
    }

    fn fresh_data(&self) {
        let fresh = self
            .stack
            .profile
            .as_ref()
            .is_some_and(|p| p.data == devstack_config::model::DataMode::Fresh);
        if !fresh || self.external_infra().is_some() || self.fresh_done.swap(true, Ordering::SeqCst)
        {
            return;
        }
        for g in self.group_order() {
            let group = self.compose_group(&g);
            let message = match self.docker.remove(&group) {
                Ok(()) => "its data goes: the profile starts it fresh".to_string(),
                Err(e) => format!("its data could not go: {e}"),
            };
            self.emit(EventBody::Infra { group: g, message });
        }
    }

    /// Stop every service, last started first; with `infra` the containers
    /// go too, volumes kept (`devstack down` does, ADR-0068). Without it,
    /// as for a daemon that stops or restarts, they run on. Returns what did
    /// not stop.
    pub fn down(&self, infra: bool) -> Vec<String> {
        self.down_keeping(infra, &BTreeSet::new())
    }

    /// Main tree, while lanes run: stop every service but those that serve
    /// the lanes, a service with `lanes = false` and what it starts `after`
    /// (ADR-0024 point 5: the main tree's instance serves every lane). With
    /// `infra` the containers go too (ADR-0068), but those a kept service
    /// comes `after` and, while `local_lanes`, the `shared` ones (a lane
    /// here uses them, ADR-0030; a lane on a node runs its own). The
    /// services kept, the containers kept (`<group>.<service>`), what did
    /// not stop.
    pub fn down_for_lanes(
        &self,
        infra: bool,
        local_lanes: bool,
    ) -> (Vec<String>, Vec<String>, Vec<String>) {
        let mut keep: BTreeSet<String> = self
            .stack
            .services
            .iter()
            .filter(|(_, s)| !s.lanes)
            .map(|(n, _)| n.clone())
            .collect();
        let mut containers: BTreeSet<(String, String)> = BTreeSet::new();
        let mut todo: Vec<String> = keep.iter().cloned().collect();
        while let Some(n) = todo.pop() {
            for dep in self
                .stack
                .services
                .get(&n)
                .map(|s| s.after.clone())
                .unwrap_or_default()
            {
                if let Some(rest) = dep.strip_prefix("compose.")
                    && let Some((g, s)) = rest.split_once('.')
                {
                    containers.insert((g.to_string(), s.to_string()));
                } else if self.stack.services.contains_key(&dep) && keep.insert(dep.clone()) {
                    todo.push(dep);
                }
            }
        }
        let mut failed = self.down_keeping(false, &keep);
        let mut kept = Vec::new();
        if infra && self.external_infra().is_none() {
            for g in self.group_order().into_iter().rev() {
                let Some(cfg) = self.stack.compose.get(&g) else {
                    continue;
                };
                let stays: BTreeSet<&String> = containers
                    .iter()
                    .filter(|(cg, _)| *cg == g)
                    .map(|(_, s)| s)
                    .chain(cfg.shared.iter().filter(|_| local_lanes))
                    .collect();
                let mut group = self.compose_group(&g);
                let res = if stays.is_empty() {
                    self.docker.down(&group)
                } else if group.services.is_empty() {
                    // Every service of its files: none can be told apart.
                    kept.push(g.clone());
                    Ok(())
                } else {
                    kept.extend(stays.iter().map(|s| format!("{g}.{s}")));
                    group.services.retain(|s| !stays.contains(s));
                    self.docker.remove_selected(&group)
                };
                if let Err(e) = res {
                    failed.push(format!("compose group `{g}`: {e}"));
                }
            }
            self.refresh_infra();
        }
        (keep.into_iter().collect(), kept, failed)
    }

    fn down_keeping(&self, infra: bool, keep: &BTreeSet<String>) -> Vec<String> {
        let mut failed = Vec::new();
        for name in self.service_order().into_iter().rev() {
            if keep.contains(&name) {
                continue;
            }
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
                // `stop` ends what `run` starts (ADR-0059).
                s.stop = None;
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
    /// The build `name` runs, or ran last.
    pub fn build_of(&self, name: &str) -> Option<String> {
        self.slots.lock().unwrap().get(name)?.view.build_id.clone()
    }

    /// Start `name` from `build` while its artifact is there, else as
    /// [`Self::start`] does: what a tree ran before it gave the main ports to
    /// a lane runs again, not a candidate built or held meanwhile.
    pub fn start_build(self: &Arc<Self>, name: &str, build: Option<String>) -> Result<(), String> {
        let artifact = build
            .map(|id| (self.layout.artifact(name, &id), id))
            .filter(|(dir, _)| dir.is_dir())
            .map(|(dir, id)| (id, dir));
        self.start_with(name, artifact)
    }

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
    /// times in a row. A change seen while it was down follows as a
    /// candidate once it runs again (ADR-0003; `resume_changes`).
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
            if result.is_ok() && svc.kind == Kind::Job {
                slot.ran_since = Some(requested);
            }
        }
        if let Err(e) = &result {
            self.set_status(name, Status::Failed { reason: e.clone() });
            return result;
        }
        // Only a start that succeeded uses up the restore (ADR-0016): one
        // that failed for a passing reason (a port still held, infra not
        // reachable yet) restores again, it does not build the tree.
        self.started.lock().unwrap().insert(name.to_string());
        drop(_op);
        self.resume_changes(name);
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
            return Err(super::port_taken(port));
        }
        let p =
            Proxy::bind(name, port, &self.rt).map_err(|e| format!("proxy on port {port}: {e}"))?;
        proxies.insert(name.into(), p.clone());
        Ok(p)
    }

    pub(super) fn proxy(&self, name: &str) -> Option<Proxy> {
        self.proxies.lock().unwrap().get(name).cloned()
    }

    /// The front on a dev server's fixed port (ADR-0043), bound on its first
    /// start; a port another process holds names it.
    pub(super) fn ensure_front(&self, name: &str, svc: &Service) -> Result<(), String> {
        let mut fronts = self.fronts.lock().unwrap();
        if fronts.contains_key(name) {
            return Ok(());
        }
        let port = svc.port.expect("a fronted dev server has a port");
        // Held a moment longer by something on its way out (the instance of
        // a daemon that just restarted, ADR-0052): tried again for a while;
        // a port that stays taken names its holder as before.
        let t0 = std::time::Instant::now();
        let bound = loop {
            let r = if port_in_use(port) {
                Err(std::io::Error::from(std::io::ErrorKind::AddrInUse))
            } else {
                crate::tcpfwd::Front::bind(port, &self.rt)
            };
            match r {
                Err(e)
                    if e.kind() == std::io::ErrorKind::AddrInUse
                        && t0.elapsed() < Duration::from_secs(10) =>
                {
                    std::thread::sleep(Duration::from_millis(250));
                }
                r => break r,
            }
        };
        if t0.elapsed() >= Duration::from_millis(250) {
            crate::diag::info(
                "front_port_waited",
                serde_json::json!({ "lane": self.lane, "service": name, "port": port,
                    "ms": t0.elapsed().as_millis() as u64, "bound": bound.is_ok() }),
            );
        }
        let f = match bound {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                return Err(super::port_taken(port));
            }
            Err(e) => return Err(format!("forward on port {port}: {e}")),
        };
        fronts.insert(name.into(), f);
        Ok(())
    }

    /// Set the front of `name`, if it has one, unless a lane serves the
    /// main ports (the front is the lane's then, `tcp_forward`).
    pub(super) fn set_front(&self, name: &str, state: crate::tcpfwd::FrontState) {
        if self.fronts_to_lane.lock().unwrap().contains(name) {
            return;
        }
        if let Some(f) = self.fronts.lock().unwrap().get(name) {
            f.set(state);
        }
    }

    fn start_locked(
        self: &Arc<Self>,
        name: &str,
        svc: &Service,
        prebuilt: Option<(String, PathBuf)>,
        epoch: u64,
    ) -> Result<(), String> {
        let cancel = || self.stopped_since(name, epoch);
        let first = !self.started.lock().unwrap().contains(name);
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
                    Some(self.build_locked(name, svc, &cancel, epoch)?)
                }
            },
        };
        if let Some((id, _)) = &artifact {
            self.slots.lock().unwrap().get_mut(name).unwrap().in_flight = Some(id.clone());
        }

        // 2. The port: behind the proxy an internal one, else the fixed one, free.
        let hold = svc
            .health
            .as_ref()
            .map_or(Duration::from_secs(60), |h| h.timeout);
        let fronted = svc.fronted();
        let proxy = if svc.proxied() {
            let p = self.ensure_proxy(name, svc)?;
            p.set_waiting(Waiting::Hold(hold));
            Some(p)
        } else if fronted {
            // A dev server's fixed port is devstack's (ADR-0043): new
            // connections wait while it starts.
            if let Some(e) = self.main_ports_elsewhere(name, svc) {
                return Err(e);
            }
            self.ensure_front(name, svc)?;
            self.set_front(name, crate::tcpfwd::FrontState::Hold(hold));
            None
        } else {
            if let Some(port) = svc.port
                && port_in_use(port)
            {
                return Err(super::port_taken(port));
            }
            None
        };
        let port = match &proxy {
            Some(_) => Some(free_port()?),
            None if fronted => Some(free_port()?),
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
            if fronted {
                self.set_front(name, crate::tcpfwd::FrontState::Refuse);
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
                self.end(&mut inst, None);
                refuse(&e);
                return Err(e);
            }
        };

        // 4. In service: behind the proxy, switch it on; record it; watch it.
        //    Unless a lane took the main ports meanwhile (`promote --lane`
        //    while this one built): this instance runs with workers.
        if (proxy.is_some() || fronted)
            && let Some(e) = self.main_ports_elsewhere(name, svc)
        {
            self.end(&mut inst, None);
            return Err(e);
        }
        if fronted && let Some(p) = inst.port {
            self.set_front(name, crate::tcpfwd::FrontState::To(p));
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
            // Done: its `stop` all the same (ADR-0059), for a container its
            // process left.
            self.end(&mut inst, None);
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
        // Expanded now, so it addresses this instance (its port, its lane)
        // whenever it ends (ADR-0059).
        let stop = svc
            .stop
            .as_ref()
            .map(|s| {
                devstack_config::interpolate::expand(s, &ctx, &lookup)
                    .map_err(|e| format!("stop: {e}"))
            })
            .transpose()?;
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
        let stop = stop.map(|command| StopCmd {
            command,
            cwd: cwd.clone(),
            env: env_map.clone(),
        });
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
            p.stops.retain(|s| s.record != record);
            if let Some(s) = &stop {
                p.stops.push(StopRecord {
                    record: record.clone(),
                    command: s.command.clone(),
                    cwd: s.cwd.clone(),
                    log: log.clone(),
                });
            }
            p.save(&self.state_dir);
        }
        Ok(Instance {
            tree,
            port,
            build_id: artifact.map(|(id, _)| id.clone()),
            log,
            record,
            started: Instant::now(),
            stop,
        })
    }

    pub(super) fn forget_record(&self, record: &str) {
        let mut p = self.persisted.lock().unwrap();
        p.records.retain(|r| r.name != record);
        p.stops.retain(|s| s.record != record);
        p.save(&self.state_dir);
    }

    /// End an instance, the one way every end goes: its `stop` command first
    /// (ADR-0059), then the graceful signal and a kill after `graceful`
    /// (ADR-0009), or a kill at once (`None`); then its records go.
    pub(super) fn end(&self, inst: &mut Instance, graceful: Option<Duration>) {
        if let Some(s) = &inst.stop {
            let service = inst.record.split('@').next().unwrap_or(&inst.record);
            run_stop(
                &self.lane,
                service,
                &inst.record,
                &s.command,
                &s.cwd,
                &s.env,
                &inst.log,
                &self.proc_opts,
            );
        }
        match graceful {
            Some(t) => {
                inst.tree.stop(t);
            }
            None => inst.tree.kill(),
        }
        self.forget_record(&inst.record);
    }

    fn context(
        &self,
        svc: &Service,
        artifact: Option<&Path>,
        port: Option<u16>,
        is_main: bool,
    ) -> Context {
        let projects = self.compose_projects();
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
            // `{lane}`, `{lane_suffix}`, `DEVSTACK_LANE`: on a node the lane's
            // name, as its Compose calls are told (ADR-0064 addendum).
            lane: self.presented_lane(),
            published: infra.published.clone(),
            projects,
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
                            tail(&inst.log, 15),
                            code = crate::state::exit_code_text(code)
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
                                "exited with code {code} before it became healthy{}\n{}",
                                self.copy_note(svc),
                                tail(&inst.log, 15),
                                code = crate::state::exit_code_text(code)
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
                            "exited with code {code} right after start{}\n{}",
                            self.copy_note(svc),
                            tail(&inst.log, 15),
                            code = crate::state::exit_code_text(code)
                        ));
                    }
                    Ok(Status::Running)
                }
            },
        }
    }

    /// Why a dev server that ended before it was ready may have: in a lane
    /// it runs from a copy without the ignored files (ADR-0024), and one
    /// without `prepare` fails there with only its toolchain's message
    /// ("vite is not recognized"). Empty elsewhere.
    pub(super) fn copy_note(&self, svc: &Service) -> &'static str {
        if svc.kind == Kind::Devserver && svc.prepare.is_none() && self.runs_from_copy() {
            " (the lane's copy has no ignored files such as node_modules: see `prepare`)"
        } else {
            ""
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
                            me.end(&mut i, None);
                            crate::diag::warn(
                                "service_exited",
                                serde_json::json!({
                                    "lane": me.lane,
                                    "service": name,
                                    "exit_code": code,
                                    // Windows: an exception code reads in hex.
                                    "exit": crate::state::exit_code_text(code),
                                    "ran_s": ran.as_secs(),
                                    "tree_left": left,
                                    "why": "it exited by itself: no stop, restart or switch asked for it",
                                }),
                            );
                        }
                        if let Some(p) = me.proxy(&name) {
                            p.set_upstream(None);
                            p.set_waiting(Waiting::Refuse(format!(
                                "`{name}` exited with code {} (devstack logs {name})",
                                crate::state::exit_code_text(code)
                            )));
                        }
                        me.set_front(&name, crate::tcpfwd::FrontState::Refuse);
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
                        // Set only while this instance is still the one the
                        // slot runs: a check that ended after a stop says
                        // nothing about the service now.
                        let s = match me
                            .health
                            .check(port, &h.http, h.insecure_tls, h.insecure_tls)
                        {
                            Ok(()) => Status::Healthy,
                            Err(reason) => Status::Unhealthy { reason },
                        };
                        let s = me.with_workers_note(&name, generation, s);
                        me.set_status_of(&name, Some(generation), s);
                    }
                }
            });
    }

    /// A passing health check of an instance that serves without its
    /// background workers (the relay's second start failed) still says so:
    /// the dashboard and `status` must not show it as healthy (ADR-0008).
    fn with_workers_note(&self, name: &str, generation: u64, status: Status) -> Status {
        if status != Status::Healthy {
            return status;
        }
        let slots = self.slots.lock().unwrap();
        match slots.get(name).and_then(|s| s.without_workers.as_ref()) {
            Some((g, reason)) if *g == generation => Status::Unhealthy {
                reason: reason.clone(),
            },
            _ => status,
        }
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
        if self.served_by().is_none() && !self.restarting.lock().unwrap().contains(name) {
            self.set_front(name, crate::tcpfwd::FrontState::Refuse);
        }
        if let Some(mut i) = inst {
            self.end(&mut i, Some(Duration::from_secs(20)));
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
    /// In a lane on a node its agent restarts the container members
    /// (`node_acted`, #128): this daemon has no Docker.
    pub fn group_action(
        self: &Arc<Self>,
        group: &str,
        action: &str,
        node_acted: bool,
    ) -> Result<(), String> {
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
                if self.external_infra().is_some() {
                    // Never `docker compose` here: the lane container has
                    // no Docker, and the project would be the main tree's.
                    if !containers.is_empty() && !node_acted {
                        let names: Vec<&str> = containers.iter().map(|(_, s)| s.as_str()).collect();
                        failed.push(format!(
                            "{}: {}",
                            names.join(", "),
                            super::infra::NODE_DID_NOT_ACT
                        ));
                    }
                } else {
                    for (grp, svc) in &containers {
                        if let Err(e) = self.docker.restart(&self.compose_group(grp), svc) {
                            failed.push(format!("{svc}: {e}"));
                        }
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
