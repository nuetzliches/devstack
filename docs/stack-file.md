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
starts using something new. A pre-release devstack (`0.9.8-rc1`) counts as
older than its release (`0.9.8`). Next comes `version`: a file of another
schema version says so, not which of its keys is unknown.

### Names, paths, env files

- **Names** of services, Compose groups, commands and groups use lowercase
  letters, digits and `-`, do not start with `-`, and are none of the names
  Windows reserves for devices (`con`, `prn`, `aux`, `nul`, `com1`-`com9`,
  `lpt1`-`lpt9`): they become directory names.
- **Paths** in `env_files`, `[compose.<group>] files` and `env_files`,
  `[lanes] dockerfile` and `sync` are relative to the repository and stay
  inside it: no absolute path, no drive (`C:`), no `..`, judged the same on
  every platform.
- **Env files** are read in Compose's dialect: `KEY=VALUE` lines, `#`
  comments, an optional `export `; an unquoted value ends at ` #`; a
  `"double-quoted"` one may span lines and knows the escapes `\n`, `\r`,
  `\t`, `\\`, `\"`, `\$`, and `$$` for a `$`; a `'single-quoted'` one is
  taken as written. `${VAR}`, `${VAR:-default}`, `${VAR-default}` and `$VAR`
  in an unquoted or double-quoted value come from the process environment,
  then from what the files set before; an unknown one is empty.

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
shared    = ["mailbox"]                                  # lanes use the main tree's (see [lanes])
```

The dashboard links every published port of a container to
`http://localhost:<port>`, as Docker Desktop does. `urls` sets the link of a
service where that is wrong: HTTPS, a path, or `""` for no link (a database,
SMB). Keys must be in `services`. In a lane, a `urls` port on this machine
(`localhost`, `127.0.0.1`) moves by the lane's offset like every other port
of the lane (`http://localhost:5100/ui` is `http://localhost:5200/ui` in the
first lane); another host stays as it is written.

- Keep dev-only changes (published ports on `127.0.0.1`, volumes, profiles) in
  the overlay, not in the deployment files.
- A container is ready when Compose reports it healthy, or running if it has no
  health check. Give important containers a health check in the overlay.

Rules `devstack check` enforces:

- `files` and `services` need at least one entry each; `project` is not
  empty and not used by another group.
- `project` uses lowercase letters, digits, `_` and `-`; names in
  `services` letters, digits, `_` and `-` (no `.`, which
  `compose.<group>.<service>` splits on); `networks` may also have `.`.
  Each starts with a letter or digit (a leading `-` would be a flag to
  `docker`).
- `shared`, `restart_on` and `urls` name services of `services`; `urls`
  are `http://` or `https://` addresses or `""`; `restart_on` holds globs.
- `after` names other groups, without cycles.

## `[services.<name>]`: native processes

```toml
[services.api]
kind   = "service"          # service (default) | job | devserver | tool | static
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

Rules `devstack check` enforces:

- `service`, `job`: `build` and `output` go together (both or neither).
- `devserver`, `tool`: no `build`, no `output`.
- `static`: needs `build`, `output` and `port`, has no `run`; `spa` belongs
  to `static` only.
- Every other kind needs `run`.
- `job`: no `port`, no `health`. `tool`: no `health`.
- `health` needs `port`; `proxy = true` only for a `service` or `static`
  with a `port`.
- `port` is 1-65535 and leaves room for a lane (`port` + `[lanes]
  port_step` at most 65535). Two services may share a port only if at most
  one of them starts automatically (`autostart`).
- `after` names services or `compose.<group>.<service>` (a service its
  group starts), without cycles.
- `watch`, `restart_on`, `promote.hold_if_changed` and `prepare.inputs`
  are globs (`**`, `*`, `?`, `[...]`, `{a,b}`) that must compile.

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
with the normal output directories. In `build` and `output`, `{repo}` is
the snapshot and `${VAR}` comes from devstack's environment; devstack
expands both before the shell sees the line, the same under `cmd` and `sh`.
Other placeholders are not known to a build (lanes share its artifact), and
`cwd` is a plain path relative to the repository, without placeholders. `output` is copied into an artifact named by
the build id (a hash of the inputs); `run` starts from `{artifact}`. The same
inputs never build twice, and a rollback needs no build.

What goes into the build id:

- the files `watch` matches (every file of the repository without `watch`),
  with their content;
- build-system files in the directory of a watched file or above it:
  `Directory.Build.props/.targets/.rsp`, `Directory.Packages.props`,
  `global.json`, `nuget.config`, `package.json` and lockfiles (npm, pnpm,
  yarn), `tsconfig.json` / `tsconfig.base.json`, `Cargo.toml`/`.lock`,
  `go.mod`/`.sum`, `pyproject.toml`, `pom.xml`, Gradle build and settings
  files. A change to one starts candidates like a watched file;
- the `build` command, `output`, `cwd` and `prepare` (`run`, `cwd`,
  `inputs`, and the content of the files `inputs` matches).

Not in it: ignored files (they are not in the snapshot either), the SDK and
anything outside the repository. When one of those changed, `devstack restart
<service> --rebuild` builds again anyway; the build gets the id
`<inputs>-<n>`. The daemon log says when a build was reused
(`build_reused`) or a change was not an input (`candidate_unchanged`).

### How a change arrives ([ADR-0003](adr/0003-changes-flow-through-candidates.md), [ADR-0010](adr/0010-promotion-mechanism.md))

- `watch`: a saved change starts a candidate build (debounced 2 s; a newer
  change cancels an older build). The candidate starts on an internal port and
  takes over the fixed port once `health` answers, without dropped requests.
  A failed build or health check leaves the running instance alone.
- `restart_on` (dev servers): restart in place, for files hot reload does not
  pick up (`tsconfig.json`, `angular.json`).
- `rebuild` (dev servers, [ADR-0042](adr/0042-a-dev-servers-rebuilds-show-like-builds.md)):
  how devstack reads a dev server's rebuilds from its output, to show them
  as a built service's build (rebuilding, rebuilt in how long, failed with
  its error lines). Absent or `"auto"`: the tool is recognised from its
  lines: the Angular CLI, Vite (and Nuxt, SvelteKit, Astro), webpack's dev
  server, Next.js, `dotnet watch`, `tsc --watch`. A tool by name
  (`"angular"`, `"vite"`, `"webpack"`, `"next"`, `"dotnet-watch"`,
  `"tsc"`), `"none"`, or its own lines as regular expressions:
  `rebuild = { start = '^rebuild: start', ok = '^rebuild: done in (?P<ms>\d+)', failed = '^rebuild: broken' }`
  (TOML literal strings, so a backslash stays one; `start` optional; a group
  `ms` or `s` in `ok` is its time). A service in
  hot-reload mode is read the same way (`auto`).
- `proxy = false`: no proxy; a restart happens in place on the fixed port (a
  short gap). Only for services the proxy cannot carry.

Generated directories never count as changes: `bin`, `obj`, `node_modules`,
`target`, `.git`, `.devstack`, and the caches of frontend tools (`.angular`,
`.vite`, `.next`, `.nuxt`, `.turbo`, `.cache`, `.svelte-kit`,
`.parcel-cache`), even where `.gitignore` does not list them. The watcher
does not open them either, so a package install running in one is never held
up by devstack.

A file git ignores (`.gitignore` at any depth, `.git/info/exclude`, the
global `core.excludesFile`) starts no candidate, even when `watch` matches
it: it is not in the snapshot a build runs in, so it cannot change the build.
To make a file a build input, take it out of the ignore rules. `restart_on`
is not affected: dev servers and containers read the working tree itself, so
`restart_on = [".env"]` restarts on an ignored `.env`.

A folder that is moved or renamed is reported by its own path, not by its
files; the files in it count when they are created or saved. A `watch`
pattern that names file types (`src/**/*.cs`) does not match the folder, so
moving a folder of sources into place starts no candidate; a pattern that
covers the folder (`src/Api/**`) does. After such a move, save one of its
files or run `devstack restart <service>`, which builds first.

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

`http` and `timeout` are required.

### Promotion ([ADR-0013](adr/0013-schema-changes-hold-promotion.md))

```toml
promote = { hold_if_changed = ["src/Data/Migrations/**"] }   # auto = true by default
```

A candidate that changed one of these paths is built and started, but waits
(`held`) until someone runs `devstack promote <service>`. Use it for schema
changes, where switching means running a migration. `auto = false` holds every
candidate. While one is held, every later candidate is held too (it carries the
held change); `promote` takes the newest. The gate opens with `promote`, with a
manual `restart` (which builds the tree as it is), or when the tree is back
where main is; it survives a daemon restart.

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
- The export is in Compose's env-file dialect (every value double-quoted,
  `$` as `$$`): Compose's `env_file`, `docker compose --env-file` and
  devstack's own `env_files` read it back as it was. `docker run
  --env-file` takes lines verbatim and does not.
- Every process also gets `DEVSTACK_SERVICE` (its name), `DEVSTACK_LANE`
  (its lane, `main` in the main working tree), `DEVSTACK_BUILD_ID` (the
  build it runs, a built service only) and `DEVSTACK_SLOT` (`main`, or
  `candidate` while it starts beside the running one; a lane's own instance
  is `main` too), so it can tell its instances apart in its own logs or
  telemetry.
- On Windows, variable names that differ only in case are one variable: the
  later layer wins, and `${PATH}` finds `Path`.
- `rewrite_hosts` needs `compose`; each entry is a service the service's own
  group starts, or `<group>.<service>` of another. `workers.env` names at
  least one variable.

### Hot-reload mode

```toml
[services.api.hot_reload]
run = "dotnet watch run --non-interactive"   # instead of the build and `run`
cwd = "src/Api"                              # default: the service's `cwd`
```

`run` is required and interpolated like the service's `run`, without
`{artifact}` (there is no build); `cwd` is a plain path.

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
| `lanes = false` | runs in the main working tree only, never in a lane: for what may run once per machine (a tray app, something holding a global lock); see `[lanes]` |
| `platforms = ["windows"]` | only on these platforms (`windows`, `linux`, `macos`) |
| `ensure_dirs = ["{repo}/data/files"]` | created before start |
| `prepare = { run = "npm ci", inputs = ["web/package-lock.json"] }` | installs what a copy of the source lacks (see below) |
| `timeout = "10m"` | `kind = "job"` only: how long it may run before it counts as failed and is stopped; default 30 minutes. `devstack stop <job>` ends it at once |

### `prepare`: what a copy of the source lacks

devstack builds in a snapshot, and a lane runs its dev servers from one
(below); a snapshot holds tracked and untracked non-ignored files only, so
`node_modules` and the like are not in it. `prepare` runs in the snapshot
before the service is built there, and in a lane's copy before the service
starts from it; again only when a file of `inputs` changed (none: once per
copy). `cwd` defaults to the service's. `run` is required and, like `cwd`,
used as written: placeholders and `${VAR}` are an error there. `inputs` are
globs. Its output goes to
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

## `[lanes]`: other worktrees ([ADR-0024](adr/0024-a-lane-is-a-supervisor-in-the-main-trees-daemon.md), [ADR-0030](adr/0030-each-lane-has-its-own-infrastructure.md); on other machines [ADR-0025](adr/0025-a-lane-can-run-on-another-node.md), [ADR-0028](adr/0028-lane-images.md), [ADR-0029](adr/0029-source-sync-and-port-forwarding-for-remote-lanes.md), [ADR-0035](adr/0035-lanes-can-be-shared-with-the-pool.md), and [pool.md](pool.md))

```toml
[lanes]
port_step = 100   # the default: lane 1 runs api 5000 on 5100, lane 2 on 5200
# For lanes on another machine of the pool (ADR-0025, ADR-0028), one of:
image      = "registry.example/shop-lane:1"   # a Linux image with the stack's toolchains
dockerfile = "dev/lane.Dockerfile"            # or one the node builds from the lane's source
sync       = ["web/certs/*.pem"]              # ignored files such a lane needs all the same
share      = ["team:qa"]                      # whom such a lane is shared with by default (`*`: everyone); none: nobody
keep_data  = true                             # such a lane keeps its data on its node for its next start there (ADR-0038)
```

`port_step` is 1-10000. `dockerfile` and `sync` stay inside the repository
(see "Names, paths, env files"); `sync` holds globs; `image`, if set, is
not empty.

Every `git worktree` of the repository is a **lane** of the main tree's
daemon, the moment git creates it; the dashboard shows it under the stack,
named by its branch. `devstack up` in it starts it: its own build, its own
ports (moved by `port_step` times the lane's number), **its own containers**
([ADR-0030](adr/0030-each-lane-has-its-own-infrastructure.md)), no background
workers (`devstack up --workers` turns them on). Every devstack command and the
MCP server in the worktree act on its lane.

Worktrees are git's (or your tool's): devstack never creates or removes one,
and nothing of a lane runs inside its worktree. Builds run in the lane's
snapshot, and so do its dev servers, tools and hot-reload mode; the snapshot
follows the worktree within moments of a change (hot module replacement keeps
working), and `{repo}` is the snapshot for them. So `git worktree remove`
works while the lane runs, and the lane stops with it. Ignored files are not
in the snapshot: `prepare` installs what is missing.

- A lane's containers are a Compose project of their own,
  `<project>-<lane>`, from its worktree's Compose files, with an overlay
  devstack writes (`.devstack/lane-<name>/compose/<group>.yml`): host ports
  moved by the lane's offset, no fixed `container_name`, fixed-name networks
  and volumes renamed `<name>-<lane>`. Its first start migrates and seeds its
  own data. `lane stop` removes its containers and keeps their volumes; a
  removed worktree takes them all.
- `share`: a lane on a node is private unless shared (ADR-0035): this is
  whom it is shared with until its owner says otherwise (`devstack lane
  share <lane> --with <id|tag|*>`, or the lane's share button). Members it
  is shared with see it in their dashboard and open its services (those
  with a `port`, and its Compose containers the dashboard links as a web
  page: a `urls` entry with `http`/`https`, or a published port without
  one; `urls = { db = "" }` keeps a container out) through its node, and
  view it read only: its state, logs and events (ADR-0040, never its
  environment); they do not control it. A group's `shared` containers are the main tree's and
  never shared.
- `sync`: a lane on another machine gets the worktree's files without the
  ignored ones, like a local lane's snapshot. What it needs all the same, a
  TLS certificate made for `localhost` by a local CA the browser trusts, say,
  is named here: globs relative to the repository, taken from the worktree
  or, where it has none, from the main tree. Without it, a dev server that
  makes its own certificate on the node makes one no browser here trusts.
- `[compose.<group>] shared = ["mailbox"]`: services a lane uses from the main
  tree instead of starting its own, for what holds nothing per lane (a
  telemetry dashboard, a fake SMTP server); reached by their published ports.
- Before a lane starts, devstack checks that the machine has about the memory
  the main tree takes now and refuses otherwise (`devstack up --force` starts it
  anyway).
- `{lane}` and `{lane_suffix}` (empty in the main tree) are still there, e.g.
  for a name that shows up outside the stack; data needs them no longer.
- `lanes = false` keeps a service in the main working tree: a lane neither
  starts it nor waits for it (`after`), `devstack start` refuses it there, and
  the main tree's instance serves every lane. It keeps its background workers
  while a lane holds the main slot, and `promote --lane` leaves it alone.
- A dev server runs in a lane only if its `run` takes `{port}` (otherwise its
  fixed port is the main tree's).
- `devstack promote --lane <name>` lets a lane's services answer on the main
  ports (dev servers through a TCP forward); `--lane main` gives them back.
- `devstack lane list`; `devstack lane start <name>` starts a lane from
  anywhere in the repository, as `devstack up` in its worktree does;
  `devstack lane stop <name>` stops a lane (its builds stay, the worktree
  too); `devstack lane reset <name>` throws a stopped lane's data away (its
  containers and volumes, here and on the nodes it ran on), so it starts
  fresh; `devstack lane prune [--older-than 7d] [--dry-run]` removes what
  lanes whose worktree is gone left behind, here and on the pool's nodes
  (ADR-0038).

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

`description` and `steps` (at least one) are required; `requires` names
services. Each step has exactly one action: `exec` (with `cwd`, `timeout`),
`http` (`METHOD url`, METHOD one of `GET`, `POST`, `PUT`, `PATCH`, `DELETE`;
with `headers`, `body`, `timeout`), `start`, `stop`, `restart` or `run` (a
service of kind `job`). A key of another action (`cwd` on an `http` step,
`headers` on an `exec` step) is an error. `timeout` bounds an `exec` step too
(default: none; `http` 30 s); a run can be cancelled
(`POST /v1/runs/{id}/cancel`). `exec` runs in the repository, `cwd` relative
to it; in a lane in the lane's copy, never in its worktree, and `{repo}` is
that copy. Steps may use `{repo}`, `{lane}`, `{lane_suffix}`,
`{service.<name>.url}`, `{service.<name>.port}`, `{compose.…}` (the
published address of a Compose service's port, as for services) and
`${VAR}`; `{port}`, `{artifact}` and `{is_main}` have no value in a command.
Commands run with
`devstack run <name>`, from the dashboard and through MCP (`run_command`). A
command with `confirm` runs from MCP only with `confirm: true`, which an agent
sends only after the user agreed.

## `[profiles.<name>]`: how a tree runs for a purpose ([ADR-0041](adr/0041-a-profile-says-how-a-lane-runs-for-a-purpose.md))

A lane, or the main tree, runs the stack as written for daily work: dev
servers that watch, no workers in a lane, the data the seed jobs made once.
A profile is a named overlay chosen when a tree starts, for another purpose:
an acceptance run of a given commit, say.

```toml
[profiles.acceptance]
description = "the end-to-end suite on a build"
workers   = true                         # background workers in a lane (as --workers)
data      = "fresh"                      # the tree's own volumes go first: seeded anew
autostart = ["api", "web", "worker"]     # these start, instead of each service's autostart
env       = { BASE_URL = "{service.web.url}", JOBS_URL = "{compose.app.scheduler.4000}" }
command   = "e2e"                        # `up --profile acceptance --run` runs it once ready

[profiles.acceptance.services.web]
kind   = "static"                        # a build served as files: no dev server, no watch
build  = "npm run build -- --configuration e2e"
output = "dist/web/browser"
```

```sh
devstack up --profile acceptance [--run]              # in a worktree: its lane
devstack lane start <lane> --profile acceptance --run  # from the main tree
```

- **What it may set:** `workers`, `data` (`keep`, the default, or
  `fresh`), `env` (for every service and every command's `exec` steps, with
  the usual placeholders), `autostart`, `command` (one of `[commands]`), and
  per service `kind`, `build`, `output`, `run`, `spa`, `cwd`, `health` and
  `env`. A service it does not name runs as the file says.
- **Checked like the rest of the file:** each profile's result must be a
  valid stack (a `static` needs `build`, `output` and `port`); a service, a
  command or a key that is not there is an error.
- **A tree runs with it until it stops.** `status`, `lane list` and the
  dashboard show it; a start without `--profile` runs without one. In the
  main tree the daemon starts with it: another profile needs `devstack down`
  first, and `data = "fresh"` there needs `--yes` (its data usually matters).
- A lane takes its profiles from its own worktree's stack file, so a branch
  can bring one. On a node, `fresh` empties its slot's volumes (ADR-0038).
- A file with `[profiles]` should say `requires = "0.11.0"` in `[stack]`.

## `[groups.<name>]`: views

```toml
[groups.app]
label   = "Application"
order   = 1
members = ["api", "web", "compose.app.db", "command.seed"]
```

Labels for the dashboard and `devstack status`, not dependencies: a member may
be in several groups, and members of no group appear under "Other".
`members` is required, not empty, lists each member once, and names what
exists (a `compose.<group>.<service>` member is a service its group starts).

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
| `{service.<name>.port}` | that service's port |
| `{compose.<group>.<service>.<port>}` | where a container port is published, as a URL (`http://127.0.0.1:15432`) |
| `{compose.<group>.<service>.<port>.host}`, `.port`, `.address` | its host (`127.0.0.1`), its port (`15432`), or both (`127.0.0.1:15432`), for connection strings: `Server={compose.app.sql.1433.host},{compose.app.sql.1433.port}`, `Host=…host;Port=…port`. In a lane: the lane's own container, or the main tree's for a `shared` one |
| `${VAR}`, `${VAR:-default}` | from the env files and the process environment; placeholders inside a default are resolved too |

`{{` and `}}` are literal braces, inside a `${VAR:-default}` too (where a
single `}` ends the default). Durations: `500ms`, `30s`, `5m`, `1h` (a bare
number is seconds).

Where they have a value: `{port}` only in a service with a `port`,
`{artifact}` only in a service with `build` (not in `hot_reload.run`),
`{service.<name>.…}` only for a service with a `port`, `{compose.…}` only
for a service its group starts. `build` and `output` take `{repo}` only;
commands no `{port}`, `{artifact}` or `{is_main}`; `cwd`,
`prepare.run`, `prepare.cwd` and `hot_reload.cwd` none at all.
