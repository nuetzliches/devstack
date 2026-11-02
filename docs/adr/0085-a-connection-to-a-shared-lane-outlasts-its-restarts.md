# ADR-0085: A connection to a shared lane is to the lane, and outlasts its restarts

- **Status:** Accepted (2026-11-02, card in the pull request)
- **Date:** 2026-11-02
- **Deciders:** project owner
- **Related:** [ADR-0053](0053-whoever-a-lane-is-shared-with-connects-to-it.md) (point 3 amended here), [ADR-0035](0035-lanes-can-be-shared-with-the-pool.md) (sharing), [ADR-0021](0021-a-hub-serves-the-dashboard.md) (the hub)

## Context

A member connects to a lane shared with them (ADR-0053): their hub forwards
each service the lane shares to their loopback. In use with a colleague's
eight lanes on a node, the member had to connect again and again:

- **The coordinator restarted** (a deploy of the pool). For up to half a
  minute it lists the lanes without their sharing, until each owner's daemon
  tells it again. The hub closed the forwards of every lane missing from a
  single listing.
- **The hub updated itself.** Its forwards lived in its memory only, so
  every lane was disconnected.
- **The owner started a lane again.** It came back under a new lease, and
  the hub knew connections by lease only.

The same day six lanes an owner shared from the dashboard never reached the
pool: the coordinator took a sharing without a service for "shared with
nobody" and answered with success, so the owner's dashboard showed them
shared and the member's did not list them.

## Decision

1. **A connection is to the lane, not to a lease of it:** its owner,
   repository and lane name. Listed under a new lease, it is connected again
   by itself, on the same ports where they are free here; the old lease's
   forwards close.
2. **A grace period.** A connected lane missing from the coordinator's list
   keeps its ports for two minutes. Past that its forwards close, and the
   ports are free here; it stays connected, and its forwards open again when
   it is listed again (for up to a day).
3. **Kept across a restart of the hub.** The connected lanes are written to
   `connected.json` in the pool's directory. The hub brings them in line
   with the coordinator's list every 10 s, a dashboard open or not, and at
   every listing.
4. **Only a disconnect, or the sharing taken back, ends it.** A lane that
   runs past the grace period but is no longer shared with this machine is
   no longer connected; a lane that is only missing for a while is not.
5. **A sharing with nothing to open is refused.** The coordinator answers
   `422` to a sharing with someone but without a service, and the owner's
   daemon says so before it asks: the stack declares no service with an
   `http` or `https` port, and the main tree's Compose files publish none.

## Alternatives considered

- **Keep the connection by lease, and only add the grace period.** That
  covers a coordinator restart, but not the hub's update or the lane's
  restart, which a member notices most.
- **Let the coordinator keep the sharing across its restart.** It would
  need a store of its own for what every owner's daemon tells it every
  30 s anyway; the gap is half a minute, which the grace period covers.
- **Take a sharing without a service as view-only.** The coordinator
  dropped such a sharing before; making it view-only is a new feature, not
  the fix of a lane that looks shared and is not.

## Consequences

- After a hub restart, the ports of the lanes it was connected to are taken
  again without a click. A member who no longer wants them disconnects.
- A lane the owner stops for good keeps its record for a day; started again
  under the same name within it, it is connected again.
- An owner's devstack before this one, sharing a lane with nothing to open
  against a coordinator of this one, gets the error instead of a silent
  success.

## Enforced by

- `crates/devstack-hub/src/shared.rs`: `LaneId`, `Conn`, `step`,
  `reconcile`, `follow`, `connected.json`; `connect`/`open` remember the
  lane, `disconnect` forgets it.
- `crates/devstack-hub/src/lib.rs`: `shared::follow()` at the hub's start.
- `crates/devstack-node/src/coordinator.rs`: `lease_share` refuses a sharing
  without a service.
- `crates/devstack-daemon/src/lanes.rs`: `share_lane` says it before asking.
- `docs/pool.md`, `docs/dashboard-api.md`.

## What this ADR does not say

- That a member chooses the ports here: the owner's if free, as before.
- That a sharing can be for viewing only.
