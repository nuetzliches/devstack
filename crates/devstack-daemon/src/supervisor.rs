//! The supervisor: brings the infra plane up, builds services into artifacts
//! and runs them in the main slot in `after` order, watches their processes
//! and health, and stops them. A service behind the proxy (ADR-0010) runs on an
//! internal port, and a rebuild replaces it through a candidate without
//! dropping a request (ADR-0003); without the proxy it restarts in place.

use crate::health::Checker;
use crate::proxy::{Proxy, Upstream, Waiting};
use crate::state::{
    Candidate, CommandView, ContainerView, Event, EventBody, GroupView, InfraView, Persisted,
    ServiceView, StackView, Status, now_ms,
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

/// One running process of a service.
struct Instance {
    tree: Owned,
    /// The port it listens on (internal behind the proxy), if it has one.
    port: Option<u16>,
    build_id: Option<String>,
    log: PathBuf,
    /// Its cleanup record's name (ADR-0016), `<service>@<port>`.
    record: String,
}

pub(crate) struct Slot {
    pub view: ServiceView,
    main: Option<Instance>,
    /// The build main ran before its last replacement: `rollback` (plan §3.2).
    previous: Option<String>,
    /// Bumped by every start, stop and switch; a monitor of an older generation retires.
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
    /// Runs the proxies (ADR-0010).
    rt: tokio::runtime::Handle,
    proxies: Mutex<BTreeMap<String, Proxy>>,
    /// Changes seen by the watcher, per service, not yet in a candidate.
    changes: Mutex<BTreeMap<String, Changes>>,
    /// Built candidates waiting for `promote` (ADR-0013), per service.
    held: Mutex<BTreeMap<String, (String, PathBuf)>>,
    /// Debounced restarts (dev servers, containers): last change, worker running.
    quiet: Mutex<BTreeMap<String, (Instant, bool)>>,
    /// The stack file this daemon runs from, and a hash of what it read at start.
    /// The daemon does not reload it: a change waits for the next start, and the
    /// status says so instead of keeping quiet.
    stack_file: PathBuf,
    stack_file_hash: Option<blake3::Hash>,
    config_changed: std::sync::atomic::AtomicBool,
}

/// Hash of the stack file, line endings ignored (a checkout may flip them).
fn stack_file_hash(path: &Path) -> Option<blake3::Hash> {
    let bytes = std::fs::read(path).ok()?;
    let normalized: Vec<u8> = bytes.into_iter().filter(|b| *b != b'\r').collect();
    Some(blake3::hash(&normalized))
}

/// What the watcher saw for one service since its last candidate.
#[derive(Default)]
struct Changes {
    paths: BTreeSet<String>,
    last: Option<Instant>,
    /// Bumped by every change; a build of an older epoch is superseded.
    epoch: u64,
    /// A pipeline thread runs for the service.
    worker: bool,
    /// The epoch a manual restart covered: changes up to it need no candidate.
    absorbed: u64,
}

impl Supervisor {
    pub fn new(
        stack: Stack,
        stack_file: PathBuf,
        repo: PathBuf,
        state_dir: PathBuf,
        proc_opts: devstack_proc::Options,
        rt: tokio::runtime::Handle,
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
                        url: svc
                            .port
                            .map(|p| format!("{}://localhost:{p}", svc.scheme())),
                        autostart: svc.autostart,
                        status: Status::Stopped,
                        build_id: None,
                        pid: None,
                        since: now_ms(),
                        log: None,
                        instance_port: None,
                        proxy: None,
                        candidate: None,
                    },
                    main: None,
                    previous: None,
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
            rt,
            proxies: Mutex::new(BTreeMap::new()),
            changes: Mutex::new(BTreeMap::new()),
            held: Mutex::new(BTreeMap::new()),
            quiet: Mutex::new(BTreeMap::new()),
            stack_file_hash: stack_file_hash(&stack_file),
            stack_file,
            config_changed: std::sync::atomic::AtomicBool::new(false),
        }))
    }

    pub fn stack_file(&self) -> &Path {
        &self.stack_file
    }

    /// The watcher saw the stack file change: compare with what this daemon
    /// read at start and tell the clients when the answer flips.
    pub fn stack_file_touched(&self) {
        let changed = stack_file_hash(&self.stack_file) != self.stack_file_hash;
        if self.config_changed.swap(changed, Ordering::SeqCst) != changed {
            self.emit(EventBody::Config { changed });
        }
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

    fn set_candidate(&self, name: &str, candidate: Option<Candidate>) {
        if let Some(slot) = self.slots.lock().unwrap().get_mut(name) {
            if slot.view.candidate == candidate {
                return;
            }
            slot.view.candidate = candidate.clone();
        }
        self.emit(EventBody::Candidate {
            service: name.into(),
            candidate,
        });
    }

    pub fn view(&self) -> StackView {
        let proxies = self.proxies.lock().unwrap().clone();
        let services = self
            .slots
            .lock()
            .unwrap()
            .values()
            .map(|s| {
                let mut v = s.view.clone();
                v.proxy = proxies.get(&v.name).map(Proxy::view);
                v
            })
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
        let commands = self
            .stack
            .commands
            .iter()
            .map(|(k, c)| CommandView {
                name: k.clone(),
                description: c.description.clone(),
                confirm: c.confirm.clone(),
                requires: c.requires.clone(),
            })
            .collect();
        StackView {
            name: self.stack.stack.name.clone(),
            repo: self.repo.clone(),
            version: devstack_config::VERSION.into(),
            services,
            infra,
            groups,
            commands,
            config_changed: self.config_changed.load(Ordering::SeqCst),
        }
    }

    /// Usage of every running service tree (plan §12).
    pub(crate) fn tree_usage(&self) -> Vec<(String, devstack_proc::Usage)> {
        self.slots
            .lock()
            .unwrap()
            .iter_mut()
            .filter_map(|(name, slot)| slot.main.as_mut().map(|i| (name.clone(), i.tree.usage())))
            .collect()
    }

    pub(crate) fn build_usage(&self) -> devstack_proc::Usage {
        self.builder.usage()
    }

    pub(crate) fn docker(&self) -> &Docker {
        &self.docker
    }

    pub(crate) fn layout(&self) -> &Layout {
        &self.layout
    }

    pub(crate) fn proc_options(&self) -> &devstack_proc::Options {
        &self.proc_opts
    }

    /// The newest build log of `name` (last 3000 lines), compiler paths
    /// mapped from the snapshot to the repository.
    pub fn latest_build_log(&self, name: &str) -> Option<String> {
        let dir = self.state_dir.join("logs").join(name);
        let newest = std::fs::read_dir(dir)
            .ok()?
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("build-"))
            .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())?;
        let text = tail(&newest.path(), 3000);
        let snap_dir = self.layout.lane("main").join("src");
        Some(text.replace(
            &snap_dir.to_string_lossy().into_owned(),
            &self.repo.to_string_lossy(),
        ))
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

    /// Re-read only the containers' states (`compose ps`, no `config`): cheap
    /// enough to run every few seconds, so a container that stopped or turned
    /// unhealthy shows as such. Every change is an event.
    pub fn refresh_containers(&self) {
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
                    next.push(ContainerView {
                        service: c.service,
                        name: c.name,
                        state: c.state,
                        health: c.health,
                        ports,
                    });
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
                            name: c.name,
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
        // A failed service holds back only what comes `after` it; everything
        // independent of it still starts (one broken build must not keep the
        // rest of the stack down).
        let mut failed: BTreeSet<String> = BTreeSet::new();
        for name in self.service_order() {
            let svc = self.service(&name)?;
            if !svc.autostart || !runs_here(svc) {
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
        if let Err(e) = self.ensure_after(&svc) {
            self.set_status(name, Status::Failed { reason: e.clone() });
            return Err(e);
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

    /// The proxy on the service's fixed port, bound on first use (ADR-0010).
    fn ensure_proxy(&self, name: &str, svc: &Service) -> Result<Proxy, String> {
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

    fn proxy(&self, name: &str) -> Option<Proxy> {
        self.proxies.lock().unwrap().get(name).cloned()
    }

    fn start_locked(self: &Arc<Self>, name: &str, svc: &Service) -> Result<(), String> {
        // 1. Build into an artifact (services and jobs with `build`).
        let artifact = if svc.build.is_some() {
            Some(self.build_locked(name, svc, &|| false)?)
        } else {
            None
        };

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
        let mut inst = match self.launch(name, svc, artifact.as_ref(), port, true) {
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
    }

    /// Start one process of `svc` on `port`; `is_main` decides its worker
    /// switches (ADR-0008). Its cleanup record is persisted (ADR-0016).
    fn launch(
        &self,
        name: &str,
        svc: &Service,
        artifact: Option<&(String, PathBuf)>,
        port: Option<u16>,
        is_main: bool,
    ) -> Result<Instance, String> {
        let ctx = self.context(svc, artifact.map(|(_, d)| d.as_path()), port, is_main);
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
        let cwd = match (artifact, &svc.cwd) {
            // Built services run from their artifact (ADR-0002); its files are their content root.
            (Some((_, dir)), _) => dir.clone(),
            (None, Some(c)) => self.repo.join(c),
            (None, None) => self.repo.clone(),
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
        let log = self.state_dir.join("logs").join(name).join(format!(
            "run-{}-{}.log",
            now_ms(),
            port.unwrap_or(0)
        ));
        std::fs::create_dir_all(log.parent().unwrap()).map_err(|e| e.to_string())?;
        let file = std::fs::File::create(&log).map_err(|e| e.to_string())?;
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
        })
    }

    fn forget_record(&self, record: &str) {
        let mut p = self.persisted.lock().unwrap();
        p.records.retain(|r| r.name != record);
        p.save(&self.state_dir);
    }

    /// Sync the main lane and build `name` unless its artifact exists.
    fn build_locked(
        &self,
        name: &str,
        svc: &Service,
        cancel: &dyn Fn() -> bool,
    ) -> Result<(String, PathBuf), String> {
        // While an instance serves, the build is the candidate's business: the
        // service itself stays "healthy", it still answers (ADR-0003).
        let serving = self.slots.lock().unwrap()[name].main.is_some();
        let building = |id: Option<String>| {
            if serving {
                self.set_candidate(name, Some(Candidate::new("building", id, None)));
            } else {
                self.set_status(name, Status::Building { build_id: id });
            }
        };
        building(None);
        let mut snap = self.snapshot.lock().unwrap();
        snap.sync().map_err(|e| e.to_string())?;
        let id = build_id(&snap.manifest, svc).map_err(|e| e.to_string())?;
        if self.builder.artifact(name, &id).is_none() {
            building(Some(id.clone()));
            let mut running = self
                .builder
                .start("main", &snap, name, svc)
                .map_err(|e| e.to_string())?;
            drop(snap); // other services may sync while this one builds
            // A newer change supersedes the build: kill it (ADR-0003, spike b).
            let outcome = loop {
                if let Some(o) = self.builder.poll(&mut running).map_err(|e| e.to_string())? {
                    break o;
                }
                if cancel() {
                    self.builder.cancel(&mut running);
                    return Err(SUPERSEDED.into());
                }
                std::thread::sleep(Duration::from_millis(100));
            };
            match outcome {
                Outcome::Published { .. } => {}
                Outcome::Failed { exit_code, log, .. } => {
                    // Point compiler messages at the repository, not at the
                    // snapshot, so they stay clickable in the IDE.
                    let snap_dir = self.layout.lane("main").join("src");
                    let text = build_errors(&log).replace(
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

    fn context(
        &self,
        svc: &Service,
        artifact: Option<&Path>,
        port: Option<u16>,
        is_main: bool,
    ) -> Context {
        let infra = self.infra.lock().unwrap();
        let mut ctx = Context {
            repo: self.repo.clone(),
            artifact: artifact.map(Path::to_path_buf),
            port,
            is_main,
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
    /// status to show.
    fn wait_instance(&self, svc: &Service, inst: &mut Instance) -> Result<Status, String> {
        match svc.kind {
            Kind::Job => loop {
                if let Some(code) = inst.tree.try_exit_code() {
                    if code == 0 {
                        return Ok(Status::Completed { exit_code: 0 });
                    }
                    return Err(format!(
                        "job exited with code {code}\n{}",
                        tail(&inst.log, 15)
                    ));
                }
                std::thread::sleep(Duration::from_millis(100));
            },
            _ => match (&svc.health, inst.port) {
                (Some(h), Some(port)) => {
                    let t0 = Instant::now();
                    let mut last = String::new();
                    while t0.elapsed() < h.timeout {
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
                    std::thread::sleep(Duration::from_millis(500));
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
    fn spawn_monitor(self: &Arc<Self>, name: String, svc: Service, generation: u64) {
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
                            slot.view.pid = None;
                            slot.main.take()
                        };
                        if let Some(mut i) = inst {
                            i.tree.kill();
                            me.forget_record(&i.record);
                        }
                        if let Some(p) = me.proxy(&name) {
                            p.set_upstream(None);
                            p.set_waiting(Waiting::Refuse(format!(
                                "`{name}` exited with code {code} (devstack logs {name})"
                            )));
                        }
                        me.set_status(&name, Status::Exited { exit_code: code });
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
    pub fn stop(&self, name: &str) -> Result<(), String> {
        self.service(name)?;
        let _op = self.op_locks[name].lock().unwrap();
        let inst = {
            let mut slots = self.slots.lock().unwrap();
            let slot = slots.get_mut(name).unwrap();
            slot.generation += 1;
            slot.view.pid = None;
            slot.view.instance_port = None;
            slot.main.take()
        };
        if let Some(p) = self.proxy(name) {
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

    /// Build first; only a successful build replaces the running instance
    /// (ADR-0003). Behind the proxy the replacement is a candidate that takes
    /// over without dropping a request; otherwise a restart in place.
    pub fn restart(self: &Arc<Self>, name: &str) -> Result<(), String> {
        let svc = self.service(name)?.clone();
        self.absorb(name);
        let mut artifact = None;
        if svc.build.is_some() {
            let _op = self.op_locks[name].lock().unwrap();
            let before = self.slots.lock().unwrap()[name].view.status.clone();
            match self.build_locked(name, &svc, &|| false) {
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
        // Jobs this service comes after run again first (a schema migrator for a
        // migration that came with the change); their build is cached when
        // nothing changed. A failing job leaves the running instance alone.
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
        let running = self.slots.lock().unwrap()[name].main.is_some();
        if svc.proxied() && running {
            return self.replace(name, &svc, artifact);
        }
        self.set_candidate(name, None);
        self.stop(name)?;
        self.start(name)
    }

    /// Replace the running main instance behind the proxy (ADR-0003, ADR-0010):
    ///
    /// 1. the candidate starts on a free port without workers (ADR-0008) and
    ///    must pass its health check; if not, it goes and main stays;
    /// 2. the proxy switches to it; the old instance drains and stops;
    /// 3. a service with worker switches gets the same artifact once more with
    ///    workers on, which takes over the same way ("relay"): requests never
    ///    wait, background work pauses for one start-up.
    fn replace(
        self: &Arc<Self>,
        name: &str,
        svc: &Service,
        artifact: Option<(String, PathBuf)>,
    ) -> Result<(), String> {
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
        let mut candidate =
            match self.launch(name, svc, artifact.as_ref(), Some(free_port()?), !relay) {
                Ok(i) => i,
                Err(e) => return fail(e),
            };
        if let Err(e) = self.wait_instance(svc, &mut candidate) {
            candidate.tree.kill();
            self.forget_record(&candidate.record);
            return fail(e);
        }

        self.set_candidate(name, Some(Candidate::new("switching", build.clone(), None)));
        let old = self.switch_to(name, &proxy, candidate);
        self.retire(&proxy, old);

        if relay {
            let mut fin = match self.launch(name, svc, artifact.as_ref(), Some(free_port()?), true)
            {
                Ok(i) => i,
                Err(e) => return self.running_without_workers(name, svc, e),
            };
            if let Err(e) = self.wait_instance(svc, &mut fin) {
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

    // ------------------------------------------- candidate pipeline (ADR-0003)

    /// The watcher saw `path` change, and `name` watches it. Debounced: a
    /// pipeline thread builds a candidate once the tree has been quiet for a
    /// moment; a newer change kills a build in progress and starts over.
    pub fn changed(self: &Arc<Self>, name: &str, path: &str) {
        let spawn = {
            let mut changes = self.changes.lock().unwrap();
            let c = changes.entry(name.to_string()).or_default();
            c.paths.insert(path.to_string());
            c.last = Some(Instant::now());
            c.epoch += 1;
            !std::mem::replace(&mut c.worker, true)
        };
        let (serving, state) = {
            let slots = self.slots.lock().unwrap();
            let v = &slots[name].view;
            (
                slots[name].main.is_some(),
                v.candidate.as_ref().map(|c| c.state.clone()),
            )
        };
        // A starting or switching candidate finishes first; the change follows.
        if serving && !matches!(state.as_deref(), Some("starting" | "switching")) {
            self.set_candidate(
                name,
                Some(Candidate::new("changed", None, Some(path.into()))),
            );
            if let Some(p) = self.proxy(name) {
                p.set_pending(Some("change detected".into()));
            }
        }
        if spawn {
            let (me, n) = (Arc::clone(self), name.to_string());
            let _ = std::thread::Builder::new()
                .name(format!("pipeline-{n}"))
                .spawn(move || me.pipeline(&n));
        }
    }

    fn pipeline(self: &Arc<Self>, name: &str) {
        loop {
            let (paths, epoch) = loop {
                {
                    let mut changes = self.changes.lock().unwrap();
                    let c = changes.get_mut(name).expect("changed() created it");
                    if c.last.is_some_and(|t| t.elapsed() >= DEBOUNCE) {
                        if c.absorbed >= c.epoch {
                            // A manual restart already took these changes in.
                            c.paths.clear();
                            c.worker = false;
                            c.last = None;
                            drop(changes);
                            self.clear_changed(name);
                            return;
                        }
                        break (std::mem::take(&mut c.paths), c.epoch);
                    }
                }
                std::thread::sleep(Duration::from_millis(200));
            };
            let superseded = self.candidate_from(name, &paths, epoch);
            let mut changes = self.changes.lock().unwrap();
            let c = changes.get_mut(name).expect("changed() created it");
            if superseded {
                c.paths.extend(paths); // the next candidate covers them too
            }
            if c.epoch == epoch {
                c.worker = false;
                c.last = None;
                return;
            }
        }
    }

    /// Build a candidate from the current tree and promote it unless a gate
    /// holds it. Returns true if a newer change superseded the build.
    fn candidate_from(self: &Arc<Self>, name: &str, paths: &BTreeSet<String>, epoch: u64) -> bool {
        let Ok(svc) = self.service(name).cloned() else {
            return false;
        };
        let clear = || {
            self.set_candidate(name, None);
            if let Some(p) = self.proxy(name) {
                p.set_pending(None);
            }
        };
        // Nothing runs: nothing to replace. The next start builds anyway.
        if self.slots.lock().unwrap()[name].main.is_none() {
            clear();
            return false;
        }
        let newer = || {
            self.changes
                .lock()
                .unwrap()
                .get(name)
                .is_some_and(|c| c.epoch != epoch)
        };
        if let Some(p) = self.proxy(name) {
            p.set_pending(Some("building".into()));
        }
        let artifact = {
            let _op = self.op_locks[name].lock().unwrap();
            match self.build_locked(name, &svc, &newer) {
                Ok(a) => a,
                Err(e) if e == SUPERSEDED => {
                    self.set_candidate(name, Some(Candidate::new("superseded", None, None)));
                    return true;
                }
                Err(e) => {
                    self.set_candidate(
                        name,
                        Some(Candidate::new("failed", None, Some(first_lines(&e, 8)))),
                    );
                    if let Some(p) = self.proxy(name) {
                        p.set_pending(Some("failed".into()));
                    }
                    return false;
                }
            }
        };
        // A manual restart came in meanwhile and covers this change.
        if self
            .changes
            .lock()
            .unwrap()
            .get(name)
            .is_some_and(|c| c.absorbed >= epoch)
        {
            clear();
            return false;
        }
        // The change did not touch the service's inputs after all.
        if self.slots.lock().unwrap()[name].view.build_id.as_deref() == Some(artifact.0.as_str()) {
            clear();
            return false;
        }
        if let Some(reason) = hold_reason(&svc, paths) {
            self.held
                .lock()
                .unwrap()
                .insert(name.into(), artifact.clone());
            self.set_candidate(
                name,
                Some(Candidate::new(
                    "held",
                    Some(artifact.0.clone()),
                    Some(format!("{reason}; `devstack promote {name}` takes it over")),
                )),
            );
            if let Some(p) = self.proxy(name) {
                p.set_pending(Some(format!("held {}", artifact.0)));
            }
            return false;
        }
        if let Err(e) = self.take_over(name, &svc, artifact) {
            // replace() has set the candidate to failed already where it applies.
            let already = self.slots.lock().unwrap()[name]
                .view
                .candidate
                .as_ref()
                .is_some_and(|c| c.state == "failed");
            if !already {
                self.set_candidate(
                    name,
                    Some(Candidate::new("failed", None, Some(first_lines(&e, 8)))),
                );
            }
        }
        false
    }

    /// Run the jobs `svc` comes after (a schema migrator), then put the
    /// artifact into the main slot: through a candidate behind the proxy, or by
    /// a restart in place.
    fn take_over(
        self: &Arc<Self>,
        name: &str,
        svc: &Service,
        artifact: (String, PathBuf),
    ) -> Result<(), String> {
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
        if svc.proxied() && self.slots.lock().unwrap()[name].main.is_some() {
            return self.replace(name, svc, Some(artifact));
        }
        self.set_candidate(name, None);
        self.stop(name)?;
        self.start(name)
    }

    /// Run `action` once the changes for `key` have been quiet for a moment.
    pub(crate) fn debounce(
        self: &Arc<Self>,
        key: String,
        action: impl FnOnce(&Arc<Self>) + Send + 'static,
    ) {
        let spawn = {
            let mut q = self.quiet.lock().unwrap();
            let e = q.entry(key.clone()).or_insert((Instant::now(), false));
            e.0 = Instant::now();
            !std::mem::replace(&mut e.1, true)
        };
        if !spawn {
            return; // the running worker sees the new time and waits longer
        }
        let me = Arc::clone(self);
        let _ = std::thread::Builder::new()
            .name(format!("debounce-{key}"))
            .spawn(move || {
                loop {
                    std::thread::sleep(Duration::from_millis(200));
                    let mut q = me.quiet.lock().unwrap();
                    let e = q.get_mut(&key).expect("inserted above");
                    if e.0.elapsed() >= DEBOUNCE {
                        e.1 = false;
                        break;
                    }
                }
                action(&me);
            });
    }

    pub(crate) fn service_running(&self, name: &str) -> bool {
        self.slots
            .lock()
            .unwrap()
            .get(name)
            .is_some_and(|s| s.main.is_some())
    }

    /// `restart_on` of a Compose service: restart the container.
    pub(crate) fn restart_container(&self, group: &str, service: &str) {
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

    /// A manual restart takes in every change seen so far: the watcher does
    /// not build them again.
    fn absorb(&self, name: &str) {
        if let Some(c) = self.changes.lock().unwrap().get_mut(name) {
            c.paths.clear();
            c.absorbed = c.epoch;
        }
    }

    /// Drop a "changed" marker that no candidate will follow.
    fn clear_changed(&self, name: &str) {
        let changed = self.slots.lock().unwrap()[name]
            .view
            .candidate
            .as_ref()
            .is_some_and(|c| c.state == "changed");
        if changed {
            self.set_candidate(name, None);
            if let Some(p) = self.proxy(name) {
                p.set_pending(None);
            }
        }
    }

    /// Put the build that ran before the last replacement back into the main
    /// slot, without building and without running jobs: a schema migration is
    /// never rolled back. Behind the proxy only (the replacement is a candidate).
    pub fn rollback(self: &Arc<Self>, name: &str) -> Result<(), String> {
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

    /// Take over a candidate that a promotion gate holds (ADR-0013).
    pub fn promote(self: &Arc<Self>, name: &str) -> Result<(), String> {
        let svc = self.service(name)?.clone();
        let Some(artifact) = self.held.lock().unwrap().remove(name) else {
            return Err(format!("no candidate of `{name}` waits for promotion"));
        };
        self.take_over(name, &svc, artifact)
    }

    /// Start what `svc` comes after and is not ready yet (services, jobs that
    /// have not completed). Compose services are the infra plane's job (`up`).
    fn ensure_after(self: &Arc<Self>, svc: &Service) -> Result<(), String> {
        for dep in &svc.after {
            let Some(d) = self.stack.services.get(dep) else {
                continue;
            };
            if !runs_here(d) {
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

/// The error text of a build killed for a newer change.
pub(crate) const SUPERSEDED: &str = "superseded by a newer change";

/// Quiet period after the last change before a candidate is built (plan §3.2).
const DEBOUNCE: Duration = Duration::from_secs(2);

/// Why a built candidate waits for `promote` (ADR-0013): auto-promotion is
/// off, or the change touched paths the service declares as schema inputs.
fn hold_reason(svc: &Service, paths: &BTreeSet<String>) -> Option<String> {
    let p = svc.promote.as_ref()?;
    if !p.auto {
        return Some("auto-promotion is off".into());
    }
    let set = devstack_build::watch_set(&p.hold_if_changed)
        .ok()
        .flatten()?;
    let hit: Vec<&str> = paths
        .iter()
        .filter(|x| set.is_match(x.as_str()))
        .map(String::as_str)
        .collect();
    (!hit.is_empty()).then(|| format!("changes schema inputs ({})", hit.join(", ")))
}

/// A free local port for an instance behind the proxy.
fn free_port() -> Result<u16, String> {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .map_err(|e| format!("no free port: {e}"))
}

fn first_lines(text: &str, n: usize) -> String {
    text.lines().take(n).collect::<Vec<_>>().join("\n")
}

fn runs_here(svc: &Service) -> bool {
    svc.platforms.is_empty() || svc.platforms.contains(&Platform::current())
}

/// What a failed build said: its error lines (compiler conventions `: error`,
/// `error:`, `error[`), each once and at most 20; without any, the last 15
/// lines. A .NET build ends with pages of warnings, which would bury the error.
fn build_errors(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut seen = BTreeSet::new();
    let errors: Vec<&str> = text
        .lines()
        .filter(|l| {
            let lower = l.to_ascii_lowercase();
            lower.contains(": error") || lower.starts_with("error:") || lower.contains("error[")
        })
        .filter(|l| seen.insert(l.trim()))
        .take(20)
        .collect();
    if errors.is_empty() {
        tail(path, 15)
    } else {
        errors.join("\n")
    }
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

/// Taken on IPv4 or IPv6 loopback. A dev server bound to `localhost` may hold
/// only `::1` (Node on Windows); binding `127.0.0.1` would still succeed. A host
/// without IPv6 fails the `::1` bind with another error, which is not "in use".
fn port_in_use(port: u16) -> bool {
    // Windows reports a port held exclusively by another process as access denied.
    let taken = |r: std::io::Result<std::net::TcpListener>| {
        r.err().is_some_and(|e| {
            matches!(
                e.kind(),
                std::io::ErrorKind::AddrInUse | std::io::ErrorKind::PermissionDenied
            )
        })
    };
    taken(std::net::TcpListener::bind(("127.0.0.1", port)))
        || taken(std::net::TcpListener::bind(("::1", port)))
}

#[cfg(test)]
mod port_tests {
    #[test]
    fn a_port_held_on_ipv6_loopback_only_is_in_use() {
        let Ok(l) = std::net::TcpListener::bind(("::1", 0)) else {
            return; // no IPv6 on this host
        };
        let port = l.local_addr().unwrap().port();
        assert!(super::port_in_use(port));
        drop(l);
        assert!(!super::port_in_use(port));
    }

    #[test]
    fn a_port_held_on_ipv4_loopback_is_in_use() {
        let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        assert!(super::port_in_use(l.local_addr().unwrap().port()));
    }
}

#[cfg(test)]
mod build_error_tests {
    #[test]
    fn errors_are_found_under_pages_of_warnings() {
        let d = tempfile::tempdir().unwrap();
        let log = d.path().join("build.log");
        let mut text = String::from("Program.cs(413,90): error CS1002: ; expected [Api.csproj]\n");
        for i in 0..40 {
            text.push_str(&format!(
                "x.csproj : warning NU1903: package {i} is vulnerable\n"
            ));
        }
        text.push_str("Program.cs(413,90): error CS1002: ; expected [Api.csproj]\n    0 Warning(s)\n    1 Error(s)\n");
        std::fs::write(&log, text).unwrap();
        assert_eq!(
            super::build_errors(&log),
            "Program.cs(413,90): error CS1002: ; expected [Api.csproj]"
        );
    }

    #[test]
    fn without_error_lines_the_end_of_the_log() {
        let d = tempfile::tempdir().unwrap();
        let log = d.path().join("build.log");
        std::fs::write(&log, "a\nb\nsomething went wrong\n").unwrap();
        assert!(super::build_errors(&log).ends_with("something went wrong"));
    }
}
