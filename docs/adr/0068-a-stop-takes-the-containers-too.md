# ADR-0068: A stop takes the stack's containers too, unless asked to keep them

- **Status:** Accepted (2026-10-27)
- **Date:** 2026-10-27
- **Deciders:** project owner
- **Related:** [ADR-0004](0004-infrastructure-is-delegated-to-compose.md), [ADR-0016](0016-a-daemon-restart-is-a-clean-restart.md), [ADR-0024](0024-a-lane-is-a-supervisor-in-the-main-trees-daemon.md) (point 5), [ADR-0030](0030-each-lane-has-its-own-infrastructure.md) (points 3 and 4), [ADR-0038](0038-a-lanes-data-outlives-its-lease.md), [ADR-0052](0052-a-restart-carries-the-lanes-over.md), [ADR-0058](0058-a-profile-can-leave-out-containers-and-services.md)

## Context

Until 0.24, stopping the main working tree (`devstack down`, the dashboard's
stop) ended its services and its daemon and left every container running;
`down --infra` (an unticked box in the dashboard) removed them. Stopping a
lane removed its containers (ADR-0030 point 4). So the same word did two
different things, and the more common one left the most memory taken: a
database, a broker and a search engine run on after the developer said
"stop".

No ADR decided that the main tree's containers outlive a stop. ADR-0016 and
ADR-0052 keep them across a **restart** of the daemon, which is a different
thing. The code justified it with "infra is long-lived (ADR-0004)", which
ADR-0004 does not say.

While a lane ran, the main tree kept **every** container, `--infra` or not,
because lanes once used the main tree's containers (ADR-0024 point 4). Since
ADR-0030 a lane runs its own; it uses only the `shared` ones of the main
tree. A lane that shared nothing still held all of them.

## Decision

1. **A stop takes the containers too.** `devstack down`, the dashboard's and
   the tray's stop, `POST /v1/down` and the hub's `/hub/stacks/{id}/down`
   remove the tree's containers (`docker compose down`); volumes stay, so the
   next start finds its data (as ADR-0038 has it for a lane). In a lane the
   same: its own containers go.
2. **Keeping them is the choice.** `devstack down --keep-infra`, the box
   "Keep its containers running" in the dashboard (unticked), `infra=false`
   on the API, `keep_infra` on the MCP tool. The next start is faster then.
   `--infra` stays accepted and does what `down` does now.
3. **Lanes that run keep only what they use.** The main tree's `down` while
   lanes run removes its containers except those a kept service (`lanes =
   false`, ADR-0024 point 5) comes `after`, and the `shared` ones (ADR-0030
   point 3) while a lane runs on this machine (a lane on a node runs its own).
   The answer and the CLI name what stayed. A group that lists no `services`
   cannot be split and stays whole when something of it must.
4. **A restart is not a stop.** A daemon that stops for a restart (ADR-0052)
   or because it ends (ADR-0016) leaves the containers running, as before.

## Alternatives considered

- **Keep the containers, offer the box only (0.24).** A faster next start;
  but "stop" then leaves the heaviest part running, and stopping a lane
  already meant the opposite.
- **Remove all of the main tree's containers even while lanes run.** Simple,
  but a lane that uses a `shared` container, or a `lanes = false` service
  that serves the lanes, breaks without warning.
- **Ask every time.** A prompt for the CLI and the MCP tool, where an agent
  answers it; the default is what matters.

## Consequences

- The next `devstack up` after a stop starts the containers again and waits
  for their health checks: seconds to half a minute for a database.
- Breaking for scripts that relied on `down` keeping the containers: they
  add `--keep-infra`. Named in the changelog; the stack file is unchanged.
- An older CLI in the main tree still sends `infra=false` to a newer daemon
  and keeps them (in a lane it sends nothing: they go). A newer CLI talking
  to an older daemon removes them unless lanes run.

## Enforced by

`DownQuery` (default `infra = true`) and `down` in
`crates/devstack-daemon/src/api.rs`; `Supervisor::down` and
`Supervisor::down_for_lanes` in
`crates/devstack-daemon/src/supervisor/lifecycle.rs`; `Lanes::any_local` in
`crates/devstack-daemon/src/lanes.rs`; `Docker::remove_selected` in
`crates/devstack-compose`; `DownQuery` and `stack_down` in
`crates/devstack-hub/src/lib.rs`; `Cmd::Down` (`--keep-infra`) and
`client::down` in `crates/devstack`; the MCP tool `down`; `stopStack` and
`keepContainers` in `ui/src/stores/stack.ts`, `Sidebar.vue`. Tests:
`lifecycle_with_compose`, `a_lane_has_its_own_containers`,
`a_profile_leaves_out_containers` and the dashboard's store tests.
