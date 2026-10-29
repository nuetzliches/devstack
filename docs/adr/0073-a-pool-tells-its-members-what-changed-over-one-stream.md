# ADR-0073: A pool tells its members what changed, over one stream; nobody polls the coordinator

- **Status:** Accepted (2026-10-29)
- **Date:** 2026-10-29
- **Deciders:** project owner
- **Related:** [ADR-0021](0021-a-hub-serves-the-dashboard.md) (the pool area's polling, an exception ended here), [ADR-0031](0031-a-coordinator-places-lanes.md), [ADR-0034](0034-a-machine-can-belong-to-several-pools.md), [ADR-0037](0037-telemetry-goes-to-the-pools-collector-with-consent.md), [ADR-0045](0045-the-hub-notifies-the-desktop.md) (guest lanes), [ADR-0069](0069-devstack-mcp-can-carry-the-tools-of-upstream-mcp-servers.md) (the pools' MCP upstreams), [ADR-0070](0070-a-review-card-comes-before-the-code-an-adr-records-what-was-built.md)

## Context

Everything the dashboard shows of a stack arrives on an event stream
(`docs/dashboard-api.md`, "No polling"). The pool was the exception
(ADR-0021, amended 2026-10-18): a coordinator had no stream, so its members
asked it on timers.

| Who | What | How often |
|---|---|---|
| dashboard, through the hub | the pool area (members, nodes, leases, capacity) | 10 s; 30 s for the parts other pages show |
| hub | guest lanes on this machine | 15 s per pool, after each report |
| hub | the pool's MCP upstreams (ADR-0069 option B) | 60 s per pool |
| node agent | whether the pool collects telemetry | 10 min |

A next consumer was on its way (skills a pool hands out), with a timer of its
own. Timers delay what a member sees, and a member removed from the pool kept
what it read until the next one.

## Decision

The owner approved the review card in the conversation on **2026-10-29**,
all six points:

| Category | Point | Beats | Reversing costs |
|---|---|---|---|
| concept | The coordinator offers each member an SSE stream (`GET /v1/events`, by the membership's client certificate); an event names only topic and version, the first one after connecting all topics; the member reads the content over the existing endpoints. | Content in the stream (a second data path, gaps after reconnecting); WebSocket; long polling. | Remove the endpoint; hubs need timers again. |
| contract | The topics: members, nodes, leases, upstreams, telemetry; each endpoint of one carries an ETag. A coordinator of an earlier release answers `404`. | A stream per topic. | Part of the pool protocol, which users outside the maintainers have; additive. |
| concept | Neither member nor dashboard polls the coordinator: the hub reads after an event only, the pool area gets its data through `/hub/events` like everything else, and the exception in ADR-0021 ends. | The stream as an accelerator, the timer kept. | Put the timers back. |
| concept | With a coordinator without a stream the hub does not fall back to polling: it reads once when it starts following the pool and when the dashboard opens the pool area, and the dashboard says the pool is not live. | Polling as a fallback (two paths, the polling kept for good). | Adding a fallback later is cheap. |
| concept | Reports and liveness stay timers: the share report, the lease renewal and the telemetry send something, they ask nothing. A lease does not live as long as the stream. | Liveness by the open connection (a half-open stream would keep a dead lease alive). | Liveness by the stream later is a change of its own, with consequences for the safety of leases. |
| trust | The stream shows only versions of what the member may read over the endpoints anyway. The coordinator closes the stream of a member it removes, and that member's hub drops the pool's upstreams and guests. If wrong: the stream tells a member about changes to what it may not read, or a removed member keeps access until its next read. | Revoking at the next read only. | Nothing structural; the endpoint stays. |

What was built:

1. **The stream.** `GET /v1/events` (members): `text/event-stream`, one event
   `{"topics": {"<topic>": "<version>"}}` per change, the first one with every
   topic. A version is a short hash of what every member reads for the topic,
   without what changes with time alone (how long ago a node reported, when a
   lease expires): it changes when the content does, and stays the same
   across a restart of the coordinator when the content did. Changes within
   2 s go out as one event (a pool of many nodes reports several times a
   second). A comment every 15 s keeps a quiet stream open; a member takes a
   stream silent for 45 s for dead and connects again.
2. **When versions are counted.** After every request but `GET` and `HEAD`,
   and every 5 s for what changes with time alone (a node goes offline after
   45 s without a report, an offer expires). That is the coordinator looking
   at its own state, not asking anyone.
3. **The topics** and what they cover (ADR-0074 adds `skills`, each
   member's own version of the sets given to it): `members` (`GET /v1/members`),
   `nodes` (`GET /v1/nodes`, a node's report and whether it is online),
   `leases` (`GET /v1/leases`, `GET /v1/shared`: whom a lane is shared with),
   `upstreams` (`GET /v1/upstreams`), `telemetry` (`GET /v1/telemetry`).
   Each of those endpoints answers with an `ETag` of its body and `304` to a
   matching `If-None-Match`. An ETag is the body's own, not the topic's
   version: some answers depend on who asks.
4. **A removed member.** The stream checks the member again after each
   change; removing one is a change, so its stream ends within about 2 s,
   and connecting again is refused (`403`). Its hub drops what came from the
   pool: the MCP upstreams of the pool (with their logins) and its guest
   lanes.
5. **The member's side** (`devstack_node::events::follow`): connect, compare
   each topic's version with the last one heard (across connections, so a
   reconnect reads only what changed meanwhile), connect again after a loss
   with a back-off from 1 s up to a minute and a jitter of up to a second.
6. **The hub** follows every pool the machine is an approved member of, one
   stream each. `leases` or `nodes` changed: it reads the guest lanes;
   `upstreams`: the pool's list. It tells dashboards the pools' state and
   versions on `/hub/events` (`pools`); the dashboard reads what it shows of
   a pool when a version differs from the one it read last. The capacity
   charts read their history again when `nodes` changed, at most every 15 s
   (a point is 15 s); where a lane can start, when `nodes` or `leases`
   changed, at most every 10 s. Which pools are followed is looked at when
   the hub starts, when the dashboard reads the pools, and when a membership
   was approved.
7. **The node agent** reads whether the pool collects telemetry when the
   stream says `telemetry` changed, not every 10 minutes.
8. **A coordinator before this release** (`404`): the hub reads its guests
   and upstreams once when it starts following it and each time the
   dashboard opens the pool area, and the pool area says "Not live" with
   why. The node agent reads the telemetry setting once.

## Alternatives considered

- **The content in the stream.** Saves a request per change, but makes the
  stream a second way to read every part, with its own access rules, and a
  reconnect needs a full resend anyway.
- **A stream per topic.** One connection per member and pool is enough; a
  member that follows only some topics ignores the others.
- **Long polling** (`GET ...?wait=`). No open connection between changes,
  but a request per change and topic, and the same reconnect logic.
- **Polling as a fallback for older coordinators.** Every member would keep
  the timers this ADR removes, for as long as one old coordinator exists.

## Consequences

- The coordinator holds one connection per following member and pool
  (a hub, and a node agent for its telemetry setting); its connection limit
  (1024) is far above a team's pool.
- A proxy between member and coordinator must not buffer
  `text/event-stream`.
- The one place that still reads on a timer is a machine whose request to
  join waits for an admin: it has no certificate to follow a stream with,
  and the pool area reads every 10 s until it is approved (or no longer
  shown).
- Not part of this change: the timers that send (share report every 15 s,
  lease renewal every 30 s, telemetry), the requester asking a node about a
  lane it starts (every 2 s while it starts; the node has streams, a change
  of its own), and timers on this machine alone (Docker, branches, the
  release check, the share status on the lane-start card, a daemon asking
  whether the pool collects telemetry).

## Enforced by

`crates/devstack-node/src/events.rs` (topics, versions, the member's
client), `crates/devstack-node/src/coordinator.rs` (`events`,
`count_versions`, `recount_after`, `etagged`),
`crates/devstack-node/src/tls.rs` (`send_to`),
`crates/devstack-node/src/node.rs` (`follow_telemetry`),
`crates/devstack-hub/src/poolfeed.rs`, `crates/devstack-hub/src/upstreams.rs`
(`set_pool`), `ui/src/stores/pool.ts` (`onFeed`, `watch`),
`crates/devstack-node/tests/events.rs`.
