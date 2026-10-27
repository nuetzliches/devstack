# ADR-0030: Each lane has its own infrastructure

- **Status:** Accepted (2026-10-16)
- **Date:** 2026-10-16
- **Deciders:** project owner
- **Related:** plan §3.5, [ADR-0004](0004-infrastructure-is-delegated-to-compose.md), [ADR-0008](0008-background-workers-run-only-in-the-active-slot.md), [ADR-0012](0012-shared-infrastructure-between-lanes.md) (decided here), [ADR-0014](0014-native-services-take-their-configuration-from-compose.md), [ADR-0024](0024-a-lane-is-a-supervisor-in-the-main-trees-daemon.md) (point 4 replaced here), [ADR-0025](0025-a-lane-can-run-on-another-node.md), [ADR-0057](0057-a-port-a-container-announces-moves-inside-it-too.md) (point 2 detailed: an announced port moves inside the container too), [ADR-0068](0068-a-stop-takes-the-containers-too.md) (the main tree's stop takes its containers too, but the `shared` ones a lane here uses)

## Context

ADR-0024 point 4 let every lane use the main tree's containers, separated only
by per-lane names (`Database=app{lane_suffix}`), and kept ADR-0012 open. In use,
sharing showed its costs beyond the memory it saves:

- A restart of a container of the main tree (`restart_on`, `down --infra`, an
  overlay change) hits every lane running against it.
- The main tree's data is safe only if the stack file names every resource per
  lane. One connection string without `{lane_suffix}` and a lane's migrator
  changes the main tree's database.
- A branch that changes a Compose file or adds an infrastructure service runs
  against the main tree's infrastructure all the same.
- Only some state can be named per lane at all: a database by its name, but
  broker exchanges, cache keys, search indices, buckets and server settings
  each need a prefix scheme of their own.
- Lanes run without background workers only because a shared scheduler would
  hand one lane's job to another (ADR-0012 interim rule).

The memory a second copy of the containers takes is real. When a machine does
not have it, the answer is a lane on another machine (ADR-0025), not a lane
that is half isolated.

## Decision

1. **Every lane runs its own Compose project** `<project>-<lane>` for each
   group, from the Compose files of its own worktree. Nothing of the main
   tree's infrastructure is started, stopped, restarted or read by a lane.
   ADR-0012 is decided for option A, locally as on a node (ADR-0025).
2. **devstack writes an overlay per lane and group**
   (`.devstack/lane-<name>/compose/<group>.yml`, last `-f`, written again at
   every start from the group's configuration): every published host port
   moved by the lane's offset (`[lanes] port_step` times its number, the same
   offset as its services), no fixed `container_name`, every network or volume
   with a fixed name renamed `<name>-<lane>` (declared external networks
   included, which devstack creates). External volumes stay shared: someone put
   data there on purpose. ADR-0014 host rewriting reads the moved ports from
   Compose as before.
3. **Sharing is opt-in per Compose service, for what holds nothing per lane**:
   `[compose.<group>] shared = ["mailbox", "otel"]`. A lane does not start
   those, uses the main tree's by their published ports, and never restarts
   them. Containers of a lane do not reach them by name (they are on another
   network). Services with data or that dispatch work are not meant for it;
   devstack cannot tell, the stack file's author does.
4. **Lifecycle:** `devstack up` in a worktree starts the lane's containers;
   `devstack lane stop` removes them and keeps their volumes, so the next start
   finds its data; a worktree removed takes the lane's containers, volumes and
   networks with it. A daemon restart leaves them running, like the main
   tree's (ADR-0016).
5. **Memory check:** before a lane starts, devstack compares what the main tree
   takes now (services and containers, plan §12) with the memory available,
   and refuses with that number and the ways out (stop another lane, a lane on
   another machine) unless `devstack up --force`.
   *Amended 2026-10-27 (#309):* where the containers run in a VM of their
   own (Docker Desktop on Windows and macOS), the containers' part of
   that is compared with what is free in the VM (`docker info` `MemTotal`
   less what every running container uses), and only the services' part
   with the host's free memory; the refusal names the limit that was hit.
6. **Workers:** a lane still runs without background workers by default (they
   may also talk to systems outside the stack). `devstack up --workers` turns
   them on for that lane: its coordinators are its own now. A service with
   `lanes = false` keeps its workers in the main tree regardless.

`{lane}` and `{lane_suffix}` stay available; with an infrastructure per lane
they are no longer needed to separate data.

## Alternatives considered

- **Share by default, per-lane names (ADR-0024 point 4).** Least memory; the
  costs in the context above.
- **Share passive stores, isolate coordinators (ADR-0012 option B).** Keeps the
  restart and naming problems for the stores, which hold the data that matters.
- **Isolate, nothing shared at all.** Simplest rule, but a telemetry dashboard
  or a fake SMTP server per lane costs memory and isolates nothing.
- **Containers of a lane inside the main project, renamed per lane.** One
  project to look at, but Compose merges, `down` and `ps` then mix lanes again.

## Consequences

- Each lane costs the memory and start time of a second copy of the
  containers; its first start migrates and seeds its own data.
- A lane's branch may change its Compose files and gets what it changed.
- Breaking for existing lanes (no outside users yet, no migration): after the
  update a lane starts its own containers; data in the main tree's containers
  under per-lane names stays where it is.
- Not solved: a restart of the Docker engine still affects every lane.

## Enforced by

`lane_overlay` in `crates/devstack-compose` (with its unit test),
`compose_group` / `write_lane_overlay` / `is_shared` in
`crates/devstack-daemon/src/supervisor/infra.rs`, `remove_infra` and the port ranges
in `crates/devstack-daemon/src/lanes.rs`, `memory_fits` in `api.rs`,
`memory_available` in `crates/devstack-proc`, `shared` in `crates/devstack-config`
(validated against `services`), the integration test
`a_lane_has_its_own_containers`.
