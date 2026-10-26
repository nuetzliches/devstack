# ADR-0052: A restart of the stack carries its lanes over to the next daemon

- **Status:** Accepted (2026-10-22), amended 2026-10-26 (point 6: a lane on a node the daemon ended without stopping is said, not silently gone)
- **Date:** 2026-10-22
- **Deciders:** project owner
- **Related:** [ADR-0016](0016-a-daemon-restart-is-a-clean-restart.md) (a daemon restart is clean), [ADR-0024](0024-a-lane-is-a-supervisor-in-the-main-trees-daemon.md) point 6 (lanes start only with `devstack up`), [ADR-0025](0025-a-lane-can-run-on-another-node.md) point 7 (a daemon restart releases the lanes on nodes), [ADR-0029](0029-source-sync-and-port-forwarding-for-remote-lanes.md) (sync and ports of a lane on a node), [ADR-0037](0037-telemetry-goes-to-the-pools-collector-with-consent.md) (telemetry)

## Context

The hub updates itself (and the installed devstack) in place; a stack's
daemon goes on with the release it was started from until it is started
again. The dashboard says so ("shop runs devstack 0.15.0, devstack 0.18.0
is installed") and offers "Restart stack".

While a lane runs, that restart cannot do it. The lanes live in the main
tree's daemon (ADR-0024), a restart is clean (ADR-0016), and a daemon that
stops releases its lanes on nodes (ADR-0025 point 7): to keep the lanes, the
dashboard restarts only the main tree's services inside the old daemon. Seen
in the pool's telemetry on 2026-10-22: a member pressed "Restart stack"
three times in 30 s, each time the 0.15.0 daemon answered, and the banner
stayed; it went only after the lane on a node was stopped and the stack
stopped and started. The lane had run on its node for 18 hours. Its
containers there had nothing to do with the daemon's release.

What a restart costs a lane differs by where it runs:

- **A lane on a node** runs there in containers with a daemon of its own
  (ADR-0025). Nothing of it is a process of the developer's daemon; that
  daemon holds the lease (renewed every 30 s, it lives 150 s without), syncs
  the worktree and forwards the lane's ports. All of that can be taken up by
  the next daemon from a handful of values.
- **A lane on this machine** is a supervisor in the daemon; its processes
  end with it (ADR-0009, ADR-0016). Its Compose containers stay; the
  artifacts its services ran are kept, and the next start runs them again
  without a build (ADR-0016 point 4).

## Decision

1. **A restart is a stop that leaves word for the next daemon.** A daemon
   told to stop for a restart (`POST /v1/shutdown?restart=true`; the hub's
   `POST /hub/stacks/{id}/down?restart=true`, the dashboard's "Restart
   stack") writes `.devstack/restart.json` before it stops: the lanes that
   run here (name, profile), the lanes on nodes (name, pool, node, lease,
   ports here and there), and which lane serves the main ports. It stops as
   before, except that it **does not release the leases** of its lanes on
   nodes: it only stops renewing, syncing and forwarding.
2. **The next daemon takes the lanes up**, if the file is younger than two
   minutes (a lease the old daemon renewed lives that long at least) and was
   written for this stack; the file is removed when read, so it is honoured
   once.
   - A lane on a node: its lease is renewed first. Renewed, the lane is
     followed as if this daemon had started it: synced (in full once, since
     the hashes of the last sync are not carried), renewed, its ports
     forwarded here again. The node and the lane's own daemon there notice
     nothing; this works with every node release. Not renewed (the lease is
     over, the node does not answer), the lane is not started and that is
     said.
   - A lane on this machine starts again, with its profile, from the
     artifacts it ran (ADR-0016 point 4), as `devstack up` in its worktree
     would; its candidates are not restored.
   - The main slot goes back to the lane that held it once that lane runs
     again; if it cannot (the lane did not come back, a service is not
     ready within the start's wait), the main tree keeps it and that is said.
3. **Everything else stays clean.** A daemon that crashes, is killed, or is
   stopped (`devstack down`, "Stop stack", `down --all`) leaves no file and
   behaves as ADR-0016, ADR-0024 point 6 and ADR-0025 point 7 say. A lane
   whose start or move is under way at the restart is not carried: its
   lease, if it got one, expires on the node.
4. **The dashboard's "Restart stack" always restarts the daemon**, with lanes
   running too, and says before what happens to them: the lanes on nodes
   keep running, the lanes here restart with it. A daemon older than this
   decision cannot carry its lanes (it ignores `restart`): the confirmation
   then says that its lanes stop and have to be started again.
5. **Telemetry** (ADR-0037): the stopping daemon logs what it carries
   (`daemon_stopping` with `restart` and the number of lanes by place), the
   next one each lane taken up or not (`lane_carried`, `lane_not_carried`
   with the place, the node, the time, the reason) and the main slot
   (`main_slot_carried`). The hub logs once per stack and pair of releases
   that a daemon runs another devstack than the hub (`daemon_outdated`),
   so how long stacks stay on an old release shows in the pool's data.

6. **A lane on a node lost to an unclean end is said** (amended
   2026-10-26). While a daemon follows a lane on a node, the lane's state
   holds a note of it (`.devstack/lane-<name>/followed.json`: pool, node,
   lease, ports). Stopping the lane, or seeing its lease end, removes the
   note; a restart leaves it for the next daemon, which takes the lane up
   (point 2) and writes it again. A note the next daemon finds for a lane it
   did not take up comes from a daemon that ended without stopping the lane
   (it crashed or was killed, the machine shut down or restarted): the
   lease is released on the node if it still runs (point 3: as a stop), the
   note goes, the log says `remote_lane_unfollowed` (lane, node, lease), and
   the lane shows the start that failed, with the reason and how to start it
   again (`devstack up --on <node>`, from its data kept there, ADR-0038). The
   lane is not started again by itself.

This amends ADR-0024 point 6 and ADR-0025 point 7 for a restart; ADR-0016's
rule that the daemon re-attaches to no process stays: nothing of a lane on
a node is a process of the daemon, and a lane here is started again, not
re-attached.

## Alternatives considered

- **Keep the old daemon while lanes run (as before).** The version switches
  only when every lane is stopped; the dashboard's restart does not do what
  its banner asks, as seen. Rejected.
- **The dashboard stops the lanes and starts them again itself.** Works with
  old daemons too, but restarts a lane on a node from scratch (a new lease,
  a full sync, its containers started again: 37 s for the lane above) for
  nothing that concerns the node, and the dashboard would have to remember
  which lane went where across the restart. Rejected; it is what an old
  daemon gets (point 4), by hand.
- **Re-attach local lanes' processes.** Rejected by ADR-0016 (orphans, a
  shim per process).
- **Start a lane lost to an unclean end again by itself** (point 6). Seen
  on 2026-10-26: a member's machine restarted overnight, its lane on a node
  had run for 2.8 days, and its lease expired two minutes after the machine
  went down. Starting it again on the next `up` would take a node's CPU and
  memory nobody asked for after a reboot; the kept data makes the start by
  hand fast. Rejected for now; the note would allow it.
- **Hand the lease to the new daemon by asking the node.** The node would
  have to know about restarts; renewing the same lease from the next daemon
  needs nothing new there.

## Consequences

- A restart keeps a lane on a node running: its users (and those it is
  shared with, ADR-0035) see only the ports here drop for the seconds the
  daemon is away. A lane here restarts, as the main tree does.
- `.devstack/restart.json` is the only state a daemon leaves for the next
  one besides what ADR-0016 lists; a file that is too old is ignored and
  removed.
- A lease the old daemon did not release and nobody takes up expires on the
  node after 150 s, as for a crash.

## Implemented

`crates/devstack-daemon/src/lanes.rs` (`Lanes::carry`, `Lanes::take_up`),
`crates/devstack-daemon/src/remote.rs` (`RemoteLane::adopt`; point 6:
`followed`, `forget_followed`, `Lanes::unfollowed` in `lanes.rs`),
`crates/devstack-daemon/src/api.rs` (`/v1/shutdown?restart=true`),
`crates/devstack-hub/src/lib.rs` (`down?restart=true`, `daemon_outdated`),
`ui/src/stores/stack.ts` (`restartStack`).
