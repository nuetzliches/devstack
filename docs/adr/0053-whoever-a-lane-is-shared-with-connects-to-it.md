# ADR-0053: Whoever a lane is shared with connects to it, and disconnects again

- **Status:** Accepted (2026-10-22)
- **Date:** 2026-10-22
- **Deciders:** project owner
- **Related:** [ADR-0035](0035-lanes-can-be-shared-with-the-pool.md) (point 3 amended here), [ADR-0040](0040-a-shared-lane-can-be-watched-like-ones-own.md) (point 5: the sidebar row), [ADR-0029](0029-source-sync-and-port-forwarding-for-remote-lanes.md) (the tunnel), [ADR-0021](0021-a-hub-serves-the-dashboard.md) (the hub)

## Context

A member opens a service of a lane shared with them (ADR-0035 point 3): the
hub binds a port on this machine's loopback, the owner's port if it is free
here, and forwards it to the lane on its node. Since 0.11 that is one
service per click, and the forward stays while the hub runs.

Using it showed four faults:

- **A port is taken without being seen.** Once a member has opened a
  colleague's `web`, their hub holds, say, `127.0.0.1:3000` until it stops.
  Their own `devstack up` of the same stack then fails on that port, and
  nothing on the dashboard says why: there is no list of the open
  forwards, and no way to close one.
- **One service is not the lane.** A web front end that calls its API on
  `localhost:<port>` breaks when only the front end is forwarded. A member
  has to click every service they do not want to look at, so that the one
  they do works.
- **The sidebar tells nothing.** A shared lane's row has a green dot
  whatever the lane does and whether or not anything of it is reachable
  here.
- **A forward outlives the sharing.** A lane no longer shared, or gone,
  keeps its forward on this machine; every connection is refused by its
  node.

Feedback from the pool: whoever a lane is shared with should decide
whether its ports are opened on their machine, and see whether they are.

## Decision

1. **Connecting is the member's choice, per lane.** Seeing a shared lane in
   the sidebar or the pool area, and viewing it (ADR-0040), open no port on
   this machine. **Connecting** opens a forward for every service the lane
   shares at once, each on the owner's port if it is free here, else on
   another one. **Disconnecting** closes them all, connections in flight
   included, and the ports are free again.
2. **Opening a service connects its lane.** A click on a service (in the
   pool area, on a link of the lane's pages) and `devstack open` connect the
   lane if it is not connected yet, then open the page. A lane already
   connected keeps its ports; a service the owner shares after the lane was
   connected gets its forward then.
3. **The hub knows which lanes are connected, and says so.**
   `GET /hub/pool/shared` names, per service, the port it is forwarded to
   here (`forwarded`), absent when it is not. `POST
   /hub/pool/shared/{lease}/connect` connects, `POST
   /hub/pool/shared/{lease}/disconnect` disconnects; `open` connects as
   point 2 says. A forward ends when the lane is disconnected, when the hub
   stops, or when the lane is no longer shared with this machine (seen the
   next time the hub lists the shared lanes).
4. **The dashboard shows it.**
   - In the sidebar, a shared lane that is not connected is dimmed, its
     dot hollow; connected, it is shown as one's own lane is, its dot
     filled. Its tooltip names the ports here.
   - On the lane's pages the header has *Connect* or *Disconnect*, and,
     connected, each service's port here; a port that is not the owner's is
     named as moved, since links between its services may then not work.
   - In the pool area each shared lane has the same button beside its
     services.
5. **The CLI has it too.** `devstack pool connect <lane>` and `devstack pool
   disconnect <lane>`; `devstack pool shared` shows a connected service's
   port here.

## Alternatives considered

- **One forward per service, with a close button each.** It keeps the
  member clicking through services to make one of them work, and leaves
  the sidebar with a state per service to show.
- **Connect every shared lane automatically.** That is what a member asked
  not to have: ports of other people's work taken on their machine without
  being asked, the owner's ports first.
- **Free a forward when the member's own stack wants its port.** The daemon
  would have to know the hub's forwards and take one away from under a
  page that uses it. Disconnecting is one click, and the dashboard now
  shows what holds the port.

## Consequences

- A member who opened a service had all of the lane's services reachable
  on their loopback, not one. They are reachable by this machine only, as
  before, and the node still checks the sharing at every connection.
- After a hub restart no lane is connected; a click connects it again.
- The state is the hub's, in memory: two dashboards of the same machine
  see the same connections.

## Enforced by

- `crates/devstack-hub/src/shared.rs`: `connect`, `disconnect`, `open`
  connecting the lane, `forwarded` in `list`, the forwards of lanes no
  longer listed closed.
- `crates/devstack-hub/src/lib.rs`: the routes.
- `crates/devstack/src/hubcmd.rs`, `crates/devstack/src/main.rs`: `pool
  connect`, `pool disconnect`, the ports in `pool shared`.
- `ui/src/stores/pool.ts` (`connectShared`, `disconnectShared`),
  `ui/src/components/Sidebar.vue`, `ui/src/components/StackHeader.vue`,
  `ui/src/views/PoolView.vue`.
- `docs/pool.md`, `docs/dashboard-api.md`.

*Paths as built, noted 2026-10-29.* The pool area moved from `ui/src/views/PoolView.vue` to `ui/src/views/pool/` (ADR-0076): the machine's page (`PoolMachine.vue`) holds what a node row held, the Admin tab (`PoolAdmin.vue`) the admin's lists, the Lanes tab (`PoolLanes.vue`) the lanes shared with this machine.

## What this ADR does not say

- That a member can choose the ports here. The owner's, or another free
  one, as before.
- That a connection is kept across a restart of the hub.
