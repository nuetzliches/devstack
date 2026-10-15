# ADR-0026: The requester places a lane; the node admits it, under a lease

- **Status:** Proposed (local lanes are built, ADR-0024; this extends them)
- **Date:** 2026-10-15
- **Deciders:** project owner
- **Related:** [ADR-0024](0024-a-lane-is-a-supervisor-in-the-main-trees-daemon.md), [ADR-0025](0025-a-lane-can-run-on-another-node.md), [ADR-0015](0015-devstack-is-optional.md), [ADR-0016](0016-a-daemon-restart-is-a-clean-restart.md), [ADR-0028](0028-lane-images.md), plan §12

## Context

With ADR-0025, a lane can run on one of several nodes: servers with capacity to
spare and developer machines whose owners share part of theirs. Something has
to choose the node, and nodes come and go: a laptop sleeps, an owner needs the
memory back.

The choice depends on more than free memory. A node that already has the lane
image's layers, the stack's infrastructure images or a warm copy of the same
repository starts the lane in seconds instead of minutes (ADR-0002 measured a
warm incremental build at a fifth of a cold one).

## Decision

1. **No central orchestrator.** The requesting developer's daemon chooses the
   node; every node is the only authority over its own resources. A failure of
   any one machine affects only the lanes on it (ADR-0015).
2. **A node advertises** (`GET` on its agent API): platform (architecture,
   container OS), whether it is **dedicated** (its owner never reclaims), the
   capacity its owner shares (memory, CPU, disk) minus what is reserved, its current load, the image layer digests it holds, and the
   repositories and lanes it has warm source mirrors of (ADR-0029).
3. **The requester scores** its known nodes (ADR-0027: peer list in the user's
   profile):
   1. hard constraints: reachable, platform matches, enough shared capacity for
      the lane's expected need, dedicated if the lane owns or is to own the
      main slot (ADR-0025);
   2. the lane already has a mirror on the node (re-placement, little to sync);
   3. image and infrastructure layers present;
   4. free capacity, then round-trip time.

   The expected need is the lane's measured peak from earlier runs (plan §12),
   or a value declared in the stack file, or a default.
4. **The node admits or refuses** a reservation request atomically. Accepted,
   it returns a **lease** with a TTL that the requester renews while the lane
   is in use. Two requesters choosing the same node concurrently is resolved
   there: one gets the lease, the other takes its next candidate. A node never
   over-commits its shared capacity.
5. **Lease expiry stops the lane.** A requester that disappears (machine off,
   network gone) frees the node without anyone cleaning up. The node sets the
   TTL; a dedicated node grants a main lane a TTL long enough to survive a
   requester that sleeps overnight (default 24 hours).
6. **The owner of a non-dedicated node can reclaim.** Lowering the share, or
   `devstack-node reclaim`, evicts lanes after a grace period (default two minutes), with an event to the
   requester, whose daemon re-places the lane elsewhere (a cold start, data
   recreated by seed jobs; ADR-0025). A dedicated node does not reclaim; taking
   it out of service means its owner first clears the dedicated flag, which
   tells the requesters of main lanes to move them.
7. **Manual placement first:** `--on <node>` (on `devstack up` and
   `devstack lane add`) skips scoring but not admission.
   It is the first version to be built; scoring follows.

## Alternatives considered

- **A central orchestrator** that knows all nodes. One place to see the farm,
  but a service to run and keep available, and a single point of failure for
  every remote lane. Can be added later as a node directory without changing
  admission.
- **The node pulls work from a queue.** Fits short tasks (builds), not a lane
  that holds memory for hours and needs a stable address.
- **Over-commit with eviction under pressure.** Uses capacity better, but on a
  developer machine pressure means the owner's own IDE slows down; refusing is
  cheaper than evicting.

## Consequences

- Each requester sees only its own peer list; there is no global view of the
  farm until a directory is added.
- Scoring uses information that may be a few seconds old; admission makes that
  harmless, at the cost of an occasional second attempt.
- Leases need a heartbeat from the requester's daemon.

## Enforced by

Will live in: `crates/devstack-node` (advertisement, admission, leases,
eviction), the placement module of `crates/devstack-daemon`, CLI `--on`.
