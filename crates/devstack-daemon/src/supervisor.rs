//! The supervisor (phase 1): brings the infra plane up, builds services into
//! artifacts and runs them in the main slot in `after` order, watches their
//! processes and health, and stops them. Candidates and auto-promotion come in
//! phase 2 (ADR-0003); a rebuild here replaces the main slot by restart.

use crate::health::Checker;
use crate::state::{
    ContainerView, Event, EventBody, GroupView, InfraView, Persisted, ServiceView, StackView,
    Status, now_ms,
};
use devstack_build::{Builder, Layout, Outcome, Snapshot, build_id};
use devstack_compose::{Docker, Group};
use devstack_config::Stack;
use devstack_config::model::{Kind, Platform, Service};
use devstack_env::{ComposeEnvs, Context};
use devstack_proc::{Owned, Spec};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

pub(crate) struct Slot {
    pub view: ServiceView,
    tree: Option<Owned>,
    /// Bumped by every start and stop; a monitor of an older generation retires.
    generation: u64,
}

#[derive(Default)]
struct InfraCache {
    published: BTreeMap<(String, String, u16), String>,
    pending: BTreeSet<(String, String, u16)>,
    envs: ComposeEnvs,
    views: Vec<InfraView>,
}

pub struct Supervisor {
    pub stack: Stack,
    pub repo: PathBuf,
    pub state_dir: PathBuf,
    layout: Layout,
    builder: Builder,
    docker: Docker,
    proc_opts: devstack_proc::Options,
    health: Checker,
    slots: Mutex<BTreeMap<String, Slot>>,
    /// One operation per service at a time.
    op_locks: BTreeMap<String, Mutex<()>>,
    snapshot: Mutex<Snapshot>,
    infra: Mutex<InfraCache>,
    persisted: Mutex<Persisted>,
    seq: AtomicU64,
    pub events: broadcast::Sender<Event>,
}

impl Supervisor {
    pub fn new(
        stack: Stack,
        repo: PathBuf,
        state_dir: PathBuf,
        proc_opts: devstack_proc::Options,
    ) -> Result<Arc<Self>, String> {
        let layout = Layout::new(&state_dir);
        let builder = Builder::new(layout.clone(), &proc_opts).map_err(|e| e.to_string())?;
        let snapshot = Snapshot::open(&repo, &layout.lane("main")).map_err(|e| e.to_string())?;
        let groups_of = |name: &str| -> Vec<String> {
            stack
                .groups
                .iter()
                .filter(|(_, g)| g.members.iter().any(|m| m == name))
                .map(|(k, _)| k.clone())
                .collect()
        };
        let mut slots = BTreeMap::new();
        let mut op_locks = BTreeMap::new();
        for (name, svc) in &stack.services {
            slots.insert(
                name.clone(),
                Slot {
                    view: ServiceView {
                        name: name.clone(),
                        kind: format!("{:?}", svc.kind).to_lowercase(),
                        groups: groups_of(name),
                        port: svc.port,
                        autostart: svc.autostart,
                        status: Status::Stopped,
                        build_id: None,
                        pid: None,
                        since: now_ms(),
                        log: None,
                    },
                    tree: None,
                    generation: 0,
                },
            );
            op_locks.insert(name.clone(), Mutex::new(()));
        }
        let persisted = Persisted::load(&state_dir);
        // ADR-0016: kill what a crashed previous instance left (Linux; a no-op on Windows).
        devstack_proc::cleanup(&persisted.records);
        let (events, _) = broadcast::channel(1024);
        Ok(Arc::new(Self {
            stack,
            repo,
            state_dir,
            layout,
            builder,
            docker: Docker::default(),
            proc_opts,
            health: Checker::default(),
            slots: Mutex::new(slots),
            op_locks,
            snapshot: Mutex::new(snapshot),
            infra: Mutex::new(InfraCache::default()),
            persisted: Mutex::new(Persisted {
                main: persisted.main,
                records: Vec::new(),
            }),
            seq: AtomicU64::new(0),
            events,
        }))
    }

    pub(crate) fn emit(&self, body: EventBody) {
        let _ = self.events.send(Event {
            seq: self.seq.fetch_add(1, Ordering::SeqCst),
            at: now_ms(),
            body,
        });
    }

    fn set_status(&self, name: &str, status: Status) {
        let build_id = {
            let mut slots = self.slots.lock().unwrap();
            let Some(slot) = slots.get_mut(name) else {
                return;
            };
            if slot.view.status == status {
                return;
            }
            slot.view.status = status.clone();
            slot.view.since = now_ms();
            slot.view.build_id.clone()
        };
        self.emit(EventBody::Service {
            service: name.into(),
            status,
            build_id,
        });
    }

    pub fn view(&self) -> StackView {
        let services = self
            .slots
            .lock()
            .unwrap()
            .values()
            .map(|s| s.view.clone())
            .collect();
        let infra = self.infra.lock().unwrap().views.clone();
        let groups = self
            .stack
            .groups
            .iter()
            .map(|(k, g)| GroupView {
                name: k.clone(),
                label: g.label.clone().unwrap_or_else(|| k.clone()),
                order: g.order,
                members: g.members.clone(),
            })
            .collect();
        StackView {
            name: self.stack.stack.name.clone(),
            repo: self.repo.clone(),
            services,
            infra,
            groups,
        }
    }

    pub(crate) fn proc_options(&self) -> &devstack_proc::Options {
        &self.proc_opts
    }

    pub fn service_log(&self, name: &str) -> Option<PathBuf> {
        self.slots
            .lock()
            .unwrap()
            .get(name)
            .and_then(|s| s.view.log.clone())
    }

    // -------------------------------------------------------------- infra

    fn compose_group(&self, name: &str) -> Group {
        Group::from_config(name, &self.stack.compose[name], &self.repo)
    }

    /// Compose groups in `after` order.
    fn group_order(&self) -> Vec<String> {
        topo(
            self.stack
                .compose
                .iter()
                .map(|(k, g)| (k.clone(), g.after.clone()))
                .collect(),
        )
    }

    pub fn infra_up(&self) -> Result<(), String> {
        for g in self.group_order() {
            self.emit(EventBody::Infra {
                group: g.clone(),
                message: "starting".into(),
            });
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

    /// Re-read Compose: environments of every service (also parked ones) and
    /// published addresses of running containers.
    pub fn refresh_infra(&self) {
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
                        let mut ports = Vec::new();
                        for p in &c.publishers {
                            if let Some(addr) = c.published(p.target_port) {
                                cache.published.insert(
                                    (g.clone(), c.service.clone(), p.target_port),
                                    addr.clone(),
                                );
                                let s = format!("{}->{addr}", p.target_port);
                                if !ports.contains(&s) {
                                    ports.push(s);
                                }
                            }
                        }
                        view.containers.push(ContainerView {
                            service: c.service,
                            state: c.state,
                            health: c.health,
                            ports,
                        });
                    }
                }
                Err(e) => view.error = Some(e.to_string()),
            }
            cache.views.push(view);
        }
        *self.infra.lock().unwrap() = cache;
    }

    // ----------------------------------------------------------- services

    fn service(&self, name: &str) -> Result<&Service, String> {
        self.stack
            .services
            .get(name)
            .ok_or_else(|| format!("no service `{name}`"))
    }

    /// Services in `after` order (Compose dependencies are handled by `infra_up`).
    pub fn service_order(&self) -> Vec<String> {
        topo(
            self.stack
                .services
                .iter()
                .map(|(k, s)| {
                    (
                        k.clone(),
                        s.after
                            .iter()
                            .filter(|a| !a.starts_with("compose."))
                            .cloned()
                            .collect(),
                    )
                })
                .collect(),
        )
    }

    /// Infra, then every autostart service for this platform, in order. Stops at
    /// the first service that does not become ready.
    pub fn up(self: &Arc<Self>) -> Result<(), String> {
        self.infra_up()?;
        for name in self.service_order() {
            let svc = self.service(&name)?;
            if !svc.autostart || !runs_here(svc) {
                continue;
            }
            self.start(&name)?;
        }
        Ok(())
    }

    /// Stop every service, last started first. Infra stays up (ADR-0004: it is
    /// long-lived) unless `infra` is set.
    pub fn down(&self, infra: bool) {
        for name in self.service_order().into_iter().rev() {
            let _ = self.stop(&name);
        }
        if infra {
            for g in self.group_order().into_iter().rev() {
                let _ = self.docker.down(&self.compose_group(&g));
            }
            self.refresh_infra();
        }
    }

    /// Build if needed, then start and wait until ready (jobs: until done).
    pub fn start(self: &Arc<Self>, name: &str) -> Result<(), String> {
        let svc = self.service(name)?.clone();
        if !runs_here(&svc) {
            return Err(format!("`{name}` does not run on this platform"));
        }
        let _op = self.op_locks[name].lock().unwrap();
        if self.slots.lock().unwrap()[name].view.status.is_active() {
            return Ok(());
        }
        let result = self.start_locked(name, &svc);
        if let Err(e) = &result {
            self.set_status(name, Status::Failed { reason: e.clone() });
        }
        result
    }

    fn start_locked(self: &Arc<Self>, name: &str, svc: &Service) -> Result<(), String> {
        // 1. Build into an artifact (services and jobs with `build`).
        let artifact = if svc.build.is_some() {
            Some(self.build_locked(name, svc)?)
        } else {
            None
        };

        // 2. Port free?
        if let Some(port) = svc.port
            && std::net::TcpListener::bind(("127.0.0.1", port)).is_err()
        {
            return Err(format!("port {port} is already in use by another process"));
        }

        // 3. Environment (ADR-0014) and command line.
        let ctx = self.context(svc, artifact.as_ref().map(|(_, d)| d.as_path()));
        let env = {
            let infra = self.infra.lock().unwrap();
            devstack_env::compose(name, svc, &ctx, &infra.envs, true).map_err(|e| e.to_string())?
        };
        let lookup = |k: &str| ctx.vars.get(k).cloned();
        let command = devstack_config::interpolate::expand(&svc.run, &ctx, &lookup)
            .map_err(|e| format!("run: {e}"))?;
        for d in &svc.ensure_dirs {
            let dir = devstack_config::interpolate::expand(d, &ctx, &lookup)
                .map_err(|e| format!("ensure_dirs: {e}"))?;
            std::fs::create_dir_all(&dir).map_err(|e| format!("create {dir}: {e}"))?;
        }
        let cwd = match (&artifact, &svc.cwd) {
            // Built services run from their artifact (ADR-0002); its files are their content root.
            (Some((_, dir)), _) => dir.clone(),
            (None, Some(c)) => self.repo.join(c),
            (None, None) => self.repo.clone(),
        };
        let mut env_map: BTreeMap<String, String> =
            env.into_iter().map(|(k, v)| (k, v.value)).collect();
        if let Some((id, _)) = &artifact {
            env_map.insert("DEVSTACK_BUILD_ID".into(), id.clone());
        }

        // 4. Start in an owned tree (ADR-0009).
        let log = self
            .state_dir
            .join("logs")
            .join(name)
            .join(format!("run-{}.log", now_ms()));
        std::fs::create_dir_all(log.parent().unwrap()).map_err(|e| e.to_string())?;
        let file = std::fs::File::create(&log).map_err(|e| e.to_string())?;
        let spec = Spec {
            name: name.into(),
            command,
            cwd,
            env: env_map,
            log: file,
        };
        let (tree, record) =
            devstack_proc::spawn(spec, &self.proc_opts).map_err(|e| e.to_string())?;
        let generation = {
            let mut slots = self.slots.lock().unwrap();
            let slot = slots.get_mut(name).unwrap();
            slot.generation += 1;
            slot.view.pid = Some(tree.root_pid());
            slot.view.log = Some(log.clone());
            slot.view.build_id = artifact.as_ref().map(|(id, _)| id.clone());
            slot.tree = Some(tree);
            slot.generation
        };
        {
            let mut p = self.persisted.lock().unwrap();
            p.records.retain(|r| r.name != name);
            p.records.push(record);
            if let Some((id, _)) = &artifact {
                p.main.insert(name.into(), id.clone());
            }
            p.save(&self.state_dir);
        }
        self.set_status(name, Status::Starting);

        // 5. Ready: job finished, health passed, or simply running.
        let ready = self.wait_ready(name, svc, generation, &log);
        if ready.is_ok() {
            self.spawn_monitor(name.to_string(), svc.clone(), generation);
        }
        ready
    }

    /// Sync the main lane and build `name` unless its artifact exists.
    fn build_locked(&self, name: &str, svc: &Service) -> Result<(String, PathBuf), String> {
        self.set_status(name, Status::Building { build_id: None });
        let mut snap = self.snapshot.lock().unwrap();
        snap.sync().map_err(|e| e.to_string())?;
        let id = build_id(&snap.manifest, svc).map_err(|e| e.to_string())?;
        if self.builder.artifact(name, &id).is_none() {
            self.set_status(
                name,
                Status::Building {
                    build_id: Some(id.clone()),
                },
            );
            let mut running = self
                .builder
                .start("main", &snap, name, svc)
                .map_err(|e| e.to_string())?;
            drop(snap); // other services may sync while this one builds
            match self.builder.wait(&mut running).map_err(|e| e.to_string())? {
                Outcome::Published { .. } => {}
                Outcome::Failed { exit_code, log, .. } => {
                    // Point compiler messages at the repository, not at the
                    // snapshot, so they stay clickable in the IDE.
                    let snap_dir = self.layout.lane("main").join("src");
                    let text = tail(&log, 15).replace(
                        &snap_dir.to_string_lossy().into_owned(),
                        &self.repo.to_string_lossy(),
                    );
                    return Err(format!("build failed with exit code {exit_code}\n{text}"));
                }
                Outcome::Cancelled { .. } => return Err("build cancelled".into()),
            }
        }
        Ok((id.clone(), self.layout.artifact(name, &id)))
    }

    fn context(&self, svc: &Service, artifact: Option<&Path>) -> Context {
        let infra = self.infra.lock().unwrap();
        let mut ctx = Context {
            repo: self.repo.clone(),
            artifact: artifact.map(Path::to_path_buf),
            port: svc.port,
            is_main: true,
            published: infra.published.clone(),
            pending: infra.pending.clone(),
            ..Default::default()
        };
        for (n, s) in &self.stack.services {
            if let Some(p) = s.port {
                let tls = s.health.as_ref().is_some_and(|h| h.insecure_tls);
                ctx.service_urls.insert(
                    n.clone(),
                    format!("{}://127.0.0.1:{p}", if tls { "https" } else { "http" }),
                );
            }
        }
        if let Some(r) = &svc.compose
            && let Some(g) = self.stack.compose.get(&r.group)
        {
            for f in &g.env_files {
                if let Ok(text) = std::fs::read_to_string(self.repo.join(f)) {
                    ctx.vars.extend(devstack_env::parse_env_file(&text));
                }
            }
        }
        ctx.vars.extend(std::env::vars());
        ctx
    }

    fn wait_ready(
        &self,
        name: &str,
        svc: &Service,
        generation: u64,
        log: &Path,
    ) -> Result<(), String> {
        let exited = |s: &Self| -> Option<i32> {
            let mut slots = s.slots.lock().unwrap();
            let slot = slots.get_mut(name)?;
            if slot.generation != generation {
                return Some(-1);
            }
            slot.tree.as_mut().and_then(|t| t.try_exit_code())
        };
        match svc.kind {
            Kind::Job => loop {
                if let Some(code) = exited(self) {
                    if code == 0 {
                        self.set_status(name, Status::Completed { exit_code: 0 });
                        return Ok(());
                    }
                    return Err(format!("job exited with code {code}\n{}", tail(log, 15)));
                }
                std::thread::sleep(Duration::from_millis(100));
            },
            _ => match &svc.health {
                None => {
                    // Give it a moment to fail fast, e.g. on a wrong command line.
                    std::thread::sleep(Duration::from_millis(500));
                    if let Some(code) = exited(self) {
                        return Err(format!(
                            "exited with code {code} right after start\n{}",
                            tail(log, 15)
                        ));
                    }
                    self.set_status(name, Status::Running);
                    Ok(())
                }
                Some(h) => {
                    let t0 = Instant::now();
                    let port = svc.port.expect("validated: health needs a port");
                    let mut last = String::new();
                    while t0.elapsed() < h.timeout {
                        if let Some(code) = exited(self) {
                            return Err(format!(
                                "exited with code {code} before it became healthy\n{}",
                                tail(log, 15)
                            ));
                        }
                        match self
                            .health
                            .check(port, &h.http, h.insecure_tls, h.insecure_tls)
                        {
                            Ok(()) => {
                                self.set_status(name, Status::Healthy);
                                return Ok(());
                            }
                            Err(e) => last = e,
                        }
                        std::thread::sleep(Duration::from_millis(300));
                    }
                    Err(format!(
                        "not healthy after {} s: {last}",
                        h.timeout.as_secs()
                    ))
                }
            },
        }
    }

    /// Watch a started instance: exit and health changes become events.
    fn spawn_monitor(self: &Arc<Self>, name: String, svc: Service, generation: u64) {
        let me = Arc::clone(self);
        let _ = std::thread::Builder::new()
            .name(format!("monitor-{name}"))
            .spawn(move || {
                let mut last_health = Instant::now();
                loop {
                    std::thread::sleep(Duration::from_millis(500));
                    let exit = {
                        let mut slots = me.slots.lock().unwrap();
                        let Some(slot) = slots.get_mut(&name) else {
                            return;
                        };
                        if slot.generation != generation {
                            return;
                        }
                        slot.tree.as_mut().and_then(|t| t.try_exit_code())
                    };
                    if let Some(code) = exit {
                        // Let the rest of the tree go too.
                        if let Some(mut t) = me
                            .slots
                            .lock()
                            .unwrap()
                            .get_mut(&name)
                            .and_then(|s| s.tree.take())
                        {
                            t.kill();
                        }
                        me.set_status(&name, Status::Exited { exit_code: code });
                        return;
                    }
                    if let (Some(h), Some(port)) = (&svc.health, svc.port)
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

    /// Graceful stop, then kill (ADR-0009).
    pub fn stop(&self, name: &str) -> Result<(), String> {
        self.service(name)?;
        let _op = self.op_locks[name].lock().unwrap();
        let tree = {
            let mut slots = self.slots.lock().unwrap();
            let slot = slots.get_mut(name).unwrap();
            slot.generation += 1;
            slot.view.pid = None;
            slot.tree.take()
        };
        if let Some(mut t) = tree {
            t.stop(Duration::from_secs(20));
        }
        let mut p = self.persisted.lock().unwrap();
        p.records.retain(|r| r.name != name);
        p.save(&self.state_dir);
        drop(p);
        self.set_status(name, Status::Stopped);
        Ok(())
    }

    /// Build first; only a successful build replaces the running instance (the
    /// rule of ADR-0003, ahead of candidates in phase 2). A failed build leaves
    /// the running instance as it was.
    pub fn restart(self: &Arc<Self>, name: &str) -> Result<(), String> {
        let svc = self.service(name)?.clone();
        if svc.build.is_some() {
            let _op = self.op_locks[name].lock().unwrap();
            let before = self.slots.lock().unwrap()[name].view.status.clone();
            if let Err(e) = self.build_locked(name, &svc) {
                // Back to what is actually running; the error goes to the caller.
                self.set_status(name, before);
                return Err(format!(
                    "{e}
The running instance was left as it was."
                ));
            }
        }
        self.stop(name)?;
        self.start(name)
    }
}

fn runs_here(svc: &Service) -> bool {
    svc.platforms.is_empty() || svc.platforms.contains(&Platform::current())
}

fn tail(path: &Path, n: usize) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().rev().take(n).collect();
    lines.into_iter().rev().collect::<Vec<_>>().join("\n")
}

/// Topological order (dependencies first), stable by name. The stack file is
/// validated to be acyclic; a cycle here would only leave nodes out.
fn topo(deps: BTreeMap<String, Vec<String>>) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    fn visit(
        n: &str,
        deps: &BTreeMap<String, Vec<String>>,
        seen: &mut BTreeSet<String>,
        out: &mut Vec<String>,
    ) {
        if !seen.insert(n.to_string()) {
            return;
        }
        for d in deps.get(n).into_iter().flatten() {
            if deps.contains_key(d) {
                visit(d, deps, seen, out);
            }
        }
        out.push(n.to_string());
    }
    for n in deps.keys() {
        visit(n, &deps, &mut seen, &mut out);
    }
    out
}
