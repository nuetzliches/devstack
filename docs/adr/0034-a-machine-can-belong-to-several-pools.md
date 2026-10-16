# ADR-0034: A machine can belong to several pools; pools do not trust each other

- **Status:** Accepted (2026-10-16)
- **Date:** 2026-10-16
- **Deciders:** project owner
- **Related:** [ADR-0031](0031-a-coordinator-places-lanes.md) (one pool per coordinator), [ADR-0032](0032-the-pool-is-administered-in-the-dashboard.md), [ADR-0033](0033-a-node-decides-what-runs-on-it.md), [ADR-0027](0027-trust-between-nodes.md)

## Context

A pool (ADR-0031) is one coordinator with its certificate authority, its
members and its nodes. Resources do not all belong to one organisation:
another company, or a customer, may offer servers for the lanes of a
project it commissions, under its own rules, in its own network, with its
own administrators. A developer then works with the team's pool and with
that one.

## Decision

1. **A machine holds one membership per pool:** its own key, certificate
   and coordinator for each (`devstack join <url> --fingerprint <fp>
   --pool <name>`; the first pool is `default`). Each pool's admins decide
   alone whom they admit and what their nodes run (ADR-0033).
2. **Pools do not trust each other.** There is no federation: no pool
   accepts another's certificates, no coordinator places on another's
   nodes. A lane runs in a pool because that pool admitted the machine that
   asks.
3. **A request names its pool:** `--on <pool>:<node>` or `--on <pool>`
   (that pool's coordinator places it); without a pool name, the default
   pool. A lane shows its pool with its node.
4. **The dashboard shows every pool** the machine belongs to, each with its
   own area (members, nodes, lanes, sharing, and its admin parts where this
   machine is an admin there), and the share switch per pool.
5. **Data goes directly**, as in one pool (ADR-0031 point 5): the machine
   must reach the other pool's coordinator and nodes, typically through
   that organisation's network access. A lane's source and data stay
   between the developer's machine and the node it runs on.

## Alternatives considered

- **One pool per machine (today).** Joining another organisation's pool
  would mean leaving one's own.
- **Federated pools** (coordinators that trust each other and place across
  them). One view of all resources, at the price of trust between
  organisations that each want to decide alone, and of a protocol between
  coordinators.
- **Members of one pool from several organisations, told apart by tags**
  (ADR-0033). Works inside a team, but the other organisation's servers
  would then trust the team's certificate authority and its admins.

## Consequences

- A machine keeps several sets of keys; losing a pool's coordinator does not
  touch the others.
- Every place that names a node (CLI, dashboard, the lane's placement) gets
  the pool too.

## Enforced by

Will live in: `crates/devstack-node/src/member.rs` (a membership per pool),
`crates/devstack/src/poolcmd.rs` (`--pool`), the placement in
`crates/devstack-daemon/src/lanes.rs`, the pool area in `ui/`.
