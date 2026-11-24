# ADR-0119: A lane's memory counts what it owns and names its builds; main is stopped when only what serves its lanes runs

- **Status:** Accepted (card in the pull request); points 1, 2 and 4 built, point 3 follows
- **Date:** 2026-11-24
- **Deciders:** project owner
- **Related:** [ADR-0009](0009-the-daemon-owns-every-process-tree.md) (process trees), [ADR-0020](0020-builds-share-one-long-lived-build-job.md) (the build job, its servers stay warm), [ADR-0049](0049-a-lanes-need-is-measured-disk-included.md) (a lane's need), [ADR-0080](0080-the-main-tree-answers-as-a-lane-shared-is-deprecated.md) (`singleton`), [ADR-0095](0095-the-daemon-ends-with-the-last-tree.md) (main stops alone), [ADR-0070](0070-a-review-card-comes-before-the-code-an-adr-records-what-was-built.md)

## Context

A lane of a .NET and Angular stack on a pool node showed 21.2 GB in the
dashboard. Its groups added up to 11.6 GB. The rest was the build job: 51
MSBuild worker nodes from three rounds of parallel builds on a 24-core node,
kept warm on purpose (ADR-0020), counted in the lane but in no group. The
numbers were also too high: on Linux without a cgroup per service the daemon
summed each process's resident set, so the .NET runtime's shared pages
counted once per process (the build servers: 9.3 GB resident, 5.8 GB
proportional). The pool took the same numbers as the lane's need (25.4 GB).

Stopping the main working tree while that lane ran looked as if it did
nothing: the stop kept `companion` (a `singleton`) for the lane, as it
should, but the dashboard still showed main as running, with its Stop
button, so it was pressed three times.

## Decision

The owner approved the points in the conversation on **2026-11-24**, all
four:

| # | Category | Point | Beats | Reversing costs |
|---|---|---|---|---|
| 1 | contract | A native service's memory is what its processes own: Linux the proportional set (`Pss` of `smaps_rollup`) instead of the resident set, Windows the private working set instead of the whole, macOS `phys_footprint` instead of `resident_size` (untested here: CI only). | The sum counting shared pages, such as the .NET runtime's, again with every process. | Low; the numbers in telemetry and the stored needs drop once, the old ones leave after five runs. |
| 2 | term | "Builds" is a part of its own: the lane card says "of it builds X", the overview lists *Builds* beside the groups, so the sum adds up. | 10 GB in no group, the lane's number not explained. | Low, dashboard only. |
| 3 | contract | The need a lane reports to the pool names its builds apart (services and containers plus builds); placement still takes the sum, the log names the builds' part. | Needs like 25.4 GB nobody can explain; the build servers stay in the need, they take the node's memory until MSBuild ends them. | Medium: a field through node, coordinator and hub; older nodes leave it out. |
| 4 | term | Main counts as stopped when only what it keeps for running lanes runs: its state says so, the button is Start again, the stop's message names what keeps running. | A stop that looks as if it did nothing. | Low: one rule in the dashboard, which reads `kept`. |

Decided while building, not on the card: with a cgroup per service (Linux,
delegated) the memory is `memory.current` less the cgroup's `inactive_file`,
as `docker stats` counts a container; a process whose `smaps_rollup` cannot
be read (another user's) falls back to its resident set, a Windows without
the private figure (before Windows 10 21H2) to the whole working set; a
container a kept `singleton` comes `after` still counts as main running;
point 3 comes in a pull request of its own, since the node measures a lane
as one container and has to ask the lane's daemon for its builds.

As built (points 1, 2 and 4):

1. **The measurement.** `devstack-proc`: `linux.rs` reads `Pss:` through
   `kb_field` (shared with `meminfo`) and subtracts `inactive_file` from a
   cgroup's `memory.current`; `windows.rs` asks `K32GetProcessMemoryInfo`
   for `PROCESS_MEMORY_COUNTERS_EX2` and takes `PrivateWorkingSetSize`;
   `macos.rs` takes `ri_phys_footprint` of `proc_pid_rusage`. Tests per
   platform: the private part is below the whole and above zero.
2. **Builds in the dashboard.** `LaneUsage` takes `builds` (the report's
   build job) and says "of it builds"; the overview shows a *Builds* row
   with its processes and load below the groups while it takes memory. The
   mock hub keeps its build servers warm after a build, as the daemon does.
3. **Main stopped.** The store's `mainActive` and `mainIdle` leave out a
   `singleton` while a lane runs; the stop's toast reads `kept`,
   `containers` and `lanes` of the daemon's `200`; the stack page's
   "main stopped" names what keeps running.

## Consequences

- The memory devstack shows falls for every stack with several processes
  per service, most for .NET; a pool's stored needs follow after five runs.
- The build servers are still counted, now where one sees them; ending them
  sooner (fewer MSBuild nodes per build) is the user's build command's
  choice, not devstack's.
