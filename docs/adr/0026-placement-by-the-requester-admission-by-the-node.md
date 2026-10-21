# ADR-0026: The requester places a lane; the node admits it, under a lease

- **Status:** Accepted (2026-10-16); points 1 and 3 replaced by [ADR-0031](0031-a-coordinator-places-lanes.md) (a coordinator places), the rest stands
- **Date:** 2026-10-15
- **Deciders:** project owner
- **Related:** [ADR-0024](0024-a-lane-is-a-supervisor-in-the-main-trees-daemon.md), [ADR-0025](0025-a-lane-can-run-on-another-node.md), [ADR-0015](0015-devstack-is-optional.md), [ADR-0016](0016-a-daemon-restart-is-a-clean-restart.md), [ADR-0028](0028-lane-images.md), plan §12, [ADR-0033](0033-a-node-decides-what-runs-on-it.md) (point 4 detailed there: what a node admits), [ADR-0049](0049-a-lanes-need-is-measured-disk-included.md) (point 3's expected need detailed there: measured, disk included)

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
7. **Manual placement first:** `--on <node>` (on `devstack up` in the
   worktree) skips scoring but not admission.
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

*Paths as built, noted 2026-10-18.* Point 4, how a node admits, is
detailed by [ADR-0033](0033-a-node-decides-what-runs-on-it.md) (who and what
it admits) and placement by ADR-0031. Built: `crates/devstack-node/src/node.rs`
(`admit`, `renew`, `release`: leases with a TTL of two minutes, renewed by
the requester; admission against the node's caps, the lanes it runs and the
ones still settling, each lane taken to need at least 0.25 CPU and 256 MiB),
`crates/devstack-node/src/coordinator.rs` (placement, `/v1/leases`),
`crates/devstack-daemon/src/remote.rs` (`follow_renew`), `devstack up --on`.
The admission counts what lanes declare or are taken to need, not what they
use, so "never over-commits" holds for declared needs. Not built: scoring by
mirrors and image layers (point 3, `--on pool` takes the granted node with
the most free memory) and reclaiming with a grace period (point 6).

*Noted 2026-10-21.* Point 3 as built (by the coordinator, ADR-0031 point 4):
"a mirror on the node" is a slot of the lane (ADR-0038), "image layers
present" a slot of the repository there; preferred only with room to spare
(1.5× the need), so a warm but nearly full node does not win. Added to the
hard conditions: free disk where the node keeps lanes (5 GiB at least).
Round-trip time is not measured.

*Noted 2026-10-20.* Point 4's "atomically", as built: the node checks a
lease against what it offers and records it in one step, under the lock of
its leases, so two admissions at once cannot both take the same room
(`admit`, `available_in`). The coordinator does the same for its offers: it
takes what leases offered for a node and not yet admitted there need off
the node's last report, for a minute (`OFFER_HOLDS`) or until the node's
report counts them, in placement and when it creates a lease. A node also
refuses a lease when less than 5 GiB of disk is free where it keeps lanes,
the same hard condition the coordinator places by.
