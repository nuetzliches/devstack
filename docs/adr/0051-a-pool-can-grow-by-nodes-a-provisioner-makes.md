# ADR-0051: A pool can grow by nodes a provisioner makes, fills them before it makes another, and gives back the idle ones

- **Status:** Proposed; revised 2026-10-26 before acceptance (nodes bound to the pool, not to one member; scaling out by placement)
- **Date:** 2026-10-21
- **Deciders:** project owner
- **Related:** [ADR-0025](0025-a-lane-can-run-on-another-node.md) (remote lanes, their timed phases), [ADR-0027](0027-trust-between-nodes.md) (point 6: what a node trusts), [ADR-0030](0030-each-lane-has-its-own-infrastructure.md) (a lane and its infrastructure are on one node), [ADR-0031](0031-a-coordinator-places-lanes.md) (placement, joining), [ADR-0032](0032-the-pool-is-administered-in-the-dashboard.md) (admins grant nodes), [ADR-0033](0033-a-node-decides-what-runs-on-it.md) (what a node admits), [ADR-0034](0034-a-machine-can-belong-to-several-pools.md) (pools do not trust each other), [ADR-0038](0038-a-lanes-data-outlives-its-lease.md) (slots kept after a lease), [ADR-0049](0049-a-lanes-need-is-measured-disk-included.md) (a lane's need)

## Context

A pool has the nodes its members brought. When none of them can take a lane,
`devstack up --on pool` is refused (`no node of the pool can take the lane
now`, `lease_create` in `crates/devstack-node/src/coordinator.rs`) and the
lane runs locally or not at all. The case this leaves out: a team, or a
developer on a laptop, that needs for a few hours more than its machines
have. Cloud providers rent virtual machines by the hour, created and deleted
through an API in about a minute.

The unit to rent is not a lane. A lane and all its infrastructure run on one
node (ADR-0030), and several lanes of one repository share its images,
builds and kept slots there (ADR-0038). A machine per lane would start every
lane cold and pay for a whole machine per lane. The unit is a machine of the
team, holding as many of its lanes as fit, and as many machines as the lanes
need. That is what a pool with several nodes already is: the coordinator
places each lane on one node, so growing means adding nodes, not balancing
traffic between them.

What stands against adding them by hand or with an external autoscaler:

- **Joining needs an admin twice**: approving the member, then granting the
  node (ADR-0031 point 2, ADR-0032). A machine that exists for an hour
  cannot wait for either.
- **Placement spreads.** Of the eligible nodes it prefers a warm one, then
  the one with most free capacity (`rank`). On rented machines that keeps
  every machine half full, and none ever empty enough to give back.
- **A node keeps a stopped lane's slot** for hours (ADR-0038). On a rented
  machine a kept slot is a running bill.

The provider, its API token and whatever is billed are not devstack's: a
team renting on its own account and an operator offering pools as a service
need the same mechanism and different code behind it.

## Decision

1. **A coordinator can have a provisioner**: an HTTP service it calls
   (`devstack node serve --provisioner <url>`, mutual TLS or a bearer token
   from the coordinator's state). The provisioner holds the provider's
   credentials; the coordinator never does. Without one, nothing changes.
2. **A provisioned node belongs to the pool.** The pool is the unit of
   trust already: its members trust each other's lanes on their nodes
   (ADR-0027 point 6), and pools do not trust each other (ADR-0034). An
   organisation that wants its repositories apart sets the pool to
   provision per repository (`provision_per = "repo"`); a provisioned node
   then runs that repository's lanes only, and the node refuses others.
   An operator serving several organisations runs a pool, with its own
   coordinator, per organisation.
3. **It is asked only when no node can take the lane.** For a lane placed
   by the pool (`--on pool`, not a named node) that no node of the pool can
   take, the members' own and the provisioned ones alike, and a member an
   admin allowed to cause provisioning (`devstack node members provision
   <id> on`, off by default; it costs someone money), the coordinator asks
   the provisioner for a node with at least the lane's need (ADR-0049: CPU,
   memory, disk, GPUs), with its own version and pool protocol so the
   machine runs a devstack that works with it (ADR-0031 point 8). The
   provisioner chooses the size, usually one that holds several such lanes,
   and accepts with the size and an estimate, or refuses with a reason; a
   refusal is the refusal the requester gets today, with the reason added.
   Admin caps bound it: provisioned nodes at once, and in CPU and memory.
4. **Provisioned nodes are filled, not spread.** Members' own nodes are
   ranked as today. Among provisioned nodes placement prefers, after a warm
   one, the one with the least free capacity that still takes the lane, so
   lanes gather on few machines and the others empty. A running machine is
   never resized: a provider resizes only a stopped one, which would stop
   every lane on it. A lane is never moved to empty a machine.
5. **The lease waits while the machine comes.** The lease is `provisioning`
   until the node is online and admits it, then is offered as any other.
   The requester shows a `provision` phase, timed like the others
   (ADR-0025, amended 2026-10-19). Past a limit (the estimate, at most ten
   minutes) the coordinator gives the machine back and the start fails.
6. **The machine enrols itself, once.** With the request the coordinator
   issues an enrolment: its address, its CA's fingerprint, a single-use
   token valid for the wait. `devstack join <url> --fingerprint <fp>
   --enrol <token>` on the machine makes it an approved member and a
   granted, dedicated node in one step, marked `provisioned` (and bound to
   the repository, under point 2's option). No other token admits anyone.
7. **An idle provisioned node is given back.** `lane remove` drops the
   lane's slot at once (ADR-0038). A stopped lane's slot is kept for the
   pool's idle time (an admin's, default 30 minutes, not ADR-0038's hours),
   then pruned. A provisioned node with no lease and no slot for that idle
   time is given back: the coordinator revokes its membership, then asks
   the provisioner to delete the machine, retried until it answers; a node
   not confirmed deleted stays listed. Nothing of its slots is kept
   elsewhere.
8. **The provisioner is told what happened**: a node ready (its size), a
   lease on it begun and ended (member, repository, lane, need, times), the
   node given back. It may bill from that; devstack does not count money.
9. **The pool shows provisioned nodes** in the dashboard and `devstack
   pool`, with their size, load, lanes, since when and their idle time
   left, and an admin can give one back by hand.

## Alternatives considered

- **A machine per lane.** Simplest isolation and accounting, but every lane
  starts cold, a whole machine is paid for each lane however small, and
  the lanes of one team, which trust each other anyway, gain nothing from
  being apart.
- **One machine per team, resized as lanes come and go.** No placement
  change, but a provider resizes only a stopped machine, so growing stops
  every running lane, and one machine has a largest size.
- **A load balancer in front of the team's machines.** There is nothing to
  balance: a lane is placed once, whole, on one node, and its traffic goes
  directly between the member and that node (ADR-0031 point 5).
- **Provider APIs in the coordinator** (a crate per provider). The
  coordinator's host would hold every provider's token, and devstack would
  carry vendor code and its upkeep for a feature most pools do not use.
- **An external autoscaler watching the pool** (`GET /v1/placement`, the
  capacity of ADR-0036). Needs no change, but a refused start fails instead
  of waiting, joining still needs an admin, placement keeps spreading, and
  slots are kept for hours.
- **A command the coordinator runs** instead of an HTTP service. Simpler
  for one team on one host, but the provider's token then lives with the
  coordinator, and a slow provider ties up the coordinator's process. A
  team can run the provisioner on the same host and get the same.
- **Warm spare machines.** Faster first starts, but a spare is paid while it
  waits. A provisioner may keep spares itself; until one has enrolled, it is
  no node of the pool.

## Consequences

- The first lane on a new machine starts cold: the whole tree is uploaded
  and the build starts empty. Later lanes of the same repository there are
  warm (ADR-0038's preferences). A lane image with a warm build of the main
  branch (ADR-0028) shortens the first.
- Filling machines puts more lanes on one kernel and one disk; a lane that
  overruns its need slows its neighbours. The node's own admission
  (ADR-0033) and the measured need (ADR-0049) are what bound that.
- A machine with one long-lived lane is never given back, however empty;
  nothing moves that lane away.
- A provisioned machine must be reachable from the members, usually on a
  public address with mutual TLS: the network ADR-0031 point 5 leaves to
  the deployment is then the internet, not a mesh.
- A machine the provisioner failed to delete costs money until someone
  notices; point 7 keeps it listed, nothing more.
- An enrolment token is a credential to the pool for its lifetime; it is
  single-use, short-lived, and bound to one request.
- The lease state `provisioning` is new; a requester of an earlier pool
  protocol is refused as today rather than told to wait, and the protocol
  number goes up if the offer changes shape.

## Enforced by

Will live in: `crates/devstack-node/src/coordinator.rs` (the fallback in
`lease_create`, the `provisioning` lease, filling provisioned nodes in
`rank`, giving back, the admin caps), `store.rs` (a member's `provision`
permission, a node's `provisioned` mark and repository binding), `pki.rs`
(the enrolment token), `node.rs` (refusing other repositories on a bound
node; the pool's idle time), `version.rs` (`PROTOCOL`),
`crates/devstack-daemon/src/remote.rs` (the `provision` phase),
`crates/devstack/src/poolcmd.rs` (`--provisioner`, `--enrol`, `members
provision`), the pool area under `ui/`, [pool.md](../pool.md) and the
provisioner's API in [dashboard-api.md](../dashboard-api.md).

## What this ADR does not say

- Which providers have a provisioner, or whether devstack ships one. This
  ADR is the contract; implementations are separate.
- Anything about prices, billing or payment. The events of point 8 are all
  devstack provides; the dashboard has no place to buy anything.
- That a coordinator serves several organisations. One pool is one
  organisation; an operator runs several coordinators.
- That a named node (`--on <node>`) or a local lane is ever provisioned.
