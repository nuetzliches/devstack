# ADR-0009: The daemon owns every process tree it starts

- **Status:** Proposed
- **Date:** 2026-10-14
- **Related:** [ADR-0005](0005-rust-for-daemon-and-cli.md), plan phase 0 spike (a)

## Context

`dotnet run`, `dotnet watch`, `npm` and `ng` start wrapper processes whose
children outlive them when the wrapper is killed. In the reference stack this
left processes holding ports and files, an orchestrator that reported "stopped"
while the process lived, and a launcher script that needed a hand-written
console-control helper to stop things politely. A hard kill skips graceful
shutdown: claimed jobs, unacknowledged queue messages and open transactions are
left behind.

## Decision

Every process devstack starts (builds included) is placed in an OS-level
container that guarantees termination of the whole tree:

- **Windows:** a Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`; the
  daemon holds the handle, so the tree dies with the daemon even if the daemon
  crashes. Each service gets its own console process group so a Ctrl+Break can be
  delivered to it without touching others (Ctrl+C is disabled for a new
  process group by Windows).
  The guarantee holds only if (1) the job handle is **never inheritable**, since a
  child holding a copy keeps the job open and nothing is killed, and (2)
  **breakaway is not allowed** (`JOB_OBJECT_LIMIT_BREAKAWAY_OK` unset), so no
  process can leave the job with `CREATE_BREAKAWAY_FROM_JOB`.
- **Unix:** a process group (and, on Linux, `PR_SET_PDEATHSIG` for the leader).
  This covers only the direct child: grandchildren (`dotnet run` → app,
  `npm` → `node`) survive a daemon crash. Linux therefore uses a cgroup v2 per
  service where delegation is available, and the restart cleanup of
  [ADR-0016](0016-a-daemon-restart-is-a-clean-restart.md) otherwise.

Stopping is two-stage: graceful signal, wait up to a per-service timeout
(default 20 s), then terminate the job/group. Services are started from their
real executable where possible, not through wrappers.

## Spike (a) results (2026-10-14, Windows)

Verified with `spikes/process-ownership` (details in its README):

- A hard kill of the supervisor leaves **no** process of any tree, including a
  detached, unref'd grandchild and a `dotnet watch` that creates job objects of
  its own (nested jobs work).
- Both conditions above are real: with an inheritable job handle every process
  survived the supervisor; with breakaway allowed a child escaped. With both
  guarded, breakaway is refused (access denied).
- **Graceful stop uses `CTRL_BREAK_EVENT` to the service's process group.**
  Services are started with `CREATE_NEW_PROCESS_GROUP` on the daemon's console;
  .NET hosts run their regular shutdown, node receives `SIGBREAK`, 100 ms each,
  and the signal reaches only the addressed service. A daemon started without a
  console allocates a hidden one at start; without it the signal fails and every
  service opens a console window of its own.
- A pseudo console per service (ConPTY) stopped .NET but not node; it is not
  used for stopping until that is understood.

## Spike (a) results (2026-10-14, Linux container)

- A process group or a cgroup alone does **not** die with the supervisor (8 of
  11 processes survived a hard kill in both cases).
- **A shim per service plus a cgroup per service gives the Windows guarantee:**
  the shim receives `PR_SET_PDEATHSIG` when the daemon dies and kills the
  service's cgroup (`cgroup.kill`) and process group; nothing survived, before
  any cleanup. The cgroup is what catches a child that left the process group
  with `setsid`.
- Without writable cgroups the shim still kills the process group; `setsid`
  children escape. That is the documented degradation.
- Graceful stop: `SIGTERM` to the process group; the shim survives it through a
  no-op handler (not `SIG_IGN`, which the service would inherit).

The Unix bullet of the decision therefore reads, after acceptance: **a shim per
service, holding the service in its own cgroup v2 where delegation is available
and in its own process group always.**

## Build servers (spike (b), 2026-10-14)

`dotnet build` starts long-lived servers (MSBuild node reuse, the shared C#
compiler), 32 processes for the reference host. Started inside a per-build job
they die with every build, and every build is cold: 31–35 s instead of 12–16 s
with warm servers (42 s with servers switched off). They must outlive a build
and still die with the daemon. Because a process inherits its parent's job,
that needs a deliberate design; candidates, to be decided in phase 1:

- one **daemon-wide build job** (kill-on-close) for all builds, and cancellation
  of a single build by terminating its process tree **except** recognised build
  servers;
- servers started by the daemon itself in its own job before the first build
  (would need a supported way to start them; none is documented);
- per-build jobs and a deliberately slower build (rejected unless the others
  fail).

Also: a build's root process exits while its servers stay in the job, so a
build is finished when its root exits, not when its job is empty.

## Alternatives considered

- **Kill by PID tree walk.** Races with processes spawned during shutdown;
  measured to miss them in the reference launcher.
- **Rely on the child's own shutdown handling.** Fails exactly when it matters.

## Consequences

- Tools that create their own Job Objects (some test runners, IDE debuggers)
  need nested-job support (Windows 8+); verified in spike (a).
- Attaching a debugger to a supervised process works; launching one from the
  IDE bypasses devstack and is out of scope for the main slot.

## Enforced by

- Platform process module (planned), one implementation per OS behind a trait.
