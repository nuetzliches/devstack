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
  crashes. Each service gets its own console process group so a Ctrl+C/Ctrl+Break
  can be delivered to it without touching others.
- **Unix:** a process group (and, on Linux, `PR_SET_PDEATHSIG` for the leader).

Stopping is two-stage: graceful signal, wait up to a per-service timeout
(default 20 s), then terminate the job/group. Services are started from their
real executable where possible, not through wrappers.

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
