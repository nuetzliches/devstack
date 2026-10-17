# ADR-0002: Processes run from immutable artifacts, never from the build output of the source tree

- **Status:** Accepted (2026-10-14), amended 2026-10-18 (services without `build`; a build whose inputs changed meanwhile is discarded)
- **Date:** 2026-10-14
- **Deciders:** project owner
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
source (`.devstack/lanes/<lane>/src`), **in place** with the toolchain's normal
output directories, and then **publishes** the build output by copying it into
an **artifact directory** named by the input hash
(`.devstack/artifacts/<service>/<build-id>/`). Processes are started
only from artifact directories, which are never written again after the build
succeeds. A build writes into a temporary directory that is renamed to its
final name only on success, so an artifact directory is either complete or
absent, also after a crash ([ADR-0016](0016-a-daemon-restart-is-a-clean-restart.md)).
The build id is passed to the process (`DEVSTACK_BUILD_ID`) and shown
by every client.

The snapshot copies changed files only and stamps copied files with the current
time, so incremental build tools see them as newer than their outputs. Snapshot
directories persist to keep incremental state warm.


*Amended 2026-10-14 after spike (c), before acceptance.* The first draft let the
build write straight into the artifact directory (`-o <artifact>`). Measured on
the reference API host, that turns every build into a near-full rebuild,
because each new output path invalidates the build's incremental state:

| | NTFS | ReFS | WSL ext4 |
|---|---:|---:|---:|
| Build into a fresh artifact directory, one-line change | 12.4 s | 11.0 s | 10.2 s |
| Build in place in the lane, one-line change | 2.6 s | 2.8 s | 2.0 s |
| Publish by copy (240 files) | 0.14 s | 0.11 s | 0.12 s |
| Publish by hard link | 0.04 s | 0.02 s | 0.01 s |
| Snapshot sync: full (7,140 files) / 50 changed | 3.4 / 0.21 s | 2.4 / 0.24 s | 1.0 / 0.07 s |

## Alternatives considered

- **Build in place, stop the process first.** Removes the lock, keeps the
  downtime for the whole build, still breaks on a second writer (IDE), and gives
  no rollback.
- **Build in place, copy output to an artifact afterwards.** No lock on the run
  directory, but the build still shares `obj/` with the IDE, and the input of
  the artifact is whatever the tree looked like at some point during the build.
- **Build into the artifact directory (`-o`).** See the amendment above: four
  to five times slower per change.
- **Publish by hard link instead of copy.** Faster still, but a hard link is the
  same file: a running process locks it, and the lane's next build, writing to
  the very same file in `bin/`, fails on that lock, which is exactly what this
  ADR removes; a toolchain that rewrites a file in place would also change the
  published artifact. Copy-on-write cloning (ReFS block cloning, reflinks) keeps
  copy semantics and may replace the plain copy later.
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

*Paths as built, noted 2026-10-18.* The crate is `crates/devstack-build`
(not `devstack-builder`). A build runs in the tree's snapshot
(`snapshot.rs`) and `Builder::publish` copies its `output` into
`.devstack/artifacts/<service>/<id>/` through a temporary directory and a
rename, so an artifact is complete or absent; the build command does not get
`{artifact}`, the run command does. The supervisor
(`crates/devstack-daemon/src/supervisor.rs`: `launch`, `build_locked`,
`discard_artifact`, `prune_artifacts`) starts a service with `build` from its
artifact directory only (see the addendum for services without `build`).
Retention is a constant (the newest five per service plus everything a tree
runs or may roll back to), not a setting. Tests:
`crates/devstack-build/tests/pipeline.rs`.

## What this ADR does not say

It does not cover hot-reloading dev servers (`kind = "devserver"`), which build
and serve in one process; see plan §3.4.

## Addendum 2026-10-18: services without `build`

The rule above is about *build output*: nothing runs from what a build wrote
into the working tree. A service without `build` (`kind = "service"`, `tool`,
`devserver`) has no build output; it runs a command (an installed tool, a
script, `docker run`) from the repository root, or in a lane from the lane's
copy (ADR-0024), never from an artifact. That is deliberate, not a gap, and the
supervisor does not refuse it: requiring `build` for every `kind = "service"`
would turn away services that are simply programs installed on the machine.
What the supervisor enforces is that a service *with* `build` runs from its
artifact directory (`cwd` and `{artifact}`), never from the snapshot or the
tree; the "refuses to start" line under *Enforced by* reads that way.

All services of a tree share one snapshot. A build whose inputs another
build's sync changed while it ran is not published under its id: the daemon
compares the inputs' id after the build with the one it built for, discards
the artifact if they differ (`build_discarded` in the daemon log) and builds
again.
