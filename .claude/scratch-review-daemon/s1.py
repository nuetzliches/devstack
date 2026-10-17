import sys; sys.path.insert(0,'D:/Sources/devstack/.claude/scratch-review-daemon')
from rep import apply
P='D:/Sources/devstack/.claude/worktrees/review-daemon/crates/devstack-daemon/src/supervisor.rs'
apply(P,[
("""use std::sync::atomic::{AtomicU64, Ordering};""","""use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};"""),
("""    /// Bumped by every start, stop and switch; a monitor of an older generation retires.
    generation: u64,
}
""","""    /// Bumped by every start, stop and switch; a monitor of an older generation retires.
    generation: u64,
    /// The build a start or a candidate is about to run, from the moment its
    /// artifact is chosen until it runs (or does not): pinned against
    /// pruning meanwhile (ADR-0002 retention).
    in_flight: Option<String>,
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
"""),
("""    /// Built candidates waiting for `promote` (ADR-0013), per service.
    held: Mutex<BTreeMap<String, (String, PathBuf)>>,""","""    /// Built candidates waiting for `promote` (ADR-0013), per service;
    /// persisted, so a daemon restart keeps the gate.
    held: Mutex<BTreeMap<String, Held>>,
    /// Per service, bumped by every `stop`: a start, build or job wait that
    /// began before gives up (a hanging migrator must not need the daemon
    /// killed).
    stops: BTreeMap<String, AtomicU64>,
    /// Main lane only: `promote --lane` is moving the main ports to a lane;
    /// nothing of this tree may take a proxy's upstream meanwhile (ADR-0008).
    handing_over: AtomicBool,
    /// Services started at least once by this daemon: the first start after
    /// a daemon start restores the last promoted artifact (ADR-0016).
    started: Mutex<BTreeSet<String>>,
    /// The snapshot's manifest as the previous daemon left it: what changed
    /// in the tree while no daemon watched it, against the tree now.
    opened_manifest: Mutex<Option<devstack_build::snapshot::Manifest>>,
    /// Why the file watcher does not work, if it does not (no candidates
    /// then): shown in the status, not only logged.
    watcher_error: Mutex<Option<String>>,"""),
("""/// Hash of the stack file, line endings ignored (a checkout may flip them).
/// Every state change also goes to the diagnostic log (plan §11): the event as""","""/// Every state change also goes to the diagnostic log (plan §11): the event as"""),
("""fn stack_file_hash(path: &Path) -> Option<blake3::Hash> {""","""/// Hash of the stack file, line endings ignored (a checkout may flip them).
fn stack_file_hash(path: &Path) -> Option<blake3::Hash> {"""),
("""            String::new(),
            0,
            Arc::new(Mutex::new(None)),
        )
    }""","""            String::new(),
            0,
            Arc::new(Mutex::new(None)),
            None,
        )
    }"""),
("""    /// by `port_offset` ([`crate::lanes`]) and runs its own Compose projects,
    /// their ports moved by the same offset (ADR-0030).""","""    /// by `port_offset` ([`crate::lanes`]) and runs its own Compose projects,
    /// their ports moved by the same offset (ADR-0030). `build_job`: the
    /// daemon's one build job (ADR-0020); `None` makes it (the main tree)."""),
("""        port_offset: u16,
        main_owner: Arc<Mutex<Option<String>>>,
    ) -> Result<Arc<Self>, String> {""","""        port_offset: u16,
        main_owner: Arc<Mutex<Option<String>>>,
        build_job: Option<Arc<devstack_proc::BuildJob>>,
    ) -> Result<Arc<Self>, String> {"""),
("""        let builder = Builder::new(layout.clone(), &proc_opts).map_err(|e| e.to_string())?;
        let snapshot = Snapshot::open(&repo, &layout.lane("main")).map_err(|e| e.to_string())?;""","""        let builder = match build_job {
            Some(job) => Builder::with_job(layout.clone(), job),
            None => Builder::new(layout.clone(), &proc_opts).map_err(|e| e.to_string())?,
        };
        let snapshot = Snapshot::open(&repo, &layout.lane("main")).map_err(|e| e.to_string())?;
        let opened_manifest = Some(snapshot.manifest.clone());"""),
("""                    main: None,
                    previous: None,
                    generation: 0,
                },
            );
            op_locks.insert(name.clone(), Mutex::new(()));
        }""","""                    main: None,
                    previous: None,
                    generation: 0,
                    in_flight: None,
                },
            );
            op_locks.insert(name.clone(), Mutex::new(()));
            stops.insert(name.clone(), AtomicU64::new(0));
        }"""),
("""        let mut op_locks = BTreeMap::new();
        for (name, svc) in &stack.services {""","""        let mut op_locks = BTreeMap::new();
        let mut stops = BTreeMap::new();
        for (name, svc) in &stack.services {"""),
("""        devstack_proc::cleanup(&persisted.records);
        let (events, _) = broadcast::channel(1024);""","""        devstack_proc::cleanup(&persisted.records);
        // A held candidate stays held across a daemon restart (ADR-0013).
        let held = persisted
            .held
            .iter()
            .filter(|(name, _)| stack.services.contains_key(*name))
            .map(|(name, h)| {
                let dir = layout.artifact(name, &h.build_id);
                let artifact = (!h.build_id.is_empty() && dir.is_dir())
                    .then(|| (h.build_id.clone(), dir));
                (
                    name.clone(),
                    Held {
                        artifact,
                        reason: h.reason.clone(),
                    },
                )
            })
            .collect();
        let (events, _) = broadcast::channel(1024);"""),
("""            persisted: Mutex::new(Persisted {
                main: persisted.main,
                records: Vec::new(),
            }),""","""            persisted: Mutex::new(Persisted {
                main: persisted.main,
                records: Vec::new(),
                held: persisted.held,
            }),"""),
("""            held: Mutex::new(BTreeMap::new()),
            quiet: Mutex::new(BTreeMap::new()),""","""            held: Mutex::new(held),
            stops,
            handing_over: AtomicBool::new(false),
            started: Mutex::new(BTreeSet::new()),
            opened_manifest: Mutex::new(opened_manifest),
            watcher_error: Mutex::new(None),
            quiet: Mutex::new(BTreeMap::new()),"""),
("""            main_served_by: self.served_by(),
        }
    }""","""            main_served_by: self.served_by(),
            watcher_error: self.watcher_error.lock().unwrap().clone(),
        }
    }

    /// The file watcher failed to start or lost track: say so in the status
    /// and the log, once per new reason; `None` when it works again.
    pub(crate) fn set_watcher_error(&self, error: Option<String>) {
        let mut cur = self.watcher_error.lock().unwrap();
        if *cur == error {
            return;
        }
        if let Some(e) = &error {
            crate::diag::warn(
                "watcher_failed",
                serde_json::json!({ "lane": self.lane, "error": e }),
            );
        }
        *cur = error;
    }

    /// The daemon's one build job (ADR-0020), for the lanes' builders.
    pub(crate) fn build_job(&self) -> Arc<devstack_proc::BuildJob> {
        self.builder.job()
    }"""),
])
print("ok")
