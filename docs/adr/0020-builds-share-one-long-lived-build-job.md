# ADR-0020: Builds share one long-lived build job; a cancel ends only the cancelled build's tree

- **Status:** Accepted (2026-10-14)
- **Date:** 2026-10-14
- **Deciders:** project owner
- **Related:** [ADR-0003](0003-changes-flow-through-candidates.md), [ADR-0009](0009-the-daemon-owns-every-process-tree.md), plan O-10, spike (b)

## Context

`dotnet build` starts long-lived build servers: MSBuild worker nodes (node
reuse) and the shared C# compiler, 32 processes for the reference API host.
Measured on that host (spike (b) and a follow-up on 2026-10-14):

| Where the build runs | Build after a one-type API change |
|---|---:|
| In its own job, no servers running: servers start inside the job and die with it | 29–35 s, every time |
| In its own job, servers started earlier **outside** that job | 13–16 s, no leftover processes |
| Servers outside any job, staying warm | 12–16 s |
| Servers switched off | 42–43 s |
| The build right after a **cancelled** one | 34 s: MSBuild nodes end themselves when their client disconnects abruptly, whatever job they are in |

Servers are found by named pipe, so a build uses warm servers no matter which
job they live in. They must outlive a build (or every build is twice as slow)
and still die with the daemon (ADR-0009).

## Decision

The daemon owns **one build job per toolchain user session** (kill-on-close,
like every job in ADR-0009). Every build runs in it, so the servers a build
starts stay in it, warm for the next build, and die with the daemon.

A build is finished when its root process exits (spike (b)), not when the job
is empty. **Cancelling a build ends the process tree under the build's root**
(walked from the root at the moment of cancelling), not the job. Servers that
are not part of that tree survive; MSBuild nodes that end themselves after the
disconnect are restarted by the next build, which is therefore cold. That cost
is measured and accepted: it is the same in every layout.

Services keep one job each (ADR-0009); only builds share.

**On Linux** the build job is a long-lived shim holding a cgroup for builds
(`devstack shim` with a placeholder command); every build joins that cgroup
before `exec`. Cancel kills the build's process group. Without writable cgroups
nothing holds the servers: they outlive a crashed daemon until their own idle
timeouts end them (MSBuild nodes, the compiler server); that is documented, not
prevented.

## Alternatives considered

- **One job per build.** Simple, and a cancel is a single call, but every build
  is cold: twice the time of a warm build, for every change.
- **Per-build jobs plus servers seeded by a warm-up build in a long-lived job.**
  Builds then reuse the seeded servers (measured), but MSBuild starts additional
  nodes inside a big build's own job when it needs more than are idle, and a
  cancel ends the seeded nodes anyway. More moving parts for the same result.
- **Servers switched off.** Slowest (42 s) and changes the developer's build.
- **Servers outside any job.** Fast, but they outlive a crashed daemon,
  breaking ADR-0009.

## Consequences

- Cancelling walks a process tree, the method ADR-0009 rejects for services
  because it races with processes spawned during shutdown. For builds the race
  is harmless: a straggler stays in the build job and dies with the daemon, and
  spike (b) showed that a killed build never leaves an incorrect next build.
- Builds of different lanes share warm servers; MSBuild isolates them per
  build, as it does on a developer machine anyway.
- A daemon restart starts with cold servers (ADR-0016).

## Enforced by

- Builder (planned, phase 1 WP5): one build job, cancel by tree from the root.
- `devstack-proc` (planned): a shared job and a tree-kill for its members.

*Paths as built, noted 2026-10-18.* `crates/devstack-proc/src/builds.rs`
(`BuildJob`: one job object or cgroup, cancel by the tree below a build's
root, build servers spared), `crates/devstack-build/src/lib.rs` (`Builder`
owns the job; `Builder::with_job` and `Builder::job` let a lane's builder
share the main tree's), `Supervisor::new` / `new_lane` in
`crates/devstack-daemon/src/supervisor.rs`: one build job per daemon for the
main tree and every lane (since 0.10.0; before, one per supervisor). Test:
`crates/devstack-proc/tests/builds.rs`
(`cancel_ends_the_build_tree_but_not_the_build_servers`).
