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
| `GET /hub/session` | cookie | `200 {"version": "0.2.0"}`, or `401` (not paired: show "run `devstack ui`") |
| `GET /hub/stacks` | cookie | `[StackEntry]` |
| `GET /hub/events` | cookie | SSE: `{"type":"stacks","stacks":[StackEntry]}` first and on every change |
| `POST /hub/stacks/{id}/up` | cookie + Origin | `202`; the stack starts (daemon, infrastructure, autostart services) |
| `POST /hub/stacks/{id}/down?infra=false` | cookie + Origin | `202`; services stop, the daemon exits |
| `DELETE /hub/stacks/{id}` | cookie + Origin | `204`; forget a stack (it registers again when it starts) |
| `ANY /hub/stacks/{id}/api/<path>` | cookie (+ Origin for non-GET) | the daemon's `/<path>`, streams passed through; `503 {"error":"stack not running"}` when its daemon is down |
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
};
```

## Daemon (through the hub)

Paths below are relative to `/hub/stacks/{id}/api`.

### State

`GET /v1/stack` -> `StackView` (see `ui/mock/stack.json`):

```ts
type StackView = {
  name: string; repo: string; version: string;
  services: ServiceView[]; infra: InfraView[]; groups: GroupView[]; commands: CommandView[];
};
type ServiceView = {
  name: string; kind: "service" | "job" | "devserver" | "tool";
  groups: string[]; port: number | null; autostart: boolean;
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
type ContainerView = { service: string; name: string; state: string; health: string; ports: string[] };
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
| `GET /v1/services/{name}/logs?tail=200` | text, the last lines of the current instance |
| `GET /v1/services/{name}/logs/stream?tail=200` | SSE: `{"type":"line","text":"..."}` for the last lines, then every new one; `{"type":"instance","build_id":...}` when another instance takes over (the lines that follow are the new one's) |
| `GET /v1/services/{name}/build-log` | text, the latest build log of the service (full compiler output) |
| `GET /v1/runs/{id}/log/stream` | SSE: `{"type":"line","text":"..."}`, then `{"type":"end","status":"succeeded"|"failed","error":...}` |

### Actions

| Method and path | Answer |
|---|---|
| `POST /v1/up` | `202` |
| `POST /v1/services/{name}/start` \| `stop` \| `restart` \| `promote` \| `rollback` | `200 StackView`, or `409 {"error": "..."}` (e.g. compiler errors; the running instance stays) |
| `POST /v1/infra/{group}/{service}/restart` | `204` |
| `POST /v1/commands/{name}` with `{"confirm": false}` | `202 {"run": 7}`, or `428 {"confirm": "text"}` (ask the user, then send `confirm: true`), `409 {"error"}` (a required service is not ready) |
| `GET /v1/commands/{name}/runs` | `[Run]` |
| `GET /v1/runs/{id}` | `Run` = `{ id, command, started, finished: number|null, status, error: string|null, log }` |

Long actions (`restart`, `promote`, `rollback`) answer when done; progress
arrives meanwhile as `candidate` and `service` events.
