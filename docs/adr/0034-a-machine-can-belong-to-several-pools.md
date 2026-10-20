# ADR-0034: A machine can belong to several pools; pools do not trust each other

- **Status:** Accepted (2026-10-16); noted 2026-10-20 (several agents on one Docker engine)
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

`crates/devstack-node/src/member.rs` (`pool_dir`, `pools`, `resolve_on`: a
membership per pool, the default one where it always was, the others in
`pools/<name>` beside it), `crates/devstack/src/poolcmd.rs` (`join --pool`,
`pool [--pool]`, `node agent --pool`), `crates/devstack-daemon/src/remote.rs`
(a lane leases in the pool `--on` names and shows it), the hub's `?pool=` and
`GET /hub/pools`, the share switch per pool, the pool area in `ui/` (a tab
per pool, joining another one; lanes start and move to the nodes of all).

*Noted 2026-10-20.* A machine that is a node of several pools runs one
agent per pool, usually on the same Docker engine. What an agent leaves on
the engine carries an owner tag, 8 hex digits derived from that agent's
secret (`lanerun::owner_tag`): its lane containers and lane images the label
`devstack.agent=<tag>`, its network rules the chain names
`ds-<tag>-<lease>-f`/`-i` (`egress.rs`). An agent's sweeps remove only what
carries its own tag, so one agent starting never removes another pool's
lanes or their rules. What an agent before the tag left (`devstack.lease`
without `devstack.agent`, chains `dsl-<lease>-f`/`-i`) is removed by the
agent whose lane it was: a container when it mounts that agent's state, a
chain with its lease, or by a sweep once no lane container on the engine has
that lease. Agents of different releases on one engine are not supported
across this change: update them together.
