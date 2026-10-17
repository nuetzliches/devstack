import sys; sys.path.insert(0,'D:/Sources/devstack/.claude/scratch-review-daemon')
from rep import apply
P='D:/Sources/devstack/.claude/worktrees/review-daemon/crates/devstack-daemon/src/supervisor.rs'
apply(P,[
("""    /// Build a candidate from the current tree and promote it unless a gate
    /// holds it. Returns true if a newer change superseded the build.
    fn candidate_from(self: &Arc<Self>, name: &str, paths: &BTreeSet<String>, epoch: u64) -> bool {
        let Ok(svc) = self.service(name).cloned() else {""",
"""    /// Build a candidate from the current tree and promote it unless a gate
    /// holds it. Returns true if a newer change superseded the build.
    fn candidate_from(self: &Arc<Self>, name: &str, paths: &BTreeSet<String>, epoch: u64) -> bool {
        let superseded = self.candidate_inner(name, paths, epoch);
        if let Some(s) = self.slots.lock().unwrap().get_mut(name) {
            s.in_flight = None;
        }
        superseded
    }

    fn candidate_inner(
        self: &Arc<Self>,
        name: &str,
        paths: &BTreeSet<String>,
        epoch: u64,
    ) -> bool {
        let Ok(svc) = self.service(name).cloned() else {"""),
("""        let newer = || {
            self.changes
                .lock()
                .unwrap()
                .get(name)
                .is_some_and(|c| c.epoch != epoch)
        };""",
"""        // A newer change supersedes the build; a stop ends it.
        let stops = self.stop_epoch(name);
        let newer = || {
            self.stopped_since(name, stops)
                || self
                    .changes
                    .lock()
                    .unwrap()
                    .get(name)
                    .is_some_and(|c| c.epoch != epoch)
        };"""),
("""                Ok(a) => a,
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
                }""",
"""                Ok(a) => a,
                Err(e) if e == SUPERSEDED && self.stopped_since(name, stops) => {
                    clear();
                    return false;
                }
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
                    // A held candidate's gate stays; there is nothing to
                    // promote until the tree builds again.
                    let reason = self.held.lock().unwrap().get(name).map(|h| h.reason.clone());
                    if let Some(reason) = reason {
                        self.hold(name, None, reason);
                    }
                    return false;
                }"""),
("""                    "why": "the changed files are not inputs of its build id (watch paths, build files above them)",
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
            self.set_candidate(""",
"""                    "why": "the changed files are not inputs of its build id (watch paths, build files above them)",
                }),
            );
            // The tree is back where main is: nothing waits for promote.
            self.release_held(name, None);
            clear();
            return false;
        }
        // A held candidate blocks later auto-promotions (ADR-0013): the
        // newer build carries its change too, and waits in its place.
        let waiting = self.held.lock().unwrap().get(name).map(|h| h.reason.clone());
        if let Some(reason) = hold_reason(&svc, paths).or(waiting) {
            self.hold(name, Some(artifact.clone()), reason.clone());
            self.set_candidate("""),
("""        self.set_candidate(name, None);
        self.stop(name)?;
        self.start(name)
    }

    /// Run `action` once the changes for `key` have been quiet for a moment.""",
"""        self.set_candidate(name, None);
        self.stop(name)?;
        self.start_with(name, Some(artifact))
    }

    /// Run `action` once the changes for `key` have been quiet for a moment."""),
("""    /// `restart_on` of a Compose service: restart the container.
    /// `start`, `stop` or `restart` for every member of a group (plan §6).""",
"""    /// `start`, `stop` or `restart` for every member of a group (plan §6)."""),
("""    /// A container's output (`docker compose logs`, timestamped).
    /// The branch of this working tree changed (or is first known).""",
"""    /// The branch of this working tree changed (or is first known)."""),
("""    pub fn container_logs(
        &self,""",
"""    /// A container's output (`docker compose logs`, timestamped).
    pub fn container_logs(
        &self,"""),
("""    pub(crate) fn restart_container(&self, group: &str, service: &str) {""",
"""    /// `restart_on` of a Compose service: restart the container.
    pub(crate) fn restart_container(&self, group: &str, service: &str) {"""),
("""        let svc = self.service(name)?.clone();
        let Some(artifact) = self.held.lock().unwrap().remove(name) else {
            return Err(format!("no candidate of `{name}` waits for promotion"));
        };
        self.take_over(name, &svc, artifact)
    }

    /// Start what `svc` comes after and is not ready yet (services, jobs that
    /// have not completed). Compose services are the infra plane's job (`up`).
    /// The published ports of a Compose service, as this machine reaches them.""",
"""        let svc = self.service(name)?.clone();
        let Some(held) = self.held.lock().unwrap().get(name).cloned() else {
            return Err(format!("no candidate of `{name}` waits for promotion"));
        };
        let Some(artifact) = held.artifact else {
            return Err(format!(
                "the newest build of `{name}` failed: fix it, and the next build waits here for `devstack promote {name}`"
            ));
        };
        let id = artifact.0.clone();
        // Released only once it is main: a failed take-over keeps it held.
        self.take_over(name, &svc, artifact)?;
        self.release_held(name, Some(&id));
        Ok(())
    }

    /// The published ports of a Compose service, as this machine reaches them."""),
("""    fn ensure_after(self: &Arc<Self>, svc: &Service) -> Result<(), String> {""",
"""    /// Start what `svc` comes after and is not ready yet (services, jobs that
    /// have not completed). Compose services are the infra plane's job (`up`).
    fn ensure_after(self: &Arc<Self>, svc: &Service) -> Result<(), String> {"""),
("""/// Quiet period after the last change before a candidate is built (plan §3.2).
/// Logs kept per service: the newest starts and builds (plan §11).
pub const KEEP_RUN_LOGS: usize = 20;
/// Artifacts kept per service beyond the pinned ones (ADR-0002 retention).
pub const KEEP_ARTIFACTS: usize = 5;
pub const KEEP_BUILD_LOGS: usize = 10;

const DEBOUNCE: Duration = Duration::from_secs(2);""",
"""/// The error text of a start, build or job wait that a `stop` ended.
const STOPPED: &str = "stopped while it started (`devstack stop`)";

/// How long a job may run without a `timeout` of its own.
const JOB_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// The path a change carries when the watcher lost events (an overflow, a
/// rescan): what changed is not known.
pub(crate) const RESCAN: &str = "(rescan)";

/// Logs kept per service: the newest starts and builds (plan §11).
pub const KEEP_RUN_LOGS: usize = 20;
/// Artifacts kept per service beyond the pinned ones (ADR-0002 retention).
pub const KEEP_ARTIFACTS: usize = 5;
pub const KEEP_BUILD_LOGS: usize = 10;

/// Quiet period after the last change before a candidate is built (plan §3.2).
const DEBOUNCE: Duration = Duration::from_secs(2);"""),
("""    if !p.auto {
        return Some("auto-promotion is off".into());
    }
    let set = devstack_build::watch_set(&p.hold_if_changed)
        .ok()
        .flatten()?;""",
"""    if !p.auto {
        return Some("auto-promotion is off".into());
    }
    let set = devstack_build::watch_set(&p.hold_if_changed)
        .ok()
        .flatten()?;
    if paths.contains(RESCAN) {
        return Some(
            "the file watcher lost events, so whether schema inputs changed is not known".into(),
        );
    }"""),
("""fn tail(path: &Path, n: usize) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().rev().take(n).collect();
    lines.into_iter().rev().collect::<Vec<_>>().join("\\n")
}""",
"""/// The last `n` lines of a log, read from its end (a long log is not read
/// whole; output that is not UTF-8, an OEM code page, is shown lossily).
fn tail(path: &Path, n: usize) -> String {
    crate::logfiles::last_lines(path, n, None).join("\\n")
}"""),
])
print("ok")
