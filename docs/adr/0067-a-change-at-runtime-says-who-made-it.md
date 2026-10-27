# ADR-0067: A change at runtime says where it came from, and a journal keeps the last ones

- **Status:** Accepted (2026-10-27); built 2026-10-27; addendum 2026-10-27 (the dashboard)
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

*Paths as built, noted 2026-10-27.* `crates/devstack-daemon/src/journal.rs`
(the journal, `client`, `auto`, `Change`); the `audit` layer and
`GET /v1/journal` in `crates/devstack-daemon/src/api.rs`;
`Lanes::take_main_auto` in `crates/devstack-daemon/src/lanes.rs` (the main
slot given back or handed on by devstack); `main_served` and a service's
`last_action` in `Supervisor::view` (`crates/devstack-daemon/src/supervisor/mod.rs`,
`state.rs`), read from the journal rather than kept beside it; the header in
`Client::request` (`crates/devstack/src/client.rs`, `serve_as("mcp")` in
`mcp.rs`), `tray.rs`, and the hub's `forward` and `stack_down`
(`client_of`); `devstack journal`, `status`'s lines and the MCP tool
`journal` in `crates/devstack`; the skill's row in `skills/devstack/SKILL.md`.
Point 2 is built for the main slot and services (`stop`, `restart`,
and every other `/v1/services/<name>/<action>`), not yet for
workers, profiles or sharing: those are in the journal only. The dashboard
gets the fields in its types and, since the addendum below, shows them.

## Addendum 2026-10-27: the dashboard shows the journal

Approved as a review card in [#331](https://github.com/nuetzliches/devstack/issues/331)
(ADR-0070). Events and the journal are not one or the other: the journal
records the requests, the events what followed and what no request
caused (a build held, a health check, the file watcher). The dashboard
shows them together, so a change stands next to what it caused.

1. `/v1/events` carries `{type: "journal", entry}` when the daemon records
   an entry, the same entry `GET /v1/journal` gives: the main tree's stream
   every entry, a lane's those of its lane.
2. The Events page is one timeline of events and journal entries
   (*Activity*): on opening it loads `GET /v1/journal`, after that both
   arrive live, and after a snapshot it fetches what it may have missed
   (`?since=`). A journal row shows its client; a refused one (`status`
   400 or more) its error.
3. The dashboard names a row from its method and path, with a table of the
   mutating routes; a path the table does not know shows as it came. The
   journal's format stays: no `action` or `target` field, and no API
   schema to derive the names from (a schema of its own is a question of
   its own).
4. `/v1/journal` is among what a viewer of a shared lane reads (ADR-0040).
5. The dashboard shows point 2 of this ADR: a lane on the main ports says
   since when and through what it holds them, a service's details its last
   action.

*Paths as built.* `Journal::subscribe` and `Entry::for_tree` in
`crates/devstack-daemon/src/journal.rs`; `Supervisor::follow_journal` in
`crates/devstack-daemon/src/supervisor/mod.rs` sends the entries on as
events; `viewable` in `crates/devstack-node/src/node.rs`. In the dashboard
`ui/src/lib/journal.ts` (the table, `forTree`), the store's `journal` and
`fetchJournal` (`ui/src/stores/stack.ts`), `ui/src/views/EventsView.vue`,
`servedSince` in `ui/src/components/StackHeader.vue` and the service's
*Last action*. A lane on a node, and a shared one, read the journal of
their daemon there, which runs as a main tree: its every entry, without
`lane`.
