# Dashboard API (hub and daemon)

The contract between the dashboard (`ui/`) and the hub (ADR-0021). The
dashboard talks to the hub only; the hub forwards `/hub/stacks/{id}/api/...` to
that stack's daemon (`/v1/...`). All JSON uses `snake_case`; times are Unix
milliseconds. Examples live in [`ui/mock/`](../ui/mock).

**No polling.** Every live value of the hub and the daemons arrives on an
event stream. The streams are server-sent events (one JSON object per `data:`
line), which the CLI reads as they are; the dashboard reads every one of them
over the hub's WebSocket (`GET /hub/ws?path=<stream>`, one message per event,
ADR-0021 as amended). On (re)connect a stream starts with a snapshot, so a
client never needs a timer to stay current. A client may fetch a resource once
in reaction to an event (e.g. the stack view after an `infra` event); it never
fetches on a schedule. The one exception is the pool: a coordinator has no
stream, so the pool area reads `/hub/pool...` every 10 s (30 s for the parts
other pages show) while it is shown (ADR-0021, addendum 2026-10-18).

## Hub

Origin `http://127.0.0.1:4250` (or `localhost:4250`); another port with
`DEVSTACK_HUB_PORT`.

**Auth.** `cookie` means the session cookie *and* a request from the hub's own
page: a `GET` names the hub's `Origin` or says `Sec-Fetch-Site: same-origin`
(or `none`, the user's own navigation); `+ Origin` means the hub's own
`Origin` is required, which a mutation and the WebSocket handshake always need.
A browser that says `Sec-Fetch-Site: same-site` or `cross-site` is refused
(`403`) whatever else it sends: a page on another loopback port is same-site
with the hub and its requests carry the cookie (ADR-0019, addendum
2026-10-18). Requests with `Authorization: Bearer <user token>` (the CLI) pass
without either.

**Headers.** Every answer carries `Content-Security-Policy` (`default-src
'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self'
data:; connect-src 'self' ws://127.0.0.1:<port> ws://localhost:<port>;
frame-ancestors 'none'; base-uri 'none'; form-action 'self'; object-src
'none'`, abridged), `X-Frame-Options: DENY`, `X-Content-Type-Options: nosniff`
and `Referrer-Policy: no-referrer`. The dashboard has no inline script.

| Method and path | Auth | Answer |
|---|---|---|
| `GET /`, `/assets/*`, `/theme.js`, `/manifest.webmanifest`, `/sw.js`, the icons | none | the app shell (works offline once cached) |
| `GET /auth?code=<code>[&next=<route>]` | none | sets the session cookie (one year, survives a hub restart), redirects to `/`, or to `next` when it is a dashboard route (`/#/…`, characters of a path and query only; a notification's, [ADR-0045](adr/0045-the-hub-notifies-the-desktop.md)). A browser that sends a valid session already keeps it: no second cookie |
| `GET /hub/ping` | none | `{"devstack":true,"version":"<x.y.z>"}`; CORS for the landing page only (ADR-0021; `OPTIONS /hub/ping` answers its preflight). With `?nonce=<16 to 128 characters>` also `"proof"`: hex of blake3 `keyed_hash(derive_key("devstack hub ping proof v1", token), nonce)`. The CLI asks with a fresh nonce and sends the token only to a hub whose proof matches |
| `GET /hub/notify` | cookie | desktop notifications ([ADR-0045](adr/0045-the-hub-notifies-the-desktop.md)): `{"available": bool, "unavailable": string\|null, "kinds": [{"kind", "label", "on", "default"}], "muted": [scope]}`; `unavailable` says why nothing shows (no desktop session, switched off in the system's settings, `DEVSTACK_NO_NOTIFY`, macOS). Kinds: `build_failed`, `held`, `service_failed`, `lane_failed` (on by default), `main_ports`, `live` (off). A scope is a stack id or `<stack id>/<lane>` |
| `PUT /hub/notify` | cookie + Origin | `{"kinds"?: {kind: bool}, "muted"?: [scope]}` (`muted` replaces the list): the new settings, as `GET`; `400` for an unknown kind or scope. Kept per machine in the hub's state (`notify.json`) |
| `POST /hub/notify/test` | cookie + Origin | one notification whatever the settings say: `204`, or `503` with why nothing shows. A click opens the dashboard |
| `GET /hub/session` | cookie | `200 {"version": "<x.y.z>"}`, or `401` (not paired: show "run `devstack ui`") |
| `GET /hub/stacks` | cookie | `[StackEntry]` |
| `GET /hub/ws?path=<stream>` | cookie + Origin | WebSocket that carries the server-sent event stream at `<stream>` (`/hub/events`, `/hub/stacks/{id}/api/v1/...`, or `/hub/pool/shared/{lease}/api/v1/...[?pool=]` for a lane shared with this machine), one text message per event's `data`; how the dashboard reads every stream (a browser allows six HTTP/1.1 connections per host across all its tabs, WebSockets do not count; ADR-0021). The upstream is opened before the upgrade: `503` for a stack not running, `400` for another path. When the stream ends the socket closes |
| `GET /hub/events` | cookie | SSE: `{"type":"stacks","stacks":[StackEntry]}` and `{"type":"docker","docker":DockerEngine}` and `{"type":"release","current","newer":string|null}` (a newer devstack; checked at start, every 12 h by the clock and when a dashboard connects and the last answer is over an hour old, unless `DEVSTACK_NO_UPDATE_CHECK`) and `{"type":"update","update":{"running":bool,"error":string|null,"installed":string|null,"finished":ms|null}}` (the dashboard's update; `finished` is when the last one ended, `installed` the version it installed, told by the hub it started) and `{"type":"guests","guests":[Guest]}` (lanes of other members on this machine, every pool: `{pool, lease, member, owner, repo, lane, state: "offered"\|"running", since: unix s, cpus, memory}`, the leases whose node is this machine; read after each share report, every 15 s per pool), each first and on every change |
| `POST /hub/update` | cookie + Origin | `202`: `devstack update` in the background (output in `hub/update.log`); on success it restarts the hub on the new version, so the dashboard reconnects to it, and the new hub's `update` event names the installed version (the old hub leaves `hub/update.json` for it; a hub that starts on the old version while the updater runs waits for it and reports a failure); on failure the `update` event carries the end of its output. `409` while one runs (checked and claimed in one step). Running stacks switch at their next restart |
| `POST /hub/update/check` | cookie + Origin | look for a newer release now: `{"current", "newer": string\|null}`, the same as the next `release` event; asked again within 30 s, the last answer (GitHub allows 60 unauthenticated requests an hour). `409` with `DEVSTACK_NO_UPDATE_CHECK`, `502` when GitHub cannot be asked, `504` after 30 s |
| `POST /hub/stacks/scan` | cookie + Origin | `{"path", "add"?, "depth"?}`: every repository with a `devstack.toml` below a folder (default three levels, `depth` is clamped to 1 to 6; a repository only at its root, linked worktrees skipped), `[{repo, id, name, error, registered, added}]`; with `add` the valid new ones are registered |
| `POST /hub/pick-folder` | cookie + Origin | the operating system's folder dialog, shown by the hub (Windows: Explorer picker, Linux: XDG portal): `{"path": "..."}`, `{"path": null}` when cancelled, `501` without a desktop, `409` while one is open |
| `GET /hub/openers` | cookie | what this machine opens a stack's folder with ([ADR-0046](adr/0046-the-hub-opens-a-worktree-in-the-users-programs.md)), in menu order: `[{id, label, kind}]`, `kind` one of `files`, `terminal`, `editor`, `git`, `agent`. Ids: `files`, `wt`, `powershell` (Windows), `terminal` (Linux), `vscode`, `vscode-insiders`, `cursor`, `windsurf`, `zed`, `github-desktop` (Windows), `claude`, `claude-cli`, `orca`; only those found (a scheme registered, a program on `PATH` or in its install folder). Empty without a desktop |
| `GET /hub/stacks/{id}/branches` | cookie | the repository's branches ([ADR-0047](adr/0047-the-dashboard-makes-a-lane-from-a-branch.md)), local ones first, each group newest first: `{branches: [{name, remote?, upstream?, worktree?, main, date}], fetched?}`. `remote`: a remote-tracking branch (`origin/feat/x`), listed only when no local branch is named after it or tracks it; `worktree`: the folder that has it checked out, `main` when that is the main working tree; `date`, `fetched` (when the repository last fetched): Unix seconds. Read while the stack is stopped too |
| `POST /hub/stacks/{id}/worktrees {"branch", "new"?}` | cookie + Origin | a worktree (a lane, not started) on `branch`, which must be one `branches` lists and no worktree has; with `new`, on a new branch of that name made from `branch` (git's rules for a name, `400` for one taken). A remote-tracking `branch` gets a local branch of its name that tracks it. The folder is the hub's: beside the repository's most recently added worktree, else `<repo>.worktrees/` beside the repository, named as the lane will be: `{path}`. `409 {"error"}` with what git said |
| `POST /hub/stacks/{id}/worktrees/remove {"path"}` | cookie + Origin | `git worktree remove` (never `--force`) of a linked worktree of the stack (`400` for the main working tree or anything else): `{}`; `409 {"error"}` with git's refusal, a worktree with changes. The branch stays |
| `POST /hub/stacks/{id}/fetch` | cookie + Origin | `git fetch --all --prune`, without a prompt (`GIT_TERMINAL_PROMPT=0`, `GCM_INTERACTIVE=never`) and within 120 s: `{}`; `409 {"error"}` with what git said |
| `POST /hub/open {"stack", "path", "with"}` | cookie + Origin | open `path` with the program `with`: `204`. `path` must be the stack's repository or one of its worktrees (`git worktree list`), `with` an id `GET /hub/openers` lists now; `400` otherwise, `404` for an unknown stack, `502 {"error"}` when the program could not be started |
| `GET /hub/inspect?path=` | cookie | `{repo, is_dir, has_stack_file, valid, registered, id}` for an absolute folder path (the "Add a stack" dialog). `valid` is `null` without a stack file, `{"Ok":null}` when it loads, `{"Err":"<why>"}` when not; `registered` is the id or `null`. Every route that takes a `path` takes a local one only: `400` for a network path (`\\host\share`, `//host/share`; on Windows anything but a drive, `C:\` or `\\?\C:\`) |
| `POST /hub/stacks/add {"path"}` | cookie + Origin | registers a folder that has a `devstack.toml`; `{id, name}` |
| `POST /hub/stacks/init {"path"}` | cookie + Origin | writes a first draft (`devstack init`, never over an existing file) and registers it when it loads; `{file, services, compose_files, todos, id}` |
| `GET /hub/docker` | cookie | `DockerEngine`: `{"state":"running","version"}`, `{"state":"stopped","starter":"Docker Desktop"\|null,"hint"}` or `{"state":"missing"}`; the hub looks every 10 s |
| `POST /hub/docker/start` | cookie + Origin | `202 {"started":"Docker Desktop"}`, `200` when it already runs, `409 {"error"}` where devstack cannot start it (Docker Engine as a system service on Linux needs root) |
| `POST /hub/stacks/{id}/up` | cookie + Origin | `202`; the stack starts (daemon, infrastructure, autostart services). Right after a `down` from the hub, the start waits until the old daemon is gone (at most 2 minutes) |
| `POST /hub/stacks/{id}/down?infra=false&all=false` | cookie + Origin | `202`; the daemon is told to shut down (`POST /v1/shutdown?unless_lanes=true`): from then on it takes no requests, stops its services, and exits (with `infra=true` it first stops the containers too, `POST /v1/down?infra=true`). While a lane runs, only the main tree's services stop (`POST /v1/down`) and the daemon and the infrastructure stay for the lanes (ADR-0024, ADR-0030); `all=true` stops the lanes too and exits |
| `DELETE /hub/stacks/{id}` | cookie + Origin | `204`; forget a stack (it registers again when it starts) |
| `ANY /hub/stacks/{id}/api/<path>` | cookie (+ Origin for non-GET) | the daemon's `/<path>`, streams passed through; the daemon is the one its `daemon.json` names at that moment. `503 {"error":"stack not running…"}` when it is down or does not take the connection within 5 s; `503` with `Retry-After: 2` and an `error` that says so when the stack is starting, stopping or restarting (see below). The daemon's `Set-Cookie` and hop-by-hop headers are not passed on |
| `GET /hub/pools` | cookie | the pools this machine belongs to or asked to join, the default one first: `[{name, joined, state?, coordinator?}]` ([ADR-0034](adr/0034-a-machine-can-belong-to-several-pools.md)). Every `/hub/pool...` route below takes `?pool=<name>`; without it, the default pool |
| `GET /hub/pool/default-on`, `PUT {on}` | cookie (+ Origin) | where this machine's lanes start without `--on` ([ADR-0025](adr/0025-a-lane-can-run-on-another-node.md), amended 2026-10-21): `{on}`, `null` here; `PUT` sets it (`null` or `local`: here; else a pool this machine is an approved member of, `409` otherwise) |
| `GET /hub/pool/placement?repo=&lane=&cpus=&memory=&profile=` | cookie | where the coordinator would start a lane of this machine's, best first ([ADR-0026](adr/0026-placement-by-the-requester-admission-by-the-node.md) point 3), by what the lane's runs with `profile` took ([ADR-0049](adr/0049-a-lanes-need-is-measured-disk-included.md)): `[{node, name, eligible, why, cpus, memory, disk, disk_room?}]`; `why`: the reason for its rank, or why it cannot take the lane; `disk`: what the lane needs there beyond what is there already; `disk_room`: what the node has for lanes |
| `GET /hub/pool/history?node=&range=1h\|24h` | cookie | a node's capacity over time, or the pool's without `node` ([ADR-0036](adr/0036-the-pool-shows-its-capacity-over-time.md)): `[{at, total_cpus, total_memory, offered_cpus, offered_memory, used_cpus, used_memory, gpus, gpus_offered, gpu_load, gpu_memory, gpu_memory_used}]`; the last hour as reported (every 15 s), or with `range=24h` the last day by minute (any other `range` is the hour); kept by the coordinator in memory, since it started |
| `GET /hub/pool/telemetry` | cookie | `{consent, collects, what}`: whether this machine agreed to send the pool telemetry, whether the pool collects it (its coordinator has a collector), and what is sent ([ADR-0037](adr/0037-telemetry-goes-to-the-pools-collector-with-consent.md)) |
| `PUT /hub/pool/telemetry {"enabled"}` | cookie + Origin | agree to send it, or take it back; off until given, per pool |
| `GET /hub/pool/shared` | cookie | the lanes others share with this machine ([ADR-0035](adr/0035-lanes-can-be-shared-with-the-pool.md)): `[{lease, repo, lane, owner, owner_name, node: {id, name, api, devstack?}, services: [{name, port, scheme, here}], owner_devstack?}]`; `devstack`, `owner_devstack`: `PoolRelease` of the node and of the owner, absent when they do not say (older) |
| `GET /hub/pool/shared/{lease}/api/<path>?pool=` | cookie | a lane shared with this machine, viewed ([ADR-0040](adr/0040-a-shared-lane-can-be-watched-like-ones-own.md)): the lane daemon's `<path>` through its node, streams passed through (also over `/hub/ws`). Reads only: `/v1/stack`, `/v1/events`, `/v1/usage`, `/v1/logs`, a service's `logs`, `logs/stream`, `logs/files`, `builds`, `build-log`, `/v1/commands/<c>/runs`, `/v1/runs/<id>[/log[/stream]]`, and a container's `/v1/infra/<group>/<service>/logs[/stream]` (the node answers those itself); anything else `403`. Whoever a lane is shared with may view it (up to 0.11.1 only those it was shared with `view`) |
| `POST /hub/pool/shared/{lease}/open {"service"}` | cookie + Origin | a forward on this machine to that service, through its node (which checks the sharing at every connection): on the owner's port `here` if free, else another; `{url, port, warning?}`. It stays while the hub runs. `warning`: the node runs another devstack than this machine or does not say which, or the owner's and the node's pool protocols differ ([ADR-0031](adr/0031-a-coordinator-places-lanes.md) point 8) `502` for a service shared with a scheme other than `http` or `https` |
| `GET /hub/pool` | cookie | `PoolOverview`: this machine's pool, read from the coordinator with its member certificate (ADR-0032); `{"joined":false}` in no pool; `502 {"error"}` when the coordinator does not answer |
| `POST /hub/pool/members/{id}/approve {"role"?}` | cookie + Origin | admins only: approve a request, as `member` (default) or `admin`; the member's `PoolMember` |
| `POST /hub/pool/members/{id}/remove` | cookie + Origin | admins only: remove a member or reject its request; `409 {"error"}` for the last admin |
| `POST /hub/pool/nodes/{id}/pause`, `.../resume` | cookie + Origin | admins only: a paused node takes no new lanes (the coordinator refuses leases on it); running ones stay |
| `POST /hub/pool/nodes/{id}/update {"version"?, "drain"?, "downgrade"?}` | cookie + Origin | admins only, a server that is online ([ADR-0039](adr/0039-an-admin-can-update-a-server-node.md)): `202`; the node updates its devstack after its next report (`drain`, the default: once its lanes have ended); `report.update` says how it goes (`draining`, `installing`, `restarting`, `done`, `refused`, `failed`, `cancelled`); a node gives up after eight hours of `draining`; `report.no_update` says beforehand why it would refuse (an agent in a container, its own policy). `409` for a node that is no server or offline |
| `DELETE /hub/pool/nodes/{id}/update` | cookie + Origin | admins only: withdraw the update a node waits for its lanes for; `204`, the node reports `cancelled` and takes lanes again after its next report. `404` without an order, `409` once it installs |
| `POST /hub/pool/nodes/{id}/prune` | cookie + Origin | admins only: `202`; the node removes the stopped lanes it keeps (ADR-0038) at its next report |
| `POST /hub/pool/leases/{id}/end` | cookie + Origin | admins only: `202`; the node stops that lane at its next report (within about 15 s); its requester sees the lease over |
| `POST /hub/pool/members/{id}/tags {"tags"}` | cookie + Origin | admins only: replace a member's tags (ADR-0033); the member's `PoolMember` |
| `POST /hub/pool/members/{id}/node {"node"}` | cookie + Origin | admins only: let an approved member run the pool's lanes as a node (`true`), or take that back (`false`; the coordinator places no new lanes there); the member's `PoolMember` |
| `POST /hub/pool/nodes/{id}/policy` (a `NodePolicy` or `null`) | cookie + Origin | admins only: what the node lets run on it, `{allow, repos, memory_headroom?, max_cpus?, network: "lan"\|"internet"\|"none"}`; `null` lifts it |
| `POST /hub/pool/join {"coordinator", "fingerprint", "name"?}` | cookie + Origin | ask a pool to admit this machine (`devstack join` without waiting); `GET /hub/pool` shows it pending until an admin approves. `coordinator` is `https://<host>[:<port>]` and nothing more (`400` otherwise). Errors of every `/hub/pool...` route pass on the coordinator's own `{"error"}` only, never the body of whatever else answered |
| `POST /hub/pool/members/{id}/role {"role"}` | cookie + Origin | admins only: `member` or `admin`; `409 {"error"}` for the last admin. A member who is no admin gets `403` from all three |
| `GET /hub/pool/share?pool=` | cookie | this machine in the pool, per pool: what it has and what is free (`total`, `free`), whether it is an approved member, the last report (`reported_at`, `error`), the `PoolRelease` the coordinator answered it with (`coordinator?`), and why it offers nothing (`unavailable`: a node agent runs lanes, the dashboard does not start one yet, [#171](https://github.com/nuetzliches/devstack/issues/171)). The share switch and its `PUT` are gone (#169) |
| `POST /hub/shutdown` | `Authorization: Bearer <user token>` | `202`; the hub exits (`devstack hub stop`) |
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
  nodes?: {
    id: string; name: string; online: boolean; paused?: boolean; seen?: number;
    kind?: 'server' | 'node' | 'machine' | 'unknown';   // dedicated node, node, a machine that asks for lanes, never reported
    report?: {
      sharing: boolean; cpus: number; memory: number;  // what it offers now
      api?: string; dedicated?: boolean;
      leases?: { id: string; member: string; repo: string; lane: string; expires_in: number }[];
      total?: { cpus: number; memory: number };        // what the machine has (ADR-0036)
      used?: { cpus: number; memory: number };         // what its lanes use
      gpus?: { name: string; memory: number; memory_used: number; load: number; offered: boolean }[];
      devstack?: PoolRelease;                           // what it runs; absent from a release before version reporting
    };
  }[];
  audit?: { at: number; by: string; action: 'approve' | 'reject' | 'remove' | 'role' | 'pause' | 'resume' | 'end' | 'tags' | 'policy' | 'node' | string; member: string; name: string; detail?: string }[]; // admins only, newest first
};
type PoolMember = {
  id: string; name: string; state: 'pending' | 'approved' | 'removed'; role: 'member' | 'admin'; requested_at: number;
  address?: string;           // admins only
  tags?: string[];            // ADR-0033
  policy?: NodePolicy;        // as a node
  node?: true;                // an admin granted it to run the pool's lanes (`devstack node members node <id> on` or the dashboard; ADR-0032 as amended); absent: not granted
};
// Which devstack a party of the pool runs; releases with the same `protocol` work together (ADR-0031 point 8).
type PoolRelease = { version: string; protocol: number };
type NodePolicy = { allow?: string[]; repos?: string[]; memory_headroom?: number; max_cpus?: number; network?: 'lan' | 'internet' | 'none' };
```

### A stack that starts, stops or restarts

A daemon can go between the moment the hub looks for it and the request:
it was told to stop, closes its port, and gives the requests it holds 2 s
before it cuts them. The hub reads the stack's `daemon.json` for every
forwarded request (not the stack list of up to a second ago), then:

- A request that certainly never reached the daemon (no connection, or
  hyper handed it back unsent), and a `GET`, `HEAD` or `OPTIONS` without a
  body, is sent again for up to 3 s, to the daemon `daemon.json` names then.
- A request the daemon took and cut, when that daemon is no longer the one
  `daemon.json` names (it stops, or is gone or replaced), is a `503` with
  `Retry-After: 2`; its `error` says that the request may have been carried
  out in part. The hub never sends a request with a body again once it went
  out.
- `503` with `Retry-After: 2` too while the hub's own `up` runs and no daemon
  answers yet (`"stack is starting…"`), and from the hub's `down` until that
  daemon is gone (`"stack is stopping or restarting…"`).
- `502` only for a daemon that still serves and broke the exchange.

The dashboard shows the `error` of such a `503`; for `"stack not running"`
it asks to start the stack.

## Daemon (through the hub)

Paths below are relative to `/hub/stacks/{id}/api`.

### State

`GET /v1/stack` -> `StackView` (see `ui/mock/stack.json`):

```ts
type StackView = {
  name: string; repo: string; version: string;
  services: ServiceView[]; infra: InfraView[]; groups: GroupView[]; commands: CommandView[];
  config_changed: boolean;              // the stack file differs from what the daemon read at start; applies with the next start
  lane?: string;                        // in a lane's view: its name
  branch?: string;                      // the checked-out branch of this tree
  main_served_by?: string;              // the lane that serves the main ports, if not the main tree
  no_main_ports?: true;                 // nothing moves with the main slot (no service on a port, none with workers: Compose containers only): serving the main ports is refused
  watcher_error?: string;               // the file watcher could not be set up, or lost events: edits make no candidates (or every built service got one)
};
type ServiceView = {
  name: string; kind: "service" | "job" | "devserver" | "tool" | "static";
  groups: string[]; port: number | null; autostart: boolean;
  url: string | null;                   // https://localhost:4200 (https when the health check speaks TLS)
  status: Status; build_id: string | null; pid: number | null;
  since: number; log: string | null;
  instance_port: number | null;          // internal port behind the proxy
  proxy: ProxyView | null;               // a dev server's forward too (ADR-0043): port, upstream (its instance's port), active, served
  candidate: Candidate | null;           // a newer build on its way, or why it did not arrive
  rebuild?: Rebuild;                     // a dev server's last rebuild, from its output (ADR-0042): {state: rebuilding|ok|failed, since, ms?, tool, errors?}; an ok one for a minute, a failed one until the next
  hot_reload: boolean;                   // the toolchain's watcher runs it now
  hot_reload_available: boolean;         // the stack file has `hot_reload` for it
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
type ContainerView = { service: string; name: string; state: string; health: string; exit_code: number; ports: string[];
  urls: string[];                        // see below
  unreachable: string[];                 // published ports Docker no longer forwards
  managed: boolean };                    // the group starts it (in its `services`); false: another container of the project
type GroupView = { name: string; label: string; order: number | null; members: string[] };
  // members: "<service>", "compose.<group>.<service>", "command.<name>"
type CommandView = { name: string; description: string; confirm: string | null; requires: string[] };
```

```ts
type LaneView = {
  name: string; path: string; branch: string | null; number: number; port_offset: number;
  active: boolean;                       // started (`devstack up` in it)
  owns_main: boolean;                    // it serves the main ports
  running: number; services: number;
  node?: string; pool?: string;          // on a node of the pool (ADR-0025, ADR-0034); absent: this machine
  shared_with?: string[];                // ADR-0035: member ids, tags, `*`
  node_error?: string;                   // a sync or renewal that failed lately
  node_warning?: string;                 // its node or the coordinator runs another devstack, or does not say (ADR-0031 point 8)
  moving?: { kind: "start" | "move"; to: string; step: number; steps: number; what: string; since: number;
             phases?: { phase: string; ms: number }[]; phase_since?: number;
             error?: string; failed_at?: number };  // a start on a node or a move under way, or failed lately
};
```

`GET /v1/events` -> SSE, first event `{"type":"snapshot","stack":StackView}`, then
(see `ui/mock/events.jsonl`):

```ts
type Event = { seq: number; at: number } & (
  | { type: "snapshot"; stack: StackView }
  | { type: "service"; service: string; status: Status; build_id: string | null }
  | { type: "candidate"; service: string; candidate: Candidate | null }
  | { type: "devserver_rebuild"; service: string; rebuild: Rebuild | null } // started, over, or gone from the view (ADR-0042)
  | { type: "infra"; group: string; message: string }        // a container changed: fetch /v1/stack once
  | { type: "command"; command: string; run: number; status: "running" | "succeeded" | "failed" }
  | { type: "config"; changed: boolean }   // the stack file changed since start (true) or back (false)
  | { type: "lanes"; lanes: LaneView[] }  // a worktree added or removed, a branch checked out, a lane started or stopped, the main ports moved
  | { type: "usage" } & UsageReport                           // every 2 s, without history
);
```

A client that falls more than 1024 events behind (it stalled, or a burst
came) gets a fresh `snapshot` in place of what it missed: the state it holds
is replaced, never patched with a gap. A `snapshot` always replaces the whole
state, whenever it arrives; `seq` counts the daemon's events (a snapshot has
`0`), so a gap in it is visible.

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
| `GET /v1/services/{name}/logs?tail=200&grep=` | text, the last (matching, case-insensitive) lines of the current instance; `tail` at most 20 000 |
| `GET /v1/services/{name}/logs?build=<id>` | text, the build log of that build and the logs of every instance that ran it (an id prefix is enough), each after a `── … ──` header; `tail` counts across them, cut lines are announced; `404` when none is kept |
| `GET /v1/services/{name}/logs?since=30m` | text, the logs of every instance written to within that time, oldest first, with headers (lines carry no time of their own) |
| `GET /v1/services/{name}/logs?file=<name>` | text, one kept log (a name from `logs/files`; no other path) |
| `POST /v1/infra/check` | probe every ready container's published ports from this machine: `[{group, service, ports}]` that accept and close at once on three probes a second apart (Docker lost the port forward, or nothing listens inside); recorded as `ContainerView.unreachable`. The daemon also judges each container once it has been ready for 20 s, and probes a port found lost again every few seconds until it answers; a restart from devstack clears the verdict. A service with `after = ["compose.<group>.<service>"]` restarts such a container before it starts, if it has been ready for 20 s and was not restarted for this in the last 10 minutes |
| `GET /v1/infra/{group}/{service}/logs?tail=200&grep=&since=` | text, a container's output (`docker compose logs`), stdout and stderr merged by Docker's timestamps, each line starting with one; `since` as Docker takes it (`30m`); `502` when Docker fails. `/v1/services/{name}/logs` answers the same for a name that is no service but exactly one Compose service |
| `GET /v1/infra/{group}/{service}/logs/stream?tail=500` | SSE (`tail` at most 5000): `{"type":"line","text":"..."}` per line of `docker compose logs --follow` (timestamped), the same events as a service's log stream; in a lane its own container, the main tree's for a `shared` one |
| `GET /v1/lanes` | JSON, the lanes: the repository's other worktrees ([ADR-0024](adr/0024-a-lane-is-a-supervisor-in-the-main-trees-daemon.md)), `[{name, path, branch, number, port_offset, active, owns_main, running, services, node?, pool?, shared_with?, profile?, node_error?}]`; `active`: started; `node`: the node of the pool it runs on (absent: this machine), `pool`: that node's pool, `shared_with`: whom it is shared with (member ids, tags, `*`; absent: nobody), `profile`: the profile it runs with, `node_error`: what goes wrong with it now (a sync, a renewal, a port that cannot be forwarded here), each until it works again, several joined by `; `, `node_warning`: the node or the coordinator runs another devstack than this machine, or does not say which (from the lease on, so while it starts too; [ADR-0031](adr/0031-a-coordinator-places-lanes.md) point 8); `moving`: `{kind: start|move, to, step, steps, what, since, phases, phase_since, error?, failed_at?}` (`phases`: `[{phase, ms}]` over so far, `phase_since`: when the current one began) while a start on a node or a move runs, and for ten minutes after one failed |
| `POST /v1/lanes` | `{"path", "infra"?, "workers"?, "force"?, "on"?, "profile"?}`: start the lane of that worktree, with `profile` with that profile of its stack file ([ADR-0041](adr/0041-a-profile-says-how-a-lane-runs-for-a-purpose.md); `409` when the worktree's stack file has no such profile or it does not apply); with `on` (a node's name or member id, or `pool` for the coordinator to choose; in another pool than the default one `<pool>:<node>`, or `<pool>` for its coordinator to choose, [ADR-0034](adr/0034-a-machine-can-belong-to-several-pools.md)) on that node of the pool instead, leased, synced and started there, its API passed through under `/v1/lanes/{lane}/v1/...` ([ADR-0025](adr/0025-a-lane-can-run-on-another-node.md)) (what `devstack up` in it does; `infra` starts the main tree's `shared` Compose services if needed, the lane's own containers start with its `up`; `workers` runs its background workers; `force` skips the memory check, which otherwise answers `409` with what the lane needs and what is free, ADR-0030), then `/v1/lanes/{lane}/v1/up` starts its services and its containers. `201` with its entry. With `on`: `202` with its entry once the lane is leased and admitted by the node; its sync and its start there (minutes on a first start: images pulled and built, containers created) go on in the background, shown as the entry's `moving` (`kind: "start"`, in `lanes` events and here). It runs there once `moving` is gone and `active` is `true`, which is once the lane's daemon answers there: its containers come up after, under it (#164; a node of an earlier release answers once they are healthy), and show as `created`, then `starting` and `healthy` in its `InfraView`s (then call `/v1/up`, which starts its services once their infrastructure is up); it failed when `moving` carries `error` (for ten minutes, until the lane stops, the main tree's `/v1/down` or a start of it here). What can be told at once is refused at once: no such worktree, a lane that runs elsewhere, no lease (`409` with the reason: no node, the coordinator or the node refuses, a node with another pool protocol). The same start asked again while it goes on answers `202` too; `201` when the lane runs on that node already. Up to 0.10.1 a start on a node answered `201` only when done. Worktrees are git's: there is no route that creates or removes one |
| `POST /v1/lanes/{lane}/share` | `{"with"}`: share a lane on a node with others in its pool: member ids, tags, `*` for every member; `[]` for nobody ([ADR-0035](adr/0035-lanes-can-be-shared-with-the-pool.md)). They open its services and view it, read only ([ADR-0040](adr/0040-a-shared-lane-can-be-watched-like-ones-own.md)); `view`, a separate list up to 0.11.1, is ignored. Kept per lane, told the coordinator while it runs on a node. Answers with the lanes (`LaneView.shared_with`) |
| `POST /v1/lanes/{lane}/reset` | a stopped lane's data goes, here and on the nodes it ran on (ADR-0038): `[Pruned]` (`{place, lane, bytes, why, removed, error}`); `409` while it runs |
| `POST /v1/lanes/prune` | `{"older_than"?: "7d", "dry_run"?: bool}`: what this repository's lanes leave behind: on this machine the Compose projects of lanes whose worktree is gone (only those whose files are in this repository's lane state), on the pool's nodes this machine's kept lanes of this repository that are gone or not used for `older_than`; `[Pruned]` |
| `POST /v1/lanes/{lane}/move` | `{"to"}`: a node of the pool (name or member id, `<pool>:<node>` in another pool) or `local`; the started lane moves there, a cold start that takes over once its autostart services are ready ([ADR-0025](adr/0025-a-lane-can-run-on-another-node.md) point 10). Answers when done, with the lanes; `409` with the reason when it could not (the lane stays where it was) |
| `POST /v1/lanes/{lane}/stop` | its services stop and it is no longer started (its builds stay); `409` while it serves the main ports. The lanes after it |
| `POST /v1/lanes/{lane}/main` | that lane serves the main ports (`main`: the main tree again), workers move with them. A lane on a node only on a dedicated one (ADR-0025 point 4): the fixed ports forward to its ports here, its workers run on the node, and the main tree takes the ports back by itself when the lane is gone (its lease over, its node silent for longer than a lease lives). The main ports are the fixed ports of `[services.*]`, not a Compose container's published ones: `409` for a stack with no service on a port and none with workers, which would move nothing (#184) |
| `POST /v1/main-slot {"owns"}` | only the daemon in a lane container on a node: it holds its requester's main slot, or no longer; its worker services restart with the switch. The requester says it again with each renewal. Answers `{"owns"}` |
| `/v1/lanes/{lane}/v1/...` | the lane's own API, every route of this table; `StackView` then carries `lane`, the main one `main_served_by` while a lane serves its ports |
| `GET /v1/services/{name}/builds?limit=50` | JSON, the builds of the service newest first, read from the diagnostic log (so across daemon restarts): `[{build, started_ms, updated_ms, outcome, reason, current}]`; `outcome` is `live`, `failed`, `superseded` (a newer change replaced it before it went live), `held`, or the state it is in now |
| `GET /v1/services/{name}/logs/files` | JSON, the kept logs newest first: `[{file, kind: "run"\|"build", started_ms, modified_ms, build, port, bytes, current}]` |
| `GET /v1/logs?grep=…&tail=50` | text, matching lines of every service's current log as `service \| line` (`grep` is required, `400` without it; `tail` counts per service); with `since=` every instance written to within that time |
| `GET /v1/services/{name}/logs/stream?tail=200` | SSE: `{"type":"line","text":"..."}` for the last lines (`tail` at most 5000), then every new one; `{"type":"instance","build_id":...}` when another instance takes over (the lines that follow are the new one's). With `build=<id>` it follows the newest instance of that build instead (a candidate's console), from before it exists to after it takes over |
| `GET /v1/services/{name}/build-log` | text, the latest build log of the service (full compiler output) |
| `GET /v1/runs/{id}/log/stream` | SSE: `{"type":"line","text":"..."}`, then `{"type":"end","status":"succeeded"|"failed","error":...}` |

### Actions

| Method and path | Answer |
|---|---|
| `POST /v1/up` | `202` |
| `POST /v1/down?infra=false` | `204` when every service stopped (with `infra=true` the containers too); `409 {"error"}` naming what did not stop. In the main tree while lanes run: `200 {"lanes": [...]}`, and `infra` is not done (the lanes share it) |
| `POST /v1/shutdown?unless_lanes=false` | `202`; the daemon stops its lanes and services and exits (`devstack down`). With `unless_lanes=true` and a lane running: `409 {"error", "lanes"}`, and it stays |
| `GET /v1/services/{name}/env?export=false` | JSON, its environment with each variable's origin (`devstack env`); `export=true`: only what Compose and the stack file give, for running it without devstack |
| `GET /v1/runs/{id}/log` | text, a command run's output so far (the last 20 000 lines) |
| `POST /v1/runs/{id}/cancel` | `202`; the run stops: its `exec` step's process tree is killed, its `run` step's job stopped, no further step starts; it ends `failed` with error `cancelled`. `409` when it is not running, `404` for no such run |
| `GET /v1/tunnel/{port}` with `Upgrade: devstack-tcp` | a raw TCP connection to that port on the daemon's machine (ADR-0029 point 4): a service's port on its loopback, or a lane's container port on the lane's network; other ports `403`. How the requester reaches the services of its lane on a node: each port it forwards here connects, through the node agent, to the lane daemon's tunnel there |
| `POST /v1/services/{name}/start` \| `stop` \| `restart` \| `rebuild` \| `promote` \| `rollback` | `200 StackView`, or `409 {"error": "..."}` (e.g. compiler errors; the running instance stays). `rebuild` is `restart` that builds even when an artifact for the same inputs exists (`devstack restart --rebuild`; the build gets the id `<inputs>-<n>`). A `stop` ends a start in progress (a build, a health wait, a job) at once. For a name that is no service but a group, `start`/`stop`/`restart` act on the group; for a name that is neither but a Compose service (`<group>.<service>`, the member form `compose.<group>.<service>`, or `<service>` when exactly one compose group lists it in `services`) they act on that container as `/v1/infra/{group}/{service}/…` does, and answer `200 StackView`. `404` for a name that is none of these. `hot-reload-on` / `hot-reload-off` switch a service with `hot_reload` in the stack file into and out of hot-reload mode (`ServiceView.hot_reload`, `hot_reload_available`) |
| `POST /v1/groups/{name}/start` \| `stop` \| `restart` | every member service of the group: start in member order, stop in reverse, restart (build) at the same time; `restart` also restarts container members, commands are left out. `200 StackView`, or `409` naming each member that failed (the others ran) |
| `POST /v1/infra/{group}/{service}/{start\|stop\|restart}` | `204`; one container (in a lane its own; a `shared` one: `409`). `{service}` is the Compose service name, as `ContainerView.service` gives it, never the container name (`container_name:`). The routes accept every container the view shows: one the group does not start (`managed: false`: started by hand, of a profile, left from an earlier stack file) can be stopped (`docker stop` by its name), `start` and `restart` answer `409` with the reason; a service that is neither in `services` nor a container of the project: `404` with the reason; another action: `400`. The dashboard shows each container on a page of its own (`/s/<stack>/infra/<group>/<service>`) with its log |
| `POST /v1/commands/{name}` with `{"confirm": false}` | `202 {"run": 7}`, or `428 {"confirm": "text"}` (ask the user, then send `confirm: true`), `409 {"error"}` (a required service is not ready) |
| `GET /v1/commands/{name}/runs` | `[Run]` |
| `GET /v1/runs/{id}` | `Run` = `{ id, command, started, finished: number|null, status, error: string|null, log }` |

**A lane on a node** (ADR-0025): its daemon has no Docker, so its node's
agent acts on its containers, for the lane's owner only, under the same
paths (`/v1/lanes/<lane>/v1/...` on the requester's daemon). It answers
`/v1/infra/{group}/{service}/{start|stop|restart}` itself (since 0.11.0,
#101); `start` and `restart` only for a service the lane's compose group
lists in `services` (on a node its `shared` ones are the lane's own too),
`stop` for any container of the lane's project. For
`POST /v1/services/{name}/{start|stop|restart}` and
`POST /v1/groups/{name}/restart` that name containers (a Compose service by
its name, a group's container members on `restart`) it acts on them, by
container name in the projects it started for the lease, then passes the
request on with `X-Devstack-Node-Acted: 1` for the daemon's answer (#128); a
container it could not act on makes the answer `409` naming it. The daemon
never runs Compose for them itself: a request that names containers and
lacks the header (it came past an agent that could not read the stack file,
or an older one) is answered `409`. The requester's daemon answers `409`
with what to update when the node's devstack is older than the release that
does this (0.12.0; 0.11.0 for `/v1/infra/...`).

Its daemon answers before its containers run (#164): the agent starts them
after, a Compose group at a time, and `POST /v1/up` there waits for each
group as a local one does, with the same `infra` events (`starting`,
`ready`, `failed: …`), before it starts the services. A group the node could
not bring up (a container that did not get healthy within 300 s) has its
reason in its `InfraView.error`, its containers stay listed with their
state and output, and `up` stops there, as locally.

Long actions (`restart`, `promote`, `rollback`) answer when done; progress
arrives meanwhile as `candidate` and `service` events. What can take many
minutes answers `202` at once and goes on in the background: `POST /v1/up`,
and a lane's start on a node (`POST /v1/lanes` with `on`, followed in the
lane's `moving`). `POST /v1/lanes/{lane}/move` still answers when done.
