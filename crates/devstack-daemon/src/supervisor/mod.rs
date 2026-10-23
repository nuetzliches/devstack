//! The supervisor: brings the infra plane up, builds services into artifacts
//! and runs them in the main slot in `after` order, watches their processes
//! and health, and stops them. A service behind the proxy (ADR-0010) runs on an
//! internal port, and a rebuild replaces it through a candidate without
//! dropping a request (ADR-0003); without the proxy it restarts in place.

use crate::health::Checker;
use crate::proxy::Proxy;
use crate::state::{
    Candidate, CommandView, Event, EventBody, GroupView, InfraView, Notice, NoticeLevel, Persisted,
    ServiceView, StackView, Status, now_ms,
};
use devstack_build::{Builder, Layout, Snapshot};
use devstack_compose::Docker;
use devstack_config::Stack;
use devstack_config::model::{Kind, Platform, Service};
use devstack_env::ComposeEnvs;
use devstack_proc::Owned;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

mod artifacts;
mod candidate;
mod copy;
mod handover;
mod infra;
pub use infra::ContainerTarget;
mod lifecycle;
mod rebuilds;
mod replace;

/// One running process of a service.
struct Instance {
    tree: Owned,
    /// The port it listens on (internal behind the proxy), if it has one.
    port: Option<u16>,
    build_id: Option<String>,
    log: PathBuf,
    /// Its cleanup record's name (ADR-0016), `<service>@<port>`.
    record: String,
    /// When it was spawned: how long it ran, when it exits by itself.
    started: Instant,
}

pub(crate) struct Slot {
    pub view: ServiceView,
    main: Option<Instance>,
    /// The build main ran before its last replacement: `rollback` (plan §3.2).
    previous: Option<String>,
    /// Bumped by every start, stop and switch; a monitor of an older generation retires.
    generation: u64,
    /// The build a start or a candidate is about to run, from the moment its
    /// artifact is chosen until it runs (or does not): pinned against
    /// pruning meanwhile (ADR-0002 retention).
    in_flight: Option<String>,
    /// When its process was spawned, until the start is over (ready or
    /// failed): the start's duration goes to the diagnostic log (ADR-0037).
    spawned: Option<Instant>,
    /// Processes spawned for it in this daemon's life.
    starts: u32,
    /// Times in a row it exited by itself and was started again; back to 0
    /// once an instance ran for [`lifecycle::STABLE`].
    exits: u32,
    /// The relay's second start failed (ADR-0008): the instance of this
    /// generation serves without its background workers, and says so in its
    /// status until another instance takes its place.
    without_workers: Option<(u64, String)>,
    /// A job: when its last successful run was asked for (Unix ms). A
    /// candidate built after that needs no run of its own.
    ran_since: Option<u128>,
}

/// A built candidate a promotion gate holds (ADR-0013). The gate stays until
/// `promote`, or until a build from the tree as it is now becomes main by
/// another way (a restart, a start): a later change, however harmless, must
/// not carry the held one into main.
#[derive(Clone)]
struct Held {
    /// The newest build of the tree, waiting for `promote`; `None` while the
    /// newest build failed (the gate stays, there is nothing to promote).
    artifact: Option<(String, PathBuf)>,
    reason: String,
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
    /// Per job: one run before a take-over at a time, so two services after
    /// the same job do not run it twice for one change.
    reruns: BTreeMap<String, Mutex<()>>,
    snapshot: Mutex<Snapshot>,
    infra: Mutex<InfraCache>,
    /// In a lane on a node: each published container on its loopback port
    /// (the one it has on the developer's machine), by that port.
    loopback: Mutex<BTreeMap<u16, crate::tcpfwd::Forward>>,
    persisted: Mutex<Persisted>,
    seq: AtomicU64,
    pub events: broadcast::Sender<Event>,
    /// Runs the proxies (ADR-0010).
    rt: tokio::runtime::Handle,
    proxies: Mutex<BTreeMap<String, Proxy>>,
    /// Changes seen by the watcher, per service, not yet in a candidate.
    changes: Mutex<BTreeMap<String, Changes>>,
    /// Built candidates waiting for `promote` (ADR-0013), per service;
    /// persisted, so a daemon restart keeps the gate.
    held: Mutex<BTreeMap<String, Held>>,
    /// Per service, bumped by every `stop`: a start, build or job wait that
    /// began before gives up (a hanging migrator must not need the daemon
    /// killed).
    stops: BTreeMap<String, AtomicU64>,
    /// Main lane only: `promote --lane` is moving the main ports to a lane;
    /// nothing of this tree may take a proxy's upstream meanwhile (ADR-0008).
    handing_over: AtomicBool,
    /// An `up` of this tree ran to its end (succeeded or not), since this
    /// daemon started: a restart's main slot moves after it (ADR-0052).
    upped: AtomicBool,
    /// Services started at least once by this daemon: the first start after
    /// a daemon start restores the last promoted artifact (ADR-0016).
    started: Mutex<BTreeSet<String>>,
    /// The snapshot's manifest as the previous daemon left it: what changed
    /// in the tree while no daemon watched it, against the tree now.
    opened_manifest: Mutex<Option<devstack_build::snapshot::Manifest>>,
    /// Why the file watcher does not work, if it does not (no candidates
    /// then): shown in the status, not only logged.
    watcher_error: Mutex<Option<(String, Instant)>>,
    /// What the user should know of this tree and no request reports, by
    /// id (ADR-0050).
    notices: Mutex<BTreeMap<String, Notice>>,
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
    /// A dev server's fixed port, held and forwarded to its instance
    /// (ADR-0043): bound at its first start, kept for the daemon's life.
    fronts: Mutex<BTreeMap<String, crate::tcpfwd::Front>>,
    /// Main lane only: fronted dev servers whose fixed port forwards to a
    /// lane now (`promote --lane`).
    fronts_to_lane: Mutex<BTreeSet<String>>,
    /// Main lane only: the fixed ports of `main_ports` containers devstack
    /// holds (ADR-0048), by (group, service, container port); bound once
    /// their container was moved off them, kept for the daemon's life.
    held_ports: Mutex<BTreeMap<(String, String, u16), infra::HeldFront>>,
    /// Main lane only: the offset of the lane the held ports forward to
    /// (`promote --lane`); `None`: the main tree's containers.
    held_to_lane: Mutex<Option<u16>>,
    /// Fronted dev servers being restarted: their front holds new
    /// connections across the stop instead of refusing them (ADR-0043).
    restarting: Mutex<BTreeSet<String>>,
    /// Main lane only: the builds the lanes need kept (`Lanes::pins`).
    lane_pins: Mutex<Option<LanePins>>,
    /// Set when a lane is removed: its sampler threads end.
    closed: std::sync::atomic::AtomicBool,
    /// Its profile's `data = "fresh"` was done, once per start (ADR-0041).
    fresh_done: std::sync::atomic::AtomicBool,
    /// Per Compose service: since when it is ready, whether its forwards
    /// were probed since, when it was last restarted for a lost one (#98).
    forward_checks: Mutex<BTreeMap<(String, String), infra::ForwardCheck>>,
}

/// The artifact ids of a service the lanes run or may go back to.
pub(crate) type LanePins = Box<dyn Fn(&str) -> Vec<String> + Send + Sync>;

/// Every state change also goes to the diagnostic log (plan §11): the event as
/// the clients see it, as a warning when something failed. Snapshots and load
/// samples stay out (no decision in them, and they would drown the rest).
///
/// `extra` joins the line (a start's duration, its result and restarts).
fn record(body: &EventBody, lane: &str, extra: Option<serde_json::Map<String, serde_json::Value>>) {
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
    let warn = bad.contains(&state_of("status").as_str())
        || bad.contains(&state_of("candidate").as_str())
        || bad.contains(&state_of("rebuild").as_str());
    // The main working tree is the lane `main` (the log says so).
    v.insert("lane".into(), serde_json::Value::String(lane.into()));
    v.extend(extra.unwrap_or_default());
    crate::diag::log(
        if warn { "warn" } else { "info" },
        &event,
        serde_json::Value::Object(v),
    );
}

/// What the line of the state a start ended in says about the start
/// (ADR-0037 point 4): `ms` from the spawn, `result`, `restarts` (processes
/// spawned for the service in this daemon's life before this one). A
/// service without a health check is `running` after a fixed wait
/// ([`lifecycle::SETTLE`]), not a measurement: no `ms`, `settle_ms` instead.
/// `None` for a start that was stopped.
fn start_fields(
    status: &Status,
    took: Duration,
    starts: u32,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    let result = match status {
        Status::Stopped | Status::Building { .. } | Status::Starting => return None,
        Status::Failed { reason } if reason == STOPPED => return None,
        s if s.is_ready() => "ok",
        _ => "failed",
    };
    let mut m = serde_json::Map::new();
    if *status == Status::Running {
        m.insert(
            "settle_ms".into(),
            (lifecycle::SETTLE.as_millis() as u64).into(),
        );
    } else {
        m.insert("ms".into(), (took.as_millis() as u64).into());
    }
    m.insert("result".into(), result.into());
    m.insert("restarts".into(), starts.saturating_sub(1).into());
    Some(m)
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
            None,
        )
    }

    /// A supervisor for a lane (plan §3.5): `lane` empty for the main working
    /// tree. Another lane gets a stack whose service ports are already moved
    /// by `port_offset` ([`crate::lanes`]) and runs its own Compose projects,
    /// their ports moved by the same offset (ADR-0030). `build_job`: the
    /// daemon's one build job (ADR-0020); `None` makes it (the main tree).
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
        build_job: Option<Arc<devstack_proc::BuildJob>>,
    ) -> Result<Arc<Self>, String> {
        let stack = with_static_servers(stack);
        // A lane (`.devstack/lane-<name>/`) shares the main tree's artifact store.
        let layout = match state_dir.parent() {
            Some(root) if !lane.is_empty() => Layout::shared(&state_dir, root.join("artifacts")),
            _ => Layout::new(&state_dir),
        };
        let builder = match build_job {
            Some(job) => Builder::with_job(layout.clone(), job),
            None => Builder::new(layout.clone(), &proc_opts).map_err(|e| e.to_string())?,
        };
        let snapshot = Snapshot::open(&repo, &layout.lane("main")).map_err(|e| e.to_string())?;
        let opened_manifest = Some(snapshot.manifest.clone());
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
        let mut reruns = BTreeMap::new();
        let mut stops = BTreeMap::new();
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
                        rebuild: None,
                        hot_reload: false,
                        hot_reload_available: svc.hot_reload.is_some(),
                    },
                    main: None,
                    previous: None,
                    generation: 0,
                    in_flight: None,
                    spawned: None,
                    starts: 0,
                    exits: 0,
                    without_workers: None,
                    ran_since: None,
                },
            );
            op_locks.insert(name.clone(), Mutex::new(()));
            reruns.insert(name.clone(), Mutex::new(()));
            stops.insert(name.clone(), AtomicU64::new(0));
        }
        let persisted = Persisted::load(&state_dir);
        // ADR-0016: kill what a crashed previous instance left (Linux; a no-op on Windows).
        devstack_proc::cleanup(&persisted.records);
        // A held candidate stays held across a daemon restart (ADR-0013).
        let held = persisted
            .held
            .iter()
            .filter(|(name, _)| stack.services.contains_key(*name))
            .map(|(name, h)| {
                let dir = layout.artifact(name, &h.build_id);
                let artifact =
                    (!h.build_id.is_empty() && dir.is_dir()).then(|| (h.build_id.clone(), dir));
                (
                    name.clone(),
                    Held {
                        artifact,
                        reason: h.reason.clone(),
                    },
                )
            })
            .collect();
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
            reruns,
            snapshot: Mutex::new(snapshot),
            infra: Mutex::new(InfraCache::default()),
            loopback: Mutex::new(BTreeMap::new()),
            persisted: Mutex::new(Persisted {
                main: persisted.main,
                records: Vec::new(),
                held: persisted.held,
            }),
            seq: AtomicU64::new(0),
            events,
            rt,
            proxies: Mutex::new(BTreeMap::new()),
            changes: Mutex::new(BTreeMap::new()),
            held: Mutex::new(held),
            stops,
            handing_over: AtomicBool::new(false),
            upped: AtomicBool::new(false),
            started: Mutex::new(BTreeSet::new()),
            opened_manifest: Mutex::new(opened_manifest),
            watcher_error: Mutex::new(None),
            notices: Mutex::new(BTreeMap::new()),
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
            fronts: Mutex::new(BTreeMap::new()),
            fronts_to_lane: Mutex::new(BTreeSet::new()),
            held_ports: Mutex::new(BTreeMap::new()),
            held_to_lane: Mutex::new(None),
            restarting: Mutex::new(BTreeSet::new()),
            lane_pins: Mutex::new(None),
            closed: std::sync::atomic::AtomicBool::new(false),
            fresh_done: std::sync::atomic::AtomicBool::new(false),
            forward_checks: Mutex::new(BTreeMap::new()),
            lane,
        }))
    }

    pub fn stack_file(&self) -> &Path {
        &self.stack_file
    }

    /// The watcher saw the stack file change: compare with what this daemon
    /// read at start and tell the clients when the answer flips.
    pub fn stack_file_touched(&self) {
        // Gone is no change a restart applies: a lane's worktree being
        // removed takes its stack file first, and the lane after.
        if !self.stack_file.is_file() {
            return;
        }
        let changed = stack_file_hash(&self.stack_file) != self.stack_file_hash;
        if self.config_changed.swap(changed, Ordering::SeqCst) != changed {
            self.emit(EventBody::Config { changed });
        }
    }

    pub(crate) fn emit(&self, body: EventBody) {
        self.emit_with(body, None);
    }

    /// [`Self::emit`], with fields the diagnostic line has beyond the event.
    /// [`Self::emit`] without a diagnostic line: what only the dashboard
    /// needs to hear.
    fn emit_quiet(&self, body: EventBody) {
        let _ = self.events.send(Event {
            seq: self.seq.fetch_add(1, Ordering::SeqCst),
            at: now_ms(),
            body,
        });
    }

    pub(crate) fn emit_with(
        &self,
        body: EventBody,
        extra: Option<serde_json::Map<String, serde_json::Value>>,
    ) {
        record(&body, &self.lane, extra);
        let _ = self.events.send(Event {
            seq: self.seq.fetch_add(1, Ordering::SeqCst),
            at: now_ms(),
            body,
        });
    }

    fn set_status(&self, name: &str, status: Status) {
        self.set_status_of(name, None, status);
    }

    /// [`Self::set_status`], only while the slot still runs `generation`
    /// (`None`: whatever it runs): a health check that ended after a stop or
    /// a switch says nothing about what runs now. The event goes out under
    /// the same lock as the state, so the dashboard hears the states in the
    /// order they were set (a stop's `stopped` is never overtaken by a
    /// check's `unhealthy` from before it).
    pub(super) fn set_status_of(&self, name: &str, generation: Option<u64>, status: Status) {
        let mut slots = self.slots.lock().unwrap();
        let (build_id, start) = {
            let Some(slot) = slots.get_mut(name) else {
                return;
            };
            if generation.is_some_and(|g| g != slot.generation) {
                return;
            }
            if slot.view.status == status {
                return;
            }
            slot.view.status = status.clone();
            slot.view.since = now_ms();
            let start = match status {
                Status::Starting => {
                    slot.spawned = Some(Instant::now());
                    slot.starts += 1;
                    None
                }
                _ => slot
                    .spawned
                    .take()
                    .and_then(|t0| start_fields(&status, t0.elapsed(), slot.starts)),
            };
            (slot.view.build_id.clone(), start)
        };
        self.emit_with(
            EventBody::Service {
                service: name.into(),
                status,
                build_id,
            },
            start,
        );
        drop(slots);
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
                v.proxy = proxies.get(&v.name).map(Proxy::view).or_else(|| {
                    self.fronts
                        .lock()
                        .unwrap()
                        .get(&v.name)
                        .map(crate::tcpfwd::Front::view)
                });
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
        let profiles = self
            .stack
            .profiles
            .iter()
            .map(|(name, p)| crate::state::ProfileView {
                name: name.clone(),
                description: p.description.clone(),
                fresh: p.data == Some(devstack_config::model::DataMode::Fresh),
                command: p.command.clone(),
            })
            .collect();
        StackView {
            profile: self.stack.profile.as_ref().map(|p| p.name.clone()),
            profiles,
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
            no_main_ports: !self.main_slot_moves(),
            watcher_error: self
                .watcher_error
                .lock()
                .unwrap()
                .as_ref()
                .map(|(e, _)| e.clone()),
            notices: self.notices(),
        }
    }

    /// This tree's notices (ADR-0050), by id.
    pub(crate) fn notices(&self) -> Vec<Notice> {
        self.notices.lock().unwrap().values().cloned().collect()
    }

    /// Say `text` as the notice `id` while its condition holds (ADR-0050).
    /// The same again changes nothing; another text keeps when it began.
    /// `true` when it is new: the caller logs the condition then, as before.
    pub(crate) fn set_notice(&self, id: &str, level: NoticeLevel, text: String) -> bool {
        let mut all = self.notices.lock().unwrap();
        let since = match all.get(id) {
            Some(n) if n.level == level && n.text == text => return false,
            Some(n) => n.since,
            None => now_ms(),
        };
        let new = !all.contains_key(id);
        all.insert(
            id.to_string(),
            Notice {
                id: id.to_string(),
                level,
                text,
                since,
            },
        );
        let notices = all.values().cloned().collect();
        drop(all);
        self.emit_quiet(EventBody::Notices { notices });
        new
    }

    /// The notice `id` no longer holds.
    pub(crate) fn clear_notice(&self, id: &str) {
        self.clear_notices(|n| n == id);
    }

    /// Every notice whose id `gone` names no longer holds.
    pub(crate) fn clear_notices(&self, gone: impl Fn(&str) -> bool) {
        let mut all = self.notices.lock().unwrap();
        let before = all.len();
        all.retain(|id, _| !gone(id));
        if all.len() == before {
            return;
        }
        let notices = all.values().cloned().collect();
        drop(all);
        self.emit_quiet(EventBody::Notices { notices });
    }

    /// Something happened once that no request reports (ADR-0050 point 3):
    /// the dashboard shows it as a toast. The caller logs it.
    pub(crate) fn tell(&self, level: NoticeLevel, text: String) {
        self.emit_quiet(EventBody::Notice { level, text });
    }

    /// The file watcher failed to start or lost track: say so in the status
    /// and the log, once per new reason; `None` when it works again.
    pub(crate) fn set_watcher_error(&self, error: Option<String>) {
        let mut cur = self.watcher_error.lock().unwrap();
        if cur.as_ref().map(|(e, _)| e) == error.as_ref() {
            return;
        }
        if let Some(e) = &error {
            crate::diag::warn(
                "watcher_failed",
                serde_json::json!({ "lane": self.lane, "error": e }),
            );
        }
        *cur = error.clone().map(|e| (e, Instant::now()));
        drop(cur);
        // Logged above: the event is for the dashboard.
        self.emit_quiet(EventBody::Watcher { error });
    }

    /// The watcher delivers events again: a lost-events notice goes after a
    /// minute (long enough to be seen).
    pub(crate) fn watcher_ok(&self) {
        let mut cur = self.watcher_error.lock().unwrap();
        if cur
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed() >= Duration::from_secs(60))
        {
            *cur = None;
            drop(cur);
            self.emit_quiet(EventBody::Watcher { error: None });
        }
    }

    /// The daemon's one build job (ADR-0020), for the lanes' builders.
    pub(crate) fn build_job(&self) -> Arc<devstack_proc::BuildJob> {
        self.builder.job()
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
        // A lane on a node (ADR-0029 point 5): its repository is the node's
        // mirror; the developer's files are in the worktree it mirrors. The
        // snapshot maps straight there (the mirror's path, `/src`, may well
        // occur again inside a path).
        let to = crate::display_root()
            .map(str::to_string)
            .unwrap_or_else(|| self.repo.to_string_lossy().into_owned());
        text.replace(&snap_dir.to_string_lossy().into_owned(), &to)
    }

    /// Where the kept logs of `name` are (plan §11).
    pub fn log_dir(&self, name: &str) -> PathBuf {
        self.state_dir.join("logs").join(name)
    }

    pub fn service_log(&self, name: &str) -> Option<PathBuf> {
        self.slots
            .lock()
            .unwrap()
            .get(name)
            .and_then(|s| s.view.log.clone())
    }

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

    /// `svc` runs in this supervisor: on this platform, and in a lane only
    /// unless it says `lanes = false`.
    fn runs_here(&self, svc: &Service) -> bool {
        on_this_platform(svc) && (self.lane.is_empty() || svc.lanes)
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

    /// Building, starting or running: what a hand-over has to give back.
    pub(crate) fn service_active(&self, name: &str) -> bool {
        self.slots
            .lock()
            .unwrap()
            .get(name)
            .is_some_and(|s| s.view.status.is_active())
    }

    pub(crate) fn service_running(&self, name: &str) -> bool {
        self.slots
            .lock()
            .unwrap()
            .get(name)
            .is_some_and(|s| s.main.is_some())
    }

    /// The branch of this working tree changed (or is first known).
    pub fn set_branch(&self, branch: Option<String>) {
        *self.branch.lock().unwrap() = branch;
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

/// The error text of a build killed for a newer change.
pub(crate) const SUPERSEDED: &str = "superseded by a newer change";

/// The error text of a start, build or job wait that a `stop` ended.
const STOPPED: &str = "stopped while it started (`devstack stop`)";

/// How long a job may run without a `timeout` of its own.
const JOB_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// The path a change carries when the watcher lost events (an overflow, a
/// rescan): what changed is not known.
pub(crate) const RESCAN: &str = "(rescan)";

/// Logs kept per service: the newest starts (plan §11).
pub const KEEP_RUN_LOGS: usize = 20;
/// Logs kept per service: the newest builds, and the newest `prepare`s (plan §11).
pub const KEEP_BUILD_LOGS: usize = 10;
/// Artifacts kept per service beyond the pinned ones (ADR-0002 retention).
pub const KEEP_ARTIFACTS: usize = 5;

/// Quiet period after the last change before a candidate is built (plan §3.2).
const DEBOUNCE: Duration = Duration::from_secs(2);
/// How long a lane's worktree has to be quiet before its copy follows: short,
/// a dev server's hot reload waits for it.
const COPY_QUIET: Duration = Duration::from_millis(150);

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

/// The last `n` lines of a log, read from its end (a long log is not read
/// whole; output that is not UTF-8, an OEM code page, is shown lossily).
fn tail(path: &Path, n: usize) -> String {
    crate::logfiles::last_lines(path, n, None).join("\n")
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
/// A port held on every address (`0.0.0.0`) is told on Windows by the table
/// of listeners, on macOS by a connect (#226).
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
        || (cfg!(windows) && devstack_proc::tcp_listening(port))
        || (cfg!(target_os = "macos") && answers_locally(port))
}

/// Does something accept a connection on `port` of a loopback address? Asked
/// on macOS only: with BSD sockets a bind on `127.0.0.1` succeeds while
/// another process listens on `*:port` (std sets `SO_REUSEADDR`), and that
/// listener answers. Not on Windows, where a connect to a closed loopback
/// port takes seconds of retries.
fn answers_locally(port: u16) -> bool {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
    [
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(Ipv6Addr::LOCALHOST),
    ]
    .into_iter()
    .any(|ip| {
        TcpStream::connect_timeout(&SocketAddr::new(ip, port), Duration::from_millis(250)).is_ok()
    })
}

/// "port N is already in use by another process", with that process when
/// the system says which (`node.exe, pid 4711`): a leftover of an earlier
/// run is told apart from another program.
pub fn port_taken(port: u16) -> String {
    match port_holder(port) {
        Some(who) => format!("port {port} is already in use by another process ({who})"),
        None => format!("port {port} is already in use by another process"),
    }
}

/// The process listening on TCP `port` here, as `name, pid N`, if the
/// system's tools say (a few seconds at most).
pub fn port_holder(port: u16) -> Option<String> {
    let run = |cmd: &str, args: &[&str]| -> Option<String> {
        let mut c = std::process::Command::new(cmd);
        c.args(args);
        devstack_compose::no_window(&mut c);
        let out = devstack_compose::output_within(c, std::time::Duration::from_secs(3))
            .ok()
            .flatten()?;
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    };
    if cfg!(windows) {
        let pid = netstat_pid(&run("netstat", &["-ano", "-p", "TCP"])?, port).or_else(|| {
            netstat_pid(
                &run("netstat", &["-ano", "-p", "TCPv6"]).unwrap_or_default(),
                port,
            )
        })?;
        let filter = format!("PID eq {pid}");
        let name =
            run("tasklist", &["/FI", &filter, "/FO", "CSV", "/NH"]).and_then(|t| tasklist_name(&t));
        Some(match name {
            Some(n) => format!("{n}, pid {pid}"),
            None => format!("pid {pid}"),
        })
    } else if cfg!(target_os = "macos") {
        let spec = format!("-iTCP:{port}");
        lsof_holder(&run("lsof", &["-nP", &spec, "-sTCP:LISTEN", "-Fpc"])?)
    } else {
        let filter = format!("sport = :{port}");
        ss_holder(&run("ss", &["-Hltnp", &filter])?)
    }
}

/// The pid listening on `port` in `netstat -ano` (Windows). A listening row
/// has no remote end (`0.0.0.0:0`, `[::]:0`); its state is written in the
/// system's language (`LISTENING`, `ABHÖREN`), so it is not read.
fn netstat_pid(text: &str, port: u16) -> Option<u32> {
    let suffix = format!(":{port}");
    text.lines().find_map(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        (f.len() >= 5
            && f[0] == "TCP"
            && f[1].ends_with(&suffix)
            && matches!(f[2], "0.0.0.0:0" | "[::]:0"))
        .then(|| f[4].parse().ok())
        .flatten()
    })
}

/// The image name in `tasklist /FO CSV /NH` (`"node.exe","4711",...`).
fn tasklist_name(text: &str) -> Option<String> {
    let first = text.lines().next()?.strip_prefix('"')?;
    let name = first.split('"').next()?;
    (!name.is_empty() && !name.starts_with("INFO:")).then(|| name.to_string())
}

/// `name, pid N` from `ss -Hltnp` (`users:(("node",pid=4711,fd=20))`).
fn ss_holder(text: &str) -> Option<String> {
    let users = text
        .lines()
        .find_map(|l| l.split_once("users:((").map(|(_, u)| u))?;
    let name = users.strip_prefix('"')?.split('"').next()?;
    let pid = users
        .split("pid=")
        .nth(1)?
        .split(|c: char| !c.is_ascii_digit())
        .next()?;
    Some(format!("{name}, pid {pid}"))
}

/// `name, pid N` from `lsof -Fpc` (`p4711` then `cnode`).
fn lsof_holder(text: &str) -> Option<String> {
    let pid = text.lines().find_map(|l| l.strip_prefix('p'))?;
    match text.lines().find_map(|l| l.strip_prefix('c')) {
        Some(name) => Some(format!("{name}, pid {pid}")),
        None => Some(format!("pid {pid}")),
    }
}

#[cfg(test)]
mod holder_tests {
    use super::*;

    #[test]
    fn the_holder_of_a_port_is_read_from_the_systems_tools() {
        let netstat = "\r\nActive Connections\r\n\r\n  Proto  Local Address          Foreign Address        State           PID\r\n  TCP    0.0.0.0:135            0.0.0.0:0              LISTENING       1180\r\n  TCP    127.0.0.1:8091         0.0.0.0:0              LISTENING       4711\r\n  TCP    127.0.0.1:8091         127.0.0.1:50000        ESTABLISHED     4711\r\n";
        assert_eq!(netstat_pid(netstat, 8091), Some(4711));
        let german =
            "  TCP    [::1]:8091             [::]:0                 ABHÖREN         4712\r\n";
        assert_eq!(netstat_pid(german, 8091), Some(4712));
        assert_eq!(netstat_pid(netstat, 18091), None);
        assert_eq!(netstat_pid(netstat, 809), None);
        assert_eq!(
            tasklist_name("\"node.exe\",\"4711\",\"Console\",\"1\",\"52.312 K\"\r\n").as_deref(),
            Some("node.exe")
        );
        assert_eq!(
            tasklist_name("INFO: No tasks are running which match the specified criteria.\r\n"),
            None
        );
        assert_eq!(
            ss_holder("LISTEN 0 511 127.0.0.1:8091 0.0.0.0:* users:((\"node\",pid=4711,fd=20))\n")
                .as_deref(),
            Some("node, pid 4711")
        );
        assert_eq!(
            lsof_holder("p4711\ncnode\n").as_deref(),
            Some("node, pid 4711")
        );
    }
}

#[cfg(test)]
mod port_tests {
    #[test]
    fn a_start_says_how_long_it_took_unless_it_only_waited() {
        use super::{STOPPED, Status, start_fields};
        use std::time::Duration;
        let took = Duration::from_millis(1234);
        let f = start_fields(&Status::Healthy, took, 3).unwrap();
        assert_eq!(f["ms"], 1234);
        assert_eq!(f["result"], "ok");
        assert_eq!(f["restarts"], 2);
        let f = start_fields(&Status::Failed { reason: "x".into() }, took, 1).unwrap();
        assert_eq!(
            (&f["result"], &f["restarts"]),
            (&"failed".into(), &0.into())
        );
        let f = start_fields(&Status::Completed { exit_code: 0 }, took, 1).unwrap();
        assert_eq!(f["ms"], 1234);
        // Without a health check: a fixed wait, no duration.
        let f = start_fields(&Status::Running, took, 1).unwrap();
        assert!(f.get("ms").is_none());
        assert_eq!(f["settle_ms"], 500);
        // A start that was stopped is no start that took long.
        assert!(start_fields(&Status::Stopped, took, 1).is_none());
        assert!(
            start_fields(
                &Status::Failed {
                    reason: STOPPED.into()
                },
                took,
                1
            )
            .is_none()
        );
    }

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
    fn a_port_held_on_every_address_is_in_use() {
        // Docker's published ports, a dev server on 0.0.0.0. On Windows and
        // with BSD sockets (macOS) a bind on 127.0.0.1 still succeeds; the
        // table of listeners tells (Windows), or a connect (macOS).
        let l = std::net::TcpListener::bind(("0.0.0.0", 0)).unwrap();
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
mod notice_tests {
    use super::*;

    #[test]
    fn a_notice_holds_until_it_is_cleared_and_each_change_is_told() {
        let d = tempfile::tempdir().unwrap();
        let file = d.path().join("devstack.toml");
        let stack =
            devstack_config::from_str("version = 1\n[stack]\nname = \"s\"\n", &file).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let sup = Supervisor::new(
            stack,
            file,
            d.path().to_path_buf(),
            d.path().join(".devstack"),
            devstack_proc::Options::default(),
            rt.handle().clone(),
        )
        .unwrap();
        let mut rx = sup.events.subscribe();
        assert!(sup.set_notice("copy", NoticeLevel::Warn, "behind".into()));
        assert!(
            !sup.set_notice("copy", NoticeLevel::Warn, "behind".into()),
            "the same again"
        );
        let since = sup.notices()[0].since;
        assert!(!sup.set_notice("copy", NoticeLevel::Error, "still behind".into()));
        let n = &sup.view().notices[0];
        assert_eq!(
            (n.level, n.text.as_str()),
            (NoticeLevel::Error, "still behind")
        );
        assert_eq!(n.since, since, "it began when it began");
        sup.clear_notice("other");
        sup.clear_notices(|id| id.starts_with("co"));
        assert!(sup.view().notices.is_empty());
        let mut told = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            if let EventBody::Notices { notices } = ev.body {
                told.push(notices.len());
            }
        }
        assert_eq!(told, [1, 1, 0], "set, changed, cleared");
    }
}
