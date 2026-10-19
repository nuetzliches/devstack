# ADR-0035: The pool sees the lanes its members share, and can use them

- **Status:** Accepted (2026-10-16); point 4 extended by [ADR-0040](0040-a-shared-lane-can-be-watched-like-ones-own.md) (2026-10-19: sharing for viewing)
- **Date:** 2026-10-16
- **Deciders:** project owner
- **Related:** [ADR-0025](0025-a-lane-can-run-on-another-node.md), [ADR-0029](0029-source-sync-and-port-forwarding-for-remote-lanes.md) (point 4: forwarded ports), [ADR-0031](0031-a-coordinator-places-lanes.md), [ADR-0033](0033-a-node-decides-what-runs-on-it.md) (tags), [ADR-0034](0034-a-machine-can-belong-to-several-pools.md)

## Context

A lane on a node is reachable from its requester's machine only: its ports
are forwarded there (ADR-0029). Others in the pool cannot see it, let alone
open it, although that is often what a lane is for: a reviewer looks at a
feature branch running, a tester tries a fix, a customer sees a state on
the server, none of them with the stack set up on their own machine. The
coordinator already knows every lane on a node (its lease: repository,
lane, node, requester).

## Decision

1. **A lane is private unless its owner shares it.** Sharing is opt-in per
   lane, with a default per stack: nobody (the default), members with
   given tags (ADR-0033: `team:qa`, `customer:acme`), or everyone in the
   pool. The owner can stop sharing at any time.
2. **The pool shows what is shared.** The owner's daemon reports a shared
   lane's services, with their ports and links, to the coordinator; every
   member it is shared with sees it in the pool area and in the sidebar,
   grouped by node, beside its own stacks.
3. **Using one is opening a forward.** A member opens a shared lane's
   service from the dashboard (or `devstack open <lane> <service>`): its
   hub forwards a local port to the lane on its node, the same tunnel as
   the owner's (ADR-0029 point 4), and opens the page. The node lets the
   tunnel through only for members the lane is shared with, checked with
   the coordinator by the caller's certificate, and only to the shared
   services' ports.
4. **Using is not controlling.** Others reach the shared services; they do
   not start, stop, rebuild or see the logs of someone else's lane, unless
   a later decision says so.
5. **Lanes on nodes first.** A lane on its owner's own machine stays out of
   this: sharing it would mean that machine accepting connections from the
   pool, a step of its own.

## Alternatives considered

- **Everything visible to every member.** Simplest, and wrong for lanes
  that run unfinished work, customer data or another customer's project in
  the same pool.
- **Sharing links with a secret in them.** Easy to pass around, and just as
  easy to pass on; the pool's members and tags are already who is meant.

## Consequences

- A shared lane's services are reachable by more people: what it holds
  (seed data, keys in its environment) is seen by them too; the owner
  decides by sharing.
- The node checks every tunnel against the lane's sharing, so revoking a
  member or unsharing a lane takes effect at the next connection.

## Enforced by

`crates/devstack-daemon/src/lanes.rs` (sharing per lane, `[lanes] share`,
`.devstack/lane-shares.json`), `crates/devstack-daemon/src/remote.rs` (told
the coordinator with each renewal), `crates/devstack-node/src/coordinator.rs`
(`PUT /v1/leases/{id}/share`, `GET /v1/shared`, `GET /v1/leases/{id}/shared`
for the node), `crates/devstack-node/src/node.rs` (`/v1/leases/{id}/shared/{port}`:
a tunnel for others, checked with the coordinator at every connection, to
shared ports only), `crates/devstack-hub/src/shared.rs` (listing, opening a
forward), `crates/devstack-node/tests/node.rs` (unshared, shared, a port not
shared, unshared again), `ui/` (pool area and sidebar by node, the lane's
share button). Not yet: `devstack open <lane> <service>` from the CLI.

*Noted 2026-10-18.* `devstack open <lane> [<service>]` is built
(`crates/devstack/src/main.rs`, through the hub's
`/hub/pool/shared/{lease}/open`); it opens `http` and `https` addresses only,
and the coordinator refuses to keep a shared service with another scheme
(`crates/devstack-node/src/coordinator.rs`), as do the hub and the dashboard.
