# ADR-0016: A daemon restart is a clean restart from the last promoted artifacts

- **Status:** Accepted (2026-10-14)
- **Date:** 2026-10-14
- **Deciders:** project owner
- **Related:** [ADR-0002](0002-processes-run-from-immutable-artifacts.md), [ADR-0009](0009-the-daemon-owns-every-process-tree.md), plan phase 0 spike (a)

## Context

ADR-0009 ties every application process to the daemon: when the daemon dies,
its process trees die with it. That is intended, because orphaned processes
holding ports and files were the reference stack's most expensive failure. It
raises two questions: what happens to the stack when the daemon crashes or is
updated, and what is left behind that a restarted daemon has to deal with.

The alternative to dying with the daemon is re-attaching: processes outlive the
daemon and a new daemon instance adopts them. That requires processes that are
not bound to the daemon's lifetime (reintroducing orphans), and output that does
not flow through pipes to the daemon (a writer to a pipe whose reader died gets
errors and may crash), in practice a per-process shim like `containerd-shim`.

## Decision

devstack does not re-attach. A daemon restart, after a crash, a kill or an
update, is a **clean restart**:

1. **Single instance by OS lock.** The daemon holds an exclusive lock on a file
   in its state directory through an open handle. The OS releases it on any
   exit, so there is never a stale lock to break and never a PID file to trust.
2. **Clean up.** On Windows nothing is left by construction (ADR-0009). On
   Linux the daemon kills what its previous instance recorded: per-service
   cgroups (`cgroup.kill`) where cgroup v2 delegation is available, otherwise
   recorded process groups, matched by PID **and** start time so a reused PID is
   never hit. Incomplete builds are removed (artifacts are written to a temporary
   directory and renamed atomically only on success, ADR-0002).
3. **Leave infrastructure alone.** Compose groups survive the daemon; they are
   checked for health, not restarted.
4. **Restore the previous state without building.** The daemon persists, per
   lane and service, which artifact is in the main slot (and which candidates
   were held). It starts each service from that artifact. Candidates are not
   restored; the next change produces new ones.

A daemon update is the same path preceded by a graceful stop of every service.

devstack does not restart itself after a crash. CLI, MCP and dashboard report
"daemon not running" and offer `devstack up`.

Verified on Windows by spike (a): a second daemon on the same state directory
is refused; after a hard kill of the first, the lock is free at once and the
new instance starts. On Linux (container) the same holds for `flock`, the
cleanup by `cgroup.kill` removes everything a dead instance left, the cleanup by
recorded process group removes all but `setsid` children, and a process that
only shares a number with a recorded group is spared (start-time check).

## Alternatives considered

- **Re-attach to running processes.** Saves the restart of the application
  processes on a daemon update. Costs a shim per process, file-only logging, and
  brings back the orphan class of failures. Rejected; daemon updates are rare.
- **Rebuild everything on restart.** Simple, but turns every daemon restart into
  minutes of building and may promote code that was never promoted before.
- **Autostart as a Windows service or scheduled task.** Hides crashes, adds an
  installation step, and a service runs in another session than the user's
  terminals and IDE. May be revisited as an opt-in.

## Consequences

- A daemon crash costs one start-up of every application process (seconds),
  plus whatever the applications do on a hard kill (job claims and queue leases
  expire or are taken over, open transactions roll back). Applications must
  already survive that; a crash simply exercises it.
- The persisted state is small and owned by the daemon (slot assignments, build
  history, held candidates); its format is internal and may change between
  versions with a migration on start.
- On Linux without delegated cgroups, cleanup depends on recorded process groups;
  a process that changes its own process group escapes. Documented, accepted.

## Enforced by

- Daemon start-up sequence (planned): lock, cleanup, health check of Compose
  groups, restore.
- Spike (a): kill the daemon hard and assert that no process of the stack
  survives (Windows); same with cleanup on restart (Linux).

*Paths as built, noted 2026-10-18.* `crates/devstack-daemon/src/lib.rs`
(`serve`: `StateLock::acquire` from `crates/devstack-proc/src/lock.rs`),
`Supervisor::new` (`devstack_proc::cleanup` of the recorded process groups
and cgroups), `crates/devstack-daemon/src/state.rs` (what `state.json`
keeps: main's builds, the promotion gate), and `restorable` /
`follow_restore` in `crates/devstack-daemon/src/supervisor/`: the first
start of a service after a daemon start runs the build last promoted to main
without building (`restored` in the daemon log, since 0.10.0); what changed
in the tree meanwhile becomes a candidate. Tests:
`crates/devstack-proc/tests/ownership.rs` (`supervisor_death_ends_every_tree`)
and `lifecycle` in `crates/devstack/tests/daemon_lifecycle.rs` (a daemon
killed hard, then restored from the promoted build, not built).

## Addendum 2026-11-18: without `watch`, the first start builds

Point 4 holds for a service whose changes arrive as candidates, one with
`watch`. Without `watch` no change makes a candidate (ADR-0003), so a
restored build would run until a `restart`, whatever the tree held: a
static bundle in a lane that was stopped and started again served the code
of its first run, and an acceptance suite ran against it (#562). A service
or job with `build` and without `watch` therefore goes through its build at
the first start after a daemon start. The build id covers every file of the
tree then (ADR-0002), so an unchanged tree reuses its artifact at once
(`build_reused` in the daemon log) and only a changed one builds; `up`
reports the service healthy once the tree's code runs.

Not chosen: restoring and then making a candidate when the tree's build id
differs. The start stays fast, but `up --run` and a command after `up`
would run against the old build while the candidate builds. Nor a `watch`
key on a profile's service: the fix needs no change to the stack file.

Owner's approval 2026-11-18 (review card on the pull request). Built in
`restorable` (`crates/devstack-daemon/src/supervisor/artifacts.rs`); test
`without_watch_the_first_start_builds_from_the_tree` in
`crates/devstack/tests/daemon_lifecycle.rs`.
