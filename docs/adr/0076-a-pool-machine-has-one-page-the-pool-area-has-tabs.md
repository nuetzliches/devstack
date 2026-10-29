# ADR-0076: A machine of the pool has one page; the pool area has tabs, and the pool and the tab are in the URL

- **Status:** Accepted (card in the pull request, points 1 to 4 approved 2026-10-29)
- **Date:** 2026-10-29
- **Deciders:** project owner
- **Related:** [ADR-0032](0032-the-pool-is-administered-in-the-dashboard.md) (point 4, the pool area; amended 2026-10-17, admins grant nodes), [ADR-0034](0034-a-machine-can-belong-to-several-pools.md), [ADR-0025](0025-a-lane-can-run-on-another-node.md) (where lanes start), [ADR-0045](0045-the-hub-notifies-the-desktop.md) (links to `#/pool`), [ADR-0070](0070-a-review-card-comes-before-the-code-an-adr-records-what-was-built.md)

## Context

The pool area had grown to fifteen cards on one page. A machine of the pool
showed in two tables: as a node (state, version, what it shares, pause,
policy, update, prune) and as a member (role, tags, whether it runs lanes,
whether its lanes provision, remove). Since ADR-0032's amendment a node
*is* a member an admin granted lanes, yet its actions were split between
the two, with up to five buttons per row and nowhere to see one machine
whole. Settings that hold for this machine in every pool (where lanes
start, the lanes of others it runs) stood under the tab of one pool, beside
settings that hold for that pool alone. The pool shown was UI state only:
no link led to a pool, a part of it, or a machine.

## Decision

1. **A machine of the pool has one page**, its membership and its node
   side together: role, tags, grants; what it reports, its version, its
   policy, its capacity over time, the lanes on it and its lanes elsewhere,
   its provisioning, every admin action on it, and its part of the admin
   log. The pool's list of machines is one table that tells the state; what
   can be done is on the page.
2. **The URL carries the pool and the part shown**: `#/pool/<pool>`,
   `#/pool/<pool>/machines`, `#/pool/<pool>/machines/<id>`,
   `#/pool/<pool>/lanes`, `#/pool/<pool>/admin`. `#/pool`, which
   notifications and `devstack://open?page=pool` open, stays and opens the
   pool shown last. A pool the hub does not know falls back to the default
   one, and the URL follows.
3. **The pool area has tabs**: *Overview* (the coordinator, the join
   command, what waits for an admin, the capacity per node as starting a lane shows
   it and for the whole pool over time, this machine's settings
   for this pool), *Machines*, *Lanes* (the pool's lanes, the lanes shared
   with this machine), and for admins *Admin* (requests to join, with a
   badge; provisioning; the MCP upstreams the pool names; the skill sets it hands
   out, ADR-0074; the admin log).
4. **What holds for this machine in every pool has a place of its own**,
   *This machine* (`#/pool/this-machine`): what it has, has free and
   reports (the hub's share report), its capacity over time as a pool
   recorded it, how each of its pools sees it (role, tags, kind, whether
   and what it runs for others, a link to its page there), the lanes of
   other members it runs, its own lanes on other machines, and where its
   lanes start. What holds for one pool (telemetry, lanes on provisioned
   nodes) stays with that pool.

## Alternatives considered

- **A page for nodes only, the members table beside it.** Smaller, but a
  machine would still have two places and its actions would stay split.
- **Tabs without the URL.** No change to routes, but no link to a machine,
  no way back with the browser, and a notification could never lead further
  than the pool's first page.
- **One tab per card.** Follows the old page exactly and fixes nothing: the
  number of places stays.

## Consequences

- `this-machine` cannot be the name of a pool here; joining one under that
  name is refused in the dashboard.
- Links into a pool's parts are possible now; notifications still open
  `#/pool`, a deep link from them is a later step.
- The dashboard still only hides what the role does not allow; the
  coordinator checks every action (ADR-0032 point 1).

## Enforced by

`ui/src/router.ts` (the routes), `ui/src/views/pool/` (`PoolLayout`,
`PoolNav`, `PoolOverview`, `PoolMachines`, `PoolMachine`, `PoolLanes`,
`PoolAdmin`, `ThisMachineView`, `PoolJoin`), `ui/src/stores/pool.ts`
(`machines`, `nameOf`), `ui/src/lib/poolText.ts`.
