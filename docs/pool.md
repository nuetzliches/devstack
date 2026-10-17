# The pool: lanes on other machines

A **pool** is a group of machines that run lanes for each other. A worktree
on your laptop can run on the team's build server instead: build, services
and its own containers there, its ports forwarded to your machine as if it
ran locally. One machine runs the **coordinator**: it admits members, issues
their certificates and places lanes. Machines that run lanes run a **node
agent**. Data never goes through the coordinator, only between your machine
and the node.

Decisions behind it: [ADR-0025](adr/0025-a-lane-can-run-on-another-node.md)
to [ADR-0037](adr/0037-telemetry-goes-to-the-pools-collector-with-consent.md).

- [Set up a coordinator](#set-up-a-coordinator)
- [Join a pool](#join-a-pool)
- [Run a node](#run-a-node)
- [Run a lane on a node](#run-a-lane-on-a-node)
- [Share a lane](#share-a-lane)
- [Administer](#administer): roles, tags, what a node runs
- [Several pools](#several-pools)
- [Capacity and telemetry](#capacity-and-telemetry)
- [Reference](#reference): files, environment

## Set up a coordinator

On a machine every member reaches (a server in the office network, say):

```sh
devstack node init --address 10.0.0.5 --address build.example.internal
devstack node serve --listen 10.0.0.5:4260
devstack node fingerprint        # what members pass to `devstack join`
```

`init` creates the pool's certificate authority and a server certificate for
the addresses members use; `serve` runs the coordinator in the foreground
(as a service in practice: a container or a systemd unit). Its directory
(`--dir`, `DEVSTACK_COORDINATOR_DIR`, default devstack's per-user state)
holds the CA key: back it up, and keep it private. Losing it means every
member joins again; no lane data is lost.

The first member you approve as admin (`devstack node members approve <id>
--admin`, on the coordinator) administers the pool from the dashboard from
then on.

Anyone who reaches the coordinator's port may ask to join. It takes ten
requests a minute per address, a request of normal size, and at most 64
waiting for an admin at once.

## Join a pool

On your machine, with the coordinator's address and fingerprint (an admin
copies the full command from the dashboard's pool area):

```sh
devstack join https://10.0.0.5:4260 --fingerprint <64 hex digits>
```

It waits until an admin approves the request (Ctrl-C stops waiting; `devstack
join` again picks it up). The dashboard's **Pool** area does the same with a
form. `devstack pool` then lists the machines, what each offers, and which
lanes run where.

A member that only asks for lanes needs nothing else. The **share switch**
(sidebar, pool area) lets the pool run other members' lanes on your machine:
half of what is free, never more than the caps you set; off by default.

## Run a node

A machine that runs lanes for others, typically a server, runs the node
agent. It needs Linux with Docker (lanes run in containers; a Windows machine
asks for lanes but does not run them yet) and must have joined the pool:

```sh
devstack join https://10.0.0.5:4260 --fingerprint <fp> --name build-server
devstack node members node build-server on      # an admin, on the coordinator
devstack node agent --listen 10.0.0.7:4261 --dedicated --max-cpus 12 --max-memory-gib 48
```

A machine runs the pool's lanes only once an admin granted it (`devstack node
members node <id> on`, or `off` to take it back): until then the coordinator
ignores the API its agent reports, places nothing there, and the agent says
why. A member that only shares from the dashboard is no node.

- `--dedicated`: a server its owner never reclaims. The pool shows it as a
  server, and only a lane on such a node may take the main ports.
- `--max-cpus`, `--max-memory-gib`: caps on what its lanes take together;
  below them, half of what is free right now.
- `--api https://host:port`: where requesters reach it, when `--listen` is
  every address (`0.0.0.0:4261`).
- `--pool <name>`: the pool it serves (one agent per pool, see [Several
  pools](#several-pools)).
- In a container, give it the Docker socket, mount its directory at the same
  path as on the host (Docker resolves the lanes' mounts there), and set
  `DEVSTACK_NODE_CONTAINER` to its container name: it joins each lane's
  network to reach the lane.
- `policy.json` in its state (`node/` beside its membership): what this
  node lets run on it, in the form of an admin's policy (below), for a
  server deployed as code. It holds on top of the admin's: both must admit
  a lane, the smaller limits and the stricter network win.
- It reads each lane's Compose model in a throwaway `docker:cli` container
  (`DEVSTACK_NODE_COMPOSE_IMAGE` names another image with `docker
  compose`), pulled once.

The stack must say what a lane runs in on Linux:

```toml
[lanes]
image      = "registry.example/shop-lane:1"   # a Linux image with the stack's toolchains
# or
dockerfile = "dev/lane.Dockerfile"            # the node builds it from the lane's source
sync       = ["web/certs/*.pem"]              # files git ignores that such a lane needs
```

## Run a lane on a node

In a worktree:

```sh
devstack up --on build-server     # that node
devstack up --on pool             # the coordinator picks the granted node with the most free memory
```

devstack leases the lane, syncs the worktree to the node (by content, only
what changed, every 2 s from then on), starts it there and forwards its ports
to your machine: `localhost:<port>` reaches it as if it ran locally, its
containers' ports too. The dashboard shows where each lane runs, its start
step by step, and offers **Start here / on a node / in the pool** for a lane
that is not started.

```sh
devstack lane list                      # RUNS ON: local, build-server, customer:gpu-box
devstack lane move login-fix --to local # a cold start there, takes over once ready
devstack promote --lane login-fix       # it answers on the main ports (dedicated nodes only)
devstack promote --lane main            # back to the main working tree
```

What a lane's Compose files may ask of a node is less than on your machine:
the node runs them on its own Docker engine. It refuses, naming each, a
service that is privileged, adds capabilities or devices, shares a namespace
of the host (`pid`, `ipc`, `network_mode: host`, ...), sets `userns_mode`,
`cgroup_parent`, `security_opt` or `sysctls`, or mounts, builds from or
reads an env file outside the lane's files; external volumes, volumes with
`driver_opts`, external networks the stack file does not list under
`[compose.<group>] networks`, network drivers other than `bridge`, and, on
an `internet` or `none` node, subnets or IPv6 of its own. Compose files,
env files, `extends` and `include` are read from the lane's files only.
Every container of the lane gets the node's limits and a process limit.

A lane on a node is disposable: its data comes from the seed jobs, and a
move is a cold start. It runs without background workers unless it holds the
main ports. When its lease ends (an admin ends it, the node goes away for
longer than a lease lives), the main working tree takes the main ports back
by itself.

## Share a lane

A lane on a node is private. Its owner shares it with members (by id or tag)
or everyone in the pool:

```sh
devstack lane share login-fix --with team:qa    # or --with '*'; without --with: nobody again
```

or with the lane's **share** button in the dashboard; `[lanes] share = [...]`
in the stack file sets the default. Members it is shared with see it under
**Shared with you** (sidebar and pool area, by node) and open its services:

```sh
devstack pool shared
devstack open login-fix web                     # a forward on your machine, then the browser
```

They reach its services, nothing else: no logs, no restart, no stop. The node
checks every connection against the coordinator, so unsharing holds from the
next one.

## Administer

Admins do this in the dashboard's pool area, or on the coordinator:

```sh
devstack node members                           # who asked, who is in
devstack node members approve <id> [--admin]
devstack node members role <id> admin|member    # the last admin stays one
devstack node members remove <id>
devstack node members tags <id> team:backend customer:acme
devstack node members node <id> on|off          # whether the pool runs lanes there
devstack node members policy build-server --allow team:backend --repo shop \
    --memory-headroom 2 --max-cpus 8 --network internet
```

A node's **policy** (ADR-0033): who may run lanes there (member ids or
tags), which repositories (`[stack] name`; a guard against mistakes, not
against a member who lies), a memory limit (the lane's expected need times
the headroom, at least 2 GiB), a CPU limit per lane, both on every container
of the lane, and its network:

| `--network` | A lane there reaches |
|---|---|
| `lan` (default) | the internet and the local network |
| `internet` | the internet, not the local network and not the node itself (iptables rules per lane, IPv4; the lane's networks have no IPv6) |
| `none` | only its own containers (`internal` networks and the same rules) |

On an `internet` or `none` node, the lane's images (`[lanes] dockerfile`,
Compose `build`) are built without a network: `RUN` steps that download
need a `lan` node, or a prebuilt `[lanes] image`.

A lane is taken to need at least 0.25 CPU and 256 MiB, whatever it asks
for. The coordinator places by the policy and the node admits by it, and by
its own `policy.json`. An admin can
also pause a node (no new lanes) and end a lease; every change is in the
admin log.

## Several pools

A machine can belong to several pools, a customer's beside the team's, each
with its own certificate and admins; pools do not trust each other
(ADR-0034):

```sh
devstack join https://172.20.1.5:4260 --fingerprint <fp> --pool customer
devstack up --on customer:gpu-box               # a node there
devstack up --on customer                       # its coordinator picks
devstack node agent --pool customer --listen ...
```

The dashboard shows a tab per pool; lanes start and move to the nodes of all
of them. The first pool is `default`.

## Capacity and telemetry

The pool area shows what the pool has, offers and its lanes use, GPUs
included (found with `nvidia-smi`), for the last hour or day, and the same per
node. The coordinator keeps that history in memory.

A pool can collect **telemetry**: devstack's diagnostic log and how long
lane starts, syncs and moves take. The collector (OTLP/HTTP) is set on the
coordinator only:

```sh
DEVSTACK_TELEMETRY_ENDPOINT=https://collector.example/tenant
DEVSTACK_TELEMETRY_TOKEN=<bearer token>
```

or `telemetry.json` (`{"endpoint", "token"}`) in its directory; none means
off. Members send through the coordinator, which sets their identity from
their certificate, and **only with their consent**, per pool, off by default:

```sh
devstack pool telemetry on     # or `join --telemetry`, or the switch in the pool area
devstack pool telemetry        # whether the pool collects, and what is sent
```

Never sent: file contents, environment values, command lines, service output.

## Reference

| What | Where |
|---|---|
| Membership (key, certificate, coordinator) | devstack's per-user state, `pool/`; other pools in `pools/<name>/`; `DEVSTACK_POOL_DIR` overrides the default pool's |
| Share settings, telemetry consent | `share.json`, `telemetry.json` beside the membership |
| Coordinator | `DEVSTACK_COORDINATOR_DIR`; `members.json` (changed under `members.lock`), `audit.jsonl`, `telemetry.json` |
| Node agent state (leases, mirrors, uploaded files per member, `policy.json`, `secret`) | `node/` beside its membership |
| Ports | coordinator and node agent: as you choose (`--listen`); mutual TLS with the pool's CA |
| HTTP API | the hub's `/hub/pool...` routes in [dashboard-api.md](dashboard-api.md) |
