# ADR-0037: Telemetry goes to the pool's collector, if the pool has one and the member agrees

- **Status:** Accepted (2026-10-16)
- **Date:** 2026-10-16
- **Deciders:** project owner
- **Related:** [ADR-0031](0031-a-coordinator-places-lanes.md) (the coordinator, member certificates), [ADR-0034](0034-a-machine-can-belong-to-several-pools.md), [ADR-0036](0036-the-pool-shows-its-capacity-over-time.md), plan §11 (the diagnostic log)

## Context

Whoever runs a pool wants to know how it is used and where it hurts: how
long a lane takes to start on a node, how often a sync or a move fails,
which services fail to start, how busy the nodes are over weeks. Today a
daemon and the hub can send their diagnostic log as OTLP to a collector
named in `DEVSTACK_OTLP_ENDPOINT`, a setting of the machine itself; there
are no metrics, and nothing reaches whoever runs the pool.

Measurements from a developer's machine are that developer's data: they
must not leave it unasked, and not to anyone but the pool they joined.

## Decision

1. **The collector is configured on the coordinator.** Its OTLP/HTTP
   endpoint and its credentials (a bearer token) are the coordinator's
   settings; empty means off, and off is the default. Members never see
   the credentials.
2. **Members send to the coordinator, which passes it on.** Over the
   member's mTLS connection, `POST /v1/telemetry/{logs,metrics}`. The
   coordinator sets the sender's identity from its certificate
   (`devstack.member.id`, its name, the pool), overwriting whatever the
   payload claims, and forwards to the collector. Without a collector it
   answers `404` and the member stops sending.
3. **A member sends only what it agreed to.** Consent is per machine and
   per pool (ADR-0034), off until given: asked at `devstack join`
   (`--telemetry`), switched in the pool area beside sharing, revoked at
   any time. The pool area says whether the pool collects and what is
   sent. A node that runs lanes for the pool sends what concerns those
   lanes with the coordinator's setting alone: the lane runs on the pool's
   resource, and its owner agreed to the node's terms by placing it there.
4. **What is sent:**
   - *Metrics* (every 60 s): a node's capacity, offer and use (ADR-0036);
     lanes running, local and remote; per lane start, sync, move and stop:
     duration and outcome, by phase; synced bytes and files; tunnels open,
     bytes through them; per service start: duration, outcome, restarts,
     health; builds: duration and outcome.
   - *Logs*: the diagnostic log (plan §11), as today with
     `DEVSTACK_OTLP_ENDPOINT`.
   - Labels: repository (the stack's name), lane, service, node, member,
     devstack version, operating system.
   - Never: file contents, environment values, command lines, paths
     outside the repository, service output.
5. **Lossy by design.** Sending never slows or fails the work it
   measures; what the coordinator cannot pass on is dropped, a bounded
   queue on the member, none on the coordinator.
6. `DEVSTACK_OTLP_ENDPOINT` stays: a developer's own collector, without a
   pool, gets the same data directly.

## Consequences

- One place to configure, one secret, on the server; a member cannot write
  into the collector as someone else.
- The coordinator carries the telemetry traffic of its members; at one
  batch per minute per member that is small.
- A pool that wants telemetry from developers' machines has to ask them;
  some will say no, and its numbers are incomplete for those.

## Alternatives considered

- **Members send to the collector directly.** Every member would need the
  token, and could claim any identity in the payload.
- **Consent once, by joining.** Joining a pool is about lanes and
  resources; sending measurements is a separate question.

## Enforced by

Will live in: `crates/devstack-node/src/coordinator.rs` (setting, relay,
identity), a telemetry module beside `crates/devstack-daemon/src/diag.rs`
(metrics, queue), the join command and the pool area in `ui/`.
