# ADR-0033: A node decides who runs lanes on it, and what a lane there may do

- **Status:** Proposed
- **Date:** 2026-10-16
- **Deciders:** project owner
- **Related:** [ADR-0025](0025-a-lane-can-run-on-another-node.md), [ADR-0026](0026-placement-by-the-requester-admission-by-the-node.md) (point 4: the node admits), [ADR-0027](0027-trust-between-nodes.md), [ADR-0031](0031-a-coordinator-places-lanes.md), [ADR-0032](0032-the-pool-is-administered-in-the-dashboard.md)

## Context

A lane on a node runs the requester's code there: its build scripts, its
package installs, its services. Today every approved member may run any
repository on every node that shares, and a node checks only that the lane
fits what it offers. The lane container has no Docker socket, runs as the
agent's user and sees only its mirror, its home and the devstack binary,
but it has the node's network: on a server in an office network, that is
the office network.

For a developer machine that shares, its owner decides by sharing or not.
A server shared by a team needs more: who may run lanes there, and what a
lane may reach and take. Restricting *which repositories* run is what one
asks for first, but the requester names the repository and sends its
files: a node can hold an honest member to a list, it cannot hold a
dishonest one to it. What a node can enforce is who calls it (the member's
certificate) and what the container it starts may do.

## Decision

1. **Tags.** Admins give members tags, free-form and flat (`team:backend`,
   `company:nuts`, `customer:acme`); groups are tags. Nodes get tags too
   (`site:office`, `gpu`). The role (admin, member) stays what administers
   the pool (ADR-0032); tags say who may run what where.
2. **Who:** each node says which members may start lanes on it, by tag
   (`allow = ["team:backend"]`) or by member; by default every approved
   member. A lease for a member it does not allow is refused when the node
   admits it (ADR-0026 point 4), and the coordinator does not place such a
   lane there. A request may ask for node tags (`--on pool:gpu`), and the
   coordinator places only on nodes that carry them.
3. **Which repositories:** a node may name the repositories it runs (by the
   name the stack file gives, `[stack] name`); others are refused the same
   way. This guards against mistakes, not against a member who lies about
   the name, and the dashboard says so where it is set.
4. **How much:** a lane's container gets hard limits, CPU and memory, from
   what its lease admitted (`--cpus`, `--memory`), with room above the
   expected need the node's owner sets; a lane that outgrows them is
   stopped by the kernel, not the node.
5. **Which network:** a node sets what its lane containers may reach
   beyond their own infrastructure: the internet and the local network
   (the default today), the internet only (no private address ranges), or
   nothing. The node enforces it on the lane's networks (firewall rules on
   the host, or an egress proxy).
6. **Where it is set:** on the node (its agent's configuration, for a
   server deployed as code) and, for admins, in the pool area of the
   dashboard; the node reports its policy to the coordinator, which
   shows it and places lanes accordingly. The node is the one that
   enforces it.

## Alternatives considered

- **Trust every approved member (today).** Simple, and enough for a pool of
  a few colleagues' machines; not for a server in a network others rely on.
- **Repositories by content (a signed commit, a hash of the stack file).**
  The requester syncs a worktree with uncommitted changes, so there is no
  fixed content to sign; checking the remote URL only moves the trust to a
  string the requester sends.
- **Fixed groups or more roles.** A role per kind of access (lane runner,
  node owner, ...) multiplies with every node; groups that are tags do not.
- **A sandbox runtime (gVisor, Kata) for lane containers.** Stronger
  isolation of the kernel, at a cost in speed and setup per node; it can
  come on top of points 3 and 4 for nodes that want it.

## Consequences

- An admin decides per node; a node without a policy behaves as today.
- Limits make a lane that needs more than expected fail visibly instead of
  slowing the node down for everyone.
- Restricting the network breaks lanes that need a host on the local
  network (an internal package feed); such a node names exceptions.

## Enforced by

Will live in: `crates/devstack-node/src/node.rs` (admission by member and
repository), `crates/devstack-node/src/lanerun.rs` (container limits, the
lane networks' egress), `crates/devstack-node/src/coordinator.rs`
(placement respects the reported policy), the pool area in `ui/`.
