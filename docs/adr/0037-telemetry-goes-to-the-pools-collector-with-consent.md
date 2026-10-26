# ADR-0037: Telemetry goes to the pool's collector, if the pool has one and the member agrees

- **Status:** Accepted (2026-10-16); addendum 2026-10-26 proposed (a lane's own daemon on a node: its lines go through its requester, #287)
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

*Noted 2026-10-20.* A node sends its own lines too, not only its capacity
(point 3: what concerns the pool's lanes): `node_lease_admitted`,
`node_lease_refused`, `node_lane_started`, `node_lane_not_started` (phase,
first line of the reason), `node_lane_stopped`, `node_lease_ended`,
`node_update_refused`, `node_update_failed` (`node_log` in
`crates/devstack-node/src/telemetry.rs`). A refused request's log line
carries the first line of its `error`. *Noted 2026-10-21 (#164):* also
`node_lane_infra_up` and `node_lane_infra_failed`, a lane's Compose group
brought up after its daemon answered, or not (`group`, `ms`, and the last
line of Compose's reason, not the command and paths of its first).

*Noted 2026-10-20, later.* Point 4's "never paths outside the repository"
did not hold for texts: an error's message names the paths it concerns (a
watcher's `about ["D:\\src\\shop"]`, a refused lane request's "`<path>` is
gone"), and only user names in home paths were masked. Now `outbound`
replaces the roots the process knows in every text: the repository as
`<repo>`, a lane's worktree as `<worktree>`, devstack's state as
`<devstack>`, the home directory as `~` (`diag::add_root`). The masking of
`/users/` no longer touches a URL's path segment (`https://x/users/42`).
Point 2's "the member stops sending" now holds at once: a `404` to a send
marks the pool as not collecting until it is asked again (10 minutes),
where before the member sent on until then.

## Addendum 2026-10-26 (proposed): a lane's own daemon on a node

*[#287](https://github.com/nuetzliches/devstack/issues/287).* The daemon in
a node's lane container sent nothing: a daemon sends to the pools its state
is a member of, and the container has no membership (and must get no member
key, ADR-0027, ADR-0033). The collector saw a lane on a node from the
outside only, the requester's `remote_lane_*` and the node's `node_*` lines;
not its services, builds, `prepare`, notices or failed requests.

1. **The lane's daemon keeps its lines and sends none.** In a lane on a node
   (`DEVSTACK_REMOTE_LANE`) its telemetry neither asks pools nor sends; it
   keeps its diagnostic lines, the newest 5,000, already masked as point 4
   and the 2026-10-20 note say, and hands them over on `POST
   /v1/telemetry/take`, each once.
2. **The requester sends them as its own.** The daemon that follows the
   lane takes them every 30 seconds, through the node as every request to
   the lane goes, while it sends telemetry at all, and records them with
   its own lines as lines of that lane (`lane` its name, as the developer
   calls it), on that node (`node`), `place` `remote`. They go where its own
   lines go: to the pools it agreed to send to, with its member identity set
   by the coordinator (point 2). Without its consent nothing is taken.
3. **Not the node.** Point 3 lets a node send what concerns a lane on the
   coordinator's setting alone; the lane's own lines are the member's
   (its services, its repository) and go under the member's consent.
4. **Lossy, as point 5.** A requester that is away takes the latest 5,000
   lines when it is back; what the lane logs in its last 30 seconds before
   it stops is not taken; a lane daemon of a node that does not keep them
   (before 0.24.0) answers `404`, and nothing is taken. The lane's gauges
   are not relayed: the requester counts the lane already.

*Enforced by:* `crates/devstack-daemon/src/telemetry.rs` (`RELAY`,
`start_relay`, `take_relayed`, `record_relayed`), `diag.rs` (relay in a lane
on a node), `api.rs` (`POST /v1/telemetry/take`), `remote.rs`
(`follow_telemetry`), `docs/pool.md`, `docs/dashboard-api.md`.
