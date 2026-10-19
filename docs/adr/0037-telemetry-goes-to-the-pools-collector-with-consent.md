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

`crates/devstack-node/src/telemetry.rs` (the collector setting:
`DEVSTACK_TELEMETRY_ENDPOINT` and `DEVSTACK_TELEMETRY_TOKEN`, else
`telemetry.json` in the coordinator's directory; consent per pool; the
identity stamp; OTLP metrics), `crates/devstack-node/src/coordinator.rs`
(`GET /v1/telemetry`, `POST /v1/telemetry/{logs,metrics}`),
`crates/devstack-daemon/src/telemetry.rs` (a daemon's and the hub's
diagnostic lines, durations by event as histograms, lanes and services
running), `crates/devstack-node/src/node.rs` (a node's capacity, with the
coordinator's setting alone), `devstack join --telemetry`, `devstack pool
telemetry on|off`, the pool area in `ui/`, `crates/devstack-node/tests/telemetry.rs`.

*Noted 2026-10-18.* Point 4's "never service output" did not hold until
0.10.0: a failed job's or build's reason carried the end of its output. Now
`outbound` in `crates/devstack-daemon/src/diag.rs` cuts every text of a line
that leaves the machine (to a pool's collector or to
`DEVSTACK_OTLP_ENDPOINT`) to its first line; `daemon.log` keeps all. The
coordinator strips identity attributes a member sets on records and data
points too, not only on the resource (`strip_identity` in
`crates/devstack-node/src/telemetry.rs`), and passes collector errors to
admins only.

*Amended 2026-10-19.* A review of three days of a pool's telemetry found
point 4 kept only in part. Now: a process keeps its lines (bounded) from its
start until the first round has asked the pools, so its start, restores and
early errors arrive; nothing is sent before that round finds a pool that
collects and this machine's consent, and without one what was kept is
dropped (point 3 holds). Durations (`ms`, a `devstack.duration` histogram by
event, repository, lane, node, place, service and result) come with lane
stops (local and remote), local lane starts (opening the lane), service
starts (from the spawn to ready or failed, with `result` and `restarts`; a
service without a health check is running after a fixed wait, which is sent
as `settle_ms`, not as a duration), builds (`build`, with `result`) and
tunnel connections (`tunnel`; with `devstack.tunnels` open and
`devstack.tunnel.bytes` through those that closed). Log records carry
`os.type` and `devstack.repository` as metrics do, nested values as dotted
attributes (`devstack.status.state`) and their observed time; the main
working tree is the lane `main`. The histogram bounds reach 30 minutes.
Point 4's "never command lines" did not hold either: a `prepare` line
carried its `run`. Now `outbound` leaves out fields by name, at any depth
(`LOCAL_ONLY`: `run`, `cmd`, `command_line`, `args`, `env`, `output`,
`repo`, `worktree`, `file`, `cwd`, `dir`; a line that carries a command
line, an environment, process output or a path on the machine uses one of
them), and masks user names in home paths (`C:\Users\~`, `/home/~`) and
credentials in URLs (`https://***@host`) in every text; `daemon.log`
keeps all.
`crates/devstack-daemon/src/telemetry.rs` and `diag.rs` enforce it.
