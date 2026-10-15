# The stack file (`devstack.toml`)

One file in the repository root describes the local stack: the containers it
borrows from Compose, the native processes devstack builds and runs, and the
commands people and agents can trigger. Schema version 1 ([ADR-0017](adr/0017-stack-file-is-toml.md)).

- **Every table rejects unknown keys.** A typo is an error, never a silently
  ignored setting.
- **Validate after every edit:** `devstack check` (add `--compose` to let
  `docker compose config` validate each Compose group too). It reports every
  problem at once.
- **Editor support:** `devstack schema > devstack.schema.json` and point your
  editor's TOML extension at it (Taplo: `#:schema ./devstack.schema.json` as
  the first line).
- **A running daemon does not reload the file.** `devstack status` says when it
  changed since the start; it applies with `devstack down` + `devstack up`.
- **No file yet?** `devstack init` writes a first draft from what the repository
  shows (Compose files, .NET web projects, Node dev servers, Go main packages,
  Django, FastAPI and Flask apps, Spring Boot through Maven or Gradle), every guess marked
  `TODO`. The dashboard does the same under "Add a stack".

A complete example: [`examples/devstack.toml`](../examples/devstack.toml).

## Top level

```toml
version = 1

[stack]
name      = "shop"              # display name, prefix of the daemon's state
requires  = "0.2.0"             # oldest devstack that can run this file
env_files = [".env.defaults", ".env"]   # for ${VAR} everywhere; missing files are skipped
```

`requires` is checked before anything else: an older devstack asks to be
updated instead of failing on a key it does not know. Raise it when the file
starts using something new.

## `[compose.<group>]`: the infra plane

Containers come from your deployment Compose files plus a dev overlay
([ADR-0004](adr/0004-infrastructure-is-delegated-to-compose.md)). devstack runs
`docker compose up --no-deps` for the listed services only.

```toml
[compose.app]
project   = "shop-dev"                                   # fixed, never derived from a path
files     = ["deploy/compose.yml", "deploy/dev/compose.dev.yml"]   # deployment first, overlay last
env_files = ["deploy/dev/.env.defaults", "deploy/dev/.env"]        # later files win
services  = ["db", "cache"]                              # everything else in the files stays down
networks  = ["shop-shared"]                              # external networks created before `up`
after     = ["search"]                                   # other groups that must be healthy first
restart_on = { db = ["deploy/dev/db/init/**"] }          # a change restarts that container
urls      = { scheduler = "http://localhost:5100/ui", db = "" }   # dashboard links (below)
```

The dashboard links every published port of a container to
`http://localhost:<port>`, as Docker Desktop does. `urls` sets the link of a
service where that is wrong: HTTPS, a path, or `""` for no link (a database,
SMB). Keys must be in `services`.

- Keep dev-only changes (published ports on `127.0.0.1`, volumes, profiles) in
  the overlay, not in the deployment files.
- A container is ready when Compose reports it healthy, or running if it has no
  health check. Give important containers a health check in the overlay.

## `[services.<name>]`: native processes

```toml
[services.api]
kind   = "service"          # service (default) | job | devserver | tool
build  = "dotnet build src/Api/Api.csproj -c Debug -nologo -v q"
output = "src/Api/bin/Debug/net10.0"     # copied into an immutable artifact after the build
run    = "dotnet {artifact}/Api.dll"
cwd    = "src/Api"                       # working directory of `run`, relative to the repo
port   = 5080
health = { http = "/health", timeout = "60s" }
after  = ["migrate", "compose.app.db"]   # ready first: services or compose.<group>.<service>
watch  = ["src/Api/**", "Directory.Packages.props"]
```

| Kind | Lifetime | Health | Typical use |
| --- | --- | --- | --- |
| `service` | long-running, built into an artifact | `health` | APIs, workers |
| `job` | runs to completion before its dependants start | exit code | schema migrator, seed |
| `devserver` | builds and serves in one process, hot reload | `health` | `ng serve`, `vite` |
| `tool` | long-running, interactive, no health check | none | a test UI |
| `static` | a frontend's production bundle, built into an artifact and served by devstack | `health` | end-to-end runs on a pinned build |

### A static frontend (`kind = "static"`)

```toml
[services.web-e2e]
kind   = "static"
cwd    = "web"
build  = "npm run build -- --configuration production"
output = "web/dist/web/browser"   # the directory with index.html
port   = 4300
watch  = ["web/src/**", "web/angular.json"]
# spa = false                      # no index.html for unknown paths
```

devstack builds the bundle into an artifact like any service and serves its
files itself (no `run`), behind the proxy, with candidates: a test run against
`:4300` knows the build id it ran against (`x-devstack-build`). Paths without a
file extension get `index.html` (client-side routes) unless `spa = false`;
`index.html` is sent with `no-cache`. The API the bundle calls must be
reachable from the browser as the bundle expects it (an absolute URL, or the
same origin through your own proxy setup).

### Builds and artifacts ([ADR-0002](adr/0002-processes-run-from-immutable-artifacts.md))

`build` runs in a snapshot of the working tree (the lane), not in your checkout,
with the normal output directories. `output` is copied into an artifact named by
the build id (a hash of the inputs); `run` starts from `{artifact}`. The same
inputs never build twice, and a rollback needs no build.

### How a change arrives ([ADR-0003](adr/0003-changes-flow-through-candidates.md), [ADR-0010](adr/0010-promotion-mechanism.md))

- `watch`: a saved change starts a candidate build (debounced 2 s; a newer
  change cancels an older build). The candidate starts on an internal port and
  takes over the fixed port once `health` answers, without dropped requests.
  A failed build or health check leaves the running instance alone.
- `restart_on` (dev servers): restart in place, for files hot reload does not
  pick up (`tsconfig.json`, `angular.json`).
- `proxy = false`: no proxy; a restart happens in place on the fixed port (a
  short gap). Only for services the proxy cannot carry.

Generated directories never count as changes: `bin`, `obj`, `node_modules`,
`target`, `.git`, `.devstack`.

### Health

```toml
health = { http = "/health/ready", timeout = "180s", insecure_tls = true }
```

- `http`: a path on the service's port; any 2xx is healthy. Prefer a
  **readiness** endpoint that checks what the service needs (database), not a
  bare liveness probe: devstack switches traffic to a candidate only once it is
  healthy.
- `timeout`: how long a start may take until it counts as failed.
- `insecure_tls`: the service speaks HTTPS with a self-signed certificate. The
  service's URL (`{service.x.url}`, the dashboard link) then uses `https`.

### Promotion ([ADR-0013](adr/0013-schema-changes-hold-promotion.md))

```toml
promote = { hold_if_changed = ["src/Data/Migrations/**"] }   # auto = true by default
```

A candidate that changed one of these paths is built and started, but waits
(`held`) until someone runs `devstack promote <service>`. Use it for schema
changes, where switching means running a migration. `auto = false` holds every
candidate.

### Background workers ([ADR-0008](adr/0008-background-workers-run-only-in-the-active-slot.md))

```toml
[services.api.workers]
env = { Jobs__Enabled = "{is_main}" }
```

While a candidate and the main instance run side by side, only the main one may
run schedulers and queue consumers. `{is_main}` is `true` in the main slot and
`false` in a candidate; devstack starts the takeover instance with workers on.

### Environment ([ADR-0014](adr/0014-native-services-take-their-configuration-from-compose.md))

```toml
[services.api]
compose       = { group = "app", service = "api" }   # base: that Compose service's environment
rewrite_hosts = ["db", "cache"]                       # db:5432 -> 127.0.0.1:<published port>

[services.api.env]
ASPNETCORE_URLS = "http://127.0.0.1:{port}"
Telemetry__Endpoint = "http://127.0.0.1:4317"
```

- With `compose = {…}` the native process gets the environment the container
  would get, with container host names rewritten to the published addresses.
  `env` wins over it.
- `inherit = false` makes the Compose service a **reference only**: nothing is
  taken over, and `devstack check --compose` lists every variable of it that the
  stack file neither sets nor names in `ignore = [...]` (globs). That way a new
  production setting is noticed instead of silently missing.
- `devstack env <service>` shows the resulting environment with the origin of
  every variable; `--export dotenv --out <file>` exports it for running the
  service without devstack ([ADR-0015](adr/0015-devstack-is-optional.md)).

### Hot-reload mode

```toml
[services.api.hot_reload]
run = "dotnet watch run --non-interactive"   # instead of the build and `run`
cwd = "src/Api"                              # default: the service's `cwd`
```

`devstack hot-reload api on` (or the dashboard) stops the built instance and
any candidate and runs this command from the **source tree** behind the same
port, with the same environment and health check. Edits are applied by the
toolchain's own watcher in place; devstack's watcher and candidates stay quiet
for the service, and a broken edit is no longer kept away from it. `off` builds
and starts it as usual again. For a debugging session, not as the default
([plan §3.4a](plan.md)); only for services with `build`.

### Other keys

| Key | Meaning |
| --- | --- |
| `autostart = false` | started only on demand (`devstack start <service>`) |
| `platforms = ["windows"]` | only on these platforms (`windows`, `linux`, `macos`) |
| `ensure_dirs = ["{repo}/data/files"]` | created before start |
| `prepare = { run = "npm ci", inputs = ["web/package-lock.json"] }` | installs what a copy of the source lacks (see below) |

### `prepare`: what a copy of the source lacks

devstack builds in a snapshot, and a lane runs its dev servers from one
(below); a snapshot holds tracked and untracked non-ignored files only, so
`node_modules` and the like are not in it. `prepare` runs in the snapshot
before the service is built there, and in a lane's copy before the service
starts from it; again only when a file of `inputs` changed (none: once per
copy). `cwd` defaults to the service's. Its output goes to
`.devstack/logs/<service>/prepare-*.log`; a failure stops the build or start
with its tail. The main working tree runs dev servers in place, with the
developer's own `node_modules`.

```toml
[services.web]
kind    = "devserver"
cwd     = "web"
run     = "npx ng serve --port {port}"
port    = 4200
prepare = { run = "npm ci", inputs = ["web/package-lock.json"] }
```

## `[lanes]`: other worktrees ([ADR-0024](adr/0024-a-lane-is-a-supervisor-in-the-main-trees-daemon.md))

```toml
[lanes]
port_step = 100   # the default: lane 1 runs api 5000 on 5100, lane 2 on 5200
```

Every `git worktree` of the repository is a **lane** of the main tree's
daemon, the moment git creates it; the dashboard shows it under the stack,
named by its branch. `devstack up` in it starts it: its own build, its own
ports (moved by `port_step` times the lane's number), the same infrastructure,
no background workers. Every devstack command and the MCP server in the
worktree act on its lane.

Worktrees are git's (or your tool's): devstack never creates or removes one,
and nothing of a lane runs inside its worktree. Builds run in the lane's
snapshot, and so do its dev servers, tools and hot-reload mode; the snapshot
follows the worktree within moments of a change (hot module replacement keeps
working), and `{repo}` is the snapshot for them. So `git worktree remove`
works while the lane runs, and the lane stops with it. Ignored files are not
in the snapshot: `prepare` installs what is missing.

- Per-lane resources on the shared infrastructure: `{lane}` and
  `{lane_suffix}` (empty in the main tree), e.g.
  `ConnectionStrings__Db = "Server=127.0.0.1,1433;Database=app{lane_suffix}"`;
  a migrator job then creates the lane's database.
- A dev server runs in a lane only if its `run` takes `{port}` (otherwise its
  fixed port is the main tree's).
- `devstack promote --lane <name>` lets a lane's services answer on the main
  ports (dev servers through a TCP forward); `--lane main` gives them back.
- `devstack lane list`; `devstack lane stop <name>` stops a lane (its
  builds stay, the worktree too).

## `[commands.<name>]`: operations

```toml
[commands.reset-db]
description = "Drop the database, migrate, seed"
confirm     = "The local database will be dropped. Continue?"   # nothing runs without a yes
requires    = ["api"]                                           # enabled only while these are healthy
steps = [
  { stop = "api" },
  { exec = "docker compose -p shop-dev exec -T db dropdb -U dev shop", cwd = "." },
  { run = "migrate" },                                          # a job service, to completion
  { start = "api" },
  { http = "POST {service.api.url}/dev/seed", headers = { "X-Dev" = "1" }, timeout = "60s" },
]
```

Each step has exactly one action: `exec`, `http` (`METHOD url`, with `headers`,
`body`, `timeout`), `start`, `stop`, `restart` or `run`. Commands run with
`devstack run <name>`, from the dashboard and through MCP (`run_command`). A
command with `confirm` runs from MCP only with `confirm: true`, which an agent
sends only after the user agreed.

## `[groups.<name>]`: views

```toml
[groups.app]
label   = "Application"
order   = 1
members = ["api", "web", "compose.app.db", "command.seed"]
```

Labels for the dashboard and `devstack status`, not dependencies: a member may
be in several groups, and members of no group appear under "Other".

## Placeholders

| Placeholder | Value |
| --- | --- |
| `{repo}` | absolute path of the lane's source root |
| `{artifact}` | artifact directory of the build being started |
| `{port}` | port of the instance being started (an internal one behind the proxy) |
| `{is_main}` | `true` in the main slot, `false` in a candidate and in a lane without the main slot |
| `{lane}` | the lane's name, `main` in the main working tree |
| `{lane_suffix}` | empty in the main working tree, `_<lane>` in a lane: `app{lane_suffix}` keeps the main tree's name |
| `{service.<name>.url}` | URL of that service's main slot (`http(s)://127.0.0.1:<port>`) |
| `{compose.<group>.<service>.<port>}` | published host address of a container port |
| `${VAR}`, `${VAR:-default}` | from the env files and the process environment; placeholders inside a default are resolved too |

`{{` and `}}` are literal braces. Durations: `500ms`, `30s`, `5m`, `1h` (a bare
number is seconds).
