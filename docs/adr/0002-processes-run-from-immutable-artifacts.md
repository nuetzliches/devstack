# ADR-0002: Processes run from immutable artifacts, never from the build output of the source tree

- **Status:** Proposed
- **Date:** 2026-10-14
- **Related:** [problem statement, root cause A](../problem-statement.md#root-cause-a-building-and-running-share-one-output-directory), [plan §3.2](../plan.md#32-change--candidate--main)

## Context

When a process runs from the same directory a build writes to, three things go
wrong at once: the build fails on locked files (Windows), the output is left
partly old without an error, and nothing can tell which code is running. In the
reference stack this produced endpoints answering 404 next to their own source,
fields silently ignored, and an unkillable zombie locking the output directory.
A second writer to the same `bin/`/`obj/` (the IDE) adds its own collisions.

## Decision

devstack builds every application service from a **snapshot** of the lane's
source (`.devstack/lanes/<lane>/src`) into an **artifact directory** named by the
input hash (`.devstack/artifacts/<service>/<build-id>/`). Processes are started
only from artifact directories, which are never written again after the build
succeeds. A build writes into a temporary directory that is renamed to its
final name only on success, so an artifact directory is either complete or
absent, also after a crash ([ADR-0016](0016-a-daemon-restart-is-a-clean-restart.md)).
The build id is passed to the process (`DEVSTACK_BUILD_ID`) and shown
by every client.

The snapshot copies changed files only and stamps copied files with the current
time, so incremental build tools see them as newer than their outputs. Snapshot
directories persist to keep incremental state warm.

## Alternatives considered

- **Build in place, stop the process first.** Removes the lock, keeps the
  downtime for the whole build, still breaks on a second writer (IDE), and gives
  no rollback.
- **Build in place, copy output to an artifact afterwards.** No lock on the run
  directory, but the build still shares `obj/` with the IDE, and the input of
  the artifact is whatever the tree looked like at some point during the build.
- **Container images per build.** Strong isolation, but image builds for every
  change are slow on Windows and lose native debugging of the process.

## Consequences

- Disk: one snapshot per lane plus N artifacts per service. Retention is
  configurable (default 5).
- Snapshot sync costs time on every cycle; measured in phase 0 spike (c).
- Debuggers attach to a process whose binaries are not in the source tree;
  symbol paths point into the snapshot, which mirrors the source layout.
- Rollback becomes free: a previous artifact is still there.

## Enforced by

- Builder (`crates/devstack-builder`, planned): build commands receive
  `{artifact}` as their only output path.
- Supervisor (planned): refuses to start a service whose working directory is
  not inside `.devstack/artifacts/`, except `kind = "devserver"`.

## What this ADR does not say

It does not cover hot-reloading dev servers (`kind = "devserver"`), which build
and serve in one process; see plan §3.4.
