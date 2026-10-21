//! The main slot and its hand-over between lanes (ADR-0008, ADR-0024).

use super::{LanePins, Supervisor};
use crate::proxy::{Upstream, Waiting};
use devstack_config::model::{Kind, Service};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

impl Supervisor {
    /// This lane's instances hold the main slot: workers on (ADR-0008).
    pub fn owns_main(&self) -> bool {
        // A daemon that runs a lane on another machine holds the main slot
        // only when its requester gave it (ADR-0025 point 4): workers then.
        if crate::remote_lane() {
            return crate::REMOTE_MAIN.load(Ordering::SeqCst);
        }
        let owner = self.main_owner.lock().unwrap();
        match owner.as_deref() {
            None => self.lane.is_empty(),
            Some(l) => l == self.lane,
        }
    }

    /// The shared main-slot owner, for the lanes' supervisors.
    pub(crate) fn main_owner(&self) -> Arc<Mutex<Option<String>>> {
        self.main_owner.clone()
    }

    pub(crate) fn set_main_owner(&self, lane: Option<String>) {
        *self.main_owner.lock().unwrap() = lane;
    }

    pub(crate) fn set_lane_pins(&self, pins: LanePins) {
        *self.lane_pins.lock().unwrap() = Some(pins);
    }

    /// A removed lane: stop its services, release its ports, end its threads.
    pub(crate) fn close(&self) {
        self.down(false);
        self.unlink_prepared();
        self.closed.store(true, Ordering::SeqCst);
        for (_, p) in std::mem::take(&mut *self.proxies.lock().unwrap()) {
            p.close();
        }
        for (_, f) in std::mem::take(&mut *self.forwards.lock().unwrap()) {
            f.close();
        }
        for (_, f) in std::mem::take(&mut *self.fronts.lock().unwrap()) {
            f.close();
        }
    }

    pub(crate) fn closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// `devstack up --workers` in a lane (ADR-0030).
    pub fn set_lane_workers(&self, on: bool) {
        self.lane_workers.store(on, Ordering::SeqCst);
    }

    /// Main lane: the lane serving its fixed ports, if one does (`None` in a
    /// lane: its own ports are its own).
    pub fn served_by(&self) -> Option<String> {
        if self.lane.is_empty() {
            self.main_owner.lock().unwrap().clone()
        } else {
            None
        }
    }

    /// Services whose fixed port is a devstack proxy: what `promote --lane`
    /// moves (a dev server keeps its port).
    pub fn proxied_services(&self) -> Vec<String> {
        self.stack
            .services
            .iter()
            .filter(|(_, s)| s.proxied() && s.lanes && self.runs_here(s))
            .map(|(n, _)| n.clone())
            .collect()
    }

    /// Long-running services with a fixed port but without devstack's HTTP
    /// proxy (dev servers, `proxy = false`): `promote --lane` forwards their
    /// port byte for byte.
    pub fn unproxied_services(&self) -> Vec<String> {
        self.stack
            .services
            .iter()
            .filter(|(_, s)| {
                s.port.is_some()
                    && !s.proxied()
                    && s.kind != Kind::Job
                    && s.lanes
                    && self.runs_here(s)
            })
            .map(|(n, _)| n.clone())
            .collect()
    }

    /// The main slot moves something in this stack: see [`moves_main_slot`].
    pub fn main_slot_moves(&self) -> bool {
        moves_main_slot(&self.stack)
    }

    /// Main lane: forward the fixed port of `name` to `target` (`None` ends
    /// the forward). The previous forward's listener is closed before this
    /// returns, so the port can be bound again at once.
    pub fn tcp_forward(&self, name: &str, target: Option<u16>) -> Result<(), String> {
        let svc = self.service(name)?.clone();
        // A fronted dev server's port is devstack's already (ADR-0043): its
        // front points at the lane, or refuses until the main tree's own
        // instance is back.
        if let Some(f) = self.fronts.lock().unwrap().get(name) {
            let mut to_lane = self.fronts_to_lane.lock().unwrap();
            match target {
                Some(t) => {
                    f.set(crate::tcpfwd::FrontState::To(t));
                    to_lane.insert(name.to_string());
                }
                None => {
                    f.set(crate::tcpfwd::FrontState::Refuse);
                    to_lane.remove(name);
                }
            }
            return Ok(());
        }
        if svc.fronted() && target.is_some() {
            // Never started here, so never bound: bind it now, for the lane.
            self.ensure_front(name, &svc)?;
            return self.tcp_forward(name, target);
        }
        let mut fw = self.forwards.lock().unwrap();
        if let Some(old) = fw.remove(name) {
            old.close();
        }
        if let (Some(port), Some(t)) = (svc.port, target) {
            let f = crate::tcpfwd::start(port, t, &self.rt)
                .map_err(|e| format!("forward port {port} to {t}: {e}"))?;
            fw.insert(name.to_string(), f);
        }
        Ok(())
    }

    /// Main lane: the services whose fixed port forwards to a lane now.
    pub fn forwarded_services(&self) -> Vec<String> {
        let mut v: Vec<String> = self.forwards.lock().unwrap().keys().cloned().collect();
        v.extend(self.fronts_to_lane.lock().unwrap().iter().cloned());
        v
    }

    /// Running services that have worker switches (ADR-0008).
    pub fn running_worker_services(&self) -> Vec<String> {
        let slots = self.slots.lock().unwrap();
        self.stack
            .services
            .iter()
            // `lanes = false` keeps its workers wherever the main slot is.
            .filter(|(n, s)| {
                s.workers.is_some() && s.lanes && slots[n.as_str()].view.status.is_active()
            })
            .map(|(n, _)| n.clone())
            .collect()
    }

    /// A start of `name` that would take its main port while a lane serves
    /// the main ports, or while they move to one (ADR-0008: the workers
    /// would run in both trees): why not.
    pub(super) fn main_ports_elsewhere(&self, name: &str, svc: &Service) -> Option<String> {
        if !svc.proxied()
            && !self.forwards.lock().unwrap().contains_key(name)
            && !self.fronts_to_lane.lock().unwrap().contains(name)
        {
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

    /// Main lane: the fixed port of `name` forwards to another lane's port
    /// (`Some`), or is released for this lane's own instances (`None`).
    pub fn forward(
        &self,
        name: &str,
        to: Option<(&str, u16, Option<String>)>,
    ) -> Result<(), String> {
        let svc = self.service(name)?.clone();
        let proxy = self.ensure_proxy(name, &svc)?;
        match to {
            Some((lane, port, build)) => {
                proxy.set_waiting(Waiting::Refuse(format!("lane `{lane}` is not answering")));
                proxy.set_upstream(Some(Upstream { port, build }));
            }
            None => {
                // Until this tree's own instance runs (a start sets its own
                // waiting), the port says why it does not answer, not that a
                // lane does not.
                proxy.set_upstream(None);
                proxy.set_waiting(Waiting::Refuse(format!(
                    "`{name}` is stopped (devstack start {name})"
                )));
            }
        }
        Ok(())
    }
}

/// What `promote --lane` moves in the main tree: the fixed ports of its
/// services (proxied, or forwarded byte for byte) and their workers
/// (ADR-0008), and the containers' ports devstack holds (`main_ports`,
/// ADR-0048). Any other container's published port is Docker's: a stack of
/// such containers only has nothing to move (#184).
pub(crate) fn moves_main_slot(stack: &devstack_config::Stack) -> bool {
    stack.services.values().any(|s| {
        s.lanes
            && super::on_this_platform(s)
            && ((s.port.is_some() && s.kind != Kind::Job) || s.workers.is_some())
    }) || stack.compose.values().any(|g| !g.main_ports.is_empty())
}

#[cfg(test)]
mod tests {
    use super::moves_main_slot;
    use std::path::Path;

    fn stack(services: &str) -> devstack_config::Stack {
        let text = format!(
            "version = 1
[stack]
name = \"s\"
{services}"
        );
        devstack_config::from_str(&text, Path::new("devstack.toml")).unwrap()
    }

    #[test]
    fn a_stack_of_containers_only_moves_nothing() {
        assert!(!moves_main_slot(&stack("")));
        // A service kept out of lanes stays in the main tree.
        assert!(!moves_main_slot(&stack(
            "[services.once]
run = \"x\"
port = 9001
lanes = false
"
        )));
    }

    #[test]
    fn containers_named_in_main_ports_move() {
        assert!(moves_main_slot(&stack(
            "[compose.app]
project = \"p\"
files = [\"c.yml\"]
services = [\"web\"]
main_ports = [\"web\"]
"
        )));
        assert!(!moves_main_slot(&stack(
            "[compose.app]
project = \"p\"
files = [\"c.yml\"]
services = [\"web\"]
"
        )));
    }

    #[test]
    fn a_service_on_a_port_or_with_workers_moves() {
        assert!(moves_main_slot(&stack(
            "[services.api]
run = \"x\"
port = 8080
"
        )));
        assert!(moves_main_slot(&stack(
            "[services.worker]
run = \"x\"
workers = { env = { WORKERS = \"{is_main}\" } }
"
        )));
    }
}
