# ADR-0025: A lane can run on another node, as a Linux container with its own infrastructure

- **Status:** Accepted (2026-10-16), point 10 amended and accepted (2026-10-16); amended 2026-10-19 (a start on a node answers once leased, and its phases are timed); points 5 and 7 amended by [ADR-0038](0038-a-lanes-data-outlives-its-lease.md) (2026-10-19: a lane's mirror and data outlive its lease, for a while); placement and trust in detail: ADR-0026 to ADR-0031
- **Date:** 2026-10-15
- **Deciders:** project owner
- **Related:** plan §3.5, §9, [ADR-0024](0024-a-lane-is-a-supervisor-in-the-main-trees-daemon.md) (local lanes, built in 0.5.0), [ADR-0002](0002-processes-run-from-immutable-artifacts.md), [ADR-0004](0004-infrastructure-is-delegated-to-compose.md), [ADR-0008](0008-background-workers-run-only-in-the-active-slot.md), [ADR-0011](0011-platform-windows-native-first.md), [ADR-0012](0012-shared-infrastructure-between-lanes.md), [ADR-0015](0015-devstack-is-optional.md), [ADR-0026](0026-placement-by-the-requester-admission-by-the-node.md), [ADR-0027](0027-trust-between-nodes.md), [ADR-0028](0028-lane-images.md), [ADR-0029](0029-source-sync-and-port-forwarding-for-remote-lanes.md)

## Context

A second lane is a second copy of the stack: its own infrastructure, its own
services, its own builds and dev servers. On a typical developer machine one
stack already takes several gigabytes of memory (database containers, a .NET
API with workers, an Angular dev server); a second or third worktree, often
opened by an AI agent rather than a person, exceeds what the machine has.

Teams usually have spare capacity elsewhere: a server, or colleagues' machines
that are idle. The aim is to run a lane there while the developer keeps editing
locally, with the same feedback loop: a save in the IDE starts a build, the
service is reachable on `localhost`, logs and build results appear in the same
CLI, MCP tools and dashboard.

Plan §9 sets a guard against becoming a general orchestrator: "devstack never
schedules, places or networks containers itself." Running a lane elsewhere is a
placement decision, so this ADR has to move that guard on purpose.

## Decision

1. **The unit is the lane.** A lane (a worktree, run by a supervisor in the
   main tree's daemon, ADR-0024) has a **placement**: `local` (ADR-0024) or a
   **node**, chosen when it starts (`devstack up --on <node>` in the
   worktree, or the dashboard). The worktree, and with it the source, stays on
   the developer's machine; git creates and removes it, never devstack. A local
   lane already runs from a copy of its worktree (ADR-0024); a remote one runs
   from a copy on the node, so a lane runs the same way wherever it is. All of a lane runs on its node: infrastructure,
   builds, services, dev servers. devstack never places a single service or a
   single build elsewhere.
2. **A node runs `devstack-node`**, an agent binary from this repository, on any
   host with a Docker engine (a Linux server, or a developer machine with Docker
   Desktop). Per remote lane it starts:
   - the lane's **infrastructure** as a Compose project of its own on the node's
     Docker engine, from the Compose files in the lane's source (ADR-0004
     holds: Compose owns every container);
   - one **lane container** from the stack's lane image (ADR-0028), in which an
     unmodified Linux `devstackd` runs the lane's application plane exactly as it
     would locally: snapshot, build, artifacts, candidates, promotion
     (ADR-0002, ADR-0003, ADR-0009). The lane container has no access to the
     Docker engine.
3. **Remote lanes share no infrastructure**, with each other or with local
   lanes; local lanes have their own as well ([ADR-0030](0030-each-lane-has-its-own-infrastructure.md)), so a lane is the
   same wherever it runs, and on a node the memory it costs is the node's. A node keeps an artifact store of its
   own; the shared store of ADR-0024 is per machine.
4. **The main slot may be remote, on a dedicated node only.** A node is
   *dedicated* when its owner has declared that it never reclaims capacity (a
   server, ADR-0026); developer machines take non-main lanes only. A remote lane
   owning the main slot runs its workers there (ADR-0008); this is safe because
   its infrastructure is its own (point 3). Conditions: the stack has no
   Windows-only services in the remote lane (point 8), and `devstack promote
   --lane <local lane>` is always the way back, a cold start with data
   recreated by seed jobs. The move itself is ADR-0024 point 5: the fixed ports
   forward to the lane's forwarded ports, and the workers move. A remote
   non-main lane runs without workers, like a local one (ADR-0012
   interim rule).
5. **A remote lane is disposable.** Its source lives with the requester; its
   data (databases, volumes) is lost when the lane is evicted or moved and is
   recreated by the stack's seed jobs. Moving data between nodes is not
   supported.
6. **The developer's machine sees a local lane**: source changes are synced to
   the node and service ports are forwarded to `localhost` (ADR-0029); the
   requester's daemon proxies the remote daemon's API and event stream under
   the same `/v1/lanes/<name>/v1/...` as a local lane (ADR-0024), so CLI, MCP
   and dashboard need no notion of "remote" (ADR-0007).
7. **Lifecycle follows ADR-0024.** A daemon restart on the developer's machine
   is clean (ADR-0016): the main slot returns to the main tree, remote lanes are
   stopped and their leases released; `devstack up` starts them again, on the
   node that still has their mirror if it can. Removing the lane releases the
   lease and deletes the mirror on the node.
8. **Remote lanes run Linux containers.** Windows-only services do not run in
   a remote lane. When a remote lane owns the main slot they keep running in
   the main tree, as any service the lane does not run (ADR-0024 point 5);
   that is correct only for a service that reaches the lane through its fixed
   ports and does not use the infrastructure itself, which `devstack check`
   warns about. Locally, ADR-0011 is unchanged.
9. **Not supported:** attaching a debugger to a process in a remote lane. Logs,
   the candidate pipeline and hot-reload mode (plan §3.4a) are the feedback
   loop.

10. *Amended 2026-10-16.* **A lane shows where it runs and can move.** Its
    placement (`local` or the node's name) is part of the lane everywhere it
    appears: dashboard, `devstack lane list`, MCP. `devstack lane move <lane>
    --to <node|local>` (and the dashboard) moves it, between nodes and between
    a node and this machine: a cold start at the target, as point 5 has it
    (data recreated by seed jobs), made before the old placement is broken:
    the target is leased, synced and started, the lane's forwarded ports
    switch to it once it is healthy, and only then is the old lease released.
    Back to this machine the ports cannot be held twice: they are handed
    over first, and the lane on the node keeps running until the local one
    is ready (and takes them back if it does not get there). *Measured with
    M3b:* from this machine to a node in 8 s, back in 3 s, the lane's port
    unanswered for about a second, then 0.5 s.

The guard of plan §9 becomes: *devstack places whole lanes on nodes; inside a
lane, Compose owns containers and devstack never schedules, places or networks
individual containers or services.*

*Measured in spike M0, 2026-10-16* ([`spikes/remote-lane`](../../spikes/remote-lane/README.md)):
an unmodified daemon in the stack's toolchain image ran the fixture stack's
lane from a mirror without `.git`, with the infrastructure started by the
agent and described to it in a file instead of Docker (`DEVSTACK_EXTERNAL_INFRA`:
environments, `service:port` addresses on the lane's network, states). The API
built and was healthy in 5 s, an edit in the mirror was a running new build 5 s
later, the container took about 300 MiB. Hosts keep their Compose names, which
resolve on the lane's network. Still to do: a lane mode for that daemon (no
workers, ADR-0012), and a forward inside the container, since services and the
daemon listen on its loopback. A lane container needs no port offset; only the
requester's forwards do.

*Measured with M2b, 2026-10-16*, on a Linux server as the node, driven from a
Windows machine over the pool's network: the fixture's lane leased, synced (6
files) and started there in 2.8 s, its API built and healthy 5 s later, the
lane container at 157 MiB. The daemon there runs in lane mode
(`DEVSTACK_REMOTE_LANE`: no workers; `DEVSTACK_LISTEN`: its API on the lane's
network, still behind its token and a loopback `Host`), as the agent's user
with a home of its own, and the lane publishes no host port on the node
(`devstack_compose::node_overlay`). An expired lease took container, Compose
project, network and mirror with it.

*Measured with M2c, 2026-10-16:* `devstack up --on <node>` in a worktree of a
copy of the fixture: leased, 7 files synced in 0.2 s, started on the node in
3 s, the API healthy 2.4 s after `up`; an edit in the worktree was a new
healthy build on the node about 8 s later; `devstack lane stop` and `devstack
down` (the daemon ends) each left nothing on the node.

*Amended 2026-10-19.* **A start on a node answers once the lane is leased,
and is followed.** A real stack's first start on a node took 4.4 minutes
(sync 0.7 s, then one Compose group 39 s and another 3 min 41 s), and
`POST /v1/lanes` held its request open all that time: a proxy or client with
a shorter timeout ends it, and the minutes could not be broken down. Now the
requester's daemon answers `202` once what can be told at once is told (the
worktree, a clash with where the lane runs, the coordinator's lease and the
node's admission: refused with `409`), and syncs and starts the lane in the
background, as `POST /v1/up` does for services. Progress and outcome are the
lane's `moving` (point 10's steps, in `GET /v1/lanes` and the `lanes`
event): gone once it runs there, carrying the error when it failed. The CLI
(`devstack up --on`), the MCP `up` and the dashboard follow it, so `up`
still ends when the lane runs, or fails with the reason. The node times
each phase of the start itself (its polling requester would miss a short
one) and splits each Compose group into checking, images (pull, build) and
containers, starting, and waiting until healthy; the requester logs every
phase as `remote_lane_phase` with its `ms`, the lease and the sync
included, so each phase is a duration of its own in the pool's telemetry
([ADR-0037](0037-telemetry-goes-to-the-pools-collector-with-consent.md)).
A move (point 10) still answers when done.

## Alternatives considered

- **Remote builds only, run locally.** Moves CPU, not memory; the memory of a
  lane is mostly in its running services and infrastructure. Also needs
  cross-built artifacts for Windows.
- **Remote infrastructure only (Docker context over SSH).** Cheap, and still a
  reasonable manual setup, but leaves the native services and dev server of
  every lane on the developer's machine.
- **Main always local.** Simplest, and the stack keeps working without the
  farm, but leaves the heaviest lane, the one used all day, on the machine
  that lacks the memory.
- **Placing individual services across nodes.** A service talks to its lane's
  database and siblings on every request; spreading a lane over the network
  adds latency and failure modes, and needs cross-node service networking,
  which is an orchestrator's job (Kubernetes), not devstack's.
- **Kubernetes or a cluster scheduler as the substrate.** Designed for
  long-lived workloads on dedicated nodes, not for developer machines that
  join and leave; a dependency the team has to run. Nomad is under a
  non-MIT-compatible licence (BUSL).
- **Docker-in-Docker inside the lane container.** Needs a privileged container
  or the Docker socket, which is root on the host; not acceptable on a
  colleague's machine.

## Consequences

- A second binary (`devstack-node`) and a Linux build of `devstackd` become
  release artefacts.
- How a native service in the lane container reaches the lane's Compose
  services (ADR-0014 rewrites hosts to published ports on `localhost`) has to be
  settled in a spike: joining the Compose network and keeping service names, or
  publishing ports into the container.
- Forwarded ports follow ADR-0024's allocation (`[lanes] port_step`), so a
  lane has the same ports whether it runs locally or on a node.
- The requester's daemon gains a second kind of lane, and the dashboard shows
  where each lane runs.
- With a remote main, the developer's daily stack depends on the node and the
  network; when either is gone, work continues only after promoting a local
  lane. Goal 2 of the plan (main never regresses) holds only because main is
  restricted to nodes that do not reclaim.

## Enforced by

Will live in: `crates/devstack-node` (agent: lane container, Compose project per
lane), the lane model of phase 3 in `crates/devstack-daemon` (placement), plan
§3.5 and §9, `devstack check` (Windows-only components). Point 4 lives in
`crates/devstack-daemon/src/lanes.rs` (`take_main` for a lane on a dedicated
node, `main_slot_check` giving the ports back when it is gone),
`crates/devstack-daemon/src/remote.rs` (`set_main`, a lane counts as gone after
a lease's life without renewal), `POST /v1/main-slot` in the lane daemon, and
`crates/devstack/tests/remote_lane.rs`.

*Paths as built, noted 2026-10-18.* The node side:
`crates/devstack-node/src/node.rs` (the agent's API: admission, leases,
mirrors, the lane's API and tunnels), `crates/devstack-node/src/lanerun.rs`
(the lane container and its own Compose projects; the container gets
`DEVSTACK_LISTEN`, `DEVSTACK_REMOTE_LANE`, `DEVSTACK_PORT_OFFSET`,
`DEVSTACK_DISPLAY_ROOT` and `DEVSTACK_EXTERNAL_INFRA`, see
[pool.md](../pool.md#reference)), `confine.rs` and `safefs.rs` (what a lane
may ask of the node). The requester side: `crates/devstack-daemon/src/remote.rs`
(lease, sync, renewal, forwards), `lanes.rs` (start, move, `take_main`),
`external.rs` (the lane daemon reads its infrastructure from the file
`DEVSTACK_EXTERNAL_INFRA` names). A service's `platforms` in the stack file
leaves it out where it cannot run (`on_this_platform` in the supervisor);
`devstack check` does not judge Windows-only components.

*Noted 2026-10-19, with the amendment.* The start that answers once leased:
`Lanes::activate_remote` and `launch_remote` in
`crates/devstack-daemon/src/lanes.rs`, `RemoteLane::lease` and `launch` in
`remote.rs`, `lanes_start` (`202`) in `api.rs`; its clients `join_lane` in
`crates/devstack/src/client.rs` (the CLI and the MCP server) and `startLane`
in `ui/src/stores/stack.ts`. The phase times: `LaneState::phases` in
`crates/devstack-node/src/node.rs`, the phases of `LaneRun::start` in
`lanerun.rs`, `remote_lane_phase` in `remote.rs`.
