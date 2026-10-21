# ADR-0051: A pool can grow by nodes a provisioner makes for one member's lane, and gives back when it is removed

- **Status:** Proposed
- **Date:** 2026-10-21
- **Deciders:** project owner
- **Related:** [ADR-0025](0025-a-lane-can-run-on-another-node.md) (remote lanes, their timed phases), [ADR-0027](0027-trust-between-nodes.md) (point 6: what a node trusts), [ADR-0031](0031-a-coordinator-places-lanes.md) (placement, joining), [ADR-0032](0032-the-pool-is-administered-in-the-dashboard.md) (admins grant nodes), [ADR-0033](0033-a-node-decides-what-runs-on-it.md) (what a node admits), [ADR-0038](0038-a-lanes-data-outlives-its-lease.md) (slots kept after a lease), [ADR-0049](0049-a-lanes-need-is-measured-disk-included.md) (a lane's need)

## Context

A pool has the nodes its members brought. When none of them can take a lane,
`devstack up --on pool` is refused (`no node of the pool can take the lane
now`, `lease_create` in `crates/devstack-node/src/coordinator.rs`) and the
lane runs locally or not at all. The case this leaves out: a developer on a
laptop who needs, for a few hours, more than the laptop and the pool have.
Cloud providers rent virtual machines by the hour, created and deleted
through an API in about a minute. A machine made for that lane, and deleted
with it, would cover the case.

Three things in the pool stand against doing that by hand or with an
external autoscaler:

- **Joining needs an admin twice**: approving the member, then granting the
  node (ADR-0031 point 2, ADR-0032). A machine that exists for an hour
  cannot wait for either.
- **A node trusts every member it runs lanes for** (ADR-0027 point 6), and
  isolation between members on one node is a container's (ADR-0033). For a
  machine paid for by, or on behalf of, one member, the lanes of others
  have no business there.
- **A node keeps a stopped lane's slot** (ADR-0038). On a rented machine a
  kept slot is a running bill.

The provider, its API token and whatever is billed are not devstack's: a
team renting on its own account and an operator selling a pool as a
service need the same mechanism and different code behind it.

## Decision

1. **A coordinator can have a provisioner**: an HTTP service it calls
   (`devstack node serve --provisioner <url>`, mutual TLS or a bearer token
   from the coordinator's state). The provisioner holds the provider's
   credentials; the coordinator never does. Without one, nothing changes.
2. **It is asked only when placement finds nothing.** For a lane placed by
   the pool (`--on pool`, not a named node) with no eligible node, and a
   member an admin allowed to cause provisioning (`devstack node members
   provision <id> on`, off by default; it costs someone money), the
   coordinator asks the provisioner for a node with the lane's need
   (ADR-0049: CPU, memory, disk, GPUs) and its own version and pool
   protocol, so the machine runs a devstack that works with it (ADR-0031
   point 8). The provisioner accepts with an estimate or refuses with a
   reason; a refusal is the refusal the requester gets today, with the
   provisioner's reason added. Admin caps bound it: provisioned nodes at
   once, in all and per member.
3. **The lease waits while the machine comes.** The lease is `provisioning`
   until the node is online and admits it, then is offered as any other.
   The requester shows a `provision` phase, timed like the others
   (ADR-0025, amended 2026-10-19). Past a limit (the estimate, at most ten
   minutes) the coordinator gives the machine back and the start fails.
4. **The machine enrols itself, once.** With the request the coordinator
   issues an enrolment: its address, its CA's fingerprint, a single-use
   token valid for the wait. `devstack join <url> --fingerprint <fp>
   --enrol <token>` on the machine makes it an approved member and a
   granted, dedicated node in one step, marked `provisioned` and bound to
   the member whose lane asked for it. No other token admits anyone.
5. **A provisioned node runs that member's lanes only.** Placement puts no
   other member's lane there, and the node refuses one. A later lane of the
   same member may use it while it exists, which is what makes a second
   lane on the same trip fast.
6. **It is given back when its last slot goes.** `lane remove` drops the
   lane's slot (ADR-0038); a provisioned node without slots is given back
   at once. A stopped lane's slot is kept for the node's idle time (the
   admin's, default 30 minutes, not ADR-0038's hours), then pruned, and the
   node is given back with it. Giving back revokes the member first, then
   asks the provisioner to delete the machine, retried until it answers;
   a node that is not confirmed deleted stays listed. What was on it is
   gone: nothing of a provisioned node's slots is kept elsewhere.
7. **The provisioner is told what happened**: a node ready, a lease on it
   begun and ended (member, lane, need, times), the node given back. It may
   bill from that; devstack does not count money.
8. **The pool shows provisioned nodes** in the dashboard and `devstack
   pool`, with whose they are, since when and their idle time left, and an
   admin can give one back by hand.

## Alternatives considered

- **Provider APIs in the coordinator** (a crate per provider). The
  coordinator's host would hold every provider's token, and devstack would
  carry vendor code and its upkeep for a feature most pools do not use.
- **An external autoscaler watching the pool** (`GET /v1/placement`, the
  capacity of ADR-0036). Needs no change, but a refused start fails instead
  of waiting, joining still needs an admin, and the machine is a node like
  any other, trusted by every member and keeping slots for hours.
- **A command the coordinator runs** instead of an HTTP service. Simpler
  for one team on one host, but the provider's token then lives with the
  coordinator, and a slow provider ties up the coordinator's process. A
  team can run the provisioner on the same host and get the same.
- **A pool of warm, unbound spare nodes.** Faster starts, but a spare is
  paid while it waits and is shared by whoever comes. A provisioner may keep
  spares itself; until one is bound, it is no node of the pool.

## Consequences

- A start on a provisioned node is cold every time: the whole tree is
  uploaded and the build starts empty, since no slot survives the machine
  (ADR-0038's preferences cannot help). A lane image with a warm build of
  the main branch (ADR-0028) is what shortens it.
- A provisioned machine must be reachable from the member, usually on a
  public address with mutual TLS: the network ADR-0031 point 5 leaves to
  the deployment is then the internet, not a mesh.
- A machine the provisioner failed to delete costs money until someone
  notices; point 6 keeps it listed, nothing more.
- An enrolment token is a credential to the pool for its lifetime; it is
  single-use, short-lived, and bound to one request.
- The lease state `provisioning` is new; a requester of an earlier pool
  protocol is refused as today rather than told to wait, and the protocol
  number goes up if the offer changes shape.

## Enforced by

Will live in: `crates/devstack-node/src/coordinator.rs` (the fallback in
`lease_create`, the `provisioning` lease, giving back, the admin caps),
`store.rs` (a member's `provision` permission, a node's `provisioned` mark
and binding), `pki.rs` (the enrolment token), `node.rs` (refusing other
members on a bound node; the short idle time), `version.rs` (`PROTOCOL`),
`crates/devstack-daemon/src/remote.rs` (the `provision` phase),
`crates/devstack/src/poolcmd.rs` (`--provisioner`, `--enrol`, `members
provision`), the pool area under `ui/`, [pool.md](../pool.md) and the
provisioner's API in [dashboard-api.md](../dashboard-api.md).

## What this ADR does not say

- Which providers have a provisioner, or whether devstack ships one. This
  ADR is the contract; implementations are separate.
- Anything about prices, billing or payment. The events of point 7 are all
  devstack provides; the dashboard has no place to buy anything.
- That provisioned nodes are shared, pooled or kept warm between members.
- That a named node (`--on <node>`) or a local lane is ever provisioned.
