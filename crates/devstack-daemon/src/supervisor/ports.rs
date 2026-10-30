//! What this tree holds of the machine's ports (ADR-0082 point 1), as
//! [`crate::ports::tree_entries`] reads it.

use super::Supervisor;
use crate::ports::{ContainerFacts, ServiceFacts, TreeFacts};
use crate::tcpfwd::FrontState;
use std::collections::{BTreeMap, BTreeSet};

impl Supervisor {
    /// This tree's services' and containers' ports as it holds them now.
    /// Each lock is taken on its own.
    pub fn port_facts(&self) -> TreeFacts {
        let served_by = self.served_by();
        let proxies: BTreeMap<String, Option<u16>> = self
            .proxies
            .lock()
            .unwrap()
            .iter()
            .map(|(n, p)| (n.clone(), p.upstream().map(|u| u.port)))
            .collect();
        let fronts: BTreeMap<String, Option<u16>> = self
            .fronts
            .lock()
            .unwrap()
            .iter()
            .map(|(n, f)| {
                let to = match f.state() {
                    FrontState::To(p) => Some(p),
                    _ => None,
                };
                (n.clone(), to)
            })
            .collect();
        let forwards: BTreeMap<String, Option<u16>> = self
            .forwards
            .lock()
            .unwrap()
            .iter()
            .map(|(n, f)| (n.clone(), f.target()))
            .collect();
        let fronts_to_lane: BTreeSet<String> = self.fronts_to_lane.lock().unwrap().clone();
        let moved: BTreeSet<String> = if served_by.is_some() {
            self.proxied_services().into_iter().collect()
        } else {
            BTreeSet::new()
        };
        let services = self
            .slots
            .lock()
            .unwrap()
            .iter()
            .map(|(name, slot)| {
                let running = slot.view.status.is_active();
                ServiceFacts {
                    name: name.clone(),
                    port: self.stack.services.get(name).and_then(|s| s.port),
                    proxy: proxies.get(name).copied(),
                    front: fronts.get(name).copied(),
                    forward: forwards.get(name).copied(),
                    to_lane: moved.contains(name)
                        || fronts_to_lane.contains(name)
                        || forwards.contains_key(name),
                    instance: slot
                        .main
                        .as_ref()
                        .and_then(|i| i.port)
                        .or(slot.view.instance_port)
                        .filter(|_| running),
                    running,
                }
            })
            .collect();
        let held_to_lane = self.held_to_lane.lock().unwrap().is_some();
        let mut held = self.held_now();
        let mut containers = Vec::new();
        for v in self.infra.lock().unwrap().views.iter() {
            for c in &v.containers {
                containers.push(ContainerFacts {
                    group: v.group.clone(),
                    service: c.service.clone(),
                    running: c.state == "running",
                    published: c.bound.clone(),
                    held: held
                        .remove(&(v.group.clone(), c.service.clone()))
                        .unwrap_or_default(),
                });
            }
        }
        // A held port stays held while its container is gone.
        containers.extend(
            held.into_iter()
                .map(|((group, service), held)| ContainerFacts {
                    group,
                    service,
                    held,
                    ..Default::default()
                }),
        );
        TreeFacts {
            lane: if self.lane.is_empty() {
                "main".into()
            } else {
                self.lane.clone()
            },
            served_by,
            held_to_lane,
            services,
            containers,
        }
    }
}
