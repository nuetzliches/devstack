# Dashboard API (hub and daemon)

The contract between the dashboard (`ui/`) and the hub (ADR-0021). The
dashboard talks to the hub only; the hub forwards `/hub/stacks/{id}/api/...` to
that stack's daemon (`/v1/...`). All JSON uses `snake_case`; times are Unix
milliseconds. Examples live in [`ui/mock/`](../ui/mock).

**No polling.** Every live value arrives on an event stream (Server-Sent
Events, one JSON object per `data:` line). On (re)connect a stream starts with a
snapshot, so a client never needs a timer to stay current. A client may fetch a
resource once in reaction to an event (e.g. the stack view after an `infra`
event); it never fetches on a schedule.

## Hub

Origin `http://127.0.0.1:4250` (or `localhost:4250`).

| Method and path | Auth | Answer |
|---|---|---|
| `GET /`, `/assets/*`, `/manifest.webmanifest`, `/sw.js` | none | the app shell (works offline once cached) |
| `GET /auth?code=<code>` | none | sets the session cookie (one year, survives a hub restart), redirects to `/` |
| `GET /hub/ping` | none | `{"devstack":true,"version":"0.3.0"}`; CORS for the landing page only (ADR-0021) |
| `GET /hub/session` | cookie | `200 {"version": "0.2.0"}`, or `401` (not paired: show "run `devstack ui`") |
| `GET /hub/stacks` | cookie | `[StackEntry]` |
| `GET /hub/ws?path=<stream>` | cookie + Origin | WebSocket that carries the server-sent event stream at `<stream>` (`/hub/events` or `/hub/stacks/{id}/api/v1/...`), one text message per event's `data`; how the dashboard reads every stream (a browser allows six HTTP/1.1 connections per host across all its tabs, WebSockets do not count; ADR-0021). The upstream is opened before the upgrade: `503` for a stack not running, `400` for another path. When the stream ends the socket closes |
| `GET /hub/events` | cookie | SSE: `{"type":"stacks","stacks":[StackEntry]}` and `{"type":"docker","docker":DockerEngine}` and `{"type":"release","current","newer":string|null}` (a newer devstack; checked at start, every 12 h by the clock and when a dashboard connects and the last answer is over an hour old, unless `DEVSTACK_NO_UPDATE_CHECK`) and `{"type":"update","update":{"running":bool,"error":string|null}}` (the dashboard's update), each first and on every change |
| `POST /hub/update` | cookie + Origin | `202`: `devstack update` in the background (output in `hub/update.log`); on success it restarts the hub on the new version, so the dashboard reconnects to it; on failure the `update` event carries the end of its output. `409` while one runs. Running stacks switch at their next restart |
| `POST /hub/stacks/scan` | cookie + Origin | `{"path", "add"?, "depth"?}`: every repository with a `devstack.toml` below a folder (default three levels; a repository only at its root, linked worktrees skipped), `[{repo, id, name, error, registered, added}]`; with `add` the valid new ones are registered |
| `POST /hub/pick-folder` | cookie + Origin | the operating system's folder dialog, shown by the hub (Windows: Explorer picker, Linux: XDG portal): `{"path": "..."}`, `{"path": null}` when cancelled, `501` without a desktop, `409` while one is open |
| `GET /hub/inspect?path=` | cookie | `{repo, is_dir, has_stack_file, valid, registered, id}` for an absolute folder path (the "Add a stack" dialog) |
| `POST /hub/stacks/add {"path"}` | cookie + Origin | registers a folder that has a `devstack.toml`; `{id, name}` |
| `POST /hub/stacks/init {"path"}` | cookie + Origin | writes a first draft (`devstack init`, never over an existing file) and registers it when it loads; `{file, services, compose_files, todos, id}` |
| `GET /hub/docker` | cookie | `DockerEngine`: `{"state":"running","version"}`, `{"state":"stopped","starter":"Docker Desktop"\|null,"hint"}` or `{"state":"missing"}`; the hub looks every 10 s |
| `POST /hub/docker/start` | cookie + Origin | `202 {"started":"Docker Desktop"}`, `200` when it already runs, `409 {"error"}` where devstack cannot start it (Docker Engine as a system service on Linux needs root) |
| `POST /hub/stacks/{id}/up` | cookie + Origin | `202`; the stack starts (daemon, infrastructure, autostart services) |
| `POST /hub/stacks/{id}/down?infra=false` | cookie + Origin | `202`; services stop, the daemon exits |
| `DELETE /hub/stacks/{id}` | cookie + Origin | `204`; forget a stack (it registers again when it starts) |
| `ANY /hub/stacks/{id}/api/<path>` | cookie (+ Origin for non-GET) | the daemon's `/<path>`, streams passed through; `503 {"error":"stack not running"}` when its daemon is down |
| `GET /hub/pools` | cookie | the pools this machine belongs to or asked to join, the default one first: `[{name, joined, state?, coordinator?}]` ([ADR-0034](adr/0034-a-machine-can-belong-to-several-pools.md)). Every `/hub/pool...` route below takes `?pool=<name>`; without it, the default pool |
| `GET /hub/pool/history?node=&range=1h\|24h` | cookie | a node's capacity over time, or the pool's without `node` ([ADR-0036](adr/0036-the-pool-shows-its-capacity-over-time.md)): `[{at, total_cpus, total_memory, offered_cpus, offered_memory, used_cpus, used_memory, gpus, gpus_offered, gpu_load, gpu_memory, gpu_memory_used}]`; the last hour as reported (every 15 s), or the last day by minute; kept by the coordinator in memory, since it started |
| `GET /hub/pool/shared` | cookie | the lanes others share with this machine ([ADR-0035](adr/0035-lanes-can-be-shared-with-the-pool.md)): `[{lease, repo, lane, owner, owner_name, node, services: [{name, port, scheme, here}]}]` |
| `POST /hub/pool/shared/{lease}/open {"service"}` | cookie + Origin | a forward on this machine to that service, through its node (which checks the sharing at every connection): on the owner's port `here` if free, else another; `{url, port}`. It stays while the hub runs |
| `GET /hub/pool` | cookie | `PoolOverview`: this machine's pool, read from the coordinator with its member certificate (ADR-0032); `{"joined":false}` in no pool; `502 {"error"}` when the coordinator does not answer |
| `POST /hub/pool/members/{id}/approve {"role"?}` | cookie + Origin | admins only: approve a request, as `member` (default) or `admin`; the member's `PoolMember` |
| `POST /hub/pool/members/{id}/remove` | cookie + Origin | admins only: remove a member or reject its request; `409 {"error"}` for the last admin |
| `POST /hub/pool/nodes/{id}/pause`, `.../resume` | cookie + Origin | admins only: a paused node takes no new lanes (the coordinator refuses leases on it); running ones stay |
| `POST /hub/pool/leases/{id}/end` | cookie + Origin | admins only: `202`; the node stops that lane at its next report (within about 15 s); its requester sees the lease over |
| `POST /hub/pool/members/{id}/tags {"tags"}` | cookie + Origin | admins only: replace a member's tags (ADR-0033); the member's `PoolMember` |
| `POST /hub/pool/nodes/{id}/policy` (a `NodePolicy` or `null`) | cookie + Origin | admins only: what the node lets run on it, `{allow, repos, memory_headroom?, network: "lan"\|"internet"\|"none"}`; `null` lifts it |
| `POST /hub/pool/join {"coordinator", "fingerprint", "name"?}` | cookie + Origin | ask a pool to admit this machine (`devstack join` without waiting); `GET /hub/pool` shows it pending until an admin approves |
| `POST /hub/pool/members/{id}/role {"role"}` | cookie + Origin | admins only: `member` or `admin`; `409 {"error"}` for the last admin. A member who is no admin gets `403` from all three |
| `POST /hub/pair` | `Authorization: Bearer <user token>` | `{"code": "...", "url": "http://127.0.0.1:4250/auth?code=..."}` (CLI only) |

```ts
type StackEntry = {
  id: string;            // stable, from the stack file path
  name: string;          // [stack].name
  file: string;          // absolute path of devstack.toml
  repo: string;
  running: boolean;      // its daemon answers
  daemon: { pid: number; port: number; started: number; version: string } | null;
  last_seen: number | null; // when the hub last saw it running
  start_error?: string;  // why the last start from the dashboard failed (end of `devstack up`'s output; hub/up-<id>.log), until it runs
};
```

```ts
// Times in the pool are Unix seconds, as the coordinator keeps them.
type PoolOverview = {
  joined: boolean;
  me?: { id: string; name: string; state: 'pending' | 'approved' | 'removed'; role?: 'member' | 'admin'; coordinator: string };
  members?: PoolMember[];   // approved ones for a member; every one, with `address`, for an admin
  leases?: { id: string; node: string; member: string; repo: string; lane: string; state: 'offered' | 'running'; created: number; expires_in?: number }[];
  join?: { coordinator: string; fingerprint: string; command: string }; // what another machine runs to join
  nodes?: { id: string; name: string; online: boolean; paused?: boolean; seen?: number; report?: { sharing: boolean; cpus: number; memory: number; api?: string } }[];
  audit?: { at: number; by: string; action: 'approve' | 'reject' | 'remove' | 'role'; member: string; name: string; detail?: string }[]; // admins only, newest first
};
type PoolMember = { id: string; name: string; state: 'pending' | 'approved' | 'removed'; role: 'member' | 'admin'; requested_at: number; address?: string };
```

## Daemon (through the hub)

Paths below are relative to `/hub/stacks/{id}/api`.

### State

`GET /v1/stack` -> `StackView` (see `ui/mock/stack.json`):

```ts
type StackView = {
  name: string; repo: string; version: string;
  services: ServiceView[]; infra: InfraView[]; groups: GroupView[]; commands: CommandView[];
  config_changed: boolean;              // the stack file differs from what the daemon read at start; applies with the next start
};
type ServiceView = {
  name: string; kind: "service" | "job" | "devserver" | "tool";
  groups: string[]; port: number | null; autostart: boolean;
  url: string | null;                   // https://localhost:4200 (https when the health check speaks TLS)
  status: Status; build_id: string | null; pid: number | null;
  since: number; log: string | null;
  instance_port: number | null;          // internal port behind the proxy
  proxy: ProxyView | null;
  candidate: Candidate | null;           // a newer build on its way, or why it did not arrive
};
type Status =
  | { state: "stopped" | "starting" | "running" | "healthy" }
  | { state: "building"; build_id: string | null }
  | { state: "unhealthy" | "failed"; reason: string }
  | { state: "completed" | "exited"; exit_code: number };
type Candidate = {
  state: "changed" | "building" | "superseded" | "starting" | "switching" | "held" | "failed";
  build_id: string | null; since: number; reason: string | null;  // reason: compiler errors, why held
};
type ProxyView = { port: number; upstream: number | null; build: string | null; pending: string | null;
                   active: number; served: number; last_error: string | null };
type InfraView = { group: string; project: string; containers: ContainerView[]; error: string | null };
type ContainerView = { service: string; name: string; state: string; health: string; ports: string[];
  urls: string[] };                      // dashboard links: the group's `urls` entry, or http://localhost:<port> per published port
type GroupView = { name: string; label: string; order: number | null; members: string[] };
  // members: "<service>", "compose.<group>.<service>", "command.<name>"
type CommandView = { name: string; description: string; confirm: string | null; requires: string[] };
```

`GET /v1/events` -> SSE, first event `{"type":"snapshot","stack":StackView}`, then
(see `ui/mock/events.jsonl`):

```ts
type Event = { seq: number; at: number } & (
  | { type: "snapshot"; stack: StackView }
  | { type: "service"; service: string; status: Status; build_id: string | null }
  | { type: "candidate"; service: string; candidate: Candidate | null }
  | { type: "infra"; group: string; message: string }        // a container changed: fetch /v1/stack once
  | { type: "command"; command: string; run: number; status: "running" | "succeeded" | "failed" }
  | { type: "config"; changed: boolean }   // the stack file changed since start (true) or back (false)
  | { type: "lanes"; lanes: LaneView[] }  // a worktree added or removed, a branch checked out, a lane started or stopped, the main ports moved
  | { type: "usage" } & UsageReport                           // every 2 s, without history
);
```

### Usage

`GET /v1/usage?history=true` -> `UsageReport` with `history` (the last ten
minutes) per entry, see `ui/mock/usage.json`. Live values arrive as `usage`
events; append them to the history client-side.

```ts
type Load = { cpu: number /* % of one core */; memory: number /* bytes */ };
type Point = { at: number; cpu: number; memory: number };
type UsageReport = {
  at: number;
  services: ({ name: string; cpu_time_ms?: number; processes?: number; history?: Point[] } & Load)[];
  builds: { name: "builds"; cpu_time_ms?: number; processes?: number; history?: Point[] } & Load;
  containers: ({ group: string; service: string; history?: Point[] } & Load)[];
  groups: ({ name: string } & Load)[];
  lanes: ({ name: string; disk?: { snapshot: number; artifacts: number; logs: number; volumes: number | null } } & Load)[];
  docker_error?: string;
};
```

### Logs

| Method and path | Answer |
|---|---|
| `GET /v1/services/{name}/logs?tail=200&grep=` | text, the last (matching, case-insensitive) lines of the current instance |
| `GET /v1/services/{name}/logs?build=<id>` | text, the build log of that build and the logs of every instance that ran it (an id prefix is enough), each after a `── … ──` header; `tail` counts across them, cut lines are announced; `404` when none is kept |
| `GET /v1/services/{name}/logs?since=30m` | text, the logs of every instance written to within that time, oldest first, with headers (lines carry no time of their own) |
| `GET /v1/services/{name}/logs?file=<name>` | text, one kept log (a name from `logs/files`; no other path) |
| `POST /v1/infra/check` | probe every running container's published ports from this machine: `[{group, service, ports}]` that accept and close at once (Docker lost the port forward, or nothing listens inside); recorded as `ContainerView.unreachable`. A service with `after = ["compose.<group>.<service>"]` restarts such a container once before it starts |
| `GET /v1/infra/{group}/{service}/logs?tail=200&grep=&since=` | text, a container's output (`docker compose logs`), stdout and stderr merged by Docker's timestamps, each line starting with one; `since` as Docker takes it (`30m`); `502` when Docker fails. `/v1/services/{name}/logs` answers the same for a name that is no service but exactly one Compose service |
| `GET /v1/infra/{group}/{service}/logs/stream?tail=500` | SSE: `{"type":"line","text":"..."}` per line of `docker compose logs --follow` (timestamped), the same events as a service's log stream; in a lane its own container, the main tree's for a `shared` one |
| `GET /v1/lanes` | JSON, the lanes: the repository's other worktrees ([ADR-0024](adr/0024-a-lane-is-a-supervisor-in-the-main-trees-daemon.md)), `[{name, path, branch, number, port_offset, active, owns_main, running, services, node?, node_error?}]`; `active`: started; `node`: the node of the pool it runs on (absent: this machine), `node_error`: what went wrong there lately (a sync, a renewal); `moving`: `{kind: start|move, to, step, steps, what, since, error?, failed_at?}` while a start on a node or a move runs, and for ten minutes after one failed |
| `POST /v1/lanes` | `{"path", "infra"?, "workers"?, "force"?, "on"?}`: start the lane of that worktree; with `on` (a node's name or member id, or `pool` for the coordinator to choose; in another pool than the default one `<pool>:<node>`, or `<pool>` for its coordinator to choose, [ADR-0034](adr/0034-a-machine-can-belong-to-several-pools.md)) on that node of the pool instead, leased, synced and started there, its API passed through under `/v1/lanes/{lane}/v1/...` ([ADR-0025](adr/0025-a-lane-can-run-on-another-node.md)) (what `devstack up` in it does; `infra` starts the main tree's `shared` Compose services if needed, the lane's own containers start with its `up`; `workers` runs its background workers; `force` skips the memory check, which otherwise answers `409` with what the lane needs and what is free, ADR-0030), then `/v1/lanes/{lane}/v1/up` starts its services and its containers. `201` with its entry. Worktrees are git's: there is no route that creates or removes one |
| `POST /v1/lanes/{lane}/share` | `{"with"}`: share a lane on a node with others in its pool: member ids, tags, `*` for every member; `[]` for nobody ([ADR-0035](adr/0035-lanes-can-be-shared-with-the-pool.md)). Kept per lane, told the coordinator while it runs on a node. Answers with the lanes (`LaneView.shared_with`) |
| `POST /v1/lanes/{lane}/move` | `{"to"}`: a node of the pool (name or member id, `<pool>:<node>` in another pool) or `local`; the started lane moves there, a cold start that takes over once its autostart services are ready ([ADR-0025](adr/0025-a-lane-can-run-on-another-node.md) point 10). Answers when done, with the lanes; `409` with the reason when it could not (the lane stays where it was) |
| `POST /v1/lanes/{lane}/stop` | its services stop and it is no longer started (its builds stay); `409` while it serves the main ports. The lanes after it |
| `POST /v1/lanes/{lane}/main` | that lane serves the main ports (`main`: the main tree again), workers move with them |
| `/v1/lanes/{lane}/v1/...` | the lane's own API, every route of this table; `StackView` then carries `lane`, the main one `main_served_by` while a lane serves its ports |
| `GET /v1/services/{name}/builds?limit=50` | JSON, the builds of the service newest first, read from the diagnostic log (so across daemon restarts): `[{build, started_ms, updated_ms, outcome, reason, current}]`; `outcome` is `live`, `failed`, `superseded` (a newer change replaced it before it went live), `held`, or the state it is in now |
| `GET /v1/services/{name}/logs/files` | JSON, the kept logs newest first: `[{file, kind: "run"\|"build", started_ms, modified_ms, build, port, bytes, current}]` |
| `GET /v1/logs?grep=…&tail=50` | text, matching lines of every service's current log as `service \| line`; with `since=` every instance written to within that time |
| `GET /v1/services/{name}/logs/stream?tail=200` | SSE: `{"type":"line","text":"..."}` for the last lines, then every new one; `{"type":"instance","build_id":...}` when another instance takes over (the lines that follow are the new one's). With `build=<id>` it follows the newest instance of that build instead (a candidate's console), from before it exists to after it takes over |
| `GET /v1/services/{name}/build-log` | text, the latest build log of the service (full compiler output) |
| `GET /v1/runs/{id}/log/stream` | SSE: `{"type":"line","text":"..."}`, then `{"type":"end","status":"succeeded"|"failed","error":...}` |

### Actions

| Method and path | Answer |
|---|---|
| `POST /v1/up` | `202` |
| `POST /v1/services/{name}/start` \| `stop` \| `restart` \| `promote` \| `rollback` | `200 StackView`, or `409 {"error": "..."}` (e.g. compiler errors; the running instance stays). For a name that is no service but a group, `start`/`stop`/`restart` act on the group. `hot-reload-on` / `hot-reload-off` switch a service with `hot_reload` in the stack file into and out of hot-reload mode (`ServiceView.hot_reload`, `hot_reload_available`) |
| `POST /v1/groups/{name}/start` \| `stop` \| `restart` | every member service of the group: start in member order, stop in reverse, restart (build) at the same time; `restart` also restarts container members, commands are left out. `200 StackView`, or `409` naming each member that failed (the others ran) |
| `POST /v1/infra/{group}/{service}/{start\|stop\|restart}` | `204`; one container (in a lane its own; a `shared` one: `409`). The dashboard shows each container on a page of its own (`/s/<stack>/infra/<group>/<service>`) with its log |
| `POST /v1/commands/{name}` with `{"confirm": false}` | `202 {"run": 7}`, or `428 {"confirm": "text"}` (ask the user, then send `confirm: true`), `409 {"error"}` (a required service is not ready) |
| `GET /v1/commands/{name}/runs` | `[Run]` |
| `GET /v1/runs/{id}` | `Run` = `{ id, command, started, finished: number|null, status, error: string|null, log }` |

Long actions (`restart`, `promote`, `rollback`) answer when done; progress
arrives meanwhile as `candidate` and `service` events.
