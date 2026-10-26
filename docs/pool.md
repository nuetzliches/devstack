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
- [Versions in a pool](#versions-in-a-pool)
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

It waits until an admin approves the request (Ctrl-C stops waiting, or
`--no-wait` asks and returns; `devstack join` again picks it up). With
`--name` the machine shows under another name than its host name, with
`--telemetry` it agrees to send the pool telemetry ([below](#capacity-and-telemetry)).
The dashboard's **Pool** area does the same with a
form. `devstack pool` then lists the machines, what each offers, and which
lanes run where.

A member that only asks for lanes needs nothing else. A machine runs other
members' lanes only with a node agent, below. The dashboard has no switch for
that: the share switch it had only reported capacity, never started an agent,
and is gone until it does
([#171](https://github.com/nuetzliches/devstack/issues/171); on Windows,
which cannot run the Linux lanes yet,
[#168](https://github.com/nuetzliches/devstack/issues/168)). The hub reports
the machine (online, what it has, its devstack) and offers nothing. While a
node agent of the same membership runs on the machine, the hub does not
report for that pool and the agent reports the machine: the agent holds
`node/lock` in the membership's folder while it runs
([#170](https://github.com/nuetzliches/devstack/issues/170)).

Lanes of other members on your machine always show: a banner below the
dashboard's top bar (it can be hidden until a lane comes or goes), a tile in
the sidebar, and **On this machine** in the pool area, with whose lane
it is, its repository, since when and what it reserved. A lane that starts
running here also notifies the desktop (`guest_lane`,
[ADR-0045](adr/0045-the-hub-notifies-the-desktop.md)). The hub reads them from
the pool's leases after each share report.

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
  below them, half of what is free right now, or the part its policy says
  (`share_percent`).
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
  a lane, the smaller limits and the stricter network win. Beyond an
  admin's fields it takes `keep_lanes_hours` and `keep_lanes_gib`: how long
  and how much of stopped lanes it keeps (ADR-0038).
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
devstack up --on pool             # the coordinator picks: warm first, then free capacity (below)
```

A lane starts on this machine unless told otherwise. A machine may start
its lanes elsewhere by default
([ADR-0025](adr/0025-a-lane-can-run-on-another-node.md), amended
2026-10-21); that is the user's consent to send worktrees there without
being asked each time, so it is set by hand, never by joining:

```sh
devstack pool default-on pool          # or a pool's name, a node, <pool>:<node>; or the pool area
devstack pool default-on local         # back to this machine
devstack pool placement <lane>         # where it could go: each node's load, the coordinator's order and why
```

`devstack up` in a worktree, `devstack lane start` and an agent's `up` and
`lane_start` without `--on`/`on` use it; `--on local` keeps one lane here.

With `pool` the coordinator places the lane
([ADR-0026](adr/0026-placement-by-the-requester-admission-by-the-node.md)
point 3): of the granted, online, sharing nodes that are not paused, admit
the lane by their policy, run a devstack of the same pool protocol, offer
what the lane needs, have the disk it needs there with 5 GiB to spare, and
offer the GPUs it asks for, it takes, among those with room to spare (half as much again as the need), one that keeps
this lane's data (a start from its mirror, builds and volumes), then one the
repository ran on (its images and builds are there); otherwise, and among
equals, the one with the most free capacity for the need (the lower of free
memory and free CPU, each over the need). The lease says why, in the lane's
start and in `daemon.log` (`remote_lane_placed`); the dashboard shows the
order and the reasons beside the nodes to start on.

**What a lane needs**
([ADR-0049](adr/0049-a-lanes-need-is-measured-disk-included.md)): CPU,
memory, disk and GPUs. A node measures each lane it runs: the peak of its
memory, of its CPU averaged over a minute, of its slot (mirror, caches,
builds, volumes) and of the images its containers run, and reports it with
its report; a run counts once its lane ran for three minutes. The
coordinator keeps the last five runs of each repository and profile
(`footprints.json` in its directory), and a lane's need is the most each
part took in them. Before any ran in the pool, the requester's estimate
from its main tree stands for them: the most its services, builds and
containers each took over the last ten minutes, and as disk its tree
(without `.git` and its state), its snapshot, artifacts, logs and volumes,
and its Compose images; with nothing running, 0.5 CPU, 1 GiB and 10 GiB of
disk. `[lanes.need]` and the profile's `need` in the stack file raise each
part to what they say. The lease says the need and where it came from, in
the lane's start and in `daemon.log` (`remote_lane_need`).

Disk counts per node: one that keeps this lane's data needs nothing more,
one the repository ran on (its images are there) the lane's own part, any
other both. A node reports what a new lane can take of its disk
(`disk_room`): what is free, and what its kept lanes no lease uses take,
less what its running lanes are still expected to write; it removes kept
lanes, the least recently used first, before a lane that needs their room
starts. A node short of disk (less than 5 GiB free) says so with every
renewal of its leases; the lane goes on, and its requester logs
`remote_lane_disk`.

devstack leases the lane, syncs the worktree to the node (by content, only
what changed: a change within two seconds of saving, and everything compared
every 30 s besides), starts it there and forwards its ports to your machine:
`localhost:<port>` reaches it as if it ran locally, its containers' ports too.
A container port published as UDP (`53/udp`, a game's voice port) is
relayed too ([ADR-0056](adr/0056-a-lane-on-a-node-relays-its-udp-ports-through-a-tunnel-per-session.md)):
a datagram to `localhost:<port>` reaches it, each client through a tunnel of its own,
which ends after 60 s without a datagram. Nothing is held while the container
starts: a datagram before it answers is lost, as on a network, and the
client's next one tries again. A datagram crosses whole, up to 64 KiB; the
container sees it come from the lane daemon, one source port per client.
A node older than 0.20.0 relays no UDP: the lane's page, its container's row
and `devstack lane list` say so ("UDP port 5353 is not forwarded to this
machine (the node … runs devstack 0.19.0, which forwards TCP only …)"); update
devstack on the node, or keep such a lane local. Ports of another protocol
(SCTP) are not forwarded (#192).
A port the server tells its clients (`[compose.<group>] announced_ports`,
[ADR-0057](adr/0057-a-port-a-container-announces-moves-inside-it-too.md)) moves inside the container on the
node as it does locally: the node's agent gives Compose
`DEVSTACK_PORT_<SERVICE>_<PORT>` with the files' port plus the lane's offset,
the container listens there, and your machine forwards and relays that same
number, so a client that follows what the server announces (an ICE
candidate on `127.0.0.1:7981`) reaches the lane's server. The agent's
Compose calls get the lane's `DEVSTACK_LANE` and `DEVSTACK_LANE_SUFFIX` as
well ([ADR-0064](adr/0064-every-compose-call-is-told-the-lane-it-runs-for.md)),
with the lane's name as you call it, as a local lane's do.
The dashboard shows where each lane runs, its start step by step, and offers
**Start here / on a node / in the pool** for a lane that is not started.

**Its daemon starts first.** The node checks the lane's Compose files, pulls
and builds their images, creates its containers and starts the lane
container; the lane runs once its daemon answers, and its page opens. Its
containers then come up under it, a compose group at a time, as a local
lane's do: the page shows them created, starting, healthy, and its services
start once their infrastructure is up. A container that does not get
healthy within 300 s shows as such, with its output, and its group's error
(Compose's reason) instead of a failed start.

**Its containers** are the lane's own, on the node, in its Compose project
`<project>-<lane slot>` (they never touch the main tree's). From the worktree
you control them as a local lane's: `devstack restart app.web` (also
`compose.app.web`, or `web` when one compose group starts it), `start` and
`stop` the same, `devstack restart <group>` with its container members, `devstack
logs web`, and the dashboard's container page. The lane container has no
Docker: the node's agent does it, for the lane's owner only, and only for a
service the group lists in `services` (`stop` for any container of the
lane's project). A node older than this machine's devstack says so instead
(`409`: update devstack on the node). Not yet through the node: a container's
`restart_on`.

**A file the node cannot write** (a directory in the lane's copy that its
code made another user's) does not hold up the others: the rest is synced,
the lane's trouble in the dashboard names the file, `remote_lane_sync_failed`
goes to `devstack logs --daemon` (and the pool's telemetry, with consent),
and the file is tried again with the next change or the next full
comparison.

**What goes to the node:** every file of the worktree git does not ignore,
tracked or not (an untracked `.env` that git does not ignore goes too), and
the ignored files `[lanes] sync` names, from the worktree or else the main
tree. The node's owner can read them. Placing a lane on a node means trusting
its owner with that; keep a lane local whose secrets must not leave your
machine ([SECURITY.md](../SECURITY.md#the-pools-trust-model)).

```sh
devstack lane list                      # RUNS ON: local, build-server, customer:gpu-box
devstack lane move login-fix --to local # a cold start there, takes over once ready
devstack promote --lane login-fix       # its services answer on the main ports (dedicated nodes only)
devstack promote --lane main            # back to the main working tree
```

What a lane's Compose files may ask of a node is less than on your machine:
the node runs them on its own Docker engine. It refuses, naming each, a
service that is privileged, adds capabilities or devices (GPUs with the
`nvidia` driver excepted), shares a namespace of the host (`pid`, `ipc`,
`network_mode: host`, ...), sets `userns_mode`, `cgroup_parent`,
`security_opt`, `sysctls`, `oom_kill_disable` or a negative `oom_score_adj`,
uses `use_api_socket` or a `provider`, logs through a driver other than
`json-file` or `local`, runs a privileged `post_start`/`pre_stop` hook, or
mounts, builds from or reads an env file outside the lane's files; a build
with `tags` (the node names the images it builds), `cache_to` or a local
`cache_from`; a secret or config from `environment` (the node's own);
external volumes, volumes with `driver_opts`, external networks the stack
file does not list under `[compose.<group>] networks`, network drivers other
than `bridge`, and, on an `internet` or `none` node, subnets or IPv6 of its
own. A service key the node does not know yet is refused too, by name. Only
what the lane starts is checked: the services `[compose.<group>] services`
names, what they depend on, and the networks, volumes and secrets those
(and their builds) use; a Compose file shared with production may hold other
services (a socket-mounting admin tool, a reverse proxy) that the lane never
starts. Compose files, env files, `extends` and `include` are read from the
lane's files only, and the sync never follows a link out of them.

The node runs what it checked, not the lane's files again: it writes the
checked model beside the lane's copy, where the lane cannot write, and
Compose takes it from there. Its bind mounts of the lane's files become
mounts of one volume bound to the lane's copy, each at its path as a
`subpath` (Docker refuses a link out of a volume, at every start of the
container); its secrets' and configs' files are copied. A variable a service
or build leaves without a value (`environment: [NAME]`) stays unset: nothing
of the node's environment reaches a lane, and Compose on the node runs with
only what Docker needs (`PATH`, `HOME`, `DOCKER_*`). While a lane starts,
the node takes no sync for it (`409`; the lane's own start syncs before),
until its daemon answers: its containers start after that from what the
node checked and built, and Compose builds nothing then (the lane runs by
that time, and its files are its own to change).
Before it checks, it stops whatever of the lane still runs; `devstack lane
stop`, the end of a lease and the removal of a member remove every container
of the lane's Compose projects, whatever its files say by then.

Images are not the member's alone: an image a lane pulls (`[lanes] image`,
a service's `image:`) is pulled with the node's registry credentials, and
any lane on the node can then run it by its name. Only images the node
builds for a member (`[lanes] dockerfile`, a Compose `build`) have names
other lanes cannot tell. Keep a private image off a shared node, or give the
node no credentials for it.

Every container of the lane gets
the node's limits and a process limit; the lane container drops every
capability. What a member uploads is kept per member, at most 8 GiB: the
end of a lease keeps it, so the same lane started again sends only what
changed ([ADR-0038](adr/0038-a-lanes-data-outlives-its-lease.md)). The
node removes what no lease of the member needs once the 8 GiB would be
exceeded (never what was uploaded in the last ten minutes), and all of a
member's files once the coordinator no longer lists the member.

This confines what a lane asks for, not the engine it runs on: the lanes of
all members share the node's kernel and Docker engine, and a container escape
is not contained. Running the pool's lanes on a machine trusts the members
who place them that far; that is why an admin grants which machines are
nodes.

A lane on a node keeps its data there when it stops
([ADR-0038](adr/0038-a-lanes-data-outlives-its-lease.md)): its mirror, its
builds, its toolchain caches and its containers' volumes stay in a slot of
that node, for the member, repository and lane, so the next start of the
lane there syncs only what changed, builds warm and finds its databases.
The node keeps a stopped lane a week if it is dedicated, a day otherwise
(`keep_lanes_hours` in its policy), and at most 20 GiB of one member's kept
lanes, mirrors, builds and volumes together (`keep_lanes_gib`; the least
recently used goes first); it removes them at once when the pool no longer
lists the member. The images Compose built for a lane go with its slot, and
the lane images built from `[lanes] dockerfile` with the member, or when the
node has less than 10 GiB free. A node takes no lane without the disk it
needs there and 5 GiB to spare (see "What a lane needs"). A stack with `[lanes]
keep_data = false` keeps nothing. `devstack lane reset <lane>` throws a
stopped lane's data away, here and on the nodes it ran on;
`devstack lane prune [--older-than 7d]` removes what this repository's
gone lanes left (both ask the node with `DELETE /v1/slots`). A move to another node is a cold start
there: data never moves between nodes. It runs without background workers unless it holds the
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
**Shared with you** (in the sidebar listed as your stacks are, and in the
pool area) and open its services:

```sh
devstack pool shared
devstack open login-fix web                     # connects the lane, then the browser
devstack pool connect login-fix                 # every service it shares forwarded here
devstack pool disconnect login-fix              # the forwards close, the ports are free again
```

Nothing of a shared lane is forwarded to your machine until you **connect**
to it ([ADR-0053](adr/0053-whoever-a-lane-is-shared-with-connects-to-it.md)):
in its header (*Connect*), in the pool area, or by opening one of its
services. Connecting forwards every service it shares at once, each on the
port its owner has it on if that is free here (so a front end that calls its
API on `localhost` works), else on another one, which the header names.
*Disconnect* closes them, connections in flight too, and the ports are free
for your own stack again. In the sidebar a shared lane you are not connected
to is dimmed. A connection ends with the hub, and when the lane is no longer
shared with you.

Its services are those with a `port` and the Compose containers the dashboard
links as a web page (a `urls` entry with `http`/`https`, or a published port
without an entry); `urls = { db = "" }` in `[compose.<group>]` keeps a
container out, a database, say. The node
checks every connection against the coordinator, so unsharing holds from the
next one.

They also **view** it ([ADR-0040](adr/0040-a-shared-lane-can-be-watched-like-ones-own.md),
amended 2026-10-20): a click on it opens the same pages as one's own lane,
read only. A click on a service's or container's link there opens the
forward to it as `devstack open` does, the link's path kept. They see its
state, its services' and containers' logs, its
events, its command runs and its usage, never its environment, and change
nothing: no restart, no stop. Logs show what the services print: share a
lane only where that is fine. The node asks the coordinator at least every
5 seconds whether a viewer still may. (Up to 0.11.1 viewing was a separate
grant, `--view`; it is gone, sharing includes viewing.)

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
    --memory-headroom 2 --max-cpus 8 --share-percent 80 --network internet
```

`devstack node members policy <node>` without options prints the node's
policy; with any option it **replaces** the whole policy: what the command
does not name goes back to its default (no `--allow` or `--repo` limit,
no memory or CPU limit, `--network lan`): repeat every option the node
should keep. `--clear` removes the policy.

A node's **policy** (ADR-0033): who may run lanes there (member ids or
tags), which repositories (`[stack] name`; a guard against mistakes, not
against a member who lies), a memory limit (the lane's expected need times
the headroom, at least 2 GiB), a CPU limit per lane, both on every container
of the lane, what of its free CPU and memory it offers (`--share-percent`,
1 to 100; default 50: a dedicated server nobody works on can offer more),
and its network:

| `--network` | A lane there reaches |
|---|---|
| `lan` (default) | the internet and the local network |
| `internet` | the internet, not the local network and not the node itself (iptables rules per lane, IPv4; the lane's networks have no IPv6) |
| `none` | only its own containers (`internal` networks and the same rules) |

On an `internet` or `none` node, the lane's images (`[lanes] dockerfile`,
Compose `build`) are built without a network: `RUN` steps that download
need a `lan` node, or a prebuilt `[lanes] image`.

A lane is taken to need at least 0.25 CPU and 256 MiB, whatever it asks
for, and its GPUs are GPUs the node has. The coordinator places by the policy and the node admits by it, and by
its own `policy.json`. An admin can
also pause a node (no new lanes) and end a lease; every change is in the
admin log.

Removing a member (`devstack node members remove <id>`) revokes it: nodes
take the member list from the coordinator and refuse who is not on it. While
the coordinator does not answer, a node trusts its last list for at most an
hour; after that it refuses every member, renewals included, until the
coordinator is back.

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

A machine that is a node of two pools runs an agent for each, usually on the
same Docker engine. Each agent labels its lane containers and lane images
`devstack.agent=<tag>` (8 hex digits of its own secret) and names its network
rules `ds-<tag>-<lease>-f`/`-i`; it only ever removes what carries its own
tag. Update the agents on one engine together: what an agent of an earlier
release left (containers labelled `devstack.lease` only, chains
`dsl-<lease>-f`/`-i`) the updated agent whose lane it was removes, an
earlier-release agent beside it would still remove the other's.

## Capacity and telemetry

The pool area shows what the pool has, offers and its lanes use, GPUs
included (found with `nvidia-smi`), for the last hour or day, and the same per
node. The coordinator keeps that history in memory.

A pool can collect **telemetry**: devstack's diagnostic log and how long
lane starts, syncs, moves and stops, service starts, builds and tunnel
connections take, with their outcome. Durations go out as
`devstack.duration` histograms labelled with the event, repository, lane
(`main` for the main working tree), node, place (`local`, `remote`),
service and result; log records carry the repository and the operating
system, and nested values as dotted attributes (`devstack.status.state`).
A service without a health check counts as running after a fixed wait
(`settle_ms`), which is no duration. The collector (OTLP/HTTP) is set on the
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

A daemon or the hub keeps its lines from its start until its first round
(after ten seconds) has asked its pools: they are sent only if a pool
collects and this machine agreed, else dropped.

A node sends what concerns the pool's lanes on the coordinator's setting
alone: its capacity every minute and its own lines (a lease admitted or
refused, a lane started, not started with its phase and reason, a compose
group of it up or failed with how long it took, stopped, an update refused
or failed).

Never sent: file contents, environment values, command lines, service output.
Every text of a log line is cut to its first line before it leaves the
machine (a failed job's reason keeps its process output in `daemon.log`
only); fields with a command line, an environment, process output or a path
on the machine stay in `daemon.log`; in every text the repository's path
goes out as `<repo>`, a lane's worktree as `<worktree>`, devstack's state
as `<devstack>` and the home directory as `~`, and user names in other home
paths and credentials in URLs are masked. A collector that is switched off
answers `404`, and the member stops sending at once.

To read what a pool collected when the collector writes to Elasticsearch
(the OpenTelemetry collector's `elasticsearch` exporter), `scripts/telemetry/`
has two scripts (Python, standard library only): `fetch.py` exports the
`logs-generic.otel-<namespace>` and `metrics-generic.otel-<namespace>`
streams to NDJSON, `analyze.py` turns the export into a Markdown report
(senders, send rounds and gaps, warnings, failed requests, durations,
service starts, candidate triggers, node capacity, data-quality checks).

## Versions in a pool

Machines are updated one by one, so a pool runs several devstack releases at
once. Each says which it runs: nodes and members in their reports, the
coordinator in its answers. The pool area shows each node's version and the
coordinator's, marked where it is not this machine's; `devstack pool` lists
each node's (`older`: one that does not say).

Releases work together when their **pool protocol** is the same: a number
bumped by a change that two releases would misread (devstack 0.10 is `1`:
a shared lane's ports and a lane on a dedicated node are the stack's main
ports). A requester gets no lane on a node with another protocol: the
coordinator does not place it there, and names both releases when the node
is named. A node of another version with the same protocol, or one too old
to say (before this was added), gets the lane, with a warning: in
`daemon.log` (`pool_version_mismatch`), under the lane in the dashboard and
in `devstack lane list`, and from `devstack up --on`. Opening a shared lane
warns the same way when its node, or its owner against its node, runs
another release, and a connection that fails names both versions.

When in doubt, update the node and the coordinator first: `devstack
--version` on each says what it runs.

**An admin updates a server from the dashboard**
([ADR-0039](adr/0039-an-admin-can-update-a-server-node.md)): "Update" on a
node of the kind server, online. The order reaches the node with the answer
to its next report (the coordinator never calls a node). The node takes no
new lane, waits until the lanes it runs have ended, installs the latest
release (from GitHub, checked against its `SHA256SUMS`, as `devstack
update` does) and starts its agent again on it; the version column says how
far it got, or why it failed. It waits at most eight hours for its lanes,
then gives up and takes lanes again; while it waits, **Cancel update**
withdraws the order (so does a coordinator restart: orders are kept in
memory). When the new release does not start, the agent goes on with the
one it runs and says so. A node refuses when its own `policy.json` says
`"remote_update": false`, when its agent runs in a container (update its
image), and an older release than it runs unless the order says
`downgrade`. Run a server's agent under systemd so it comes back whatever
happens: `devstack node agent ... --print-unit` prints the unit for the
same arguments. "Prune" on a node removes the stopped lanes it keeps
(ADR-0038).

## Reference

| What | Where |
|---|---|
| Membership (key, certificate, coordinator) | devstack's per-user state, `pool/`; other pools in `pools/<name>/`; `DEVSTACK_POOL_DIR` overrides the default pool's |
| Share settings, telemetry consent | `share.json`, `telemetry.json` beside the membership |
| Coordinator | `DEVSTACK_COORDINATOR_DIR`; `members.json` (changed under `members.lock`), `audit.jsonl`, `telemetry.json` |
| Node agent state (leases, mirrors, uploaded files per member, `policy.json`, `secret`) | `node/` beside its membership |
| Ports | coordinator and node agent: as you choose (`--listen`); mutual TLS with the pool's CA |
| Certificates | the pool's CA 20 years, the coordinator's server certificate 10, a member's 5; not renewed: when one expires, that member (for the CA, the pool) is set up again |
| Leases | live 120 s; the requester renews every 30 s |
| Uploads to a node | at most 512 MiB per file, 8 GiB per member |
| Kept lanes on a node | `GET /v1/slots` (the caller's own), `DELETE /v1/slots?repo=&lane=` (`lane reset`, `lane prune`, a removed worktree) |
| HTTP API | the hub's `/hub/pool...` routes in [dashboard-api.md](dashboard-api.md) |

What the node agent sets in a lane container, for the `devstack` daemon in it
(not for stack files; the daemon refuses `DEVSTACK_LISTEN` without
`DEVSTACK_REMOTE_LANE`):

| Variable | Meaning |
|---|---|
| `DEVSTACK_REMOTE_LANE` | the lane's name; marks the daemon as one in a lane container |
| `DEVSTACK_LISTEN` | `0.0.0.0:7600`: the daemon's API on the lane's network, for the agent (token and `Host` checked) |
| `DEVSTACK_PORT_OFFSET` | the lane's port offset on the requester; the lane runs on those ports in the container too |
| `DEVSTACK_DISPLAY_ROOT` | where the requester has the lane's files, so paths in compiler messages and logs show as the developer's |
| `DEVSTACK_EXTERNAL_INFRA` | the file (`/src/.devstack/external-infra.json`) in which the agent describes the lane's containers, which it starts itself; it writes it before the lane container starts, each compose group `created`, then each group's `state` as it brings them up after the daemon answered (`starting`, `up`, `failed` with an `error`; no `state`: an agent of an earlier release, which wrote the file once all were up), which the daemon's `up` waits for; it writes their states, health and load (`cpu`, `memory`, `at`) into it every 5 s, and answers the lane's container routes (`/v1/infra/{group}/{service}/logs`, `logs/stream`, `start`/`stop`/`restart`) itself, for the lane's owner, since the lane has no Docker (at most 16 `logs/stream` at once per member, `429` beyond); the service routes that name containers (`/v1/services/app.web/restart`, a group's restart) it acts on before it passes them to the daemon ([dashboard-api.md](dashboard-api.md#actions)) |

On the node, `DEVSTACK_NODE_CONTAINER` (the agent's container name, when it
runs in one) and `DEVSTACK_NODE_COMPOSE_IMAGE` (the image it reads Compose
models in) are the agent's own; `DEVSTACK_LANE_BIN` names the static Linux
`devstack` it mounts into lanes instead of its own binary (for tests).
