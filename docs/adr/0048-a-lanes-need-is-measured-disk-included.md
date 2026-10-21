# ADR-0048: A lane's need is measured where it ran, disk included; the stack file sets a floor

- **Status:** Accepted (2026-10-21), amended 2026-10-21 (point 9: what a lane here checks)
- **Date:** 2026-10-21
- **Deciders:** project owner
- **Related:** [ADR-0026](0026-placement-by-the-requester-admission-by-the-node.md) (point 3's "expected need" detailed here), [ADR-0031](0031-a-coordinator-places-lanes.md) (point 4), [ADR-0033](0033-a-node-decides-what-runs-on-it.md) (admission), [ADR-0036](0036-the-pool-shows-its-capacity-over-time.md) (point 4: GPUs), [ADR-0038](0038-a-lanes-data-outlives-its-lease.md) (slots), [ADR-0041](0041-a-profile-says-how-a-lane-runs-for-a-purpose.md) (profiles), plan §12

## Context

ADR-0026 point 3 places a lane by its expected need: "the lane's measured
peak from earlier runs, or a value declared in the stack file, or a
default". As built (0.16.0), none of that is measured or declared:

- **Memory** is what the main tree takes at the moment of the request,
  services and containers summed, at least 1 GiB (`lane_need` in
  `crates/devstack-daemon/src/api.rs`). It is a single reading, not a peak.
  When the main tree is not running, the reading is 1 GiB whatever the stack
  is.
- **CPU** is 0.5 for every stack.
- **Disk** is not part of the need. A node with less than 5 GiB free where it
  keeps lanes takes none (`MIN_DISK_FREE`), and every other node takes any
  lane. A lane brings its toolchain image, its infrastructure's images, the
  mirror, dependencies (`node_modules`, package caches), build output and
  Compose volumes. That is tens of GiB on a real stack. Two lanes starting
  at once both pass the 5 GiB check, and together they fill the disk.
- **GPUs** are reported (ADR-0036) and not asked for.
- The stack file has no field for any of these.

Disk also behaves differently from memory and CPU. Its need depends on the
node: the images are shared by every lane of a repository there, and a node
with a slot of the lane (ADR-0038) already holds most of what the lane needs.
It is not freed when a lane stops either, because slots are kept on purpose,
and much of what is kept may be pruned when room is needed.

The numbers that would answer this exist, in different places. A node
measures its lanes' containers (`docker stats`) and their slots
(`slot_bytes`: tree and volumes). A daemon measures its own tree, disk
included (`usage.rs`). None of this reaches placement.

## Decision

1. **A need has four parts:** CPU, memory, disk and GPUs. Disk is split
   into the **repository's part** (the lane image and the infrastructure
   images, shared by the repository's lanes on a node) and the **lane's
   part** (its slot: mirror, caches, build output, volumes). A GPU need is a
   count and the memory each GPU must have.
2. **The node measures a run.** For each lease it keeps the peak of memory
   (the lane container and its Compose project summed), the peak of CPU
   (averaged over a minute, so a build's spike counts and a single sample
   does not), the lane's part of the disk at its largest, the repository's
   part (image sizes), and the peak of GPU memory if it handed a GPU over. It
   reports these with the lease's end, and while the lease runs, so that a
   lease that expires is counted too. The node measures, not the requester,
   so a requester cannot understate a need, and the numbers are those of the
   Linux container the lane runs in, not those of the requester's tree.
3. **The coordinator keeps a footprint** per repository and profile
   (ADR-0041: a profile changes what runs). It keeps the last five runs of
   each part in its store, so the footprint survives a restart, unlike
   ADR-0036's history. The footprint is the **maximum of those five**.
4. **Where the need comes from, in order:**
   1. the footprint (3);
   2. without one, an **estimate by the requester from its main tree**: the
      peak of memory and CPU in its usage history, and as disk its
      `.devstack/` and volumes (plan §12), the size of its Compose images
      (`docker image inspect`), and the size of what `[lanes] sync` and the
      prepared directories (ADR-0044) bring along;
   3. without that, a default: 0.5 CPU, 1 GiB memory, 10 GiB disk.

   The `need` the stack file declares (5) is a floor on each of these.
5. **The stack file sets a floor, not an override.**
   `[lanes.need]` (`cpus`, `memory`, `disk`, `gpus = { count, memory }`)
   holds for every lane, and `[profiles.<name>.need]` holds for that
   profile. The need is the larger of the declared value and the one from
   point 4. A floor that is too high costs reservation ("fits nowhere", with
   the reason). An override that is too low or out of date costs a full disk
   or an OOM on someone else's machine. GPUs are only declared: a run
   without a GPU says nothing about needing one.
6. **Disk counts per candidate node.** A node with a slot of the lane needs
   no room beyond the margin. A node with a slot of the repository needs the
   lane's part. Any other node needs both parts. A node reports, besides
   `disk_free`, what it **could free**: kept slots not in use, and lane
   images no lane uses (both pruned already when space runs short,
   ADR-0038). Placement and admission require
   `free + could free − held ≥ disk need + MIN_DISK_FREE`. A node that
   admits by what it could free prunes that first, oldest slot first, before
   the lane starts.
7. **Disk is held like memory.** The lease carries the disk need. A node
   holds, for each lease it runs, the disk need less what the lease's slot
   already takes (`slot_bytes`), so what a running lane has written is not
   counted twice. The coordinator holds offered leases' disk as it holds
   their memory (`OFFER_HOLDS`).
8. **A running lane is warned, not evicted.** When free disk on a node
   falls below `MIN_DISK_FREE`, its lanes' requesters get an event, and the
   dashboard shows it with the lane and the node.
9. **A local lane checks disk too.** Besides `memory_fits`, the daemon
   refuses a lane whose estimated disk (point 4, second step; at least the floor of point 5) does not fit
   the drive of its worktree, unless forced, as it does for memory.

   *Amended 2026-10-21.* A lane here checks against what it adds, not
   against the estimate for a node: its snapshot, artifacts, logs and
   Compose volumes, as the main tree's take. Its worktree is there already,
   and it shares the images with the main tree, both of which the estimate
   for a node counts; the floor is for the pool, where those count too.
10. **Placement says where its numbers came from.** The lease offer's `why`,
    `GET /v1/placement`, the CLI and the dashboard show the need and its
    source ("measured, max of 5 runs", "estimated from the main tree",
    "declared", "default").

## Alternatives considered

- **Declared only.** Simple and predictable, but most stacks would never
  declare anything, and a declaration ages with the stack while nobody
  looks at it.
- **Measured by the requester.** Its numbers are of a Windows tree with
  native services, not of the Linux container the lane runs in, and a
  requester could report what it likes. They are kept as the estimate for a
  stack never run in the pool.
- **A moving percentile (p95) instead of the maximum of five.** It tolerates
  an outlier but needs more runs before it means anything, and it is harder
  to explain why a lane got the room it did. The maximum forgets an outlier
  after five runs.
- **The declaration as an override.** It would let a stack correct a
  measurement that is too high. The failure it allows, a need that is too
  low, falls on a node owner's machine, not on the stack's team.
- **Reserving a lane's whole disk need for its lifetime.** It is simpler,
  but it counts every byte a running lane has written twice, once in
  `disk_free` and once as held, and a node fills up on paper.

## Consequences

- A stack's first lane in a pool is placed by an estimate. The footprint
  takes over from its first run on.
- An outlier, such as a run with a memory leak or a one-off large seed,
  raises the need for five runs.
- Reports, leases and the coordinator's store grow by a few numbers per
  repository and profile. The fields are additive: an older node ignores
  the disk need and admits by memory and CPU only, while the coordinator
  still places by disk.
- The footprint is keyed by repository name (`[stack] name`), so two
  members' lanes of one repository share it, as they share its images.
- The stack file gains `[lanes.need]` and `[profiles.<name>.need]`. These
  are additions: existing files load and behave as before.

## Enforced by

Will live in:
- `crates/devstack-node/src/share.rs` (`Amount` gains disk, the GPU need)
- `crates/devstack-node/src/node.rs` (measuring a run, `could free`,
  admission and holding by disk, pruning before a start, the warning)
- `crates/devstack-node/src/coordinator.rs` (the footprint, `rank`,
  `OFFER_HOLDS`, `why`) and `crates/devstack-node/src/store.rs` (keeping it)
- `crates/devstack-node/src/policy.rs` (`floor_need`)
- `crates/devstack-daemon/src/api.rs` and `usage.rs` (the estimate, the local
  disk check)
- `crates/devstack-config` (`[lanes.need]`, `[profiles.<name>.need]`) with
  [stack-file.md](../stack-file.md)
- [pool.md](../pool.md)
- the lane and pool views in `ui/`

*Paths as built, noted 2026-10-21.*

- The need is a type of its own, `Need` in
  `crates/devstack-node/src/share.rs` (`disk_repo`, `disk_lane`, `gpus`;
  `disk_on`, `at_least`), not `Amount` with disk added: on the wire it is
  a superset of `Amount`, so older peers read its CPU and memory.
  `policy::floor_need` takes it.
- A node measures in `crates/devstack-node/src/runs.rs` (memory and CPU
  each round, disk every five minutes and at a lease's end) and reports the
  runs in its report (`Report::runs`); a run counts once its lane ran
  three minutes (`COUNTS_AFTER`): a start that failed, or one just begun,
  would make the footprint smaller than what the lane takes. GPU memory is
  not measured: no node hands a GPU over yet.
- The footprint is `crates/devstack-node/src/footprint.rs`
  (`footprints.json` of the coordinator), not `store.rs`; the coordinator
  takes runs of leases it knows on that node only. `need_of` in
  `coordinator.rs` picks footprint, estimate and floor; `rank` counts disk
  and GPUs, and `lease_create` checks a named node by `rank` too.
- What a node could free is its kept slots no lease uses, as last counted;
  lane images no lane uses (removed when space runs short) are not counted.
  A node reports it within `disk_room`; it holds each lease's `disk` less
  what its slot and the images it brought take (`NodeLease::disk`,
  `images`), and prunes in `make_room` after admitting a lease that needs
  the room.
- The requester's estimate is `Sampler::estimate` in
  `crates/devstack-daemon/src/usage.rs`: memory each series' highest,
  CPU each series' highest minute, summed; disk the tree without `.git`
  and its state, its snapshot, artifacts, logs and volumes, and the images
  of its Compose containers (`Docker::project_images`). The lane image is
  not counted: the node builds it. `lane_estimate` in `api.rs` fills in
  the defaults; the floor comes from the lane's own stack file
  (`Stack::lane_need`, `remote::floor_of`).
- Point 8: the warning comes with each renewal (`NodeLease::warning`), is
  shown with the lane as its `disk` trouble and logged as
  `remote_lane_disk`. The pool area does not show a node's free disk yet.
- Point 10: the lease offer carries `need` and `need_from`, the lane's
  start step says them, `daemon.log` has `remote_lane_need`; a placement
  has `disk` and `disk_room` per node, and the dashboard shows the disk a
  lane needs on each node. `devstack pool placement` does not print them
  yet.

## What this ADR does not say

- How a node hands a GPU to a lane's container (`--gpus`) and which policy
  allows it (ADR-0036 point 4). This ADR decides only how a lane asks for one.
- Evicting a lane that outgrows its need. A node refuses new lanes and
  warns; it does not stop running ones.
- Limits derived from the need (ADR-0033's memory limit) beyond using the
  new number.
