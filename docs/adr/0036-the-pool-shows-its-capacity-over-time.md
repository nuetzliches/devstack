# ADR-0036: The pool shows its capacity, GPUs included, and how it changed

- **Status:** Accepted (2026-10-16)
- **Date:** 2026-10-16
- **Deciders:** project owner
- **Related:** [ADR-0026](0026-placement-by-the-requester-admission-by-the-node.md) (what a node offers), [ADR-0031](0031-a-coordinator-places-lanes.md) (point 3: reports), [ADR-0032](0032-the-pool-is-administered-in-the-dashboard.md), [ADR-0037](0037-telemetry-goes-to-the-pools-collector-with-consent.md)

## Context

The pool area shows each node's offer at its last report (CPU and memory)
and nothing else. Whoever looks at it cannot tell what the pool as a whole
offers, how much of that the lanes use, or whether a node was busy an hour
ago and free now. A lane's detail page already shows its services' load as
a short history (plan §12); a node and the pool deserve the same.

GPUs are coming: a lane that trains or runs a model needs one, and the
pool has to know which nodes have one before it can place such a lane.

## Decision

1. **A node reports what it has, offers and uses.** Its report (every
   15 s) carries, besides the offer: the machine's total CPU cores and
   memory, what its lanes use now (CPU, memory, summed over their
   containers) and its GPUs: per GPU the model, its memory, its load and
   used memory, and whether it is offered. GPUs are found with the
   vendor's tool (`nvidia-smi` first, on Linux and Windows); a machine
   without one reports none.
2. **The coordinator keeps the history.** In memory, per node: every
   report for the last hour, then one point per minute (averaged) for 24
   hours. A restart of the coordinator starts it again; longer history is
   the collector's (ADR-0037).
3. **The pool area shows totals and charts.** At the top: total, offered
   and used CPU, memory and GPUs over all nodes online, with a chart of
   the last hour (and 24 h to choose). Each node gets the same chart in
   a detail view, like a lane's services: what it has, offers and its
   lanes use, and its GPUs.
4. **GPUs are reported before they are placed.** A lane asking for a GPU,
   the node handing one to the lane's container (`--gpus`), and the policy
   for it (ADR-0033) are a later decision; this one makes them visible.

## Consequences

- An admin sees whether the pool is too small, before members wait.
- The report grows by a few numbers per node; the history costs the
  coordinator a few thousand points per node.
- A node without the vendor tool reports no GPU even if it has one; it
  says so in its diagnostic log.

## Alternatives considered

- **History in the hub of each member.** Every member would keep its own,
  starting when its dashboard does; the coordinator sees all reports
  anyway.
- **Only the collector (ADR-0037).** History would depend on someone
  running one and every member agreeing to send to it; capacity is the
  pool's own business.

## Enforced by

Will live in: `crates/devstack-node/src/share.rs` (measuring, GPUs),
`crates/devstack-node/src/coordinator.rs` (report, history, overview), the
pool area in `ui/`.
