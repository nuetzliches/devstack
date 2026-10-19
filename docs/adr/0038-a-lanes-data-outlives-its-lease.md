# ADR-0038: A lane's mirror and data outlive its lease on a node, for a while, and can be pruned

- **Status:** Accepted (2026-10-19)
- **Date:** 2026-10-19
- **Deciders:** project owner
- **Related:** [#99](https://github.com/nuetzliches/devstack/issues/99), [ADR-0025](0025-a-lane-can-run-on-another-node.md) (points 5 and 7 amended here), [ADR-0026](0026-placement-by-the-requester-admission-by-the-node.md), [ADR-0029](0029-source-sync-and-port-forwarding-for-remote-lanes.md) (point 2 and its Consequences), [ADR-0030](0030-each-lane-has-its-own-infrastructure.md), [ADR-0032](0032-the-pool-is-administered-in-the-dashboard.md), [ADR-0033](0033-a-node-decides-what-runs-on-it.md), [ADR-0034](0034-a-machine-can-belong-to-several-pools.md)

## Context

Everything a lane has on a node is keyed by its **lease**, and a lease is new
on every start. That covers:

- the mirror (`lanes/<lease>/`, with the source, the lane daemon's state and
  artifacts, and the toolchain caches under `home/`);
- the Compose project `<project>-<lease>` and its volumes;
- the lane container.

Every end of a lease calls `drop_lease`: `lane stop`, the requester's daemon
stopping, a lease that expired (a laptop that slept for two minutes), a move,
and an admin ending it. `drop_lease` runs `compose down --volumes`, deletes
the mirror, and collects the member's blobs (all of them when no other lease
of the member runs there). So stopping a lane and starting it again a moment
later, with nothing changed:

- uploads every file again: 4,557 in #99, where ADR-0029 point 2 promises
  "only the difference";
- builds cold;
- starts with empty databases, which the stack's jobs seed again.

The data loss is what ADR-0025 point 5 decided ("a remote lane is
disposable"). The cost was not visible then. Today it is minutes per start
on a real stack, and every developer who stops a lane for the evening loses
its data. Local lanes behave differently. They keep their volumes over
`lane stop` and restarts, because their Compose project is keyed by the lane
name (ADR-0030), and lose them only when the worktree goes. On neither side
does anything remove leftovers: a volume of a lane whose branch renamed a
group, or a node's state left behind by an agent that crashed between
releasing a lease and cleaning it up, stays forever.

There is also a race. `gc_blobs` keeps only the blobs that some lease's
manifest names. A new lease that has uploaded blobs but not yet applied them
has no manifest, so the delayed cleanup of the old lease can delete what the
new one just sent.

## Decision

1. **A lane's place on a node is a slot**, keyed by member, repository and
   lane: `secret_name(node secret, [member, repo, lane])`. The member comes
   from its certificate, so a member reaches only its own slots. A false repo
   name only lands in that member's own namespace. The node secret is per
   pool (ADR-0034), so two pools on one engine never share a volume name. The
   slot holds the mirror, the lane daemon's state and artifacts, the
   toolchain caches, and the Compose project `<project>-<slot>` with its
   volumes. The lane container, its network and its egress rules stay keyed
   by lease. A slot has at most one lease at a time. A start on a slot whose
   last lease is still being cleaned up waits for that, under the slot's
   lock.
2. **The end of a lease keeps the slot.** Containers stop
   (`compose down`, no `--volumes`). The mirror, the volumes and the blobs
   stay, and the slot records when it was last used. A start on the same node
   finds the manifest (the sync sends the difference), the artifacts (no cold
   build) and the data. A start on another node is a cold start there, as
   today. Data never moves between nodes; ADR-0025 point 5 stays true for
   moves.
3. **The node decides how long it keeps slots** (ADR-0033). This is part of
   its policy: a retention time (default 7 days on a dedicated node, 24 hours
   on one that is not) and a disk cap per member over blobs, mirrors and
   volumes. Over the cap, the least recently used slot of that member goes
   first. A node purges at once:
   - every slot of a member the coordinator no longer lists (the promise
     ADR-0029 made: never kept after the key is removed);
   - everything an owner reclaims (ADR-0026);
   - on agent start, everything no slot or lease owns: mirrors, Compose
     projects and lane containers labelled `devstack.lease`.
4. **The requester ends a slot when its lane ends for good.** When the
   worktree is removed, the lane's slots on every node it ran on are purged
   (`DELETE /v1/slots/{slot}`, best effort). The requester notes those nodes
   in the lane's state, and the retention covers nodes that are offline. A
   `lane stop`, a stopped daemon, an expired lease and a move keep the slot.
5. **Prune and reset, for both kinds of lane.**
   - `devstack lane prune [<lane>] [--older-than <age>] [--local | --remote]
     [--dry-run]`:
     - locally: the Compose projects of lanes no longer in `lanes.json`, or
       stopped for longer than `<age>`, found by Compose's project label, so
       a group the lane's branch added is found too;
     - remotely: the member's own slots on each node of its pools
       (`GET /v1/slots`, then `DELETE`).
   - `devstack lane reset <lane>` throws the lane's data away. Its next
     start is a fresh one, seeded again.
   - In the dashboard, a lane gets "Discard data". The pool area shows each
     node's kept disk, with Prune for an admin (ADR-0032; logged like every
     admin action).
   - The node owner has `devstack node prune`.
6. **A stack may stay disposable**: `[lanes] keep_data = false` purges a slot
   when its lease ends. The key can only shorten what a node keeps; the node
   decides the longest. It is an additive key (ADR-0023).

The fix to #99's uploads goes first and needs no slot: `drop_lease` no longer
collects the member's blobs; collection happens under pressure only
(`MAX_MEMBER_BLOBS`). Blobs younger than ten minutes are always kept, which
closes the race.

## Alternatives considered

- **Keep only the blobs (#99, fix 1).** It ends the uploads, but the node
  still writes every file of the mirror again, builds cold and starts with
  empty databases: the largest part of the minutes stays.
- **Keep the mirror, drop the volumes.** A warm build without the data. Seed
  jobs then run on every start. Tolerable for small fixtures, not for the
  downstream stacks with large databases that this exists for.
- **Move the data with the lane** (volume export and import on `lane move`).
  Moves become minutes long and need the space twice. A move is rare, a
  restart is daily: this ADR serves the restart.
- **Docker's own `volume prune`.** It removes every unused volume of the
  engine, including those of other projects on a developer's machine that
  is also a node, and it knows nothing of mirrors and blobs.

## Consequences

- A node holds a member's source, environment files included, for up to the
  retention after the lane stopped (ADR-0027 point 6: today only while it
  runs). The node's owner could read it while it ran too; the difference is
  time. `docs/pool.md` and `SECURITY.md` must say so, and the purge on
  member removal (point 3) has to be built before this ships.
- A node's disk fills with stopped lanes until the retention or the cap
  frees it. Admission (ADR-0026) has to count kept slots against free disk,
  or a node accepts a lease it cannot store.
- A slot can hold stale data after a branch switch in the worktree
  (migrations that no longer match). `lane reset` is the way out. The lane
  start should name when the data was last used.
- Restarting a lane becomes warm only on the node that kept its slot. The
  coordinator does not prefer that node yet. ADR-0026 points 2 and 3.2
  (scoring) leave room for it.

## Enforced by

Where it will live:

- `crates/devstack-node/src/node.rs`: slots (`slot_of`, `drop_lease`
  keeping, `gc_blobs` under pressure only, a TTL sweep beside lease reaping,
  orphan sweep on start), `GET/DELETE /v1/slots`.
- `crates/devstack-node/src/lanerun.rs`: the Compose project and overlay by
  slot; `stop` without volumes, `purge` with.
- `crates/devstack-node/src/policy.rs` (NodePolicy): retention, cap.
- `crates/devstack-daemon/src/lanes.rs`: purge on `forget`, the
  `lane-<name>/remote/slots.json` note, `lane prune`, `lane reset`; local
  prune by project label.
- `crates/devstack-config`: `[lanes] keep_data`; `docs/stack-file.md`.
- `docs/pool.md` (what a member uploads is kept…), `SECURITY.md`.

## What this ADR does not say

- That lanes survive a restart of the requester's daemon. They still stop
  (ADR-0025 point 7). What survives is the slot, so the next start is warm.
- That a lane prefers the node that has its slot. Placement is unchanged.
- Anything about backing up a lane's data. A slot is a cache, not storage.
