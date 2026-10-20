# ADR-0040: A shared lane can be shared for viewing, and is then shown as one's own lane is

- **Status:** Accepted (2026-10-19), amended 2026-10-20 (sharing a lane includes viewing it: no separate `view` grant; points 1, 4 and 5)
- **Date:** 2026-10-19
- **Deciders:** project owner
- **Related:** [ADR-0035](0035-lanes-can-be-shared-with-the-pool.md) (point 4 extended here), [ADR-0025](0025-a-lane-can-run-on-another-node.md) (point 6: a lane's API through its requester), [ADR-0027](0027-trust-between-nodes.md), [ADR-0029](0029-source-sync-and-port-forwarding-for-remote-lanes.md), [ADR-0032](0032-the-pool-is-administered-in-the-dashboard.md), [ADR-0033](0033-a-node-decides-what-runs-on-it.md) (tags)

## Context

A lane shared with the pool (ADR-0035) shows up under "Shared with you" in
the sidebar as a list of service buttons. A click opens a forward and the
service's own page in a new tab, and nothing more: no state, no health, no
containers, no logs, no load. A reviewer or tester who opens a colleague's
lane cannot tell whether it is up, which build it runs, or why a page
fails. Their own lanes show all of that. Feedback from using the pool:
"the developer should see a different place, not a different picture or a
different behaviour."

ADR-0035 point 4 rules it out on purpose ("they do not … see the logs of
someone else's lane, unless a later decision says so"), and for a reason:
logs and environments can carry secrets, and an owner who shares a service
to be used has not agreed to show its insides. The data is all there: the
lane's daemon on the node has the whole read API (stack, events, logs,
containers, usage). The node lets only the lane's owner reach it
(`lease_of`); everyone else gets the tunnel to shared ports.

## Decision

1. *Amended 2026-10-20, see below: there is no level any more.*
   **Sharing gets a level.** A lane is shared with someone to **use** it
   (what ADR-0035 has: open its shared services) or to **view** it as
   well. `view` includes `use`. The owner sets the level when sharing (in
   the dashboard, `devstack lane share <lane> --with team:qa --view`, the
   MCP `lane_share` with `view`), per member, tag or `*`, as today.
   `[lanes] share` in the stack file stays `use`. Viewing is never the
   default.
2. **Viewing is reading.** Someone who may view a lane gets its stack view
   and event stream, its services and containers with their states and
   builds, their logs (once and followed), its commands' runs, and its
   usage. Nothing that changes it: no start, stop, restart, rebuild,
   promote, rollback, hot reload, command run, share, move or main slot.
   Its environment (`devstack env`) and its stack file are not shown;
   environments are where keys are.
3. **The node enforces it.** A new route on the node, `GET
   /v1/leases/{id}/view/{*rest}`, passes on to the lane's daemon only `GET`
   requests for an allowlist of paths: `/v1/stack`, `/v1/events`, the logs
   and log streams of services and containers, `/v1/usage`, `/v1/runs`.
   For every request it asks the coordinator, as the shared tunnel does
   (ADR-0035 point 3), whether the caller may **view**. So unsharing, or
   removing a member, holds from the next request, and a stream the caller
   may no longer view is cut. The container routes the node answers itself
   are allowed for viewers only in their reading form (logs, not actions).
4. *Amended 2026-10-20, see below.* **The coordinator keeps the level** with the share (`PUT
   /v1/leases/{id}/share` gets `view`), and `GET /v1/shared` tells each
   member whether it may view, so the dashboard knows which to offer.
5. **The dashboard shows a viewable lane as its own lanes are shown.**
   - In the sidebar, under "Shared with you", it is a row like one's own
     lane: its state as a dot and its node. A click opens the same pages:
     overview, a service's details and logs, containers, events, usage.
   - It has the same layout. The actions are not there, and a banner names
     the owner and that the lane is theirs.
   - A service link opens a forward, as ADR-0035 point 3 has it, never the
     owner's `localhost`.
   - The hub reaches the node through `/hub/pool/shared/{lease}/api/{*rest}`
     with this machine's certificate, as it does for the pool.
   - A lane shared to be used only stays as it is: its services as buttons.

*Amended 2026-10-20, before any other user.* Sharing for viewing as a level
of its own (point 1) proved superfluous and confusing in use: the owner had
two lists to keep for one intent, and a lane shared without `view` showed up
as a row of buttons that led nowhere a reviewer wanted to go. **Whoever a
lane is shared with may view it**, read only, as point 2 has it, and open its
services, as ADR-0035 has it. What viewing shows, and that it never shows the
environment or the stack file, stays as point 2 says; the node enforces it as
point 3 says, admitting exactly the members the lane is shared with.

- Point 1: there is no level. `devstack lane share <lane> --with <...>`, the
  share menu and the MCP `lane_share` take one list; `--view` and `view` are
  gone. `[lanes] share` in the stack file shares to view as well.
- Point 4: the coordinator keeps one list (`with`). An owner still sends it
  as `view` too, and `GET /v1/shared` still says `view: true`, so that
  devstack 0.11 coordinators, nodes, hubs and dashboards in the same pool let
  those members view; read, `view` only widens `with` (whom a 0.11 owner let
  view could use the lane too). A 0.11 owner's separate viewers
  (`lane-views.json`) are added to the lane's sharing once.
- Point 5: every shared lane is listed under "Shared with you" as one's own
  stacks are listed, its owner named faintly, and a click opens its pages.
  Its services open from there (and from the pool area).
- The first alternative below, rejected on 2026-10-19 because owners had
  shared lanes on the understanding that their logs stay theirs, is the
  decision now: those owners are this project's maintainers, and the share
  menu says that whoever a lane is shared with sees its logs.

## Alternatives considered

- **Viewing for everyone a lane is shared with.** It is the simplest
  change, and the one ADR-0035 refused: owners have shared lanes to be used
  on the understanding that their logs stay theirs. Turning that into a
  view without asking would show those logs to everyone the lane is
  already shared with.
- **Copies of the owner's view through the coordinator** (the owner's
  daemon pushes its state). That is a second copy of the stack view to keep
  in step, and logs through the coordinator are a lot of data that is not
  the coordinator's.
- **Full control for viewers ("pair on a lane").** It may come later, as a
  level of its own. Watching is what was asked.

## Consequences

- A viewer reads the lane's logs and its commands' output: whatever a
  service prints (a token in a URL, a connection string in an error) is
  read by them. The owner decides per lane, and the share dialog says so.
- The node asks the coordinator once per request. A dashboard that views a
  lane holds a few streams and polls a few reads, which is a request every
  few seconds per viewer. A short cache (a few seconds) on the node keeps
  that cheap, at the price of revocation holding within those seconds.
- Older nodes have no `view` route. The dashboard offers viewing only when
  the node's version has it (ADR-0031 point 8).

## Enforced by

Where it will live:

- `crates/devstack-node/src/coordinator.rs`: the level with the share, in
  `GET /v1/shared` and in what the node asks.
- `crates/devstack-node/src/node.rs`: `GET /v1/leases/{id}/view/{*rest}`
  with the allowlist, checked per request.
- `crates/devstack-daemon/src/lanes.rs` and `remote.rs`: the level per lane
  and per member, told to the coordinator.
- `crates/devstack-hub/src/shared.rs`: `/hub/pool/shared/{lease}/api/{*rest}`.
- `crates/devstack/src/main.rs`: `lane share --view`; `mcp.rs`: `view` on
  `lane_share` (both gone with the amendment of 2026-10-20).
- `ui/src/router.ts`, `ui/src/stores/stack.ts`: a stack store for a shared
  lane (its API base, no actions); `ui/src/components/Sidebar.vue`.
- `crates/devstack-node/tests/node.rs`: a viewer reads, a user does not, a
  change is refused, unsharing holds at the next request (since 2026-10-20:
  not shared, not viewed; shared, viewed; unshared, not viewed again).
- `docs/pool.md`, `docs/dashboard-api.md`.

*Paths as built, noted 2026-10-20.* The node's route is `view` in
`crates/devstack-node/src/node.rs`, with the allowlist in `viewable` and the
check in `may_view`; the hub's in `crates/devstack-hub/src/shared.rs`
(`view`, `view_get`). The allowlist passes a lane's `/v1/stack`,
`/v1/events`, `/v1/usage`, `/v1/logs`, a service's logs, log streams, log
files, builds and build log, and command runs to the lane's daemon. A
container's logs and log stream (`GET /v1/infra/<group>/<service>/logs`,
`.../logs/stream`, point 3) the node answers itself, before the allowlist,
as it does for the lane's owner (`InfraRoute`); a container's `start`,
`stop` and `restart` are refused with `403`.

## What this ADR does not say

- That a lane on its owner's own machine can be shared. ADR-0035 point 5
  still holds.
- That viewers can act on a lane in any way.
