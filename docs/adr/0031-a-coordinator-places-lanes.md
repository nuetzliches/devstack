# ADR-0031: A coordinator places lanes and admits members; nodes admit leases; data goes directly

- **Status:** Accepted (2026-10-16)
- **Date:** 2026-10-16
- **Deciders:** project owner
- **Related:** [ADR-0025](0025-a-lane-can-run-on-another-node.md), [ADR-0026](0026-placement-by-the-requester-admission-by-the-node.md) (points 1 and 3 replaced here), [ADR-0027](0027-trust-between-nodes.md) (points 1 and 2 replaced here), [ADR-0029](0029-source-sync-and-port-forwarding-for-remote-lanes.md), [ADR-0015](0015-devstack-is-optional.md)

## Context

ADR-0026 has every requester choose a node from its own peer list, so that no
central service is needed, and ADR-0027 has every pair of machines pin each
other's keys. In the first real setup there is an always-on server anyway:
someone has to know who is a member, what each machine shares and which lane
runs where, and the dashboard should show the whole pool. With that server in
place, deciding at every requester buys little: when the server is down, no
member can report its capacity either, so new placements could not be
trusted; and pairwise keys mean every new machine is a key exchange with every
other.

What a central service must not do is carry the lanes' traffic: a save is a
sync, a page load a forwarded connection, hot module replacement a WebSocket.
Through a central relay every byte between a developer machine and a lane on
a colleague's machine would cross the server twice, and the server would be the
bottleneck of every lane.

## Decision

1. **A coordinator** (`devstack-node --coordinator`, the same binary as a
   node) runs on an always-on host. It keeps the members, their approval, the
   capacity each node shares and its load, the leases, and decides where a lane
   goes. The host is usually a node too, often a dedicated one (ADR-0026).
2. **Members join once.** `devstack join <coordinator>` creates the machine's
   key pair (ADR-0027) and asks to be admitted; an administrator approves it on
   the coordinator (`devstack-node members approve <id>`, over SSH). The
   coordinator then issues the member a certificate from its own certificate
   authority; every connection between members is mutual TLS with those
   certificates. Removing a member revokes it: nodes take the member list from
   the coordinator and refuse what is not on it.
3. **Nodes share what they can spare.** A member that shares (a switch in the
   dashboard, prominent in its header or sidebar) offers by default half of
   its memory and CPU that are free right now, recomputed as it goes, never
   more than a fixed cap its owner sets. It reports that to the coordinator.
4. **The coordinator places.** A requester asks for a lane (repository, lane,
   expected need, constraints); the coordinator picks a node by the order of
   ADR-0026 point 3 and creates the lease. The node admits it against its own
   current capacity before anything starts, so a stale report cannot
   over-commit it. `--on <node>` names the node and skips the choice, not the
   admission.
5. **Data goes directly** between the requester and the node: sync, the lane's
   API and events, forwarded ports (ADR-0029). The network that connects them
   (a mesh VPN in the first setup) is the deployment's choice; devstack needs
   addresses that reach each other and no public port.
6. **When the coordinator is down,** running lanes keep running: the requester
   renews its lease with the node directly and the node reports the renewals
   when the coordinator is back. New placements and new members wait.
7. **The dashboard shows the pool:** members online, what each shares and
   uses, which lane runs where, and the share switch of this machine.

## Alternatives considered

- **Placement by the requester, pinned keys (ADR-0026, ADR-0027 as written).**
  No central service, but no view of the pool, a key exchange per pair of
  machines, and decisions on reports that may be stale; the server they avoid
  is there anyway.
- **A coordinator that also relays the traffic.** One connection per member
  and no direct reachability needed, but every lane's bytes cross the server
  twice; it becomes the bottleneck and its outage stops every lane.
- **Public endpoints instead of a private network.** Works without a mesh, but
  every node exposes a port to the internet; mutual TLS would be the only
  protection.

## Consequences

- One more role to operate: the coordinator, deployed like any service.
- Its certificate authority's key is the most sensitive file of the pool.
- A member that cannot reach the coordinator cannot start a new remote lane;
  local lanes are unaffected (ADR-0015).

## Enforced by

Will live in: `crates/devstack-node` (coordinator: members, approvals, CA,
leases, placement; node: admission, reports), `devstack join` and the remote
client in `crates/devstack-daemon`, the dashboard's pool view and share switch.
