//! The infra plane: Compose groups and their containers.

use super::{InfraCache, Supervisor, topo};
use crate::state::{ContainerView, EventBody, InfraView};
use devstack_compose::{Container, Group};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// In a lane on a node, its agent answers for the containers (#101); one
/// that passes such a request on to the lane is older than that.
const NODE_TOO_OLD: &str = "the node runs this lane's containers, and its devstack is too old to show their output: update devstack on the node";
/// The same for starting, stopping and restarting them (#128): the agent
/// does it and says so ([`devstack_node::laneinfra::ACTED_HEADER`]); a
/// request without that word reached the daemon past an older agent, or
/// one that could not read the lane's stack file.
pub(crate) const NODE_DID_NOT_ACT: &str = "the node runs this lane's containers and did not start, stop or restart them: its devstack is too old (update devstack on the node), or it could not read the lane's stack file";

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
        if let Some(file) = self.external_infra() {
            // The node's agent starts it, after this daemon (ADR-0025, #164).
            return self.node_infra_up(&file);
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

    /// In a lane on a node: wait until its agent says every group is up, or
    /// one failed, as it brings them up after this daemon answered (#164),
    /// with the events a local start has. A node that says nothing new for
    /// [`NODE_GROUP_WAIT`] counts as failed. A file of an older agent,
    /// written once everything was up, says so at once.
    fn node_infra_up(&self, file: &Path) -> Result<(), String> {
        let order = self.group_order();
        let mut follow = NodeBringUp::default();
        let mut moved = Instant::now();
        loop {
            self.refresh_external(file);
            // A file being written, or not there yet: read again.
            if let Ok(ext) = crate::external::read(file) {
                let (events, done) = follow.step(&order, &ext);
                if !events.is_empty() {
                    moved = Instant::now();
                }
                for (group, message) in events {
                    self.emit(EventBody::Infra { group, message });
                }
                if let Some(done) = done {
                    return done;
                }
            }
            if moved.elapsed() > NODE_GROUP_WAIT {
                let g = follow
                    .pending(&order)
                    .unwrap_or_else(|| order.first().cloned().unwrap_or_default());
                let msg = format!(
                    "its node said nothing of it for {} s (is the node's agent running?)",
                    NODE_GROUP_WAIT.as_secs()
                );
                self.emit(EventBody::Infra {
                    group: g.clone(),
                    message: format!("failed: {msg}"),
                });
                return Err(format!("compose group `{g}`: {msg}"));
            }
            std::thread::sleep(Duration::from_secs(1));
        }
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
            managed: self.starts(group, &c.service),
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
                    let changed = before.as_deref() != Some(now.as_str());
                    if changed {
                        changes.push(format!("{}: {now}", c.service));
                    }
                    self.forward_checks
                        .lock()
                        .unwrap()
                        .entry((g.clone(), c.service.clone()))
                        .or_default()
                        .observe(is_ready(&c), changed, Instant::now());
                    // The last verdict on its forwards stays while its state
                    // does; `recheck_forwards` looks again (#98).
                    if !changed && c.state == "running" {
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
        if self.external_infra().is_some() {
            return Err(NODE_TOO_OLD.into());
        }
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
        if self.external_infra().is_some() {
            return Err(NODE_TOO_OLD.into());
        }
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

    /// The Compose group `group` starts `service` (it is in its `services`).
    pub fn starts(&self, group: &str, service: &str) -> bool {
        self.stack
            .compose
            .get(group)
            .is_some_and(|g| g.services.iter().any(|s| s == service))
    }

    /// What `/v1/infra/{group}/{service}/…` names: `None` without such a
    /// group. The routes accept what the view shows, and the view shows every
    /// container `compose ps` reports for the group's project, also one of a
    /// service the group does not start.
    pub fn container_target(&self, group: &str, service: &str) -> Option<ContainerTarget> {
        let cfg = self.stack.compose.get(group)?;
        let infra = self.infra.lock().unwrap();
        let shown = infra
            .views
            .iter()
            .filter(|v| v.group == group)
            .flat_map(|v| v.containers.iter());
        Some(container_target(&cfg.services, shown, service))
    }

    /// Stop a container of the group's project the group does not start
    /// (`docker stop` by its name: Compose may not know its service, an
    /// orphan or one of an inactive profile).
    pub fn stop_unmanaged(
        &self,
        group: &str,
        service: &str,
        name: &str,
        node_acted: bool,
    ) -> Result<(), String> {
        if self.external_infra().is_some() {
            return self.acted_on_node(group, service, "stop", node_acted);
        }
        let result = self.docker.stop_container(&self.repo, name);
        let message = match &result {
            Ok(()) => format!("{service}: stop done"),
            Err(e) => format!("{service}: stop failed: {e}"),
        };
        self.emit(EventBody::Infra {
            group: group.into(),
            message,
        });
        self.refresh_containers();
        result.map_err(|e| e.to_string())
    }

    /// In a lane on a node: its agent started, stopped or restarted the
    /// container (`node_acted`), and the view shows it at once; or it did
    /// not, and this daemon cannot (it has no Docker, ADR-0025).
    fn acted_on_node(
        &self,
        group: &str,
        service: &str,
        action: &str,
        node_acted: bool,
    ) -> Result<(), String> {
        if !node_acted {
            return Err(NODE_DID_NOT_ACT.into());
        }
        self.emit(EventBody::Infra {
            group: group.into(),
            message: format!("{service}: {action} done (by its node)"),
        });
        self.refresh_containers();
        Ok(())
    }

    /// `start`, `stop` or `restart` of one container from the dashboard or
    /// the CLI. A lane acts on its own containers only (ADR-0030); a lane on
    /// a node has its node's agent act on them (`node_acted`, #128).
    pub fn container_action(
        &self,
        group: &str,
        service: &str,
        action: &str,
        node_acted: bool,
    ) -> Result<(), String> {
        if self.external_infra().is_some() {
            return self.acted_on_node(group, service, action, node_acted);
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
        self.forwards_afresh(group, service);
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
        let restarted = self.docker.restart(&self.compose_group(group), service);
        self.forwards_afresh(group, service);
        let message = match restarted {
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

    /// Probe the published ports of every ready container from this machine
    /// and record lost port forwards in the view (on demand: a probe is a
    /// connection the server sees, so it does not run with every refresh).
    /// `settled`: only containers ready for [`FORWARD_GRACE`]; one that just
    /// started is judged by [`Self::recheck_forwards`] once it is.
    pub fn check_forwards(&self, settled: bool) -> Vec<(String, String, Vec<String>)> {
        if self.external_infra().is_some() {
            return Vec::new(); // addresses on a Compose network, not host forwards
        }
        let now = Instant::now();
        let targets: Vec<(String, String, Vec<String>)> = {
            let infra = self.infra.lock().unwrap();
            let checks = self.forward_checks.lock().unwrap();
            infra
                .views
                .iter()
                .flat_map(|v| {
                    v.containers
                        .iter()
                        .filter(|c| is_ready(c) && !c.ports.is_empty())
                        .filter(|c| {
                            !settled
                                || checks
                                    .get(&(v.group.clone(), c.service.clone()))
                                    .is_some_and(|f| f.settled(now))
                        })
                        .map(|c| (v.group.clone(), c.service.clone(), c.ports.clone()))
                        .collect::<Vec<_>>()
                })
                .collect()
        };
        let mut out = Vec::new();
        for (g, svc, ports) in targets {
            let lost = lost_forwards(&ports, FORWARD_TRIES);
            self.judged(&g, &svc);
            self.set_unreachable(&g, &svc, lost.clone());
            if !lost.is_empty() {
                out.push((g, svc, lost));
            }
        }
        out
    }

    /// From the sampler: judge a container's forwards once it has been ready
    /// for [`FORWARD_GRACE`], and look again at those found lost, so a
    /// verdict neither comes from a server still starting inside nor
    /// outlives the repair (#98).
    pub fn recheck_forwards(&self) {
        if self.external_infra().is_some() {
            return;
        }
        let now = Instant::now();
        let targets: Vec<(String, String, Vec<String>, bool)> = {
            let infra = self.infra.lock().unwrap();
            let mut checks = self.forward_checks.lock().unwrap();
            let mut t = Vec::new();
            for v in &infra.views {
                for c in v
                    .containers
                    .iter()
                    .filter(|c| is_ready(c) && !c.ports.is_empty())
                {
                    let f = checks
                        .entry((v.group.clone(), c.service.clone()))
                        .or_default();
                    let flagged = !c.unreachable.is_empty();
                    if flagged || (!f.checked && f.settled(now)) {
                        t.push((v.group.clone(), c.service.clone(), c.ports.clone(), flagged));
                    }
                }
            }
            t
        };
        for (g, svc, ports, flagged) in targets {
            // One probe that answers clears a verdict; a new one is confirmed.
            let lost = lost_forwards(&ports, if flagged { 1 } else { FORWARD_TRIES });
            self.judged(&g, &svc);
            self.set_unreachable(&g, &svc, lost.clone());
            let message = match (flagged, lost.is_empty()) {
                (false, false) => format!(
                    "{svc}: {} closes connections from this machine at once (Docker lost the port forward, or nothing listens inside)",
                    lost.join(", ")
                ),
                (true, true) => format!("{svc}: reachable from this machine again"),
                _ => continue,
            };
            self.emit(EventBody::Infra { group: g, message });
        }
    }

    /// The forwards of `service` were judged since it became ready.
    fn judged(&self, group: &str, service: &str) {
        self.forward_checks
            .lock()
            .unwrap()
            .entry((group.to_string(), service.to_string()))
            .or_default()
            .checked = true;
    }

    /// `service` (re)started from here: no verdict on its forwards until it
    /// is ready again, for a while.
    fn forwards_afresh(&self, group: &str, service: &str) {
        self.forward_checks
            .lock()
            .unwrap()
            .entry((group.to_string(), service.to_string()))
            .or_default()
            .afresh();
        self.set_unreachable(group, service, Vec::new());
    }

    /// How long `service` is ready (running, healthy if it has a check);
    /// `None` while it is not.
    fn ready_for(&self, group: &str, service: &str) -> Option<Duration> {
        let ready = self.infra.lock().unwrap().views.iter().any(|v| {
            v.group == group
                && v.containers
                    .iter()
                    .any(|c| c.service == service && is_ready(c))
        });
        if !ready {
            return None;
        }
        let now = Instant::now();
        let mut checks = self.forward_checks.lock().unwrap();
        let f = checks
            .entry((group.to_string(), service.to_string()))
            .or_default();
        Some(now.duration_since(*f.ready_since.get_or_insert(now)))
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
        let ports = self.container_ports(group, service);
        let mut lost = lost_forwards(&ports, FORWARD_TRIES);
        self.set_unreachable(group, service, lost.clone());
        if lost.is_empty() {
            return Ok(());
        }
        // Only a container ready for a while is judged: until the server
        // inside listens, Docker Desktop's proxy closes a connection just as
        // for a lost forward (#98). One that is not ready is not repaired by
        // a restart either.
        match self.ready_for(group, service) {
            None => return Ok(()),
            Some(d) if d < FORWARD_GRACE => {
                std::thread::sleep(FORWARD_GRACE - d);
                lost = lost_forwards(&ports, FORWARD_TRIES);
                self.set_unreachable(group, service, lost.clone());
                if lost.is_empty() {
                    return Ok(());
                }
            }
            Some(_) => {}
        }
        let may = self
            .forward_checks
            .lock()
            .unwrap()
            .entry((group.to_string(), service.to_string()))
            .or_default()
            .restart_now(Instant::now());
        if !may {
            self.emit(EventBody::Infra {
                group: group.into(),
                message: format!(
                    "{service}: {} closes connections from this machine at once again; restarted lately, so not again (if a service cannot reach it, restart Docker Desktop)",
                    lost.join(", ")
                ),
            });
            return Ok(());
        }
        self.emit(EventBody::Infra {
            group: group.into(),
            message: format!(
                "{service}: {} accepted but closed at once from this machine (lost port forward); restarting the container",
                lost.join(", ")
            ),
        });
        let restarted = self.docker.restart(&self.compose_group(group), service);
        self.forwards_afresh(group, service);
        restarted.map_err(|e| format!("restart `{service}`: {e}"))?;
        let t0 = Instant::now();
        loop {
            self.refresh_containers();
            let still = lost_forwards(&self.container_ports(group, service), 1);
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
                    // Its node could not bring it up (#164): shown as a
                    // local group that failed, its containers beside.
                    if let crate::external::GroupPhase::Failed(e) = eg.phase() {
                        view.error = Some(e);
                    }
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
                                managed: true,
                            });
                        }
                    }
                }
                Err(e) => view.error = Some(e.clone()),
            }
            cache.views.push(view);
        }
        // What changed since the last read, as `refresh_containers` tells it
        // (the agent writes the file anew every few seconds).
        let changes: Vec<(String, String)> = {
            let before = self.infra.lock().unwrap();
            let shown = |c: &ContainerView| {
                if c.health.is_empty() {
                    c.state.clone()
                } else {
                    format!("{} ({})", c.state, c.health)
                }
            };
            let mut out = Vec::new();
            for v in &cache.views {
                let old = before.views.iter().find(|o| o.group == v.group);
                let Some(old) = old else { continue }; // the first read
                for c in &v.containers {
                    let was = old.containers.iter().find(|o| o.service == c.service);
                    if was.map(shown) != Some(shown(c)) {
                        out.push((v.group.clone(), format!("{}: {}", c.service, shown(c))));
                    }
                }
                for gone in old
                    .containers
                    .iter()
                    .filter(|o| !v.containers.iter().any(|c| c.service == o.service))
                {
                    out.push((v.group.clone(), format!("{}: removed", gone.service)));
                }
            }
            out
        };
        *self.infra.lock().unwrap() = cache;
        for (group, message) in changes {
            self.emit(EventBody::Infra { group, message });
        }
    }
}

/// How long a lane on a node waits for its agent to say anything new of
/// its groups: the agent gives a group's containers 300 s to be healthy
/// once started (`lanerun::HEALTHY_WITHIN`, as a local start does), and
/// starting them takes a while on top.
const NODE_GROUP_WAIT: Duration = Duration::from_secs(300 + 120);

/// An `Infra` event: the group, the message.
type GroupEvent = (String, String);

/// What a lane on a node was told of its groups so far, as its agent
/// brings them up (#164).
#[derive(Debug, Default)]
struct NodeBringUp {
    /// Each group said to start, and whether it was said ready.
    told: BTreeMap<String, bool>,
}

impl NodeBringUp {
    /// The events of what the agent's file says now, as a local start has
    /// them (`starting`, `ready`, `failed: …`), each once; and the outcome
    /// once every group is up (`Ok`) or one failed (the first in `order`).
    fn step(
        &mut self,
        order: &[String],
        ext: &crate::external::ExternalInfra,
    ) -> (Vec<GroupEvent>, Option<Result<(), String>>) {
        use crate::external::GroupPhase;
        let mut events = Vec::new();
        let mut failed = None;
        let mut all_up = true;
        for g in order {
            // A group the agent did not write (the stack file changed since
            // it started the lane) has nothing to wait for.
            let phase = ext.groups.get(g).map_or(GroupPhase::Up, |eg| eg.phase());
            if phase == GroupPhase::Created {
                all_up = false;
                continue;
            }
            if !self.told.contains_key(g) {
                self.told.insert(g.clone(), false);
                events.push((g.clone(), "starting".to_string()));
            }
            match phase {
                GroupPhase::Up => {
                    if !self.told[g] {
                        self.told.insert(g.clone(), true);
                        events.push((g.clone(), "ready".to_string()));
                    }
                }
                GroupPhase::Failed(e) => {
                    all_up = false;
                    if failed.is_none() {
                        events.push((g.clone(), format!("failed: {e}")));
                        failed = Some(Err(format!("compose group `{g}`: {e}")));
                    }
                }
                _ => all_up = false,
            }
        }
        let done = failed.or_else(|| all_up.then_some(Ok(())));
        (events, done)
    }

    /// The first group of `order` not said ready yet.
    fn pending(&self, order: &[String]) -> Option<String> {
        order
            .iter()
            .find(|g| self.told.get(*g) != Some(&true))
            .cloned()
    }
}

/// What a container route names (see [`Supervisor::container_target`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContainerTarget {
    /// In the group's `services`: started, stopped and restarted by devstack.
    Managed,
    /// A container of the group's project the group does not start (`name`:
    /// its container name, which an explicit `container_name:` sets).
    Unmanaged { name: String },
    /// Neither started by the group nor a container of its project.
    Unknown,
}

/// The routes key a container by its Compose service name, as `compose ps`
/// reports it (`Service`), never by its container name.
fn container_target<'a>(
    services: &[String],
    shown: impl IntoIterator<Item = &'a ContainerView>,
    service: &str,
) -> ContainerTarget {
    if services.iter().any(|s| s == service) {
        return ContainerTarget::Managed;
    }
    match shown.into_iter().find(|c| c.service == service) {
        Some(c) => ContainerTarget::Unmanaged {
            name: c.name.clone(),
        },
        None => ContainerTarget::Unknown,
    }
}

/// How long a container is ready before its published ports are judged:
/// Docker calls one without a health check ready once it runs, and until the
/// server inside listens, Docker Desktop's proxy accepts a connection and
/// closes it, just as for a lost forward (#98).
pub(crate) const FORWARD_GRACE: Duration = Duration::from_secs(20);
/// Probes that must all find a forward closed at once, [`FORWARD_GAP`] apart.
const FORWARD_TRIES: usize = 3;
const FORWARD_GAP: Duration = Duration::from_secs(1);
/// At most one restart of a container for a lost forward this often.
const FORWARD_RESTART_EVERY: Duration = Duration::from_secs(600);

/// Running, and healthy if it has a health check.
fn is_ready(c: &ContainerView) -> bool {
    c.state == "running" && (c.health.is_empty() || c.health == "healthy")
}

/// What is known of one Compose service's port forwards (#98).
#[derive(Debug, Default, Clone)]
pub(crate) struct ForwardCheck {
    /// Since when it is ready.
    ready_since: Option<Instant>,
    /// Its forwards were judged since it became ready.
    checked: bool,
    /// When it was last restarted for a lost forward.
    restarted: Option<Instant>,
}

impl ForwardCheck {
    /// `compose ps` saw it `ready` or not; `changed`: its state or health is
    /// not what it was.
    fn observe(&mut self, ready: bool, changed: bool, now: Instant) {
        if !ready {
            self.afresh();
        } else if changed || self.ready_since.is_none() {
            self.ready_since = Some(now);
            self.checked = false;
        }
    }

    /// It starts again: judged once it is ready again, for a while.
    fn afresh(&mut self) {
        self.ready_since = None;
        self.checked = false;
    }

    /// Ready long enough for a verdict on its forwards.
    fn settled(&self, now: Instant) -> bool {
        self.ready_since
            .is_some_and(|t| now.duration_since(t) >= FORWARD_GRACE)
    }

    /// A restart for a lost forward, unless there was one lately.
    fn restart_now(&mut self, now: Instant) -> bool {
        if self
            .restarted
            .is_some_and(|t| now.duration_since(t) < FORWARD_RESTART_EVERY)
        {
            return false;
        }
        self.restarted = Some(now);
        true
    }
}

/// The published ports (`5432->127.0.0.1:5432`) whose forward is lost: each
/// of `tries` probes found it accepted and closed at once. One that answers
/// once, or waits for the client, is not lost (#98).
fn lost_forwards(ports: &[String], tries: usize) -> Vec<String> {
    let mut lost: Vec<String> = ports.to_vec();
    for i in 0..tries.max(1) {
        if i > 0 {
            if lost.is_empty() {
                break;
            }
            std::thread::sleep(FORWARD_GAP);
        }
        lost.retain(|p| {
            p.split_once("->").is_some_and(|(_, addr)| {
                devstack_compose::probe(addr) == devstack_compose::Reach::ClosedAtOnce
            })
        });
    }
    lost
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_forward_is_lost_only_when_every_probe_says_so() {
        use std::net::TcpListener;
        // Closes the first connection at once, then holds them: a server
        // that came up between two probes (#98).
        let late = TcpListener::bind("127.0.0.1:0").unwrap();
        let late_port = late.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let mut held = Vec::new();
            for (i, c) in late.incoming().enumerate() {
                if i > 0 {
                    held.push(c);
                }
            }
        });
        // Closes every connection at once: a lost forward.
        let dead = TcpListener::bind("127.0.0.1:0").unwrap();
        let dead_port = dead.local_addr().unwrap().port();
        std::thread::spawn(move || for _c in dead.incoming() {});
        let ports = vec![
            format!("80->127.0.0.1:{late_port}"),
            format!("81->127.0.0.1:{dead_port}"),
        ];
        assert_eq!(lost_forwards(&ports, 3), vec![ports[1].clone()]);
    }

    #[test]
    fn a_forward_is_judged_once_the_container_is_ready_for_a_while() {
        let t0 = Instant::now();
        let mut f = ForwardCheck::default();
        f.observe(false, true, t0);
        assert!(!f.settled(t0 + FORWARD_GRACE), "not ready: no verdict");
        f.observe(true, true, t0);
        assert!(!f.settled(t0 + FORWARD_GRACE / 2), "just ready: wait");
        assert!(f.settled(t0 + FORWARD_GRACE));
        // Its health changed (a restart Docker reported): from the start.
        f.observe(true, true, t0 + FORWARD_GRACE);
        assert!(!f.settled(t0 + FORWARD_GRACE + FORWARD_GRACE / 2));
        // A restart from here clears it until the next `ps`.
        f.afresh();
        assert!(!f.settled(t0 + FORWARD_GRACE * 10));
    }

    #[test]
    fn a_container_is_restarted_for_a_lost_forward_once_in_a_while() {
        let t0 = Instant::now();
        let mut f = ForwardCheck::default();
        assert!(f.restart_now(t0));
        assert!(!f.restart_now(t0 + Duration::from_secs(60)));
        assert!(f.restart_now(t0 + FORWARD_RESTART_EVERY));
    }

    fn ext(json: &str) -> crate::external::ExternalInfra {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn a_lane_on_a_node_waits_for_its_groups_as_the_node_brings_them_up() {
        let order = ["db".to_string(), "app".to_string()];
        let mut f = NodeBringUp::default();
        let s = |f: &mut NodeBringUp, db: &str, app: &str| {
            f.step(
                &order,
                &ext(&format!(
                    r#"{{"groups":{{"db":{{"state":"{db}"}},"app":{{"state":"{app}"}}}}}}"#
                )),
            )
        };
        let ev = |v: &[(&str, &str)]| -> Vec<(String, String)> {
            v.iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect()
        };
        // Created: nothing to say, nothing done.
        assert_eq!(s(&mut f, "created", "created"), (Vec::new(), None));
        assert_eq!(
            s(&mut f, "starting", "created"),
            (ev(&[("db", "starting")]), None)
        );
        assert_eq!(s(&mut f, "starting", "created"), (Vec::new(), None), "once");
        assert_eq!(f.pending(&order).as_deref(), Some("db"));
        // `app` went from created to up between two reads: both said.
        assert_eq!(
            s(&mut f, "up", "up"),
            (
                ev(&[("db", "ready"), ("app", "starting"), ("app", "ready")]),
                Some(Ok(()))
            )
        );
        assert_eq!(f.pending(&order), None);
    }

    #[test]
    fn a_group_its_node_could_not_bring_up_fails_the_start() {
        let order = ["db".to_string(), "app".to_string()];
        let mut f = NodeBringUp::default();
        let (events, done) = f.step(
            &order,
            &ext(
                r#"{"groups":{"db":{"state":"failed","error":"container shop-db-1 is unhealthy"},
                    "app":{"state":"failed","error":"not started: compose group `db` failed"}}}"#,
            ),
        );
        assert_eq!(events[0], ("db".into(), "starting".into()));
        assert_eq!(
            events[1],
            (
                "db".into(),
                "failed: container shop-db-1 is unhealthy".into()
            )
        );
        assert!(
            !events.iter().any(|(_, m)| m.contains("not started")),
            "only the first failure is said: {events:?}"
        );
        assert_eq!(
            done,
            Some(Err(
                "compose group `db`: container shop-db-1 is unhealthy".into()
            ))
        );
    }

    #[test]
    fn an_older_nodes_file_is_up_at_once() {
        // Written after `up`, without a group's state (before #164).
        let order = ["db".to_string(), "tools".to_string()];
        let (events, done) = NodeBringUp::default().step(
            &order,
            &ext(r#"{"groups":{"db":{"project":"shop-ab12","services":{}}}}"#),
        );
        assert_eq!(done, Some(Ok(())));
        assert_eq!(events.len(), 4, "starting and ready, for each: {events:?}");
    }

    /// A container as `compose ps` reports it, through the parser.
    fn shown(line: &str, managed: bool) -> ContainerView {
        let c = devstack_compose::parse_ps(line).unwrap().remove(0);
        ContainerView {
            service: c.service,
            name: c.name,
            state: c.state,
            health: c.health,
            exit_code: c.exit_code,
            ports: Vec::new(),
            urls: Vec::new(),
            unreachable: Vec::new(),
            managed,
        }
    }

    const BLOBS: &str = r#"{"Service":"blobs","Name":"shop-blobs","State":"running","Health":"","ExitCode":0,"Publishers":[]}"#;

    #[test]
    fn an_explicit_container_name_is_addressed_by_its_service() {
        // `container_name: shop-blobs` on service `blobs`: the view keys it
        // by the service, and so does the route.
        let view = [shown(BLOBS, true)];
        assert_eq!(view[0].service, "blobs");
        let services = ["db".to_string(), "blobs".to_string()];
        assert_eq!(
            container_target(&services, &view, "blobs"),
            ContainerTarget::Managed
        );
        assert_eq!(
            container_target(&services, &view, "shop-blobs"),
            ContainerTarget::Unknown
        );
    }

    #[test]
    fn a_container_the_group_does_not_start_is_known_but_unmanaged() {
        // The finding: the project runs `blobs` (started by hand, a profile,
        // an earlier stack file), the group's `services` do not list it. The
        // view shows it; the route used to answer a bare 404.
        let view = [shown(BLOBS, false)];
        let services = ["db".to_string()];
        assert_eq!(
            container_target(&services, &view, "blobs"),
            ContainerTarget::Unmanaged {
                name: "shop-blobs".into()
            }
        );
        assert_eq!(
            container_target(&services, &view, "db"),
            ContainerTarget::Managed
        );
        assert_eq!(
            container_target(&services, &view, "cache"),
            ContainerTarget::Unknown
        );
    }
}
