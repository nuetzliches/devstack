//! The infra plane: Compose groups and their containers.

use super::{InfraCache, Supervisor, topo};
use crate::state::{ContainerView, EventBody, InfraView};
use devstack_compose::{Container, Group};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

impl Supervisor {
    /// The published address of every Compose port, `(group, service,
    /// container port) -> host:port`, as last seen (`{compose.…}`).
    pub(crate) fn published_ports(&self) -> BTreeMap<(String, String, u16), String> {
        self.infra.lock().unwrap().published.clone()
    }

    /// A Compose group as this supervisor runs it. In a lane (ADR-0030): its
    /// own project `<project>-<lane>`, its own external networks
    /// `<name>-<lane>`, the overlay written by [`Self::write_lane_overlay`]
    /// last, and without the `shared` services, which the main tree runs.
    pub(super) fn compose_group(&self, name: &str) -> Group {
        let cfg = &self.stack.compose[name];
        let mut g = Group::from_config(name, cfg, &self.repo);
        if self.lane.is_empty() {
            return g;
        }
        g.project = format!("{}-{}", cfg.project, self.lane);
        g.networks = cfg
            .networks
            .iter()
            .map(|n| format!("{n}-{}", self.lane))
            .collect();
        g.services.retain(|s| !cfg.shared.contains(s));
        let overlay = self.lane_overlay_file(name);
        if overlay.is_file() {
            g.files.push(overlay);
        }
        g
    }

    fn lane_overlay_file(&self, group: &str) -> PathBuf {
        self.state_dir.join("compose").join(format!("{group}.yml"))
    }

    /// The overlay that gives a lane's copy of `group` its own ports, names,
    /// networks and volumes, written from the group's configuration as it
    /// stands now (the lane's branch may have changed its Compose files).
    fn write_lane_overlay(&self, group: &str) -> Result<(), String> {
        let file = self.lane_overlay_file(group);
        let _ = std::fs::remove_file(&file);
        let g = self.compose_group(group);
        let model = self.docker.config(&g).map_err(|e| e.to_string())?;
        let shared = &self.stack.compose[group].shared;
        let text = devstack_compose::lane_overlay(
            &model,
            &g.project,
            &self.lane,
            self.port_offset,
            shared,
            &self.stack.compose[group].networks,
        )
        .map_err(|e| format!("compose group `{group}`: {e}"))?;
        std::fs::create_dir_all(file.parent().unwrap()).map_err(|e| e.to_string())?;
        std::fs::write(&file, text).map_err(|e| e.to_string())
    }

    /// The host ports the main tree's containers publish: a lane's range
    /// must not meet them either.
    pub fn infra_host_ports(&self) -> Vec<u16> {
        self.infra
            .lock()
            .unwrap()
            .published
            .values()
            .filter_map(|a| a.rsplit(':').next()?.parse().ok())
            .collect()
    }

    /// Compose groups in `after` order.
    pub(super) fn group_order(&self) -> Vec<String> {
        topo(
            self.stack
                .compose
                .iter()
                .map(|(k, g)| (k.clone(), g.after.clone()))
                .collect(),
        )
    }

    pub fn infra_up(&self) -> Result<(), String> {
        if self.external_infra().is_some() {
            // The node's agent started it before this daemon (ADR-0025).
            self.refresh_infra();
            return Ok(());
        }
        for g in self.group_order() {
            self.emit(EventBody::Infra {
                group: g.clone(),
                message: "starting".into(),
            });
            if !self.lane.is_empty()
                && let Err(e) = self.write_lane_overlay(&g)
            {
                self.emit(EventBody::Infra {
                    group: g.clone(),
                    message: format!("failed: {e}"),
                });
                return Err(e);
            }
            let group = self.compose_group(&g);
            if let Err(e) = self.docker.up(&group, Duration::from_secs(300)) {
                let msg = e.to_string();
                self.emit(EventBody::Infra {
                    group: g.clone(),
                    message: format!("failed: {msg}"),
                });
                self.refresh_infra();
                return Err(format!("compose group `{g}`: {msg}"));
            }
            self.emit(EventBody::Infra {
                group: g.clone(),
                message: "ready".into(),
            });
        }
        self.refresh_infra();
        Ok(())
    }

    /// Where the dashboard links a container to: the group's `urls` entry for
    /// it (`""` for none), otherwise `http://localhost:<port>` per published
    /// port, as Docker Desktop does.
    fn container_urls(&self, group: &str, service: &str, ports: &[String]) -> Vec<String> {
        match self
            .stack
            .compose
            .get(group)
            .and_then(|g| g.urls.get(service))
        {
            Some(u) if u.is_empty() => Vec::new(),
            Some(u) => vec![u.clone()],
            None => ports
                .iter()
                .filter_map(|p| p.rsplit(':').next())
                .map(|port| format!("http://localhost:{port}"))
                .collect(),
        }
    }

    /// A container as `compose ps` reports it, as the view shows it (nothing
    /// marked unreachable); its published addresses go into `published`.
    fn container_view(
        &self,
        group: &str,
        c: Container,
        published: &mut BTreeMap<(String, String, u16), String>,
    ) -> ContainerView {
        let mut ports = Vec::new();
        for p in &c.publishers {
            if let Some(addr) = c.published(p.target_port) {
                published.insert(
                    (group.to_string(), c.service.clone(), p.target_port),
                    addr.clone(),
                );
                let s = format!("{}->{addr}", p.target_port);
                if !ports.contains(&s) {
                    ports.push(s);
                }
            }
        }
        ContainerView {
            service: c.service.clone(),
            name: c.name,
            exit_code: c.exit_code,
            state: c.state,
            health: c.health,
            urls: self.container_urls(group, &c.service, &ports),
            ports,
            unreachable: Vec::new(),
        }
    }

    /// Re-read only the containers' states (`compose ps`, no `config`): cheap
    /// enough to run every few seconds, so a container that stopped or turned
    /// unhealthy shows as such. Every change is an event.
    pub fn refresh_containers(&self) {
        if let Some(f) = self.external_infra() {
            return self.refresh_external(&f);
        }
        for g in self.group_order() {
            let group = self.compose_group(&g);
            let Ok(containers) = self.docker.ps(&group) else {
                continue; // Docker unreachable: keep what is known, the sampler reports it
            };
            let mut changes = Vec::new();
            {
                let mut cache = self.infra.lock().unwrap();
                let cache = &mut *cache;
                let Some(view) = cache.views.iter_mut().find(|v| v.group == g) else {
                    continue;
                };
                let mut next = Vec::new();
                for c in containers {
                    let mut c = self.container_view(&g, c, &mut cache.published);
                    let state = |s: &str, h: &str| {
                        if h.is_empty() {
                            s.to_string()
                        } else {
                            format!("{s} ({h})")
                        }
                    };
                    let before = view
                        .containers
                        .iter()
                        .find(|o| o.service == c.service)
                        .map(|o| state(&o.state, &o.health));
                    let now = state(&c.state, &c.health);
                    if before.as_deref() != Some(now.as_str()) {
                        changes.push(format!("{}: {now}", c.service));
                    }
                    // The last verdict of `check_forwards` stays until the
                    // container stops (a restart repairs a lost forward).
                    let up = c.state == "running";
                    if up {
                        c.unreachable = view
                            .containers
                            .iter()
                            .find(|o| o.service == c.service)
                            .map(|o| o.unreachable.clone())
                            .unwrap_or_default();
                    }
                    next.push(c);
                }
                for gone in view
                    .containers
                    .iter()
                    .filter(|o| !next.iter().any(|n| n.service == o.service))
                {
                    changes.push(format!("{}: removed", gone.service));
                }
                view.containers = next;
            }
            for message in changes {
                self.emit(EventBody::Infra {
                    group: g.clone(),
                    message,
                });
            }
        }
    }

    /// Re-read Compose: environments of every service (also parked ones) and
    /// published addresses of running containers.
    pub fn refresh_infra(&self) {
        if let Some(f) = self.external_infra() {
            return self.refresh_external(&f);
        }
        let mut cache = InfraCache::default();
        for g in self.group_order() {
            let group = self.compose_group(&g);
            let mut view = InfraView {
                group: g.clone(),
                project: group.project.clone(),
                containers: Vec::new(),
                error: None,
            };
            match self.docker.config(&group) {
                Ok(model) => {
                    for (svc, m) in model.services {
                        for p in &m.ports {
                            let key = (g.clone(), svc.clone(), p.target);
                            match &p.published {
                                Some(pubd) => {
                                    cache.published.insert(key, format!("127.0.0.1:{pubd}"));
                                }
                                None => {
                                    cache.pending.insert(key);
                                }
                            }
                        }
                        cache.envs.insert((g.clone(), svc), m.environment);
                    }
                }
                Err(e) => view.error = Some(e.to_string()),
            }
            match self.docker.ps(&group) {
                Ok(containers) => {
                    for c in containers {
                        let c = self.container_view(&g, c, &mut cache.published);
                        view.containers.push(c);
                    }
                }
                Err(e) => view.error = Some(e.to_string()),
            }
            // A lane's `shared` services run in the main tree's project: their
            // addresses come from there, also when Docker assigned the port.
            let shared = &self.stack.compose[&g].shared;
            if !self.lane.is_empty() && !shared.is_empty() {
                let main = Group::from_config(&g, &self.stack.compose[&g], &self.repo);
                for c in self.docker.ps(&main).unwrap_or_default() {
                    if !shared.contains(&c.service) {
                        continue;
                    }
                    for p in &c.publishers {
                        if let Some(addr) = c.published(p.target_port) {
                            let key = (g.clone(), c.service.clone(), p.target_port);
                            cache.pending.remove(&key);
                            cache.published.insert(key, addr);
                        }
                    }
                }
            }
            cache.views.push(view);
        }
        *self.infra.lock().unwrap() = cache;
    }

    /// Follow one container's output (`docker compose logs --follow`); in a
    /// lane its own container, the main tree's for a `shared` one.
    pub fn follow_container_logs(
        &self,
        group: &str,
        service: &str,
        tail: usize,
    ) -> Result<std::process::Child, String> {
        let g = if self.is_shared(group, service) {
            Group::from_config(group, &self.stack.compose[group], &self.repo)
        } else {
            self.compose_group(group)
        };
        self.docker
            .follow_logs(&g, service, tail)
            .map_err(|e| e.to_string())
    }

    /// A container's output (`docker compose logs`, timestamped).
    pub fn container_logs(
        &self,
        group: &str,
        service: &str,
        tail: usize,
        since: Option<&str>,
    ) -> Result<String, String> {
        self.docker
            .logs(&self.compose_group(group), service, tail, since)
            .map_err(|e| e.to_string())
    }

    /// The group of the Compose service called `name`, if exactly one group
    /// starts one by that name.
    pub fn container_group(&self, name: &str) -> Option<String> {
        let mut groups = self
            .stack
            .compose
            .iter()
            .filter(|(_, g)| g.services.iter().any(|s| s == name))
            .map(|(k, _)| k.clone());
        let first = groups.next()?;
        groups.next().is_none().then_some(first)
    }

    /// `start`, `stop` or `restart` of one container from the dashboard or
    /// the CLI. A lane acts on its own containers only (ADR-0030).
    pub fn container_action(&self, group: &str, service: &str, action: &str) -> Result<(), String> {
        if self.external_infra().is_some() {
            return Err("the node runs this lane's containers (ADR-0025)".into());
        }
        if self.is_shared(group, service) {
            return Err(format!(
                "`{service}` is shared: the main working tree runs it (`shared` in the stack file)"
            ));
        }
        let mut g = self.compose_group(group);
        let result = match action {
            "start" => {
                g.services = vec![service.to_string()];
                self.docker.up(&g, Duration::from_secs(300))
            }
            "stop" => self.docker.stop(&g, service),
            "restart" => self.docker.restart(&g, service),
            _ => return Err(format!("unknown action `{action}`")),
        };
        let message = match &result {
            Ok(()) => format!("{service}: {action} done"),
            Err(e) => format!("{service}: {action} failed: {e}"),
        };
        self.emit(EventBody::Infra {
            group: group.into(),
            message,
        });
        self.refresh_containers();
        result.map_err(|e| e.to_string())
    }

    /// `restart_on` of a Compose service: restart the container.
    pub(crate) fn restart_container(&self, group: &str, service: &str) {
        if self.is_shared(group, service) || self.external_infra().is_some() {
            return; // the main tree's container (ADR-0030)
        }
        let message = match self.docker.restart(&self.compose_group(group), service) {
            Ok(()) => format!("{service}: restarted after a change"),
            Err(e) => format!("{service}: restart after a change failed: {e}"),
        };
        self.emit(EventBody::Infra {
            group: group.into(),
            message,
        });
        self.refresh_containers();
    }

    /// The published ports of a Compose service, as this machine reaches them.
    fn container_ports(&self, group: &str, service: &str) -> Vec<String> {
        self.infra
            .lock()
            .unwrap()
            .published
            .iter()
            .filter(|((g, s, _), _)| g == group && s == service)
            .map(|((_, _, t), addr)| format!("{t}->{addr}"))
            .collect()
    }

    /// Probe the published ports of every running container from this machine
    /// and record lost port forwards in the view (on demand: a probe is a
    /// connection the server sees, so it does not run with every refresh).
    pub fn check_forwards(&self) -> Vec<(String, String, Vec<String>)> {
        if self.external_infra().is_some() {
            return Vec::new(); // addresses on a Compose network, not host forwards
        }
        let targets: Vec<(String, String, Vec<String>)> = self
            .infra
            .lock()
            .unwrap()
            .views
            .iter()
            .flat_map(|v| {
                v.containers
                    .iter()
                    .filter(|c| {
                        c.state == "running" && (c.health.is_empty() || c.health == "healthy")
                    })
                    .map(|c| (v.group.clone(), c.service.clone(), c.ports.clone()))
                    .collect::<Vec<_>>()
            })
            .collect();
        let mut out = Vec::new();
        for (g, svc, ports) in targets {
            let lost = lost_forwards(&ports);
            self.set_unreachable(&g, &svc, lost.clone());
            if !lost.is_empty() {
                out.push((g, svc, lost));
            }
        }
        out
    }

    fn set_unreachable(&self, group: &str, service: &str, lost: Vec<String>) {
        let mut cache = self.infra.lock().unwrap();
        if let Some(c) = cache
            .views
            .iter_mut()
            .filter(|v| v.group == group)
            .flat_map(|v| v.containers.iter_mut())
            .find(|c| c.service == service)
        {
            c.unreachable = lost;
        }
    }

    /// A Compose service a native one needs must be reachable from here, not
    /// only healthy inside Docker: a lost port forward (Docker Desktop after a
    /// restart) is repaired by restarting the container once. Never fails the
    /// start: a port with nothing listening inside looks the same.
    pub(super) fn ensure_reachable(&self, group: &str, service: &str) -> Result<(), String> {
        if self.is_shared(group, service) || self.external_infra().is_some() {
            return Ok(()); // the main tree's container; it repairs its own
        }
        let lost = lost_forwards(&self.container_ports(group, service));
        self.set_unreachable(group, service, lost.clone());
        if lost.is_empty() {
            return Ok(());
        }
        self.emit(EventBody::Infra {
            group: group.into(),
            message: format!(
                "{service}: {} accepted but closed at once from this machine (lost port forward); restarting the container",
                lost.join(", ")
            ),
        });
        self.docker
            .restart(&self.compose_group(group), service)
            .map_err(|e| format!("restart `{service}`: {e}"))?;
        let t0 = Instant::now();
        loop {
            self.refresh_containers();
            let still = lost_forwards(&self.container_ports(group, service));
            let up = self.infra.lock().unwrap().views.iter().any(|v| {
                v.group == group
                    && v.containers.iter().any(|c| {
                        c.service == service
                            && c.state == "running"
                            && (c.health.is_empty() || c.health == "healthy")
                    })
            });
            if up && still.is_empty() {
                self.set_unreachable(group, service, Vec::new());
                return Ok(());
            }
            if t0.elapsed() > Duration::from_secs(60) {
                // Not a lost forward after all (nothing listens on that port
                // inside), or Docker Desktop itself is stuck: say so, but do not
                // keep the service from starting.
                self.emit(EventBody::Infra {
                    group: group.into(),
                    message: format!(
                        "{service}: {} still closes connections from this machine after a restart of the container; if a service cannot reach it, restart Docker Desktop",
                        still.join(", ")
                    ),
                });
                return Ok(());
            }
            std::thread::sleep(Duration::from_secs(2));
        }
    }

    /// A Compose service a lane uses from the main tree (`shared`, ADR-0030).
    fn is_shared(&self, group: &str, service: &str) -> bool {
        !self.lane.is_empty()
            && self
                .stack
                .compose
                .get(group)
                .is_some_and(|g| g.shared.iter().any(|s| s == service))
    }

    /// A loopback forward on `host` to `addr`, started once; whether it runs.
    fn loopback_forward(&self, host: u16, addr: &str) -> bool {
        let mut fw = self.loopback.lock().unwrap();
        if fw.contains_key(&host) {
            return true;
        }
        match crate::tcpfwd::start_to(host, addr.to_string(), &self.rt) {
            Ok(f) => {
                fw.insert(host, f);
                true
            }
            Err(e) => {
                crate::diag::warn(
                    "loopback_forward_failed",
                    serde_json::json!({ "port": host, "to": addr, "error": e.to_string() }),
                );
                false
            }
        }
    }

    /// In a lane on a node: the container address a tunnel on `port` goes
    /// to, when `port` is where a container of the lane is published for its
    /// developer (ADR-0029 point 4).
    pub fn infra_tunnel(&self, port: u16) -> Option<String> {
        let ext = crate::external::read(&self.external_infra()?).ok()?;
        ext.groups
            .values()
            .flat_map(|g| g.services.values())
            .find_map(|s| {
                s.published
                    .iter()
                    .find(|(_, h)| **h == port)
                    .and_then(|(target, _)| s.ports.get(target).cloned())
            })
    }

    /// The infrastructure is run by a node's agent (ADR-0025): its file, if so.
    pub fn external_infra(&self) -> Option<std::path::PathBuf> {
        crate::external::path()
    }

    /// The cache from the agent's file: environments, addresses on the
    /// lane's Compose network, states.
    fn refresh_external(&self, file: &Path) {
        let mut cache = InfraCache::default();
        let ext = crate::external::read(file);
        for g in self.group_order() {
            let mut view = InfraView {
                group: g.clone(),
                project: String::new(),
                containers: Vec::new(),
                error: None,
            };
            match &ext {
                Ok(ext) => {
                    let eg = ext.groups.get(&g).cloned().unwrap_or_default();
                    view.project = eg.project.clone();
                    for (svc, s) in eg.services {
                        for (port, addr) in &s.ports {
                            // A published container is reached on loopback,
                            // as on the developer's machine: the same
                            // addresses for the services (`rewrite_hosts`),
                            // and loopback where they insist on it.
                            let at = match s.published.get(port) {
                                Some(host) if self.loopback_forward(*host, addr) => {
                                    format!("127.0.0.1:{host}")
                                }
                                _ => addr.clone(),
                            };
                            cache.published.insert((g.clone(), svc.clone(), *port), at);
                        }
                        cache
                            .envs
                            .insert((g.clone(), svc.clone()), s.environment.clone());
                        if !s.state.is_empty() {
                            // Shown, and linked, as the developer reaches it.
                            let ports: Vec<String> = s
                                .ports
                                .iter()
                                .map(|(p, a)| match s.published.get(p) {
                                    Some(h) => format!("{h}->{a}"),
                                    None => format!("{p}->{a}"),
                                })
                                .collect();
                            let here: Vec<String> = s
                                .published
                                .values()
                                .map(|h| format!("localhost:{h}"))
                                .collect();
                            view.containers.push(ContainerView {
                                service: svc.clone(),
                                name: format!("{}-{svc}", eg.project),
                                exit_code: 0,
                                state: s.state.clone(),
                                health: s.health.clone(),
                                urls: self.container_urls(&g, &svc, &here),
                                ports,
                                unreachable: Vec::new(),
                            });
                        }
                    }
                }
                Err(e) => view.error = Some(e.clone()),
            }
            cache.views.push(view);
        }
        *self.infra.lock().unwrap() = cache;
    }
}

/// The published ports (`5432->127.0.0.1:5432`) whose forward is lost.
fn lost_forwards(ports: &[String]) -> Vec<String> {
    ports
        .iter()
        .filter(|p| {
            p.split_once("->").is_some_and(|(_, addr)| {
                devstack_compose::probe(addr) == devstack_compose::Reach::ClosedAtOnce
            })
        })
        .cloned()
        .collect()
}
