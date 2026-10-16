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
    /// One `prepare` at a time: two in the same directory would collide.
    preparing: Mutex<()>,
    /// The branch of this working tree, as `git worktree list` says
    /// (`Lanes::sync` keeps it current).
    branch: Mutex<Option<String>>,
    /// What a lane's ports are moved by (0 in the main tree): its own
    /// services' and its own containers' (ADR-0030).
    port_offset: u16,
    /// `devstack up --workers` in a lane: its main instances run their
    /// background workers although the lane does not own the main slot; its
    /// coordinators are its own (ADR-0030).
    lane_workers: std::sync::atomic::AtomicBool,
    /// Services whose next build runs even if an artifact for their inputs
    /// exists (`devstack restart --rebuild`, #13).
    force_build: Mutex<BTreeSet<String>>,
    /// The stack file this daemon runs from, and a hash of what it read at start.
    /// The daemon does not reload it: a change waits for the next start, and the
    /// status says so instead of keeping quiet.
    stack_file: PathBuf,
    stack_file_hash: Option<blake3::Hash>,
    config_changed: std::sync::atomic::AtomicBool,
    /// Services switched to hot-reload mode (plan §3.4a), for this daemon's life.
    hot: Mutex<BTreeSet<String>>,
    /// The lane this supervisor runs (plan §3.5): empty for the main working
    /// tree, which also owns the infrastructure.
    pub lane: String,
    /// Which tree's instances are the main slot (ADR-0024): `None` the main
    /// working tree, else a lane's name. One value, shared by the main
    /// supervisor and every lane, set only by `Lanes::take_main`.
    main_owner: Arc<Mutex<Option<String>>>,
    /// Main lane only: TCP forwards on the ports of services without the HTTP
    /// proxy (dev servers) while a lane serves the main ports.
    forwards: Mutex<BTreeMap<String, crate::tcpfwd::Forward>>,
    /// Main lane only: the builds the lanes need kept (`Lanes::pins`).
    lane_pins: Mutex<Option<LanePins>>,
    /// Set when a lane is removed: its sampler threads end.
    closed: std::sync::atomic::AtomicBool,
}

/// The artifact ids of a service the lanes run or may go back to.
pub(crate) type LanePins = Box<dyn Fn(&str) -> Vec<String> + Send + Sync>;

/// Hash of the stack file, line endings ignored (a checkout may flip them).
/// Every state change also goes to the diagnostic log (plan §11): the event as
/// the clients see it, as a warning when something failed. Snapshots and load
/// samples stay out (no decision in them, and they would drown the rest).
fn record(body: &EventBody, lane: &str) {
    // Lanes have log lines of their own (`lane_found`, `lane_started`, ...).
    if matches!(
        body,
        EventBody::Snapshot { .. } | EventBody::Usage(_) | EventBody::Lanes { .. }
    ) {
        return;
    }
    let Ok(serde_json::Value::Object(mut v)) = serde_json::to_value(body) else {
        return;
    };
    let event = v
        .remove("type")
        .and_then(|t| t.as_str().map(str::to_string))
        .unwrap_or_else(|| "event".into());
    let state_of = |key: &str| {
        v.get(key)
            .and_then(|s| s.get("state").or(Some(s)))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let bad = ["failed", "unhealthy", "exited"];
    let warn =
        bad.contains(&state_of("status").as_str()) || bad.contains(&state_of("candidate").as_str());
    if !lane.is_empty() {
        v.insert("lane".into(), serde_json::Value::String(lane.into()));
    }
    crate::diag::log(
        if warn { "warn" } else { "info" },
        &event,
        serde_json::Value::Object(v),
    );
}

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
        Self::new_lane(
            stack,
            stack_file,
            repo,
            state_dir,
            proc_opts,
            rt,
            String::new(),
            0,
            Arc::new(Mutex::new(None)),
        )
    }

    /// A supervisor for a lane (plan §3.5): `lane` empty for the main working
    /// tree. Another lane gets a stack whose service ports are already moved
    /// by `port_offset` ([`crate::lanes`]) and runs its own Compose projects,
    /// their ports moved by the same offset (ADR-0030).
    #[allow(clippy::too_many_arguments)]
    pub fn new_lane(
        stack: Stack,
        stack_file: PathBuf,
        repo: PathBuf,
        state_dir: PathBuf,
        proc_opts: devstack_proc::Options,
        rt: tokio::runtime::Handle,
        lane: String,
        port_offset: u16,
        main_owner: Arc<Mutex<Option<String>>>,
    ) -> Result<Arc<Self>, String> {
        let stack = with_static_servers(stack);
        // A lane (`.devstack/lane-<name>/`) shares the main tree's artifact store.
        let layout = match state_dir.parent() {
            Some(root) if !lane.is_empty() => Layout::shared(&state_dir, root.join("artifacts")),
            _ => Layout::new(&state_dir),
        };
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
                        hot_reload: false,
                        hot_reload_available: svc.hot_reload.is_some(),
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
            preparing: Mutex::new(()),
            branch: Mutex::new(None),
            port_offset,
            lane_workers: std::sync::atomic::AtomicBool::new(false),
            force_build: Mutex::new(BTreeSet::new()),
            stack_file_hash: stack_file_hash(&stack_file),
            stack_file,
            config_changed: std::sync::atomic::AtomicBool::new(false),
            hot: Mutex::new(BTreeSet::new()),
            main_owner,
            forwards: Mutex::new(BTreeMap::new()),
            lane_pins: Mutex::new(None),
            closed: std::sync::atomic::AtomicBool::new(false),
            lane,
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
        record(&body, &self.lane);
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
            lane: (!self.lane.is_empty()).then(|| self.lane.clone()),
            branch: self.branch.lock().unwrap().clone(),
            main_served_by: self.served_by(),
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
                            cache
                                .published
                                .insert((g.clone(), svc.clone(), *port), addr.clone());
                        }
                        cache
                            .envs
                            .insert((g.clone(), svc.clone()), s.environment.clone());
                        if !s.state.is_empty() {
                            let ports: Vec<String> =
                                s.ports.iter().map(|(p, a)| format!("{p}->{a}")).collect();
                            view.containers.push(ContainerView {
                                service: svc.clone(),
                                name: format!("{}-{svc}", eg.project),
                                exit_code: 0,
                                state: s.state.clone(),
                                health: s.health.clone(),
                                urls: Vec::new(),
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
        let dir = self.log_dir(name);
        let newest = std::fs::read_dir(dir)
            .ok()?
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("build-"))
            .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())?;
        Some(self.repo_paths(&tail(&newest.path(), 3000)))
    }

    /// Compiler paths in a build log point into the snapshot; map them to the
    /// repository, where the user's files are.
    pub fn repo_paths(&self, text: &str) -> String {
        let snap_dir = self.layout.lane("main").join("src");
        text.replace(
            &snap_dir.to_string_lossy().into_owned(),
            &self.repo.to_string_lossy(),
        )
    }

    /// Where the kept logs of `name` are (plan §11).
    pub fn log_dir(&self, name: &str) -> PathBuf {
        self.state_dir.join("logs").join(name)
    }

    /// `svc` runs in this supervisor: on this platform, and in a lane only
    /// unless it says `lanes = false`.
    fn runs_here(&self, svc: &Service) -> bool {
        on_this_platform(svc) && (self.lane.is_empty() || svc.lanes)
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

    /// A lane runs from a copy of its worktree, never from the worktree itself
    /// (ADR-0024): git and the tools that manage worktrees can remove one at
    /// any time, and a process inside it would hold it open (Windows).
    fn runs_from_copy(&self) -> bool {
        !self.lane.is_empty()
    }

    /// Where processes without an artifact run and what `{repo}` means for
    /// them: the main tree itself, or a lane's snapshot.
    pub fn run_root(&self) -> PathBuf {
        if self.runs_from_copy() {
            self.layout.lane("main").join("src")
        } else {
            self.repo.clone()
        }
    }

    /// What `prepare` of `svc` would run in `snap`; `None` when it ran for the
    /// same command and inputs already.
    fn prepare_job(
        &self,
        name: &str,
        svc: &Service,
        snap: &Snapshot,
    ) -> Result<Option<PrepareJob>, String> {
        let Some(p) = &svc.prepare else {
            return Ok(None);
        };
        let cwd = p.cwd.as_ref().or(svc.cwd.as_ref());
        let mut h = blake3::Hasher::new();
        h.update(p.run.as_bytes());
        h.update(cwd.map_or("", String::as_str).as_bytes());
        if let Some(g) = devstack_build::watch_set(&p.inputs).map_err(|e| e.to_string())? {
            for (rel, e) in snap
                .manifest
                .files
                .iter()
                .filter(|(r, _)| g.is_match(r.as_str()))
            {
                h.update(rel.as_bytes());
                h.update(e.hash.as_bytes());
            }
        }
        let key = h.finalize().to_hex().to_string();
        let mark = self.layout.lane("main").join("prepared").join(name);
        if std::fs::read_to_string(&mark).is_ok_and(|k| k == key) {
            return Ok(None);
        }
        Ok(Some(PrepareJob {
            key,
            mark,
            cwd: cwd.map_or_else(|| snap.dir.clone(), |c| snap.dir.join(c)),
            run: p.run.clone(),
        }))
    }

    /// Run a `prepare` to completion; its output goes to a log of its own.
    fn run_prepare(&self, name: &str, job: PrepareJob) -> Result<(), String> {
        let _one = self.preparing.lock().unwrap();
        let dir = self.log_dir(name);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        crate::diag::prune(&dir, "prepare-", KEEP_BUILD_LOGS);
        let log = dir.join(format!("prepare-{}.log", now_ms()));
        let file = std::fs::File::create(&log).map_err(|e| e.to_string())?;
        crate::diag::info(
            "prepare",
            serde_json::json!({ "lane": self.lane, "service": name, "run": job.run }),
        );
        std::fs::create_dir_all(&job.cwd).map_err(|e| e.to_string())?;
        let spec = Spec {
            name: format!("{name}-prepare"),
            command: job.run.clone(),
            cwd: job.cwd,
            env: BTreeMap::new(),
            log: file,
        };
        let (mut tree, _) =
            devstack_proc::spawn(spec, &self.proc_opts).map_err(|e| e.to_string())?;
        let code = loop {
            if let Some(code) = tree.try_exit_code() {
                break code;
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        if code != 0 {
            return Err(format!(
                "prepare `{}` exited with code {code}\n{}",
                job.run,
                tail(&log, 15)
            ));
        }
        if let Some(parent) = job.mark.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&job.mark, job.key).map_err(|e| e.to_string())
    }

    /// A lane's copy, current and prepared for `svc`, before it starts from it.
    fn copy_ready(&self, name: &str, svc: &Service) -> Result<(), String> {
        let job = {
            let mut snap = self.snapshot.lock().unwrap();
            snap.sync().map_err(|e| e.to_string())?;
            self.prepare_job(name, svc, &snap)?
        };
        match job {
            Some(job) => self.run_prepare(name, job),
            None => Ok(()),
        }
    }

    /// Something changed in a lane's worktree: bring the copy its dev servers
    /// and tools run from up to date, after a short quiet time, so their own
    /// watchers (hot module replacement) see the change.
    pub fn copy_changed(self: &Arc<Self>) {
        if !self.runs_from_copy() {
            return;
        }
        self.debounce_for("copy".into(), COPY_QUIET, |s| {
            let from_copy = s
                .slots
                .lock()
                .unwrap()
                .values()
                .any(|slot| slot.main.as_ref().is_some_and(|i| i.build_id.is_none()));
            if from_copy && let Err(e) = s.snapshot.lock().unwrap().sync() {
                crate::diag::warn(
                    "copy_not_synced",
                    serde_json::json!({ "lane": s.lane, "error": e.to_string() }),
                );
            }
        });
    }

    pub fn service_log(&self, name: &str) -> Option<PathBuf> {
        self.slots
            .lock()
            .unwrap()
            .get(name)
            .and_then(|s| s.view.log.clone())
    }

    // -------------------------------------------------------------- infra

    /// A Compose group as this supervisor runs it. In a lane (ADR-0030): its
    /// own project `<project>-<lane>`, its own external networks
    /// `<name>-<lane>`, the overlay written by [`Self::write_lane_overlay`]
    /// last, and without the `shared` services, which the main tree runs.
    fn compose_group(&self, name: &str) -> Group {
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
        )
        .map_err(|e| format!("compose group `{group}`: {e}"))?;
        std::fs::create_dir_all(file.parent().unwrap()).map_err(|e| e.to_string())?;
        std::fs::write(&file, text).map_err(|e| e.to_string())
    }

    /// `devstack up --workers` in a lane (ADR-0030).
    pub fn set_lane_workers(&self, on: bool) {
        self.lane_workers.store(on, Ordering::SeqCst);
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
                    // The last verdict of `check_forwards` stays until the
                    // container stops (a restart repairs a lost forward).
                    let up = c.state == "running";
                    let unreachable = if up {
                        view.containers
                            .iter()
                            .find(|o| o.service == c.service)
                            .map(|o| o.unreachable.clone())
                            .unwrap_or_default()
                    } else {
                        Vec::new()
                    };
                    next.push(ContainerView {
                        service: c.service.clone(),
                        name: c.name,
                        exit_code: c.exit_code,
                        state: c.state,
                        health: c.health,
                        urls: self.container_urls(&g, &c.service, &ports),
                        ports,
                        unreachable,
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
                            service: c.service.clone(),
                            name: c.name,
                            exit_code: c.exit_code,
                            state: c.state,
                            health: c.health,
                            urls: self.container_urls(&g, &c.service, &ports),
                            ports,
                            unreachable: Vec::new(),
                        });
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
    /// This lane's instances hold the main slot: workers on (ADR-0008).
    pub fn owns_main(&self) -> bool {
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
        self.closed.store(true, Ordering::SeqCst);
        for (_, p) in std::mem::take(&mut *self.proxies.lock().unwrap()) {
            p.close();
        }
        for (_, f) in std::mem::take(&mut *self.forwards.lock().unwrap()) {
            f.close();
        }
    }

    pub(crate) fn closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    pub fn up(self: &Arc<Self>) -> Result<(), String> {
        {
            // Every lane has its own infrastructure (ADR-0030).
            self.infra_up()?;
            // Healthy inside Docker is not reachable from here: say so
            // (a service that needs the container repairs it, ensure_after).
            for (g, svc, lost) in self.check_forwards() {
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
    /// long-lived) unless `infra` is set.
    pub fn down(&self, infra: bool) {
        for name in self.service_order().into_iter().rev() {
            let _ = self.stop(&name);
        }
        // A lane's infrastructure is its own (ADR-0030); `shared` services
        // are not in its groups. On a node the agent runs it (ADR-0025).
        if infra && self.external_infra().is_none() {
            for g in self.group_order().into_iter().rev() {
                let _ = self.docker.down(&self.compose_group(&g));
            }
            self.refresh_infra();
        }
    }

    /// Build if needed, then start and wait until ready (jobs: until done).
    fn is_hot(&self, name: &str) -> bool {
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
        self.held.lock().unwrap().remove(name);
        self.set_candidate(name, None);
        crate::diag::log(
            "info",
            "hot_reload",
            serde_json::json!({ "service": name, "on": on }),
        );
        self.stop(name)?;
        self.start(name)
    }

    /// Keep the newest artifacts of a service and everything something runs or
    /// may go back to (ADR-0002 retention): this tree's main, previous,
    /// candidate and held builds, and what every lane runs. Only the main tree
    /// prunes: the store is shared with the lanes.
    fn prune_artifacts(&self, name: &str) {
        if !self.layout.owns_artifacts() {
            return;
        }
        let mut pinned = self.pins(name);
        // What the started lanes run or may go back to.
        if let Some(lane_pins) = self.lane_pins.lock().unwrap().as_ref() {
            pinned.extend(lane_pins(name));
        }
        // What the lanes not started now restore at their next start (their
        // state.json).
        if let Ok(rd) = std::fs::read_dir(&self.state_dir) {
            for e in rd.flatten() {
                if e.file_name().to_string_lossy().starts_with("lane-") {
                    pinned.extend(Persisted::load(&e.path()).main.get(name).cloned());
                }
            }
        }
        let refs: Vec<&str> = pinned.iter().map(String::as_str).collect();
        let removed = self.builder.prune(name, KEEP_ARTIFACTS, &refs);
        if !removed.is_empty() {
            crate::diag::info(
                "artifacts_pruned",
                serde_json::json!({ "service": name, "removed": removed }),
            );
        }
    }

    /// The builds of a service this tree runs or may go back to: main,
    /// previous, candidate, held and the one restored at the next start.
    pub(crate) fn pins(&self, name: &str) -> BTreeSet<String> {
        let mut pinned: BTreeSet<String> = BTreeSet::new();
        if let Some(slot) = self.slots.lock().unwrap().get(name) {
            pinned.extend(slot.view.build_id.clone());
            pinned.extend(slot.previous.clone());
            pinned.extend(
                slot.view
                    .candidate
                    .as_ref()
                    .and_then(|c| c.build_id.clone()),
            );
        }
        if let Some((id, _)) = self.held.lock().unwrap().get(name) {
            pinned.insert(id.clone());
        }
        pinned.extend(self.persisted.lock().unwrap().main.get(name).cloned());
        pinned
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

    /// Main lane: forward the fixed port of `name` to `target` (`None` ends
    /// the forward). The previous forward's listener is closed before this
    /// returns, so the port can be bound again at once.
    pub fn tcp_forward(&self, name: &str, target: Option<u16>) -> Result<(), String> {
        let svc = self.service(name)?.clone();
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
        self.forwards.lock().unwrap().keys().cloned().collect()
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

    /// Start a running service again from its current build, so a changed
    /// worker switch takes effect; nothing is built that is not cached.
    pub fn relaunch(self: &Arc<Self>, name: &str) -> Result<(), String> {
        self.stop(name)?;
        self.start(name)
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
            None => proxy.set_upstream(None),
        }
        Ok(())
    }

    pub fn start(self: &Arc<Self>, name: &str) -> Result<(), String> {
        let svc = self.effective(name, self.service(name)?);
        if let Some(lane) = self
            .served_by()
            .filter(|_| svc.proxied() || self.forwards.lock().unwrap().contains_key(name))
        {
            return Err(format!(
                "lane `{lane}` serves the main ports: `devstack promote --lane main` first"
            ));
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
        if artifact.is_none() && self.runs_from_copy() {
            self.copy_ready(name, svc)?;
        }
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
        let mut id = build_id(&snap.manifest, svc).map_err(|e| e.to_string())?;
        // `--rebuild`: the same inputs once more, under `<id>-<n>`; the artifact
        // that exists may be running and is never overwritten (ADR-0002).
        if self.force_build.lock().unwrap().remove(name) {
            let base = id.clone();
            id = (2..)
                .map(|n| format!("{base}-{n}"))
                .find(|i| self.builder.artifact(name, i).is_none())
                .expect("a free id");
            crate::diag::info(
                "build_forced",
                serde_json::json!({ "lane": self.lane, "service": name, "inputs": base, "build_id": id }),
            );
        } else if self.builder.artifact(name, &id).is_some() {
            crate::diag::info(
                "build_reused",
                serde_json::json!({
                    "lane": self.lane,
                    "service": name,
                    "build_id": id,
                    "why": "an artifact for these inputs exists (watch paths, build files above them, build command); `devstack restart --rebuild` builds anyway",
                }),
            );
        }
        if self.builder.artifact(name, &id).is_none() {
            building(Some(id.clone()));
            if let Some(job) = self.prepare_job(name, svc, &snap)? {
                drop(snap);
                self.run_prepare(name, job)?;
                snap = self.snapshot.lock().unwrap();
            }
            let mut running = self
                .builder
                .start_as("main", &snap, name, svc, id.clone())
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
                Outcome::Published { .. } => self.prune_artifacts(name),
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
            repo: self.run_root(),
            artifact: artifact.map(Path::to_path_buf),
            port,
            // A lane without the main slot never runs workers (ADR-0012 interim rule).
            // What runs in the main tree only keeps its workers: no lane takes them over.
            is_main: is_main
                && (self.owns_main() || !svc.lanes || self.lane_workers.load(Ordering::SeqCst)),
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

    pub fn restart(self: &Arc<Self>, name: &str) -> Result<(), String> {
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
        if self.is_hot(name) {
            return; // its own watcher reloads it
        }
        if self.served_by().is_some() && self.stack.services.get(name).is_some_and(|s| s.proxied())
        {
            return; // another lane serves its port; no candidate may take it back
        }
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
            crate::diag::info(
                "candidate_unchanged",
                serde_json::json!({
                    "lane": self.lane,
                    "service": name,
                    "build_id": artifact.0,
                    "paths": paths,
                    "why": "the changed files are not inputs of its build id (watch paths, build files above them)",
                }),
            );
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
        self.debounce_for(key, DEBOUNCE, action);
    }

    /// [`Self::debounce`] with a quiet time of `quiet`.
    pub(crate) fn debounce_for(
        self: &Arc<Self>,
        key: String,
        quiet: Duration,
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
                    std::thread::sleep((quiet / 4).min(Duration::from_millis(200)));
                    let mut q = me.quiet.lock().unwrap();
                    let e = q.get_mut(&key).expect("inserted above");
                    if e.0.elapsed() >= quiet {
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

    /// A container's output (`docker compose logs`, timestamped).
    /// The branch of this working tree changed (or is first known).
    pub fn set_branch(&self, branch: Option<String>) {
        *self.branch.lock().unwrap() = branch;
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

    /// Take over a candidate that a promotion gate holds (ADR-0013).
    pub fn promote(self: &Arc<Self>, name: &str) -> Result<(), String> {
        if self.is_hot(name) {
            return Err(format!(
                "`{name}` runs in hot-reload mode: switch it off first"
            ));
        }
        let svc = self.service(name)?.clone();
        let Some(artifact) = self.held.lock().unwrap().remove(name) else {
            return Err(format!("no candidate of `{name}` waits for promotion"));
        };
        self.take_over(name, &svc, artifact)
    }

    /// Start what `svc` comes after and is not ready yet (services, jobs that
    /// have not completed). Compose services are the infra plane's job (`up`).
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
    fn ensure_reachable(&self, group: &str, service: &str) -> Result<(), String> {
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

/// `kind = "static"` services run devstack's own file server on their
/// artifact (plan §3.4); with that command they are ordinary services for the
/// pipeline, the proxy and the health check.
fn with_static_servers(mut stack: Stack) -> Stack {
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "devstack".into());
    for svc in stack.services.values_mut() {
        if svc.kind == Kind::Static && svc.run.is_empty() {
            svc.run = format!(
                "\"{exe}\" serve-static \"{{artifact}}\" --port {{port}}{}",
                if svc.spa == Some(false) {
                    " --no-spa"
                } else {
                    ""
                }
            );
        }
    }
    stack
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

/// The error text of a build killed for a newer change.
pub(crate) const SUPERSEDED: &str = "superseded by a newer change";

/// Quiet period after the last change before a candidate is built (plan §3.2).
/// Logs kept per service: the newest starts and builds (plan §11).
pub const KEEP_RUN_LOGS: usize = 20;
/// Artifacts kept per service beyond the pinned ones (ADR-0002 retention).
pub const KEEP_ARTIFACTS: usize = 5;
pub const KEEP_BUILD_LOGS: usize = 10;

const DEBOUNCE: Duration = Duration::from_secs(2);
/// How long a lane's worktree has to be quiet before its copy follows: short,
/// a dev server's hot reload waits for it.
const COPY_QUIET: Duration = Duration::from_millis(150);

/// A `prepare` to run ([`Supervisor::prepare_job`]).
struct PrepareJob {
    key: String,
    /// Holds `key` once it succeeded.
    mark: PathBuf,
    cwd: PathBuf,
    run: String,
}

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

fn on_this_platform(svc: &Service) -> bool {
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
pub fn port_in_use(port: u16) -> bool {
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
