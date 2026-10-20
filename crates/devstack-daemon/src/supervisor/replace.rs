//! Restart, replacement behind the proxy, rollback.

use super::{Instance, Supervisor, first_lines, free_port};
use crate::proxy::{Proxy, Upstream};
use crate::state::{Candidate, Status, now_ms};
use devstack_config::model::{Kind, Service};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

impl Supervisor {
    /// `restart --rebuild`: build again even if an artifact for the same
    /// inputs exists (an input devstack does not see changed), then restart.
    pub fn rebuild(self: &Arc<Self>, name: &str) -> Result<(), String> {
        if self.service(name)?.build.is_none() {
            return Err(format!("`{name}` has no `build`"));
        }
        self.force_build.lock().unwrap().insert(name.to_string());
        let result = self.restart(name);
        self.force_build.lock().unwrap().remove(name);
        result
    }

    /// Build first; only a successful build replaces the running instance
    /// (ADR-0003). Behind the proxy the replacement is a candidate that takes
    /// over without dropping a request; otherwise a restart in place, from
    /// the artifact just built.
    pub fn restart(self: &Arc<Self>, name: &str) -> Result<(), String> {
        // A fronted dev server keeps its address across the stop: new
        // connections wait for the new instance (ADR-0043).
        let svc = self.service(name)?.clone();
        let fronted = svc.fronted();
        if fronted {
            let hold = svc
                .health
                .as_ref()
                .map_or(std::time::Duration::from_secs(60), |h| h.timeout);
            self.restarting.lock().unwrap().insert(name.to_string());
            self.set_front(name, crate::tcpfwd::FrontState::Hold(hold));
        }
        let result = self.restart_inner(name);
        if fronted {
            self.restarting.lock().unwrap().remove(name);
            if result.is_err() && !self.service_running(name) {
                self.set_front(name, crate::tcpfwd::FrontState::Refuse);
            }
        }
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
            match self.build_locked(name, &svc, &|| self.stopped_since(name, epoch)) {
                Ok(a) => artifact = Some(a),
                Err(e) => {
                    // Back to what is actually running; the error goes to the caller.
                    self.set_status(name, before);
                    if svc.proxied() && self.slots.lock().unwrap()[name].main.is_some() {
                        self.set_candidate(
                            name,
                            Some(Candidate::new("failed", None, Some(first_lines(&e, 5)))),
                        );
                    }
                    return Err(format!(
                        "{e}
The running instance was left as it was."
                    ));
                }
            }
        }
        self.rerun_after_jobs(&svc)?;
        let running = self.slots.lock().unwrap()[name].main.is_some();
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
    }

    /// Jobs `svc` comes after run again first (a schema migrator for a
    /// migration that came with the change); their build is cached when
    /// nothing changed. A failing job leaves the running instance alone.
    pub(super) fn rerun_after_jobs(self: &Arc<Self>, svc: &Service) -> Result<(), String> {
        for dep in &svc.after {
            if self
                .stack
                .services
                .get(dep)
                .is_some_and(|d| d.kind == Kind::Job)
            {
                self.restart(dep).map_err(|e| {
                    format!("`{dep}` failed:\n{e}\nThe running instance was left as it was.")
                })?;
            }
        }
        Ok(())
    }

    /// Replace the running main instance behind the proxy (ADR-0003, ADR-0010):
    ///
    /// 1. the candidate starts on a free port without workers (ADR-0008) and
    ///    must pass its health check; if not, it goes and main stays;
    /// 2. the proxy switches to it; the old instance drains and stops;
    /// 3. a service with worker switches gets the same artifact once more with
    ///    workers on, which takes over the same way ("relay"): requests never
    ///    wait, background work pauses for one start-up.
    pub(super) fn replace(
        self: &Arc<Self>,
        name: &str,
        svc: &Service,
        artifact: Option<(String, PathBuf)>,
    ) -> Result<(), String> {
        let epoch = self.stop_epoch(name);
        let cancel = || self.stopped_since(name, epoch);
        let _op = self.op_locks[name].lock().unwrap();
        let proxy = self.ensure_proxy(name, svc)?;
        let build = artifact.as_ref().map(|(id, _)| id.clone());
        let relay = svc.workers.is_some();
        let fail = |e: String| -> Result<(), String> {
            self.set_candidate(
                name,
                Some(Candidate::new(
                    "failed",
                    build.clone(),
                    Some(first_lines(&e, 5)),
                )),
            );
            proxy.set_pending(None);
            Err(format!("{e}\nThe running instance was left as it was."))
        };

        self.set_candidate(name, Some(Candidate::new("starting", build.clone(), None)));
        proxy.set_pending(Some(format!(
            "starting {}",
            build.as_deref().unwrap_or("a new instance")
        )));
        let mut candidate = match self.launch(
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

        self.set_candidate(name, Some(Candidate::new("switching", build.clone(), None)));
        let old = self.switch_to(name, &proxy, candidate);
        self.retire(&proxy, old);

        if relay {
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
            if let Err(e) = self.wait_instance(svc, &mut fin, &cancel) {
                fin.tree.kill();
                self.forget_record(&fin.record);
                return self.running_without_workers(name, svc, e);
            }
            let without_workers = self.switch_to(name, &proxy, fin);
            self.retire(&proxy, without_workers);
        }

        if let Some(id) = &build {
            let mut pers = self.persisted.lock().unwrap();
            pers.main.insert(name.into(), id.clone());
            pers.save(&self.state_dir);
        }
        proxy.set_pending(None);
        self.set_candidate(name, None);
        let generation = self.slots.lock().unwrap()[name].generation;
        self.set_status(name, Status::Healthy);
        self.spawn_monitor(name.to_string(), svc.clone(), generation);
        Ok(())
    }

    /// Point the proxy at `inst` and make it main; returns the previous main.
    fn switch_to(&self, name: &str, proxy: &Proxy, inst: Instance) -> Option<Instance> {
        proxy.set_upstream(Some(Upstream {
            port: inst.port.expect("instances behind the proxy have a port"),
            build: inst.build_id.clone(),
        }));
        let mut slots = self.slots.lock().unwrap();
        let slot = slots.get_mut(name).unwrap();
        slot.generation += 1;
        slot.view.pid = Some(inst.tree.root_pid());
        // A new instance is a new "since", even when the status stays healthy.
        slot.view.since = now_ms();
        slot.view.log = Some(inst.log.clone());
        slot.view.build_id = inst.build_id.clone();
        slot.view.instance_port = inst.port;
        let old = slot.main.replace(inst);
        // The relay swaps a build for itself; only a different build is a step back.
        if let Some(prev) = old.as_ref().and_then(|o| o.build_id.clone())
            && Some(&prev) != slot.view.build_id.as_ref()
        {
            slot.previous = Some(prev);
        }
        old
    }

    /// Let a replaced instance finish what it serves (streams get a deadline,
    /// then the proxy closes them, ADR-0010 condition 4), then stop it.
    fn retire(&self, proxy: &Proxy, old: Option<Instance>) {
        if let Some(mut old) = old {
            if let Some(port) = old.port {
                proxy.drain(port, Duration::from_secs(5));
            }
            old.tree.stop(Duration::from_secs(10));
            self.forget_record(&old.record);
        }
    }

    /// The relay's second start failed: the new build serves, but without its
    /// background workers. Said loudly, since nothing else would notice.
    fn running_without_workers(
        self: &Arc<Self>,
        name: &str,
        svc: &Service,
        e: String,
    ) -> Result<(), String> {
        let reason = format!(
            "serving without background workers: their start failed: {}",
            first_lines(&e, 3)
        );
        self.set_candidate(name, None);
        if let Some(p) = self.proxy(name) {
            p.set_pending(None);
        }
        self.set_status(
            name,
            Status::Unhealthy {
                reason: reason.clone(),
            },
        );
        let generation = self.slots.lock().unwrap()[name].generation;
        self.spawn_monitor(name.to_string(), svc.clone(), generation);
        Err(reason)
    }

    /// Put the build that ran before the last replacement back into the main
    /// slot, without building and without running jobs: a schema migration is
    /// never rolled back. Behind the proxy only (the replacement is a candidate).
    pub fn rollback(self: &Arc<Self>, name: &str) -> Result<(), String> {
        if self.is_hot(name) {
            return Err(format!(
                "`{name}` runs in hot-reload mode: switch it off first"
            ));
        }
        let svc = self.service(name)?.clone();
        if !svc.proxied() {
            return Err(format!(
                "`{name}` does not run behind the proxy; rollback needs it"
            ));
        }
        let Some(id) = self.slots.lock().unwrap()[name].previous.clone() else {
            return Err(format!("`{name}` has no previous build to go back to"));
        };
        let dir = self.layout.artifact(name, &id);
        if !dir.is_dir() {
            return Err(format!("the artifact of build {id} is gone"));
        }
        if !self.service_running(name) {
            return Err(format!("`{name}` is not running"));
        }
        self.absorb(name);
        self.replace(name, &svc, Some((id, dir)))
    }
}
