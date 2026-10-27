# ADR-0067: A change at runtime says where it came from, and a journal keeps the last ones

- **Status:** Proposed
- **Date:** 2026-10-27
- **Deciders:** project owner
- **Related:** [ADR-0007](0007-one-http-api-is-the-control-surface.md) (one API, several clients), [ADR-0024](0024-a-lane-is-a-supervisor-in-the-main-trees-daemon.md), [ADR-0048](0048-a-containers-main-port-can-move-with-the-main-slot.md), [ADR-0052](0052-a-restart-carries-the-lanes-over.md), [ADR-0068](0068-a-stop-takes-the-containers-too.md)

## Context

The state of a running stack is changed from several clients (ADR-0007): the
dashboard, the tray, the CLI, the MCP server an agent uses, and devstack
itself (a restart that gives the main slot back to its lane, ADR-0052). Who
changed it is recorded nowhere.

A downstream agent found a lane serving the main ports without having run
`promote --lane`, and reported it as a devstack fault. The developer had
done it in the dashboard. The state said *which* lane serves the ports
(`main_served_by`), not since when or by whom; the events that carried the
change are a live stream (`/v1/events`) and gone for whoever looks later;
the daemon's diagnostic log has a `main_slot` line without a source. An
agent cannot tell a decision of its user from a fault, and either reports
the decision or undoes it.

The same holds for every change that affects others working on the stack:
a service stopped or restarted, a build promoted or rolled back, workers or
hot reload turned on, a profile, a lane shared, the stack stopped.

## Decision

1. **Every client says what it is.** A request that changes state carries
   `X-Devstack-Client: cli | mcp | dashboard | tray`; the hub sets
   `dashboard` or `tray` on what it forwards for them and drops what a
   browser sent. devstack's own changes are `auto`. Missing: `unknown`
   (an older client, a script). It is information, not authentication.
2. **The state that others see carries its origin.** Where a value says
   who holds something, it says since when and by what: `main_served_by`
   is joined by `main_served: {lane, via, at}`; a service's stop, a promote
   and a rollback, hot reload and workers carry `{via, at}` in their views.
   `devstack status` shows it ("lane `x` serves the main ports since 10:42,
   set in the dashboard").
3. **A journal keeps the last changes.** The daemon appends each change to
   a ring of the last 500 entries, `{at, via, lane?, action, target,
   detail?}`, kept in `.devstack/journal.ndjson` across restarts:
   `GET /v1/journal?since=` and `devstack journal`. Events stay what they
   are, the live view; the journal is the record.
4. **The agent skill looks before it reports.** A state that looks wrong
   (a lane on the main ports, a service stopped, containers gone) is
   checked against `devstack status` and the journal first; a change a
   client made is the user's decision, not a fault, and is not undone
   without asking.

Changes to the stack file are out of scope: git records them.

## Alternatives considered

- **Events only.** Already there, but a stream: someone who looks later
  sees nothing. Persisting them is the journal under another name.
- **Journal only.** Complete, but every reader has to query and match it;
  the common question ("why does lane x hold the ports?") is answered where
  the agent already looks, in the state.
- **Who, not only through what** (member, OS user). Locally it is always the
  same user; in a pool the member is known from the lease. Not needed for
  the case at hand; the journal can grow a field.

## Consequences

- Every mutating route records an entry: one place in the router, not one
  per handler, or some changes stay unrecorded.
- An older CLI or MCP server shows as `unknown` until it is updated.
- The journal is local to the daemon; a lane on a node keeps its own, read
  through the same passed-through API.

## Enforced by

(to be built) a middleware on the mutating routes in
`crates/devstack-daemon/src/api.rs`; `Client` in `crates/devstack/src/client.rs`
(CLI, MCP); the forwarding in `crates/devstack-hub/src/lib.rs` and
`crates/devstack/src/tray.rs`; `StackView` in `crates/devstack-daemon/src/state.rs`;
`skills/devstack/SKILL.md`; `docs/dashboard-api.md`.
