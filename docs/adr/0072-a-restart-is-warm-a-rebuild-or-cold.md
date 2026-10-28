# ADR-0072: A restart is warm, a rebuild or cold; a group acts on its containers too

- **Status:** Accepted (2026-10-28)
- **Date:** 2026-10-28
- **Deciders:** project owner
- **Related:** [ADR-0038](0038-a-lanes-data-outlives-its-lease.md) ("cold": no artifacts, no data), [ADR-0025](0025-a-lane-can-run-on-another-node.md) (a lane on a node; a move is a cold start), [ADR-0030](0030-each-lane-has-its-own-infrastructure.md) (`shared` containers), [ADR-0062](0062-what-prepare-makes-is-kept-in-a-store-per-machine-and-repository-keyed-by-its-inputs.md) (what is kept for the same inputs), [ADR-0070](0070-a-review-card-comes-before-the-code-an-adr-records-what-was-built.md)

## Context

The dashboard had one restart: a service's, a group's, and none for a lane.
`devstack restart --rebuild` (build again although a build for the same
inputs exists) was in the CLI and the service page only. A lane could be
stopped from the header and its data thrown away from there once stopped,
but restarted only by stopping and starting it again, which on a node ends
its lease and may place it on another node; its worktree was removed from
the branch picker only.

A group's `start` and `stop` left its container members alone, while its
`restart` restarted them: a group of containers only (an observability
group) got no buttons at all, since they would have done nothing.

The owner asked for warm and cold restarts in the dashboard. "Cold" had a
meaning in devstack already: ADR-0038 and `lane move` call a start without
artifacts **and** without data (seeded again) cold. A choice between warm
and cold alone would either name a rebuild cold or throw data away without
saying so.

## Decision

1. **A restart has three levels, named as ADR-0038 names them.**
   **Restart** (warm): from the build there is for the inputs (else built),
   the data kept. **Rebuild**: built again even for inputs built before, the
   data kept (`restart --rebuild`, `POST /v1/services/{name}/rebuild`).
   **Cold**: built again and its data seeded again; for a lane only, never
   for the main working tree (whose data a profile with `data = "fresh"`
   throws away, ADR-0041).
2. **Each restart button is a split button.** A click restarts warm; its
   arrow offers Rebuild, and Cold… for a lane. Only Cold asks first: it
   throws data away. A service row, a group's head and the lane's header
   have one; the command palette has the same actions.
3. **A lane restarts where it runs:** `POST /v1/lanes/{lane}/restart` with
   `{"mode": "warm" | "rebuild" | "cold"}` on the requester's daemon. Warm
   and rebuild restart its running services in place, its containers stay
   up; on a node through the lane's own API, so the lease, the node and the
   ports stay. Cold stops the lane, throws its data away as `reset` does,
   and starts it again at the same place (this machine or its node) with the
   same profile and workers. A group has `rebuild` as well
   (`POST /v1/groups/{name}/rebuild`).
4. **A group's `start` and `stop` take its containers along**, as its
   `restart` did: `start` starts its containers before its services, `stop`
   stops them after its services. A lane leaves the main tree's `shared`
   containers alone; on a node the node's agent acts on them (#128), and
   the requester asks for a node of 0.28.0 or later for these. So every
   stack-file group with services or containers has its buttons.
5. **A lane's actions are in one menu.** At the top right of the header the
   main button restarts a started lane and starts a stopped one; its menu
   has Rebuild, Cold…, Stop, and for a stopped lane Discard data… and
   Remove worktree…. The same menu opens from a lane's "…" button and by a
   right click on its row in the branch picker. The separate "Stop
   worktree" and "Discard data" buttons are gone; Remove worktree stays in
   the branch picker too.

The card and its approval are in the pull request that built this.

## Consequences

- A group's `stop` (`devstack stop <group>`, its dashboard button) stops
  its databases too where it lists them; before, it stopped its services
  only. A group meant as a view of services only should not list
  containers.
- A cold restart of a lane on a node starts it on the same node only if the
  node takes it again (a new lease); if not, it stays stopped and its
  `moving` says why. Its data is gone either way.
- The CLI keeps `restart --rebuild`; there is no `--cold` and no CLI or MCP
  command for a lane restart yet.

## Alternatives considered

- **Warm and cold only.** Rejected: either "cold" is a rebuild under the
  name ADR-0038 gives a start without data, or every cold restart throws
  data away.
- **A confirmation dialog with the choice on every restart.** Rejected: the
  warm restart is the common one and would take a click more each time;
  only the level that throws data away asks.
- **A lane restart as stop and start in the client.** Rejected: on a node
  the stop ends the lease, and the start may land on another node, a cold
  start there.
- **Buttons only for groups with services.** Rejected: a group of
  containers is a unit worth starting and stopping, and the buttons did
  nothing for its containers only because `start` and `stop` left them out.

## Where it lives

`crates/devstack-daemon/src/supervisor/lifecycle.rs` (`group_action`,
`group_containers`, `restart_running`), `crates/devstack-daemon/src/lanes.rs`
(`Lanes::restart`), `crates/devstack-daemon/src/remote.rs`
(`RemoteLane::restart_services`, `containers_need`),
`crates/devstack-daemon/src/api.rs` (`lanes_restart`),
`crates/devstack-node/src/laneinfra.rs` (`named`), `node.rs`
(`named_route`, `act_named`); `ui/src/components/SplitButton.vue`,
`ui/src/lib/laneActions.ts`, `StackHeader.vue`, `BranchPicker.vue`,
`ServiceRow.vue`, `OverviewView.vue`, `CommandPalette.vue`.
